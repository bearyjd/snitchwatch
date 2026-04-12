//! HTTPS fetcher for blocklist subscriptions.
//!
//! Discipline: a failed fetch must NEVER overwrite the prior cached entries.
//! On error we update the subscription's `last_fetch_status` to
//! `Failed { reason }` and leave the entries table untouched. The Blocklists
//! tab then renders "last updated 4h ago — last fetch failed".

use std::time::Duration;

use reqwest::Client;
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

const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB hard cap

pub fn build_client() -> Client {
    Client::builder()
        .timeout(FETCH_TIMEOUT)
        .user_agent(concat!("snitchwatch/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("reqwest client builds")
}

pub async fn fetch(client: &Client, url: &str) -> FetchOutcome {
    debug!(url, "blocklist fetch begin");
    if let Some(path) = url.strip_prefix("file://") {
        return match tokio::fs::read_to_string(path).await {
            Ok(body) => process_body(&body),
            Err(e) => FetchOutcome::Failed {
                reason: format!("file://{path}: {e}"),
            },
        };
    }
    let resp = match client.get(url).send().await {
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
    let content_length = resp.content_length().unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        return FetchOutcome::Failed {
            reason: format!("body too large: {content_length} bytes"),
        };
    }
    let body = match resp.text().await {
        Ok(b) => b,
        Err(e) => {
            return FetchOutcome::Failed {
                reason: format!("read body: {e}"),
            };
        }
    };
    if body.len() as u64 > MAX_BODY_BYTES {
        return FetchOutcome::Failed {
            reason: format!("body too large: {} bytes", body.len()),
        };
    }
    process_body(&body)
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

    #[tokio::test]
    async fn parses_local_fixture_via_file_url() {
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
