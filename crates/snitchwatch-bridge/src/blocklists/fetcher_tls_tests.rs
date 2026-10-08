//! The fetcher against local TLS servers (issue #45), with no network.
//!
//! The production client refuses loopback, so these tests build it with one
//! loopback address allowed, `127.0.0.2` (`allow_test_server`), and run a
//! second server on the refused `127.0.0.1`. Its accept counter proves a
//! refused target is never even connected to:
//! - an IP-literal URL (which never reaches a resolver), a name resolving to
//!   it (`localhost`), and a redirect to either;
//! - an `https → http` redirect (a plain-http listener counts accepts too);
//! - the body cap counts decoded bytes while streaming: an endless chunked
//!   body and a gzip bomb both stop at the cap;
//! - transport failures reach the user as one generic reason.
//!
//! The server certificate (`tests/fixtures/tls/localhost-test-only.*.der`) is
//! a self-signed, test-only key pair for `127.0.0.1`, `127.0.0.2` and
//! `localhost`, generated with the `openssl` CLI. It secures nothing. This
//! module is `cfg(test)`: no trust or address override ships.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::TlsAcceptor;

use super::*;

const CERT_DER: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/tls/localhost-test-only.cert.der"
));
const KEY_DER: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/tls/localhost-test-only.key.der"
));
/// ~1 MiB of hosts lines, gzip-compressed to ~2.5 KiB.
const GZIP_BOMB: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/blocklists/gzip-1mib-hosts.gz"
));
const HOSTS_BODY: &str = "0.0.0.0 ads.example\n0.0.0.0 tracker.example\n";
const CHUNK: usize = 64 * 1024;
const TEST_SERVER_IP: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);
const REFUSED_IP: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 1);

/// The production policy plus the test server's own address.
fn allow_test_server(ip: IpAddr) -> bool {
    ip == IpAddr::V4(TEST_SERVER_IP) || is_allowed_fetch_target(ip)
}

struct Servers {
    /// The allowed TLS server.
    https: SocketAddr,
    /// A TLS server on the refused address.
    refused: SocketAddr,
    refused_accepts: Arc<AtomicUsize>,
    plain_http_accepts: Arc<AtomicUsize>,
    endless_bytes_sent: Arc<AtomicU64>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Servers {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Servers {
    fn url(&self, path: &str) -> String {
        format!("https://{}{path}", self.https)
    }
}

fn acceptor() -> TlsAcceptor {
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from(CERT_DER.to_vec())],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY_DER.to_vec())),
    )
    .unwrap();
    TlsAcceptor::from(Arc::new(config))
}

/// Accept TLS connections on `listener`, counting TCP accepts, and answer
/// with [`respond`].
fn serve_tls(
    listener: TcpListener,
    accepts: Arc<AtomicUsize>,
    ports: (SocketAddr, SocketAddr, SocketAddr),
    sent: Arc<AtomicU64>,
) -> tokio::task::JoinHandle<()> {
    let acceptor = acceptor();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            accepts.fetch_add(1, Ordering::SeqCst);
            let acceptor = acceptor.clone();
            let sent = sent.clone();
            tokio::spawn(async move {
                let Ok(mut stream) = acceptor.accept(stream).await else {
                    return;
                };
                let Some(path) = read_request_path(&mut stream).await else {
                    return;
                };
                let _ = respond(&mut stream, &path, ports, &sent).await;
            });
        }
    })
}

async fn start_servers() -> Servers {
    let plain = TcpListener::bind((TEST_SERVER_IP, 0)).await.unwrap();
    let plain_addr = plain.local_addr().unwrap();
    let plain_http_accepts = Arc::new(AtomicUsize::new(0));
    let counter = plain_http_accepts.clone();
    let plain_task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = plain.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            let _ = read_request_path(&mut stream).await;
            let _ = write_body(&mut stream, HOSTS_BODY).await;
        }
    });

    let tls = TcpListener::bind((TEST_SERVER_IP, 0)).await.unwrap();
    let https = tls.local_addr().unwrap();
    let refused_listener = TcpListener::bind((REFUSED_IP, 0)).await.unwrap();
    let refused = refused_listener.local_addr().unwrap();
    let ports = (https, refused, plain_addr);
    let endless_bytes_sent = Arc::new(AtomicU64::new(0));
    let refused_accepts = Arc::new(AtomicUsize::new(0));
    let tls_task = serve_tls(
        tls,
        Arc::new(AtomicUsize::new(0)),
        ports,
        endless_bytes_sent.clone(),
    );
    let refused_task = serve_tls(
        refused_listener,
        refused_accepts.clone(),
        ports,
        Arc::new(AtomicU64::new(0)),
    );

    Servers {
        https,
        refused,
        refused_accepts,
        plain_http_accepts,
        endless_bytes_sent,
        tasks: vec![plain_task, tls_task, refused_task],
    }
}

