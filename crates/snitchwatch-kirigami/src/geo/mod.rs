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

pub mod flag;
pub mod paths;
pub mod resolver;
pub mod store;
