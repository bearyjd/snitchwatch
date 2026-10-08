//! HTTPS fetcher for blocklist subscriptions.
//!
//! Discipline: a failed fetch must NEVER overwrite the prior cached entries.
//! On error we update the subscription's `last_fetch_status` to
//! `Failed { reason }` and leave the entries table untouched. The Blocklists
//! tab then renders "last updated 4h ago — last fetch failed".
//!
//! Subscription URLs come from any GUI that can reach the bridge (in system
//! mode, every `snitchwatch-ui` member), so production fetches are bounded
//! (issue #45):
//! - `https` only, including every redirect hop (`https_only` on the client,
//!   plus an explicit scheme check in [`fetch`]); there is no `file://` path;
//! - only addresses [`fetch_guard::is_allowed_fetch_target`] allows (no
//!   loopback, link-local, CGNAT, …), checked on resolution and for IP
//!   literals at validation and on every redirect; no proxy;
//! - at most [`MAX_REDIRECTS`] redirects and [`FETCH_TIMEOUT`] per fetch;
//! - the decoded body is read chunk by chunk against a running
//!   [`MAX_BODY_BYTES`] cap (so a gzip bomb stops too), and a list holds at
//!   most `format::MAX_ENTRIES` hosts;
//! - transport and HTTP errors reach the user as one generic reason; the
//!   detail is only logged.
//!
//! Tests that need list content without a network implement
//! [`BlocklistFetch`] themselves (see `BlocklistsManager::with_fetcher`).

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use reqwest::{redirect, Client};
use tracing::{debug, warn};

use crate::blocklists::fetch_guard::{
    check_literal_host, is_allowed_fetch_target, GuardedResolver,
};
use crate::blocklists::format::{parse, sniff_format, ListFormat, TooManyEntries};

#[derive(Debug, Clone)]
pub enum FetchOutcome {
    Ok {
        hosts: Vec<String>,
        format: ListFormat,
    },
    Failed {
        reason: String,
    },
}

pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Hard cap on a (decoded) list body, enforced while streaming (64 MiB).
pub const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;
/// Longest subscription URL accepted.
pub const MAX_URL_LEN: usize = 2048;
pub const MAX_REDIRECTS: usize = 5;
/// The only reason a GUI sees for a transport, TLS, redirect or HTTP error.
pub const DOWNLOAD_FAILED_REASON: &str = "Couldn't download the list";

/// The production client: [`client_builder_with`] the production address
/// policy.
pub fn build_client() -> Client {
    client_builder_with(is_allowed_fetch_target)
        .build()
        .expect("reqwest client builds")
}

/// The client configuration, parameterized only by the address policy so the
/// local-server tests can allow their own loopback address.
pub(crate) fn client_builder_with(allow: fn(IpAddr) -> bool) -> reqwest::ClientBuilder {
    Client::builder()
        .timeout(FETCH_TIMEOUT)
        .https_only(true)
        .no_proxy()
        .dns_resolver(Arc::new(GuardedResolver { allow }))
        .redirect(redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else if let Err(reason) = check_literal_host(attempt.url(), allow) {
                attempt.error(reason)
            } else {
                attempt.follow()
            }
        }))
        .user_agent(concat!("snitchwatch/", env!("CARGO_PKG_VERSION")))
}

/// A subscription URL must be `https`, name a host that isn't a refused IP
/// literal, and be at most [`MAX_URL_LEN`] bytes. Returns the parsed URL or a
/// user-facing reason.
pub fn validate_subscription_url(url: &str) -> Result<reqwest::Url, String> {
    validate_with(url, is_allowed_fetch_target)
}

fn validate_with(url: &str, allow: fn(IpAddr) -> bool) -> Result<reqwest::Url, String> {
    if url.len() > MAX_URL_LEN {
        return Err(format!(
            "URL not allowed: it is longer than {MAX_URL_LEN} characters"
        ));
    }
    let parsed =
        reqwest::Url::parse(url).map_err(|_| "URL not allowed: not a valid URL".to_string())?;
    if parsed.scheme() != "https" {
        return Err("URL not allowed: only https:// addresses are supported".to_string());
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err("URL not allowed: it has no host".to_string());
    }
    check_literal_host(&parsed, allow)?;
    Ok(parsed)
}

/// Fetch and parse a list with the production body cap and address policy.
pub(crate) async fn fetch(client: &Client, url: &str) -> FetchOutcome {
    fetch_checked(client, url, MAX_BODY_BYTES, is_allowed_fetch_target).await
}

