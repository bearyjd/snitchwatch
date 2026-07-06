//! Qt-free lookup logic: reverse DNS + RDAP, with a hard per-lookup timeout
//! and a per-IP cache. See the module docs on [`super`] for the full
//! "strictly decorative" contract.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;

/// Hard ceiling on each individual lookup (reverse DNS, RDAP). Chosen so a
/// slow/unreachable resolver or RDAP endpoint can never keep the insight
/// panel spinning for more than a few seconds — the dialog itself has no
/// upper bound since submitting a verdict never waits on this.
pub const LOOKUP_TIMEOUT: Duration = Duration::from_secs(4);

/// RDAP registration info for an IP.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RdapInfo {
    pub org: Option<String>,
    pub registrar: Option<String>,
    pub country: Option<String>,
}

/// Combined result the insight panel displays for one IP.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InsightResult {
    pub hostname: Option<String>,
    pub org: Option<String>,
    pub registrar: Option<String>,
    pub country: Option<String>,
}

impl InsightResult {
    /// Whether there's anything at all to show — drives the QML "unavailable
    /// (offline?)" fallback.
    pub fn has_any_data(&self) -> bool {
        self.hostname.is_some()
            || self.org.is_some()
            || self.registrar.is_some()
            || self.country.is_some()
    }
}

/// The two network lookups the insight panel needs, behind a trait so tests
/// never touch a real network — see `insight::client::tests`' `FakeSource`.
#[async_trait]
pub trait InsightSource: Send + Sync {
    /// Reverse-DNS (PTR) lookup. `None` on any failure (NXDOMAIN, timeout,
    /// malformed IP, offline, etc.) — the caller never distinguishes why.
    async fn reverse_dns(&self, ip: &str) -> Option<String>;

    /// RDAP registration lookup. `None` on any failure.
    async fn rdap(&self, ip: &str) -> Option<RdapInfo>;
}

/// Production [`InsightSource`]: PTR via `dns-lookup` (a blocking syscall,
/// run via `spawn_blocking`) and RDAP via `https://rdap.org/ip/<ip>` — the
/// IANA-endorsed RDAP bootstrap redirector, so one endpoint covers every
/// regional registry (ARIN/RIPE/APNIC/LACNIC/AFRINIC) without us maintaining
/// a bootstrap table.
pub struct RealInsightSource {
    http: Client,
}

impl RealInsightSource {
    pub fn new() -> Self {
        Self {
            http: build_http_client(),
        }
    }
}

impl Default for RealInsightSource {
    fn default() -> Self {
        Self::new()
    }
}