async fn respond<S: AsyncWrite + Unpin>(
    stream: &mut S,
    path: &str,
    (https, refused, plain): (SocketAddr, SocketAddr, SocketAddr),
    sent: &AtomicU64,
) -> std::io::Result<()> {
    match path {
        "/hosts.txt" => write_body(stream, HOSTS_BODY).await,
        "/to-https" => redirect(stream, &format!("https://{https}/hosts.txt")).await,
        "/to-http" => redirect(stream, &format!("http://{plain}/hosts.txt")).await,
        "/to-file" => redirect(stream, "file:///etc/hosts").await,
        "/to-refused-ip" => redirect(stream, &format!("https://{refused}/hosts.txt")).await,
        "/to-refused-name" => {
            let location = format!("https://localhost:{}/hosts.txt", refused.port());
            redirect(stream, &location).await
        }
        "/declared-huge" => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                MAX_BODY_BYTES + 1
            );
            stream.write_all(head.as_bytes()).await?;
            stream.flush().await
        }
        "/gzip-bomb" => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                GZIP_BOMB.len()
            );
            stream.write_all(head.as_bytes()).await?;
            stream.write_all(GZIP_BOMB).await?;
            stream.shutdown().await
        }
        "/endless" => {
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await?;
            let line = "0.0.0.0 x.example\n";
            let data = line.repeat(CHUNK / line.len());
            let frame = format!("{:x}\r\n{data}\r\n", data.len());
            loop {
                stream.write_all(frame.as_bytes()).await?;
                sent.fetch_add(data.len() as u64, Ordering::SeqCst);
            }
        }
        _ => {
            stream
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .await
        }
    }
}

async fn read_request_path<S: tokio::io::AsyncRead + Unpin>(stream: &mut S) -> Option<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 * 1024 || stream.read(&mut byte).await.ok()? == 0 {
            return None;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head);
    head.split_whitespace().nth(1).map(str::to_string)
}

async fn write_body<S: AsyncWrite + Unpin>(stream: &mut S, body: &str) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

async fn redirect<S: AsyncWrite + Unpin>(stream: &mut S, location: &str) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

/// The production client configuration with the test server's address
/// allowed and its certificate trusted, nothing else.
fn test_client() -> reqwest::Client {
    client_builder_with(allow_test_server)
        .add_root_certificate(reqwest::Certificate::from_der(CERT_DER).unwrap())
        .build()
        .unwrap()
}

async fn fetch_test(url: &str, cap: u64) -> FetchOutcome {
    fetch_checked(&test_client(), url, cap, allow_test_server).await
}

fn expect_failed(outcome: FetchOutcome, what: &str) -> String {
    match outcome {
        FetchOutcome::Failed { reason } => reason,
        FetchOutcome::Ok { hosts, .. } => panic!("{what}: expected a failure, fetched {hosts:?}"),
    }
}

#[tokio::test]
async fn https_list_and_https_redirect_are_fetched() {
    let servers = start_servers().await;
    for path in ["/hosts.txt", "/to-https"] {
        match fetch_test(&servers.url(path), MAX_BODY_BYTES).await {
            FetchOutcome::Ok { hosts, .. } => {
                assert!(
                    hosts.contains(&"ads.example".to_string()),
                    "{path}: {hosts:?}"
                )
            }
            FetchOutcome::Failed { reason } => panic!("{path}: {reason}"),
        }
    }
}