/// [`fetch`] with an explicit body cap and address policy (tests).
pub(crate) async fn fetch_checked(
    client: &Client,
    url: &str,
    max_body_bytes: u64,
    allow: fn(IpAddr) -> bool,
) -> FetchOutcome {
    debug!(url, "blocklist fetch begin");
    let parsed = match validate_with(url, allow) {
        Ok(parsed) => parsed,
        Err(reason) => return FetchOutcome::Failed { reason },
    };
    let mut resp = match client.get(parsed).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(url, error = %e, "blocklist fetch transport error");
            return download_failed();
        }
    };
    let status = resp.status();
    if !status.is_success() {
        warn!(url, %status, "blocklist fetch non-2xx");
        return download_failed();
    }
    if let Some(declared) = resp.content_length().filter(|n| *n > max_body_bytes) {
        warn!(url, declared, "blocklist body declared over the cap");
        return too_large(max_body_bytes);
    }
    let mut body: Vec<u8> = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() as u64 + chunk.len() as u64 > max_body_bytes {
                    warn!(url, max_body_bytes, "blocklist body exceeds cap; aborted");
                    return too_large(max_body_bytes);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(e) => {
                warn!(url, error = %e, "blocklist body read failed");
                return download_failed();
            }
        }
    }
    // Parsing a large list is CPU work: keep it off the async threads.
    tokio::task::spawn_blocking(move || process_body(&String::from_utf8_lossy(&body)))
        .await
        .unwrap_or_else(|e| {
            warn!(url, error = %e, "blocklist parse task failed");
            download_failed()
        })
}

fn download_failed() -> FetchOutcome {
    FetchOutcome::Failed {
        reason: DOWNLOAD_FAILED_REASON.to_string(),
    }
}

fn too_large(max_body_bytes: u64) -> FetchOutcome {
    const MIB: u64 = 1024 * 1024;
    let limit = if max_body_bytes >= MIB && max_body_bytes.is_multiple_of(MIB) {
        format!("{} MiB", max_body_bytes / MIB)
    } else {
        format!("{max_body_bytes} bytes")
    };
    FetchOutcome::Failed {
        reason: format!("The list is too large (over {limit})"),
    }
}

/// Where a [`BlocklistsManager`](crate::blocklists::BlocklistsManager) gets
/// list bodies from. Production uses [`HttpsFetcher`]; tests inject a fixture
/// fetcher so no test touches the network and production keeps no
/// file-reading path.
// clippy 1.99's `double_must_use` fires on async_trait's generated
// `#[must_use]` methods (same as `RuleSink`).
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait BlocklistFetch: Send + Sync + 'static {
    async fn fetch(&self, url: &str) -> FetchOutcome;
}

/// The production [`BlocklistFetch`]: [`fetch`] with [`build_client`].
pub struct HttpsFetcher {
    client: Client,
}

impl HttpsFetcher {
    pub fn new() -> Self {
        Self {
            client: build_client(),
        }
    }
}

impl Default for HttpsFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl BlocklistFetch for HttpsFetcher {
    async fn fetch(&self, url: &str) -> FetchOutcome {
        fetch(&self.client, url).await
    }
}

pub fn process_body(body: &str) -> FetchOutcome {
    let format = sniff_format(body);
    match parse(format, body) {
        Ok(hosts) => FetchOutcome::Ok { hosts, format },
        Err(TooManyEntries { limit }) => FetchOutcome::Failed {
            reason: format!("The list has too many entries (more than {limit})"),
        },
    }
}

