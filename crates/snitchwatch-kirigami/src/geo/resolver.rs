//! IP -> country resolution: private/loopback/link-local classification, the
//! [`CountryLookup`] trait wrapping `maxminddb`, and the startup
//! discover-then-open glue.
//!
//! Classification of private/local addresses is independent of whether a
//! database is available at all — a connection to `192.168.1.1` is always
//! "Local network", database or not. Only real public addresses need the
//! `.mmdb` lookup, and only when one is present; otherwise they land in
//! "Unknown". This keeps the panel useful (local traffic is instantly
//! bucketed) even before an operator has installed a GeoLite2 database.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Country info returned by a successful public-IP lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountryInfo {
    /// Uppercase ISO 3166-1 alpha-2 code, e.g. `"US"`.
    pub code: String,
    /// Human-readable English name, falling back to the code if the database
    /// has no `names.en` entry for this country.
    pub name: String,
}

/// A source of IP -> country data. Real production code implements this over
/// `maxminddb::Reader` ([`MmdbLookup`]); unit tests implement it directly
/// with a canned `HashMap`-backed fake — no real `.mmdb` file is ever
/// required to test the aggregation logic.
pub trait CountryLookup: Send + Sync {
    fn lookup(&self, addr: IpAddr) -> Option<CountryInfo>;
}

/// The bucket a resolved connection falls into.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Bucket {
    /// Private (RFC1918/ULA), loopback, or link-local address.
    Local,
    /// A public address with no resolvable country (no database installed,
    /// database has no entry for the address, or the address string didn't
    /// parse as an IP at all).
    Unknown,
    /// A public address resolved to a country, keyed by uppercase ISO
    /// alpha-2 code.
    Country(String),
}

/// Full resolution result: the bucket plus the display name to show for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub bucket: Bucket,
    pub display_name: String,
}

/// Resolve one destination IP (as the bridge sends it — a plain string) to a
/// [`Resolved`] bucket. `lookup` is `None` when no database is available.
pub fn resolve(ip: &str, lookup: Option<&dyn CountryLookup>) -> Resolved {
    let Ok(addr) = ip.parse::<IpAddr>() else {
        return Resolved {
            bucket: Bucket::Unknown,
            display_name: "Unknown".to_string(),
        };
    };

    if is_local(&addr) {
        return Resolved {
            bucket: Bucket::Local,
            display_name: "Local network".to_string(),
        };
    }

    match lookup.and_then(|l| l.lookup(addr)) {
        Some(info) => Resolved {
            bucket: Bucket::Country(info.code),
            display_name: info.name,
        },
        None => Resolved {
            bucket: Bucket::Unknown,
            display_name: "Unknown".to_string(),
        },
    }
}

/// Thread-safe, shareable IP -> country resolver: an optional
/// [`CountryLookup`] plus a `Mutex`-guarded per-IP cache, wrapped in `Arc`
/// internally so cloning is cheap.
///
/// The point of sharing this (rather than each consumer owning its own
/// cache) is the live feed: `GeoModel::start_bridge_feed`'s Tokio task
/// resolves every incoming row's destination IP *before* queueing the
/// message onto the Qt thread — that's the actual GeoIP lookup work, done
/// off the UI thread as specced. When the message reaches the Qt thread and
/// `GeoStore::apply` asks this same `SharedResolver` (a clone, same
/// underlying cache) to resolve the identical IP, it's already warm: a
/// `HashMap` get, never a database read. `GeoStore` never needs its own
/// separate cache or to know whether it's running the first (cold) or a
/// later (warm) resolution — the cache makes that transparent.
#[derive(Clone)]
pub struct SharedResolver {
    lookup: Option<Arc<dyn CountryLookup>>,
    cache: Arc<Mutex<HashMap<String, Resolved>>>,
}

