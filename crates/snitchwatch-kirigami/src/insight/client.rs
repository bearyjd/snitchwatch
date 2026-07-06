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
        // A hand-rolled `Client` (no custom TLS/proxy/timeout config beyond
        // what we just set) fails to build only in truly exotic environments
        // (e.g. no usable TLS backend). Degrading to `Client::new()` — and,
        // failing that, giving up the user agent — keeps startup infallible;
        // every request already tolerates network failure via `.ok()?`.
        .unwrap_or_else(|_| Client::new())
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
        let url = rdap_url(ip)?;
        let resp = self.http.get(&url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: Value = resp.json().await.ok()?;
        Some(parse_rdap(&body))
    }
}

/// Build the RDAP request URL for `ip`, or `None` if it isn't a valid IP
/// address. Parsing first (rather than interpolating the raw string) is
/// load-bearing: `ip` ultimately comes from a pending connection's remote
/// address, so a hostile value (e.g. containing `/../` or embedded host
/// segments) must never reach the request path unparsed. Routing everything
/// through [`std::net::IpAddr`]'s formatter also normalizes the address
/// (e.g. IPv6 zone/scope handling) before it's sent.
fn rdap_url(ip: &str) -> Option<String> {
    let addr: IpAddr = ip.parse().ok()?;
    Some(format!("https://rdap.org/ip/{addr}"))
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
///
/// `rdap_enabled` gates the RDAP lookup only — reverse DNS always runs. RDAP
/// sends the pending connection's remote IP to a third-party registry
/// (`rdap.org`), so it is opt-in (default off); see `PendingInsight::lookup`
/// for where the persisted setting is read.
pub async fn fetch(source: &dyn InsightSource, ip: &str, rdap_enabled: bool) -> InsightResult {
    let rdap_lookup = async {
        if rdap_enabled {
            tokio::time::timeout(LOOKUP_TIMEOUT, source.rdap(ip))
                .await
                .ok()
                .flatten()
        } else {
            None
        }
    };
    let (hostname_res, rdap) = tokio::join!(
        tokio::time::timeout(LOOKUP_TIMEOUT, source.reverse_dns(ip)),
        rdap_lookup,
    );
    let hostname = hostname_res.ok().flatten();
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
///
/// Caches every outcome, including an all-`None` [`InsightResult`] from a
/// failing/offline resolver — otherwise reopening the dialog for the same IP
/// re-runs (and re-blocks a thread on) a lookup already known to fail. The
/// cache key folds in `rdap_enabled` so a result cached while RDAP was off
/// doesn't mask RDAP data once the user turns it on later in the session.
pub async fn resolve(
    source: &dyn InsightSource,
    cache: &Mutex<HashMap<String, InsightResult>>,
    ip: &str,
    rdap_enabled: bool,
) -> InsightResult {
    let key = cache_key(ip, rdap_enabled);
    if let Some(cached) = cache.lock().unwrap().get(&key).cloned() {
        return cached;
    }
    let result = fetch(source, ip, rdap_enabled).await;
    cache.lock().unwrap().insert(key, result.clone());
    result
}

/// Exposed to `insight_model` so its fast synchronous cache-hit path (before
/// spawning any network work) uses the exact same key `resolve` does.
pub(crate) fn cache_key(ip: &str, rdap_enabled: bool) -> String {
    format!("{ip}|rdap={rdap_enabled}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct FakeSource {
        calls: AtomicUsize,
        rdap_calls: AtomicUsize,
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
            self.rdap_calls.fetch_add(1, Ordering::SeqCst);
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
        let result = fetch(&source, "8.8.8.8", true).await;
        assert_eq!(result.hostname.as_deref(), Some("dns.google"));
        assert_eq!(result.org.as_deref(), Some("Google LLC"));
        assert_eq!(result.country.as_deref(), Some("US"));
        assert!(result.has_any_data());
    }

    #[tokio::test]
    async fn fetch_degrades_gracefully_when_source_returns_nothing() {
        let source = FakeSource::default();
        let result = fetch(&source, "10.0.0.1", true).await;
        assert!(!result.has_any_data());
    }

    #[tokio::test(start_paused = true)]
    async fn fetch_times_out_instead_of_hanging() {
        // Both lookups sleep for 30s; LOOKUP_TIMEOUT is 4s. With the tokio
        // clock paused, this resolves instantly (no real 4s wait) while
        // still proving the timeout — not the source — is what bounds this.
        let result = fetch(&SlowSource, "1.2.3.4", true).await;
        assert!(
            !result.has_any_data(),
            "a hung lookup must degrade to unavailable, never block"
        );
    }

    #[tokio::test]
    async fn fetch_never_calls_rdap_source_when_disabled() {
        let source = FakeSource {
            hostname: Some("dns.google".to_string()),
            rdap: Some(RdapInfo {
                org: Some("Google LLC".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = fetch(&source, "8.8.8.8", false).await;
        assert_eq!(result.hostname.as_deref(), Some("dns.google"));
        assert!(result.org.is_none());
        assert_eq!(
            source.rdap_calls.load(Ordering::SeqCst),
            0,
            "RDAP must be opt-in: disabled means the RDAP source is never invoked"
        );
    }

    #[tokio::test]
    async fn resolve_caches_and_avoids_a_second_fetch() {
        let source = FakeSource {
            hostname: Some("cached.example".to_string()),
            ..Default::default()
        };
        let cache = Mutex::new(HashMap::new());
        let first = resolve(&source, &cache, "1.1.1.1", true).await;
        let second = resolve(&source, &cache, "1.1.1.1", true).await;
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
        resolve(&source, &cache, "1.1.1.1", true).await;
        resolve(&source, &cache, "2.2.2.2", true).await;
        assert_eq!(source.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn resolve_caches_a_failing_lookup_and_never_refetches() {
        // Negative caching: a resolver that returns nothing (dead/offline)
        // must still be cached, so reopening the dialog for the same IP
        // doesn't re-run (and re-park a thread on) a lookup already known
        // to fail.
        let source = FakeSource::default();
        let cache = Mutex::new(HashMap::new());
        let first = resolve(&source, &cache, "10.0.0.1", true).await;
        let second = resolve(&source, &cache, "10.0.0.1", true).await;
        assert!(!first.has_any_data());
        assert_eq!(first, second);
        assert_eq!(
            source.calls.load(Ordering::SeqCst),
            1,
            "a second resolve() for a known-failing IP must hit the cache, not refetch"
        );
    }

    #[test]
    fn rdap_url_rejects_a_hostile_string_masquerading_as_an_ip() {
        // A string smuggling a path-traversal / alternate host segment past
        // naive interpolation must never become a request URL.
        assert_eq!(rdap_url("1.2.3.4/../domain/evil.com"), None);
        assert_eq!(rdap_url("8.8.8.8@evil.com"), None);
        assert_eq!(rdap_url(""), None);
    }

    #[test]
    fn rdap_url_accepts_valid_ipv4_and_ipv6() {
        assert_eq!(
            rdap_url("8.8.8.8").as_deref(),
            Some("https://rdap.org/ip/8.8.8.8")
        );
        assert_eq!(
            rdap_url("2001:4860:4860::8888").as_deref(),
            Some("https://rdap.org/ip/2001:4860:4860::8888")
        );
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
