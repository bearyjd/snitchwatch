//! Qt-free geographic breakdown logic (per-country aggregation of connections).
//!
//! Mirrors the `connections::row_store` split: everything that can be tested
//! without a Qt runtime lives here, and the thin cxx-qt `GeoModel` wrapper
//! (see `crate::geo_model`) only replays [`store::GeoStore`] output behind the
//! right `QAbstractListModel` begin/end signals.
//!
//! Submodules:
//!   * [`paths`] — GeoLite2-Country.mmdb discovery (env override, XDG data
//!     dir, common system paths). Pure/testable: filesystem existence is
//!     injected as a closure so tests never need a real `.mmdb`.
//!   * [`flag`] — ISO alpha-2 country code -> flag emoji (regional indicator
//!     symbols), a pure function.
//!   * [`resolver`] — the [`resolver::CountryLookup`] trait wrapping
//!     `maxminddb`, private/loopback/link-local IP classification, and
//!     startup discovery-and-open glue.
//!   * [`store`] — [`store::GeoStore`], the per-country aggregate, consuming
//!     the same `ServerMessage` connection-row variants
//!     `connections::row_store::RowStore` does.
//!   * [`world_map`] — bundled Natural Earth country shapes and marker
//!     centroids (offline data, see `docs/map-data.md`).

pub mod flag;
pub mod paths;
pub mod resolver;
pub mod store;
pub mod world_map;

use std::path::PathBuf;
use std::sync::OnceLock;

use resolver::SharedResolver;

/// What the one process-wide discovery found.
#[derive(Clone)]
struct SharedGeo {
    resolver: SharedResolver,
    available: bool,
    path: PathBuf,
}

static SHARED_GEO: OnceLock<SharedGeo> = OnceLock::new();

fn shared_geo() -> &'static SharedGeo {
    SHARED_GEO.get_or_init(|| {
        let outcome = resolver::discover_and_open();
        SharedGeo {
            resolver: SharedResolver::new(outcome.lookup),
            available: outcome.available,
            path: outcome.path,
        }
    })
}

/// The process-wide resolver: the database is opened once and every model
/// shares its cache. Tests that need a fake build their own `SharedResolver`
/// and inject it (`ConnectionFilter::with_geo`, `GeoStore::with_resolver`).
pub fn shared_resolver() -> SharedResolver {
    shared_geo().resolver.clone()
}

/// [`shared_resolver`] plus the discovery result the Geo page reports.
pub fn shared_discovery() -> (SharedResolver, resolver::DiscoveryOutcome) {
    let geo = shared_geo();
    (
        geo.resolver.clone(),
        resolver::DiscoveryOutcome {
            lookup: None,
            path: geo.path.clone(),
            available: geo.available,
        },
    )
}

#[cfg(test)]
mod shared_tests {
    use super::*;

    #[test]
    fn the_shared_resolver_is_one_instance_with_one_cache() {
        let a = shared_resolver();
        let (b, outcome) = shared_discovery();
        assert!(std::sync::Arc::ptr_eq(&a.cache_handle(), &b.cache_handle()));
        assert_eq!(outcome.available, a.has_database());
    }
}