impl SharedResolver {
    pub fn new(lookup: Option<Arc<dyn CountryLookup>>) -> Self {
        Self {
            lookup,
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Resolve `ip`, consulting (and populating) the shared cache first.
    pub fn resolve(&self, ip: &str) -> Resolved {
        if let Some(cached) = self.cache.lock().unwrap().get(ip) {
            return cached.clone();
        }
        let resolved = resolve(ip, self.lookup.as_deref());
        self.cache
            .lock()
            .unwrap()
            .insert(ip.to_string(), resolved.clone());
        resolved
    }
}

impl Default for SharedResolver {
    fn default() -> Self {
        Self::new(None)
    }
}

/// True for private (RFC 1918 / IPv6 ULA), loopback, or link-local addresses
/// — i.e. traffic that never leaves the local network and therefore has no
/// meaningful country.
///
/// Implemented with explicit range checks (rather than the `std` nightly- or
/// version-gated `is_private`/`is_unique_local` helpers) so behaviour doesn't
/// shift under us across toolchain versions.
fn is_local(addr: &IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => is_local_v4(v4),
        IpAddr::V6(v6) => is_local_v6(v6),
    }
}

fn is_local_v4(addr: &Ipv4Addr) -> bool {
    let [a, b, _c, _d] = addr.octets();
    addr.is_loopback() // 127.0.0.0/8
        || (a == 10) // 10.0.0.0/8
        || (a == 172 && (16..=31).contains(&b)) // 172.16.0.0/12
        || (a == 192 && b == 168) // 192.168.0.0/16
        || (a == 169 && b == 254) // 169.254.0.0/16 link-local
}

fn is_local_v6(addr: &Ipv6Addr) -> bool {
    if addr.is_loopback() {
        return true; // ::1
    }
    if let Some(v4) = addr.to_ipv4_mapped() {
        return is_local_v4(&v4);
    }
    let segments = addr.segments();
    let is_unique_local = (segments[0] & 0xfe00) == 0xfc00; // fc00::/7
    let is_link_local = (segments[0] & 0xffc0) == 0xfe80; // fe80::/10
    is_unique_local || is_link_local
}

/// A [`CountryLookup`] backed by a real GeoLite2/GeoIP2 `.mmdb` file, read
/// fully into memory (no `mmap`, no C dependency — pure Rust via the
/// `maxminddb` crate).
pub struct MmdbLookup {
    reader: maxminddb::Reader<Vec<u8>>,
}

impl MmdbLookup {
    /// Open and parse the database at `path`. Returns an error for a missing
    /// or corrupt file; callers degrade to the no-DB state rather than
    /// propagating a panic.
    pub fn open(path: &std::path::Path) -> Result<Self, maxminddb::MaxMindDbError> {
        let reader = maxminddb::Reader::open_readfile(path)?;
        Ok(Self { reader })
    }
}

impl CountryLookup for MmdbLookup {
    fn lookup(&self, addr: IpAddr) -> Option<CountryInfo> {
        let result = self.reader.lookup(addr).ok()?;
        let country: maxminddb::geoip2::Country = result.decode().ok().flatten()?;
        let code = country.country.iso_code?.to_uppercase();
        let name = country
            .country
            .names
            .english
            .map(str::to_string)
            .unwrap_or_else(|| code.clone());
        Some(CountryInfo { code, name })
    }
}

/// Outcome of the startup discover-then-open sequence, exposed to the QML
/// setup state regardless of whether a usable database was found.
pub struct DiscoveryOutcome {
    /// `Some` only when a database was found *and* opened successfully. An
    /// `Arc` (not `Box`) because the live feed task and the Qt-thread-side
    /// `GeoStore` share ownership of it via [`SharedResolver`].
    pub lookup: Option<Arc<dyn CountryLookup>>,
    /// The database path actually in use, or — when none is available — the
    /// default location an operator should place one at.
    pub path: PathBuf,
    pub available: bool,
}

/// Run [`super::paths::discover_geoip_db`] and try to open whatever it finds.
/// Never panics: a missing file yields the no-DB state; a found-but-corrupt
/// file is logged once and also yields the no-DB state (the classification
/// of local/private addresses still works either way).
pub fn discover_and_open() -> DiscoveryOutcome {
    match super::paths::discover_geoip_db() {
        Some(path) => match MmdbLookup::open(&path) {
            Ok(lookup) => DiscoveryOutcome {
                lookup: Some(Arc::new(lookup)),
                path,
                available: true,
            },
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "geo: found a GeoIP database but failed to open it; \
                     falling back to the no-database state"
                );
                DiscoveryOutcome {
                    lookup: None,
                    path,
                    available: false,
                }
            }
        },
        None => {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            let xdg_data_home = std::env::var("XDG_DATA_HOME").ok();
            let suggested = super::paths::default_suggested_path(&home, xdg_data_home.as_deref());
            tracing::info!(
                suggested_path = %suggested.display(),
                "geo: no GeoLite2-Country.mmdb found; geographic breakdown will show \
                 Local network / Unknown only until one is installed"
            );
            DiscoveryOutcome {
                lookup: None,
                path: suggested,
                available: false,
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod fakes {
    use super::*;
    use std::collections::HashMap;

    /// Deterministic fake resolver for tests — never touches a real `.mmdb`.
    #[derive(Default)]
    pub struct FakeLookup {
        entries: HashMap<IpAddr, CountryInfo>,
    }

    impl FakeLookup {
        pub fn with(mut self, ip: &str, code: &str, name: &str) -> Self {
            self.entries.insert(
                ip.parse().unwrap(),
                CountryInfo {
                    code: code.to_string(),
                    name: name.to_string(),
                },
            );
            self
        }
    }

    impl CountryLookup for FakeLookup {
        fn lookup(&self, addr: IpAddr) -> Option<CountryInfo> {
            self.entries.get(&addr).cloned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fakes::FakeLookup;
    use super::*;

    #[test]
    fn loopback_v4_is_local() {
        let r = resolve("127.0.0.1", None);
        assert_eq!(r.bucket, Bucket::Local);
        assert_eq!(r.display_name, "Local network");
    }

    #[test]
    fn private_ranges_are_local() {
        for ip in ["10.0.0.5", "172.16.0.5", "172.31.255.255", "192.168.1.1"] {
            let r = resolve(ip, None);
            assert_eq!(r.bucket, Bucket::Local, "expected {ip} to be Local");
        }
    }

    #[test]
    fn link_local_v4_is_local() {
        let r = resolve("169.254.1.1", None);
        assert_eq!(r.bucket, Bucket::Local);
    }

    #[test]
    fn adjacent_public_ranges_are_not_local() {
        // 172.15.x and 172.32.x sit just outside the 172.16.0.0/12 block.
        for ip in ["172.15.255.255", "172.32.0.0", "11.0.0.1", "192.167.1.1"] {
            let r = resolve(ip, None);
            assert_ne!(r.bucket, Bucket::Local, "expected {ip} to NOT be Local");
        }
    }

    #[test]
    fn loopback_v6_is_local() {
        let r = resolve("::1", None);
        assert_eq!(r.bucket, Bucket::Local);
    }

    #[test]
    fn unique_local_v6_is_local() {
        let r = resolve("fd12:3456:789a::1", None);
        assert_eq!(r.bucket, Bucket::Local);
    }

    #[test]
    fn link_local_v6_is_local() {
        let r = resolve("fe80::1", None);
        assert_eq!(r.bucket, Bucket::Local);
    }

    #[test]
    fn ipv4_mapped_v6_private_address_is_local() {
        let r = resolve("::ffff:192.168.1.1", None);
        assert_eq!(r.bucket, Bucket::Local);
    }

    #[test]
    fn public_v6_is_not_local() {
        let r = resolve("2606:4700:4700::1111", None);
        assert_ne!(r.bucket, Bucket::Local);
    }

    #[test]
    fn public_ip_without_lookup_is_unknown() {
        let r = resolve("140.82.121.4", None);
        assert_eq!(r.bucket, Bucket::Unknown);
        assert_eq!(r.display_name, "Unknown");
    }

    #[test]
    fn public_ip_with_no_db_entry_is_unknown() {
        let lookup = FakeLookup::default().with("1.1.1.1", "US", "United States");
        let r = resolve("8.8.8.8", Some(&lookup));
        assert_eq!(r.bucket, Bucket::Unknown);
    }

    #[test]
    fn public_ip_resolves_via_fake_lookup() {
        let lookup = FakeLookup::default().with("140.82.121.4", "US", "United States");
        let r = resolve("140.82.121.4", Some(&lookup));
        assert_eq!(r.bucket, Bucket::Country("US".to_string()));
        assert_eq!(r.display_name, "United States");
    }

    #[test]
    fn unparsable_ip_string_is_unknown() {
        let r = resolve("not-an-ip", None);
        assert_eq!(r.bucket, Bucket::Unknown);
    }

    #[test]
    fn local_classification_wins_over_lookup_even_when_db_present() {
        // A private address must never hit the database, regardless of what
        // it (hypothetically) contains for that address.
        let lookup = FakeLookup::default().with("192.168.1.1", "US", "United States");
        let r = resolve("192.168.1.1", Some(&lookup));
        assert_eq!(r.bucket, Bucket::Local);
    }

    #[test]
    fn mmdb_lookup_open_fails_gracefully_on_missing_file() {
        let err = MmdbLookup::open(std::path::Path::new("/nonexistent/GeoLite2-Country.mmdb"));
        assert!(err.is_err());
    }

    #[test]
    fn mmdb_lookup_open_fails_gracefully_on_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.mmdb");
        std::fs::write(&path, b"not a real mmdb").unwrap();
        let err = MmdbLookup::open(&path);
        assert!(err.is_err());
    }

    #[test]
    fn discover_and_open_degrades_gracefully_with_no_db_anywhere() {
        // SAFETY (test-only): no other test reads these two vars.
        std::env::remove_var("SNITCHWATCH_GEOIP_DB");
        std::env::set_var("HOME", "/nonexistent-home-for-tests");
        std::env::remove_var("XDG_DATA_HOME");

        let outcome = discover_and_open();
        assert!(!outcome.available);
        assert!(outcome.lookup.is_none());
        assert_eq!(
            outcome.path,
            PathBuf::from(
                "/nonexistent-home-for-tests/.local/share/snitchwatch/GeoLite2-Country.mmdb"
            )
        );
    }

    #[test]
    fn discover_and_open_degrades_gracefully_on_corrupt_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("GeoLite2-Country.mmdb");
        std::fs::write(&path, b"not a real mmdb").unwrap();

        // SAFETY (test-only): serialised within this function; no other test
        // reads SNITCHWATCH_GEOIP_DB concurrently in a way that matters here.
        std::env::set_var("SNITCHWATCH_GEOIP_DB", &path);
        let outcome = discover_and_open();
        std::env::remove_var("SNITCHWATCH_GEOIP_DB");

        assert!(!outcome.available);
        assert!(outcome.lookup.is_none());
        assert_eq!(outcome.path, path);
    }

    #[test]
    fn shared_resolver_caches_repeat_lookups() {
        struct OnceLookup(std::sync::atomic::AtomicBool);
        impl CountryLookup for OnceLookup {
            fn lookup(&self, addr: IpAddr) -> Option<CountryInfo> {
                assert!(
                    !self.0.swap(true, std::sync::atomic::Ordering::SeqCst),
                    "lookup called more than once for the same IP"
                );
                let expected: IpAddr = "140.82.121.4".parse().unwrap();
                assert_eq!(addr, expected);
                Some(CountryInfo {
                    code: "US".to_string(),
                    name: "United States".to_string(),
                })
            }
        }
        let resolver = SharedResolver::new(Some(Arc::new(OnceLookup(
            std::sync::atomic::AtomicBool::new(false),
        ))));
        let first = resolver.resolve("140.82.121.4");
        let second = resolver.resolve("140.82.121.4");
        assert_eq!(first, second);
        assert_eq!(first.bucket, Bucket::Country("US".to_string()));
    }

    #[test]
    fn shared_resolver_clone_shares_the_same_cache() {
        // Proves the actual property `start_bridge_feed` depends on: a clone
        // (handed to the Tokio feed task) and the original (held by
        // `GeoStore` on the Qt thread) see the same warm cache, not
        // independent copies.
        struct OnceLookup(std::sync::atomic::AtomicBool);
        impl CountryLookup for OnceLookup {
            fn lookup(&self, _addr: IpAddr) -> Option<CountryInfo> {
                assert!(
                    !self.0.swap(true, std::sync::atomic::Ordering::SeqCst),
                    "lookup called more than once across resolver clones"
                );
                Some(CountryInfo {
                    code: "GB".to_string(),
                    name: "United Kingdom".to_string(),
                })
            }
        }
        let resolver = SharedResolver::new(Some(Arc::new(OnceLookup(
            std::sync::atomic::AtomicBool::new(false),
        ))));
        let feed_task_clone = resolver.clone();

        // The feed task resolves first (warming the shared cache)...
        let warmed = feed_task_clone.resolve("81.2.69.142");
        // ...then the Qt-thread-side clone resolves the same IP and must hit
        // the cache instead of calling `lookup` again.
        let from_store = resolver.resolve("81.2.69.142");
        assert_eq!(warmed, from_store);
    }

    #[test]
    fn shared_resolver_default_has_no_lookup() {
        let resolver = SharedResolver::default();
        let r = resolver.resolve("140.82.121.4");
        assert_eq!(r.bucket, Bucket::Unknown);
    }

    #[test]
    fn shared_resolver_local_addresses_never_reach_the_lookup() {
        struct PanicsIfCalled;
        impl CountryLookup for PanicsIfCalled {
            fn lookup(&self, _addr: IpAddr) -> Option<CountryInfo> {
                panic!("lookup should never be called for a local address");
            }
        }
        let resolver = SharedResolver::new(Some(Arc::new(PanicsIfCalled)));
        let r = resolver.resolve("192.168.1.1");
        assert_eq!(r.bucket, Bucket::Local);
    }
}