fn build_http_client() -> Client {
    Client::builder()
        .user_agent(concat!("snitchwatch/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("reqwest client builds")
}

#[async_trait]
impl InsightSource for RealInsightSource {
    async fn reverse_dns(&self, ip: &str) -> Option<String> {
        let addr: IpAddr = ip.parse().ok()?;
        tokio::task::spawn_blocking(move || dns_lookup::lookup_addr(&addr).ok())
            .await
            .ok()
            .flatten()
    }

    async fn rdap(&self, ip: &str) -> Option<RdapInfo> {
        let url = format!("https://rdap.org/ip/{ip}");
        let resp = self.http.get(&url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: Value = resp.json().await.ok()?;
        Some(parse_rdap(&body))
    }
}

/// Extract org/registrar/country from an RDAP IP-network response. Every
/// field is optional in the wild (registries vary in what they publish), so
/// this degrades to `None`s rather than erroring on an unexpected shape.
pub fn parse_rdap(body: &Value) -> RdapInfo {
    let org = body
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| find_entity_fn(body, "registrant"));
    let registrar = find_entity_fn(body, "registrar");
    let country = body
        .get("country")
        .and_then(Value::as_str)
        .map(str::to_string);
    RdapInfo {
        org,
        registrar,
        country,
    }
}

/// Find the `fn` (formatted name) vCard field of the entity whose `roles`
/// array contains `role`, per the RDAP vCard-in-JSON shape (RFC 9083 ยง5.4 /
/// jCard RFC 7095). Any unexpected shape falls through to `None` via `?`.
fn find_entity_fn(body: &Value, role: &str) -> Option<String> {
    let entities = body.get("entities")?.as_array()?;
    for entity in entities {
        let roles = entity.get("roles")?.as_array()?;
        let has_role = roles.iter().any(|r| r.as_str() == Some(role));
        if !has_role {
            continue;
        }
        let vcard_fields = entity.get("vcardArray")?.as_array()?.get(1)?.as_array()?;
        for field in vcard_fields {
            let field = field.as_array()?;
            if field.first().and_then(Value::as_str) == Some("fn") {
                return field.get(3).and_then(Value::as_str).map(str::to_string);
            }
        }
    }
    None
}

/// Run both lookups (with the per-lookup timeout applied to each) and fold
/// them into one [`InsightResult`]. Pure with respect to caching — callers
/// that want caching go through [`resolve`].
pub async fn fetch(source: &dyn InsightSource, ip: &str) -> InsightResult {
    let (hostname_res, rdap_res) = tokio::join!(
        tokio::time::timeout(LOOKUP_TIMEOUT, source.reverse_dns(ip)),
        tokio::time::timeout(LOOKUP_TIMEOUT, source.rdap(ip)),
    );
    let hostname = hostname_res.ok().flatten();
    let rdap = rdap_res.ok().flatten();
    InsightResult {
        hostname,
        org: rdap.as_ref().and_then(|r| r.org.clone()),
        registrar: rdap.as_ref().and_then(|r| r.registrar.clone()),
        country: rdap.as_ref().and_then(|r| r.country.clone()),
    }
}

/// [`fetch`] with a per-IP session cache. `cache` uses `std::sync::Mutex`
/// (not `tokio::sync::Mutex`) since every critical section is synchronous —
/// no `.await` is ever held across the lock.
pub async fn resolve(
    source: &dyn InsightSource,
    cache: &Mutex<HashMap<String, InsightResult>>,
    ip: &str,
) -> InsightResult {
    if let Some(cached) = cache.lock().unwrap().get(ip).cloned() {
        return cached;
    }
    let result = fetch(source, ip).await;
    cache.lock().unwrap().insert(ip.to_string(), result.clone());
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct FakeSource {
        calls: AtomicUsize,
        hostname: Option<String>,
        rdap: Option<RdapInfo>,
    }

    #[async_trait]
    impl InsightSource for FakeSource {
        async fn reverse_dns(&self, _ip: &str) -> Option<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.hostname.clone()
        }

        async fn rdap(&self, _ip: &str) -> Option<RdapInfo> {
            self.rdap.clone()
        }
    }

    struct SlowSource;

    #[async_trait]
    impl InsightSource for SlowSource {
        async fn reverse_dns(&self, _ip: &str) -> Option<String> {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Some("too-late.example".to_string())
        }

        async fn rdap(&self, _ip: &str) -> Option<RdapInfo> {
            tokio::time::sleep(Duration::from_secs(30)).await;
            None
        }
    }

    #[tokio::test]
    async fn fetch_combines_reverse_dns_and_rdap() {
        let source = FakeSource {
            hostname: Some("dns.google".to_string()),
            rdap: Some(RdapInfo {
                org: Some("Google LLC".to_string()),
                registrar: None,
                country: Some("US".to_string()),
            }),
            ..Default::default()
        };
        let result = fetch(&source, "8.8.8.8").await;
        assert_eq!(result.hostname.as_deref(), Some("dns.google"));
        assert_eq!(result.org.as_deref(), Some("Google LLC"));
        assert_eq!(result.country.as_deref(), Some("US"));
        assert!(result.has_any_data());
    }

    #[tokio::test]
    async fn fetch_degrades_gracefully_when_source_returns_nothing() {
        let source = FakeSource::default();
        let result = fetch(&source, "10.0.0.1").await;
        assert!(!result.has_any_data());
    }

    #[tokio::test(start_paused = true)]
    async fn fetch_times_out_instead_of_hanging() {
        // Both lookups sleep for 30s; LOOKUP_TIMEOUT is 4s. With the tokio
        // clock paused, this resolves instantly (no real 4s wait) while
        // still proving the timeout — not the source — is what bounds this.
        let result = fetch(&SlowSource, "1.2.3.4").await;
        assert!(
            !result.has_any_data(),
            "a hung lookup must degrade to unavailable, never block"
        );
    }

    #[tokio::test]
    async fn resolve_caches_and_avoids_a_second_fetch() {
        let source = FakeSource {
            hostname: Some("cached.example".to_string()),
            ..Default::default()
        };
        let cache = Mutex::new(HashMap::new());
        let first = resolve(&source, &cache, "1.1.1.1").await;
        let second = resolve(&source, &cache, "1.1.1.1").await;
        assert_eq!(first, second);
        assert_eq!(
            source.calls.load(Ordering::SeqCst),
            1,
            "second resolve() for the same IP must hit the cache, not refetch"
        );
    }

    #[tokio::test]
    async fn resolve_does_not_cross_contaminate_different_ips() {
        let source = FakeSource {
            hostname: Some("x.example".to_string()),
            ..Default::default()
        };
        let cache = Mutex::new(HashMap::new());
        resolve(&source, &cache, "1.1.1.1").await;
        resolve(&source, &cache, "2.2.2.2").await;
        assert_eq!(source.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn parse_rdap_extracts_org_registrar_and_country_from_a_typical_response() {
        let body = serde_json::json!({
            "name": "GOOGLE",
            "country": "US",
            "entities": [{
                "roles": ["registrar"],
                "vcardArray": ["vcard", [
                    ["version", {}, "text", "4.0"],
                    ["fn", {}, "text", "MarkMonitor Inc."]
                ]]
            }]
        });
        let info = parse_rdap(&body);
        assert_eq!(info.org.as_deref(), Some("GOOGLE"));
        assert_eq!(info.registrar.as_deref(), Some("MarkMonitor Inc."));
        assert_eq!(info.country.as_deref(), Some("US"));
    }

    #[test]
    fn parse_rdap_falls_back_to_registrant_entity_name_when_no_top_level_name() {
        let body = serde_json::json!({
            "entities": [{
                "roles": ["registrant"],
                "vcardArray": ["vcard", [
                    ["fn", {}, "text", "Example Registrant Org"]
                ]]
            }]
        });
        let info = parse_rdap(&body);
        assert_eq!(info.org.as_deref(), Some("Example Registrant Org"));
    }

    #[test]
    fn parse_rdap_handles_a_completely_empty_response() {
        let info = parse_rdap(&serde_json::json!({}));
        assert!(info.org.is_none());
        assert!(info.registrar.is_none());
        assert!(info.country.is_none());
    }

    #[test]
    fn parse_rdap_ignores_malformed_entities_array() {
        let body = serde_json::json!({ "entities": "not-an-array" });
        let info = parse_rdap(&body);
        assert!(info.registrar.is_none());
    }
}
