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
//! - at most [`MAX_REDIRECTS`] redirects and [`FETCH_TIMEOUT`] per fetch;
//! - the body is read chunk by chunk against a running [`MAX_BODY_BYTES`]
//!   cap, so a chunked or endless body stops at the cap instead of being
//!   buffered whole.
//!
//! Tests that need list content without a network implement
//! [`BlocklistFetch`] themselves (see `BlocklistsManager::with_fetcher`).

use std::time::Duration;

use reqwest::{redirect, Client};
use tracing::{debug, warn};

use crate::blocklists::format::{parse, sniff_format, ListFormat};

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
/// Hard cap on a list body, enforced while streaming (64 MiB).
pub const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;
/// Longest subscription URL accepted.
pub const MAX_URL_LEN: usize = 2048;
pub const MAX_REDIRECTS: usize = 5;

/// The production client configuration. Tests that talk to a local TLS server
/// start from this and add only trust for the test certificate.
pub fn client_builder() -> reqwest::ClientBuilder {
    Client::builder()
        .timeout(FETCH_TIMEOUT)
        .https_only(true)
        .redirect(redirect::Policy::limited(MAX_REDIRECTS))
        .user_agent(concat!("snitchwatch/", env!("CARGO_PKG_VERSION")))
}

pub fn build_client() -> Client {
    client_builder().build().expect("reqwest client builds")
}

/// A subscription URL must be `https`, name a host, and be at most
/// [`MAX_URL_LEN`] bytes. Returns the parsed URL or a user-facing reason.
pub fn validate_subscription_url(url: &str) -> Result<reqwest::Url, String> {
    if url.len() > MAX_URL_LEN {
        return Err(format!("URL is longer than {MAX_URL_LEN} characters"));
    }
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("not a valid URL: {e}"))?;
    if parsed.scheme() != "https" {
        return Err("only https:// blocklist URLs are allowed".to_string());
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err("URL has no host".to_string());
    }
    Ok(parsed)
}

/// Fetch and parse a list with the production body cap.
pub async fn fetch(client: &Client, url: &str) -> FetchOutcome {
    fetch_with_cap(client, url, MAX_BODY_BYTES).await
}

/// [`fetch`] with an explicit body cap, so tests can prove the streaming cap
/// with a small one.
pub async fn fetch_with_cap(client: &Client, url: &str, max_body_bytes: u64) -> FetchOutcome {
    debug!(url, "blocklist fetch begin");
    let parsed = match validate_subscription_url(url) {
        Ok(parsed) => parsed,
        Err(reason) => return FetchOutcome::Failed { reason },
    };
    let mut resp = match client.get(parsed).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(url, error = %e, "blocklist fetch transport error");
            return FetchOutcome::Failed {
                reason: format!("transport: {e}"),
            };
        }
    };
    let status = resp.status();
    if !status.is_success() {
        warn!(url, %status, "blocklist fetch non-2xx");
        return FetchOutcome::Failed {
            reason: format!("HTTP {}", status.as_u16()),
        };
    }
    if let Some(declared) = resp.content_length().filter(|n| *n > max_body_bytes) {
        return FetchOutcome::Failed {
            reason: format!("body too large: {declared} bytes (limit {max_body_bytes})"),
        };
    }
    let mut body: Vec<u8> = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() as u64 + chunk.len() as u64 > max_body_bytes {
                    warn!(url, max_body_bytes, "blocklist body exceeds cap; aborted");
                    return FetchOutcome::Failed {
                        reason: format!("body too large: over {max_body_bytes} bytes"),
                    };
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(e) => {
                return FetchOutcome::Failed {
                    reason: format!("read body: {e}"),
                };
            }
        }
    }
    process_body(&String::from_utf8_lossy(&body))
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
    let hosts = parse(format, body);
    FetchOutcome::Ok { hosts, format }
}

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
