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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Country info returned by a successful public-IP lookup.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CountryInfo {
    /// Uppercase ISO 3166-1 alpha-2 code, e.g. `"US"`.
    pub code: String,
    /// Human-readable English name, falling back to the code if the database
    /// has no `names.en` entry for this country.
    pub name: String,
}

/// Database capability describes available fields, never their physical accuracy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GeoCapability {
    None,
    Country,
    City,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoCoordinates {
    pub latitude: f64,
    pub longitude: f64,
}
impl GeoCoordinates {
    pub fn new(latitude: Option<f64>, longitude: Option<f64>) -> Option<Self> {
        let (latitude, longitude) = (latitude?, longitude?);
        (latitude.is_finite()
            && longitude.is_finite()
            && (-90.0..=90.0).contains(&latitude)
            && (-180.0..=180.0).contains(&longitude))
        .then_some(Self {
            latitude: if latitude == 0.0 { 0.0 } else { latitude },
            longitude: if longitude == 0.0 { 0.0 } else { longitude },
        })
    }
}

/// No geoname ID or accuracy radius is assumed for City Lite records.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoPlace {
    pub country: CountryInfo,
    pub subdivisions: Vec<String>,
    pub city: Option<String>,
    pub coordinates: Option<GeoCoordinates>,
    pub provider: String,
    pub precision: Option<String>,
}
impl GeoPlace {
    fn country(country: CountryInfo) -> Self {
        Self {
            country,
            subdivisions: Vec::new(),
            city: None,
            coordinates: None,
            provider: String::new(),
            precision: None,
        }
    }
    /// Length-framed components prevent name/coordinate collisions. Keys last
    /// for one database snapshot; a restart may load a different dataset.
    pub fn city_key(&self) -> String {
        let components = (
            &self.country.code,
            self.subdivisions
                .iter()
                .map(|s| s.trim().to_lowercase())
                .collect::<Vec<_>>(),
            self.city.as_ref().map(|s| s.trim().to_lowercase()),
            self.coordinates
                .as_ref()
                .map(|c| (c.latitude.to_bits(), c.longitude.to_bits())),
        );
        format!(
            "city:{}",
            serde_json::to_string(&components).expect("finite geographic identity")
        )
    }
}

/// A source of IP -> country data. Real production code implements this over
/// `maxminddb::Reader` ([`MmdbLookup`]); unit tests implement it directly
/// with a canned `HashMap`-backed fake — no real `.mmdb` file is ever
/// required to test the aggregation logic.
pub trait CountryLookup: Send + Sync {
    fn lookup(&self, addr: IpAddr) -> Option<CountryInfo>;
    fn lookup_place(&self, addr: IpAddr) -> Option<GeoPlace> {
        self.lookup(addr).map(GeoPlace::country)
    }
    fn capability(&self) -> GeoCapability {
        GeoCapability::Country
    }
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
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub bucket: Bucket,
    pub display_name: String,
    pub place: Option<GeoPlace>,
}

