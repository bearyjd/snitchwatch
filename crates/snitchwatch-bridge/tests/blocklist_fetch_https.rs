//! The production blocklist fetcher against a local TLS server (issue #45).
//!
//! Proves, without any network:
//! - `https_only` covers redirects: `https → http` and `https → file` fail and
//!   the plain-http listener never sees a connection (`https → https` is
//!   followed, so the redirect tests aren't vacuous);
//! - the body cap is enforced while streaming: an endless chunked body stops
//!   at the cap instead of being buffered until the timeout;
//! - a declared `Content-Length` over the cap is refused before reading.
//!
//! The server certificate (`tests/fixtures/tls/localhost-test-only.*.der`) is
//! a self-signed, test-only key pair for `IP:127.0.0.1`, generated once with
//! the `openssl` CLI. It secures nothing. The test client is the production
//! [`client_builder`] plus trust in that certificate, nothing else.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use snitchwatch_bridge::blocklists::fetcher::{
    client_builder, fetch, fetch_with_cap, FetchOutcome, MAX_BODY_BYTES,
};
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::TlsAcceptor;

const CERT_DER: &[u8] = include_bytes!("../../../tests/fixtures/tls/localhost-test-only.cert.der");
const KEY_DER: &[u8] = include_bytes!("../../../tests/fixtures/tls/localhost-test-only.key.der");
const HOSTS_BODY: &str = "0.0.0.0 ads.example\n0.0.0.0 tracker.example\n";
const CHUNK: usize = 64 * 1024;

/// A local HTTPS server plus a plain-http one that counts connections.
struct Servers {
    https: SocketAddr,
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

async fn start_servers() -> Servers {
    let plain = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let plain_addr = plain.local_addr().unwrap();
    let plain_http_accepts = Arc::new(AtomicUsize::new(0));
    let accepts = plain_http_accepts.clone();
    let plain_task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = plain.accept().await {
            accepts.fetch_add(1, Ordering::SeqCst);
            let _ = read_request_path(&mut stream).await;
            let _ = write_body(&mut stream, HOSTS_BODY).await;
        }
    });

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
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let tls = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let https = tls.local_addr().unwrap();
    let endless_bytes_sent = Arc::new(AtomicU64::new(0));
    let sent = endless_bytes_sent.clone();
    let tls_task = tokio::spawn(async move {
        while let Ok((stream, _)) = tls.accept().await {
            let acceptor = acceptor.clone();
            let sent = sent.clone();
            tokio::spawn(async move {
                let Ok(mut stream) = acceptor.accept(stream).await else {
                    return;
                };
                let Some(path) = read_request_path(&mut stream).await else {
                    return;
                };
                let _ = respond(&mut stream, &path, https, plain_addr, &sent).await;
            });
        }
    });

    Servers {
        https,
        plain_http_accepts,
        endless_bytes_sent,
        tasks: vec![plain_task, tls_task],
    }
}

async fn respond<S: AsyncWrite + Unpin>(
    stream: &mut S,
    path: &str,
    https: SocketAddr,
    plain: SocketAddr,
    sent: &AtomicU64,
) -> std::io::Result<()> {
    match path {
        "/hosts.txt" => write_body(stream, HOSTS_BODY).await,
        "/to-https" => redirect(stream, &format!("https://{https}/hosts.txt")).await,
        "/to-http" => redirect(stream, &format!("http://{plain}/hosts.txt")).await,
        "/to-file" => redirect(stream, "file:///etc/hosts").await,
        "/declared-huge" => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                MAX_BODY_BYTES + 1
            );
            stream.write_all(head.as_bytes()).await?;
            stream.flush().await
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

/// The production client configuration, trusting only the test certificate.
fn test_client() -> reqwest::Client {
    client_builder()
        .add_root_certificate(reqwest::Certificate::from_der(CERT_DER).unwrap())
        .build()
        .unwrap()
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
    let client = test_client();
    for path in ["/hosts.txt", "/to-https"] {
        match fetch(&client, &servers.url(path)).await {
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
        fetch(&test_client(), &servers.url("/to-http")).await,
        "https -> http redirect",
    );
    assert!(
        reason.contains("redirect") || reason.contains("scheme"),
        "unexpected reason: {reason}"
    );
    assert_eq!(
        servers.plain_http_accepts.load(Ordering::SeqCst),
        0,
        "the fetcher followed a redirect to plain http"
    );
}

#[tokio::test]
async fn a_redirect_to_a_file_url_is_refused() {
    let servers = start_servers().await;
    expect_failed(
        fetch(&test_client(), &servers.url("/to-file")).await,
        "https -> file redirect",
    );
}

#[tokio::test]
async fn a_declared_oversized_body_is_refused_before_reading() {
    let servers = start_servers().await;
    let reason = expect_failed(
        fetch(&test_client(), &servers.url("/declared-huge")).await,
        "declared oversized body",
    );
    assert!(reason.contains("too large"), "unexpected reason: {reason}");
}

/// A small cap keeps this fast; the next test runs the production cap.
#[tokio::test]
async fn an_endless_body_stops_at_the_cap() {
    let servers = start_servers().await;
    let cap = 256 * 1024;
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        fetch_with_cap(&test_client(), &servers.url("/endless"), cap),
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
        fetch(&test_client(), &servers.url("/endless")),
    )
    .await
    .expect("the fetch kept reading an endless body");
    let reason = expect_failed(outcome, "endless body");
    assert!(reason.contains("too large"), "unexpected reason: {reason}");
    // The client stopped near the cap: the server got no further than the
    // cap plus what socket and TLS buffers absorb.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let sent = servers.endless_bytes_sent.load(Ordering::SeqCst);
    assert!(
        sent < MAX_BODY_BYTES + 16 * 1024 * 1024,
        "server streamed {sent} bytes; the client did not stop at the cap"
    );
}