#[tokio::test]
async fn a_redirect_to_plain_http_is_refused_without_connecting() {
    let servers = start_servers().await;
    let reason = expect_failed(
        fetch_test(&servers.url("/to-http"), MAX_BODY_BYTES).await,
        "https -> http redirect",
    );
    assert_eq!(
        reason, DOWNLOAD_FAILED_REASON,
        "detail must stay in the logs"
    );
    assert_eq!(servers.plain_http_accepts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_redirect_to_a_file_url_is_refused() {
    let servers = start_servers().await;
    expect_failed(
        fetch_test(&servers.url("/to-file"), MAX_BODY_BYTES).await,
        "https -> file redirect",
    );
}

/// An IP-literal host never reaches the resolver: validation must refuse it.
#[tokio::test]
async fn a_refused_ip_literal_is_never_connected_to() {
    let servers = start_servers().await;
    let url = format!("https://{}/hosts.txt", servers.refused);
    let reason = expect_failed(fetch_test(&url, MAX_BODY_BYTES).await, "refused literal");
    assert!(reason.starts_with("URL not allowed"), "{reason}");
    assert_eq!(servers.refused_accepts.load(Ordering::SeqCst), 0);
}

/// A name resolving only to refused addresses (`localhost`) is never
/// connected to: the resolver drops them and reqwest has nothing to dial.
#[tokio::test]
async fn a_name_resolving_to_a_refused_address_is_never_connected_to() {
    let servers = start_servers().await;
    let url = format!("https://localhost:{}/hosts.txt", servers.refused.port());
    let reason = expect_failed(fetch_test(&url, MAX_BODY_BYTES).await, "refused name");
    assert_eq!(reason, DOWNLOAD_FAILED_REASON);
    assert_eq!(servers.refused_accepts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_redirect_to_a_refused_ip_literal_is_never_followed() {
    let servers = start_servers().await;
    let reason = expect_failed(
        fetch_test(&servers.url("/to-refused-ip"), MAX_BODY_BYTES).await,
        "redirect to a refused literal",
    );
    assert_eq!(reason, DOWNLOAD_FAILED_REASON);
    assert_eq!(servers.refused_accepts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_redirect_to_a_refused_name_is_never_followed() {
    let servers = start_servers().await;
    let reason = expect_failed(
        fetch_test(&servers.url("/to-refused-name"), MAX_BODY_BYTES).await,
        "redirect to a refused name",
    );
    assert_eq!(reason, DOWNLOAD_FAILED_REASON);
    assert_eq!(servers.refused_accepts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_declared_oversized_body_is_refused_before_reading() {
    let servers = start_servers().await;
    let reason = expect_failed(
        fetch_test(&servers.url("/declared-huge"), MAX_BODY_BYTES).await,
        "declared oversized body",
    );
    assert!(reason.contains("too large"), "unexpected reason: {reason}");
}

/// The cap counts decoded bytes: ~2.5 KiB of gzip expanding to 1 MiB stops
/// at a 256 KiB cap.
#[tokio::test]
async fn a_gzip_bomb_stops_at_the_cap() {
    let servers = start_servers().await;
    let reason = expect_failed(
        fetch_test(&servers.url("/gzip-bomb"), 256 * 1024).await,
        "gzip bomb",
    );
    assert!(reason.contains("too large"), "unexpected reason: {reason}");
    // Control: the same body under a cap it fits in is decoded and parsed.
    match fetch_test(&servers.url("/gzip-bomb"), 2 * 1024 * 1024).await {
        FetchOutcome::Ok { hosts, .. } => assert_eq!(hosts, vec!["a.example".to_string()]),
        FetchOutcome::Failed { reason } => panic!("control: {reason}"),
    }
}

/// A small cap keeps this fast; the next test runs the production cap.
#[tokio::test]
async fn an_endless_body_stops_at_the_cap() {
    let servers = start_servers().await;
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        fetch_test(&servers.url("/endless"), 256 * 1024),
    )
    .await
    .expect("the fetch kept reading an endless body");
    let reason = expect_failed(outcome, "endless body");
    assert!(reason.contains("too large"), "unexpected reason: {reason}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_endless_body_stops_at_the_production_cap() {
    let servers = start_servers().await;
    let outcome = tokio::time::timeout(
        Duration::from_secs(25),
        fetch_test(&servers.url("/endless"), MAX_BODY_BYTES),
    )
    .await
    .expect("the fetch kept reading an endless body");
    let reason = expect_failed(outcome, "endless body");
    assert_eq!(reason, "The list is too large (over 64 MiB)");
    // The client stopped near the cap: the server got no further than the
    // cap plus what socket and TLS buffers absorb.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let sent = servers.endless_bytes_sent.load(Ordering::SeqCst);
    assert!(
        sent < MAX_BODY_BYTES + 16 * 1024 * 1024,
        "server streamed {sent} bytes; the client did not stop at the cap"
    );
}