#[cfg(test)]
#[path = "fetcher_tls_tests.rs"]
mod tls_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_outcome_ok_carries_parsed_hosts() {
        let outcome = FetchOutcome::Ok {
            hosts: vec!["a.example".to_string(), "b.example".to_string()],
            format: crate::blocklists::format::ListFormat::Domains,
        };
        match outcome {
            FetchOutcome::Ok { hosts, .. } => assert_eq!(hosts.len(), 2),
            _ => panic!("expected Ok"),
        }
    }

    #[test]
    fn fetch_outcome_failed_carries_reason() {
        let outcome = FetchOutcome::Failed {
            reason: "HTTP 503".to_string(),
        };
        match outcome {
            FetchOutcome::Failed { reason } => assert_eq!(reason, "HTTP 503"),
            _ => panic!("expected Failed"),
        }
    }

    /// Parses the fixture body directly; nothing here goes through a URL
    /// (production has no `file://` path).
    #[test]
    fn parses_stevenblack_fixture_body() {
        let path = std::env::current_dir()
            .unwrap()
            .join("../../tests/fixtures/blocklists/stevenblack-tiny.txt");
        let body = std::fs::read_to_string(&path).expect("fixture readable");
        let outcome = process_body(&body);
        match outcome {
            FetchOutcome::Ok { hosts, format } => {
                assert_eq!(format, crate::blocklists::format::ListFormat::Hosts);
                assert!(hosts.contains(&"doubleclick.net".to_string()));
                assert!(!hosts.iter().any(|h| h == "localhost"));
            }
            FetchOutcome::Failed { reason } => panic!("expected Ok, got Failed: {reason}"),
        }
    }

    #[test]
    fn subscription_urls_must_be_https_with_a_host_and_bounded() {
        let long = format!("https://x.example/{}", "a".repeat(MAX_URL_LEN));
        for (url, why) in [
            ("http://x.example/hosts", "plain http"),
            ("file:///dev/zero", "file"),
            ("ftp://x.example/hosts", "ftp"),
            ("HTTP://x.example/hosts", "upper-case http"),
            ("not a url", "garbage"),
            ("https://", "no host"),
            ("", "empty"),
            (long.as_str(), "too long"),
        ] {
            assert!(
                validate_subscription_url(url).is_err(),
                "{why} URL {url:?} must be rejected"
            );
        }
        let ok = validate_subscription_url("https://x.example/hosts.txt?branch=main")
            .expect("a plain https URL is valid");
        assert_eq!(ok.as_str(), "https://x.example/hosts.txt?branch=main");
    }

    /// The explicit scheme check in `fetch` must refuse `http://` even with a
    /// client that lacks `https_only` — and never open a connection.
    #[tokio::test]
    async fn fetch_refuses_plain_http_without_connecting() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepts = Arc::new(AtomicUsize::new(0));
        let counter = accepts.clone();
        let server = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            while let Ok((mut stream, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let body = "0.0.0.0 ads.example\n";
                let _ = stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            }
        });
        let url = format!("http://127.0.0.1:{port}/hosts.txt");
        for client in [reqwest::Client::new(), build_client()] {
            match fetch(&client, &url).await {
                FetchOutcome::Failed { reason } => {
                    assert!(reason.contains("https"), "unexpected reason: {reason}")
                }
                FetchOutcome::Ok { hosts, .. } => panic!("http:// was fetched: {hosts:?}"),
            }
        }
        server.abort();
        assert_eq!(
            accepts.load(Ordering::SeqCst),
            0,
            "http:// opened a connection"
        );
    }

    /// `file://` used to be read with no size cap (`file:///dev/zero` OOMs the
    /// bridge). Production has no file-reading fetch path at all now. Uses a
    /// small fixture so re-adding the old branch fails the test instead of
    /// hanging it.
    #[tokio::test]
    async fn fetch_refuses_file_urls() {
        let fixture = std::env::current_dir()
            .unwrap()
            .join("../../tests/fixtures/blocklists/domains-tiny.txt")
            .canonicalize()
            .unwrap();
        let url = format!("file://{}", fixture.display());
        for client in [reqwest::Client::new(), build_client()] {
            match fetch(&client, &url).await {
                FetchOutcome::Failed { reason } => {
                    assert!(reason.contains("https"), "unexpected reason: {reason}")
                }
                FetchOutcome::Ok { hosts, .. } => panic!("file:// was read: {hosts:?}"),
            }
        }
    }

    #[test]
    fn https_fetcher_is_object_safe() {
        let fetcher: std::sync::Arc<dyn BlocklistFetch> = std::sync::Arc::new(HttpsFetcher::new());
        drop(fetcher);
    }

    /// Issue #45: a list over the entry limit fails with a clear status.
    #[test]
    fn a_list_over_the_entry_limit_fails() {
        use crate::blocklists::format::MAX_ENTRIES;
        let body: String = (0..=MAX_ENTRIES).map(|i| format!("h{i}.x\n")).collect();
        match process_body(&body) {
            FetchOutcome::Failed { reason } => assert!(reason.contains("too many entries")),
            FetchOutcome::Ok { hosts, .. } => panic!("accepted {} hosts", hosts.len()),
        }
    }

    #[test]
    fn rejects_garbage_binary_body() {
        let garbage: Vec<u8> = vec![0u8, 1, 2, 3, 0xff, 0xfe, 0xfd, 0xfc];
        let body = String::from_utf8_lossy(&garbage).into_owned();
        let outcome = process_body(&body);
        match outcome {
            FetchOutcome::Failed { .. } => {}
            FetchOutcome::Ok { hosts, .. } if hosts.is_empty() => {}
            FetchOutcome::Ok { hosts, .. } => panic!("garbage parsed as {hosts:?}"),
        }
    }
}