/// Resolve one destination IP (as the bridge sends it — a plain string) to a
/// [`Resolved`] bucket. `lookup` is `None` when no database is available.
pub fn resolve(ip: &str, lookup: Option<&dyn CountryLookup>) -> Resolved {
    let Ok(addr) = ip.parse::<IpAddr>() else {
        return Resolved {
            bucket: Bucket::Unknown,
            display_name: "Unknown".to_string(),
            place: None,
        };
    };

    if is_local(&addr) {
        return Resolved {
            bucket: Bucket::Local,
            display_name: "Local network".to_string(),
            place: None,
        };
    }

    match lookup.and_then(|l| l.lookup_place(addr)) {
        Some(place) => Resolved {
            bucket: Bucket::Country(place.country.code.clone()),
            display_name: place.country.name.clone(),
            place: Some(place),
        },
        None => Resolved {
            bucket: Bucket::Unknown,
            display_name: "Unknown".to_string(),
            place: None,
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

    /// The cache this resolver shares with its clones; lets a test prove two
    /// handles are one resolver.
    #[cfg(test)]
    pub(crate) fn cache_handle(&self) -> Arc<Mutex<HashMap<String, Resolved>>> {
        self.cache.clone()
    }

    /// Whether a country database backs this resolver. Without one every
    /// public address resolves to [`Bucket::Unknown`].
    pub fn has_database(&self) -> bool {
        self.lookup.is_some()
    }

    pub fn capability(&self) -> GeoCapability {
        self.lookup
            .as_ref()
            .map_or(GeoCapability::None, |lookup| lookup.capability())
    }

    /// Resolve `ip`, consulting (and populating) the shared cache first.
    pub fn resolve(&self, ip: &str) -> Resolved {
        // Invalid strings do not accumulate in the cache. Canonical spellings
        // of the same IPv6 address share one entry for every consumer.
        let Ok(addr) = ip.parse::<IpAddr>() else {
            return resolve(ip, self.lookup.as_deref());
        };
        let key = addr.to_string();
        if let Some(cached) = self.cache.lock().unwrap().get(&key) {
            return cached.clone();
        }
        let resolved = resolve(ip, self.lookup.as_deref());
        let mut cache = self.cache.lock().unwrap();
        // Bounded working set. Existing tracked rows retain their own place;
        // eviction only makes a later lookup cold, never changes identity.
        if cache.len() >= 65_536 {
            cache.clear();
        }
        cache.insert(key, resolved.clone());
        resolved
    }
}

impl std::fmt::Debug for SharedResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedResolver")
            .field("has_database", &self.has_database())
            .finish_non_exhaustive()
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

    /// Identity comes from MMDB metadata, never a potentially renamed filename.
    pub fn source_label(&self) -> String {
        match self.reader.metadata().database_type.as_str() {
            "DBIP-Country-Lite" | "DBIP-City-Lite" => {
                "IP Geolocation by DB-IP (https://db-ip.com) · CC BY 4.0".into()
            }
            "GeoLite2-City" => "GeoLite2 City by MaxMind (https://www.maxmind.com)".into(),
            "GeoIP2-City" => "GeoIP2 City by MaxMind (https://www.maxmind.com)".into(),
            "GeoLite2-Country" => "GeoLite2 Country by MaxMind (https://www.maxmind.com)".into(),
            "GeoIP2-Country" => "GeoIP2 Country by MaxMind (https://www.maxmind.com)".into(),
            kind => format!("MMDB dataset: {kind}"),
        }
    }

    pub fn build_epoch(&self) -> u64 {
        self.reader.metadata().build_epoch
    }
}

impl CountryLookup for MmdbLookup {
    fn capability(&self) -> GeoCapability {
        match self.reader.metadata().database_type.as_str() {
            "DBIP-City-Lite" | "GeoLite2-City" | "GeoIP2-City" => GeoCapability::City,
            _ => GeoCapability::Country,
        }
    }

    fn lookup_place(&self, addr: IpAddr) -> Option<GeoPlace> {
        if self.capability() != GeoCapability::City {
            let mut place = GeoPlace::country(self.lookup(addr)?);
            place.provider = self.source_label();
            return Some(place);
        }
        let result = self.reader.lookup(addr).ok()?;
        let record: maxminddb::geoip2::City = result.decode().ok().flatten()?;
        let code = record.country.iso_code?.to_uppercase();
        if code.len() != 2 || !code.bytes().all(|b| b.is_ascii_uppercase()) {
            return None;
        }
        let clean = |s: &str| {
            s.len() <= 256 && !s.trim().is_empty() && !s.trim().chars().any(char::is_control)
        };
        let coordinates = GeoCoordinates::new(record.location.latitude, record.location.longitude);
        let city = record
            .city
            .names
            .english
            .filter(|s| clean(s))
            .map(|s| s.trim().to_string());
        let precision = (city.is_some() && coordinates.is_some())
            .then(|| "Approximate city estimate".to_string());
        Some(GeoPlace {
            country: CountryInfo {
                name: record
                    .country
                    .names
                    .english
                    .filter(|s| clean(s))
                    .unwrap_or(&code)
                    .trim()
                    .to_string(),
                code,
            },
            subdivisions: record
                .subdivisions
                .iter()
                .take(8)
                .filter_map(|s| s.iso_code.or(s.names.english))
                .filter(|s| clean(s))
                .map(|s| s.trim().to_string())
                .collect(),
            city,
            coordinates,
            provider: self.source_label(),
            precision,
        })
    }

    fn lookup(&self, addr: IpAddr) -> Option<CountryInfo> {
        if self.capability() == GeoCapability::City {
            return self.lookup_place(addr).map(|place| place.country);
        }
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
    pub source: String,
    pub build_epoch: u64,
}

/// Production entry point: reads `$SNITCHWATCH_GEOIP_DB`, `$XDG_DATA_HOME` and
/// the home base ([`crate::paths::home_base`], never a bare `/tmp`) from the process environment — the only place the geo
/// module does — and runs [`discover_and_open_with`] against the real
/// filesystem.
pub fn discover_and_open() -> DiscoveryOutcome {
    let env_override = std::env::var("SNITCHWATCH_GEOIP_DB").ok();
    let home = crate::paths::home_base_rechecked();
    let xdg_data_home = std::env::var("XDG_DATA_HOME").ok();
    let country = discover_and_open_with(
        |p| p.exists(),
        env_override.as_deref(),
        &home,
        xdg_data_home.as_deref(),
    );
    // Explicit country override retains highest priority, including when stale.
    if env_override.is_some() {
        return country;
    }
    let city_override = std::env::var("SNITCHWATCH_GEOIP_CITY_DB").ok();
    let target = super::paths::default_suggested_path(&home, xdg_data_home.as_deref());
    selected_database(country, city_override.as_deref(), &target)
}

fn selected_database(
    country: DiscoveryOutcome,
    city_override: Option<&str>,
    target: &Path,
) -> DiscoveryOutcome {
    let directory = target.parent().expect("application data directory");
    let country = if let Some(path) = super::dataset::country_path(directory) {
        match MmdbLookup::open(&path) {
            Ok(lookup)
                if matches!(
                    lookup.reader.metadata().database_type.as_str(),
                    "DBIP-Country-Lite" | "GeoLite2-Country" | "GeoIP2-Country"
                ) =>
            {
                DiscoveryOutcome {
                    source: lookup.source_label(),
                    build_epoch: lookup.build_epoch(),
                    lookup: Some(Arc::new(lookup)),
                    path,
                    available: true,
                }
            }
            _ => {
                tracing::warn!(path = %path.display(), "geo: selected Country generation unavailable; retaining Country fallback");
                country
            }
        }
    } else {
        country
    };
    if let Some(path) = city_override {
        return open_city_with_fallback(PathBuf::from(path), country);
    }
    match super::dataset::read(directory) {
        super::dataset::Selection::Country
        | super::dataset::Selection::Installed {
            profile: super::install::DatabaseProfile::Country,
            ..
        } => country,
        super::dataset::Selection::Installed {
            profile: super::install::DatabaseProfile::City,
            path,
        } => open_city_with_fallback(path, country),
        super::dataset::Selection::Automatic | super::dataset::Selection::City => {
            open_city_with_fallback(target.with_file_name("dbip-city-lite.mmdb"), country)
        }
    }
}

/// Run [`super::paths::discover_with`] and try to open whatever it finds.
/// Never panics: a missing file yields the no-DB state; a found-but-corrupt
/// file is logged once and also yields the no-DB state (the classification
/// of local/private addresses still works either way).
///
/// The environment values and the `exists` check are parameters so tests can
/// drive every outcome without mutating process-global env vars.
pub fn discover_and_open_with(
    exists: impl Fn(&Path) -> bool,
    env_override: Option<&str>,
    home: &str,
    xdg_data_home: Option<&str>,
) -> DiscoveryOutcome {
    match super::paths::discover_with(exists, env_override, home, xdg_data_home) {
        Some(path) => match MmdbLookup::open(&path) {
            Ok(lookup) => DiscoveryOutcome {
                source: lookup.source_label(),
                build_epoch: lookup.build_epoch(),
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
                    source: String::new(),
                    build_epoch: 0,
                }
            }
        },
        None => {
            let suggested = super::paths::default_suggested_path(home, xdg_data_home);
            tracing::info!(
                suggested_path = %suggested.display(),
                "geo: no country database found; geographic breakdown will show \
                 Local network / Unknown only until one is installed"
            );
            DiscoveryOutcome {
                lookup: None,
                path: suggested,
                available: false,
                source: String::new(),
                build_epoch: 0,
            }
        }
    }
}

struct CityFallback {
    city: Arc<dyn CountryLookup>,
    country: Option<Arc<dyn CountryLookup>>,
}
impl CountryLookup for CityFallback {
    fn lookup(&self, addr: IpAddr) -> Option<CountryInfo> {
        self.lookup_place(addr).map(|p| p.country)
    }
    fn lookup_place(&self, addr: IpAddr) -> Option<GeoPlace> {
        self.city.lookup_place(addr).or_else(|| {
            self.country
                .as_ref()
                .and_then(|lookup| lookup.lookup_place(addr))
        })
    }
    fn capability(&self) -> GeoCapability {
        GeoCapability::City
    }
}

fn open_city_with_fallback(path: PathBuf, country: DiscoveryOutcome) -> DiscoveryOutcome {
    match MmdbLookup::open(&path) {
        Ok(city) if city.capability() == GeoCapability::City => DiscoveryOutcome {
            source: city.source_label(),
            build_epoch: city.build_epoch(),
            path,
            available: true,
            lookup: Some(Arc::new(CityFallback {
                city: Arc::new(city),
                country: country.lookup,
            })),
        },
        Ok(_) => {
            tracing::warn!(path = %path.display(), "geo: selected City file has unsupported metadata; retaining Country fallback");
            country
        }
        Err(error) => {
            if path.exists() {
                tracing::warn!(path = %path.display(), %error, "geo: failed to open City file; retaining Country fallback");
            }
            country
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
        let outcome = discover_and_open_with(|_| false, None, "/nonexistent-home-for-tests", None);

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

        // The override is a parameter, not `$SNITCHWATCH_GEOIP_DB`: no
        // process-global state is touched, so this can't race other tests.
        let outcome = discover_and_open_with(
            |p| p == path,
            path.to_str(),
            "/nonexistent-home-for-tests",
            None,
        );

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

#[cfg(test)]
mod city_tests {
    use super::*;

    #[test]
    fn country_lookup_remains_country_only_and_ipv6_keys_are_canonical() {
        let lookup = fakes::FakeLookup::default().with("2001:4860::1", "CA", "Canada");
        let resolver = SharedResolver::new(Some(Arc::new(lookup)));
        let a = resolver.resolve("2001:4860::1");
        assert!(a.place.as_ref().unwrap().city.is_none());
        assert_eq!(resolver.capability(), GeoCapability::Country);
        assert_eq!(a, resolver.resolve("2001:4860:0:0:0:0:0:1"));
        assert_eq!(resolver.cache_handle().lock().unwrap().len(), 1);
        assert!(resolver.resolve("192.168.1.1").place.is_none());
        assert!(resolver.resolve("invalid").place.is_none());
    }

    #[test]
    fn absent_or_invalid_coordinates_never_become_origin() {
        assert!(GeoCoordinates::new(None, None).is_none());
        assert!(GeoCoordinates::new(Some(f64::NAN), Some(0.0)).is_none());
        assert!(GeoCoordinates::new(Some(91.0), Some(0.0)).is_none());
        assert!(GeoCoordinates::new(Some(0.0), Some(181.0)).is_none());
        assert_eq!(
            GeoCoordinates::new(Some(0.0), Some(0.0)).unwrap().latitude,
            0.0
        );
    }
}

#[cfg(test)]
mod fallback_tests {
    use super::*;
    use crate::geo::install::test_fixtures as fixtures;

    fn country() -> DiscoveryOutcome {
        DiscoveryOutcome {
            lookup: Some(Arc::new(fakes::FakeLookup::default().with(
                "8.8.8.8",
                "US",
                "United States",
            ))),
            path: PathBuf::from("fallback"),
            available: true,
            source: "fallback country".into(),
            build_epoch: 7,
        }
    }

    #[test]
    fn absent_corrupt_or_wrong_kind_city_retains_country() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("city.mmdb");
        for bytes in [
            None,
            Some(b"broken".to_vec()),
            Some(fixtures::fixture("DBIP-Country-Lite")),
        ] {
            if let Some(bytes) = bytes {
                std::fs::write(&path, bytes).unwrap();
            }
            let outcome = open_city_with_fallback(path.clone(), country());
            assert!(outcome.available);
            assert_eq!(outcome.source, "fallback country");
            let resolver = SharedResolver::new(outcome.lookup);
            assert_eq!(resolver.capability(), GeoCapability::Country);
            assert_eq!(
                resolver.resolve("8.8.8.8").bucket,
                Bucket::Country("US".into())
            );
        }
    }

    #[test]
    fn city_missing_country_uses_same_country_fallback_for_all_consumers() {
        struct MissingCity;
        impl CountryLookup for MissingCity {
            fn lookup(&self, _: IpAddr) -> Option<CountryInfo> {
                None
            }
        }
        let fallback = CityFallback {
            city: Arc::new(MissingCity),
            country: country().lookup,
        };
        let resolver = SharedResolver::new(Some(Arc::new(fallback)));
        assert_eq!(resolver.capability(), GeoCapability::City);
        let place = resolver.resolve("8.8.8.8").place.unwrap();
        assert_eq!(place.country.code, "US");
        assert!(place.city.is_none() && place.coordinates.is_none());
    }

    #[test]
    fn selected_city_uses_latest_imported_country_for_ipv6_misses() {
        use crate::geo::{dataset, install::DatabaseProfile};
        let dir = tempfile::tempdir().unwrap();
        let country_path = dataset::reserve(dir.path()).unwrap();
        std::fs::write(
            &country_path,
            fixtures::tree_fixture("DBIP-Country-Lite", &[[17, 17]], 24, 6),
        )
        .unwrap();
        dataset::commit(
            dir.path(),
            &country_path,
            DatabaseProfile::Country,
            crate::private_files::write_private,
        )
        .unwrap();
        let city_path = dataset::reserve(dir.path()).unwrap();
        std::fs::write(&city_path, fixtures::fixture("DBIP-City-Lite")).unwrap();
        dataset::commit(
            dir.path(),
            &city_path,
            DatabaseProfile::City,
            crate::private_files::write_private,
        )
        .unwrap();
        let outcome = selected_database(country(), None, &dir.path().join("GeoLite2-Country.mmdb"));
        assert_eq!(outcome.path, city_path);
        let resolver = SharedResolver::new(outcome.lookup);
        assert_eq!(resolver.capability(), GeoCapability::City);
        let resolved = resolver.resolve("2001:4860:4860::8888");
        assert_eq!(resolved.bucket, Bucket::Country("US".into()));
        assert!(resolved.place.unwrap().city.is_none());
    }
}
