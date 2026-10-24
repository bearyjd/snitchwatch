# Geographic map drilldown

Owner authorization: implement country → city → observed IP selection directly on the zoomable map, synchronize it with the existing connection tree and inspector, and retain optional City data with Country fallback. The owner approved test first implementation, a PR, independent adversarial review and fixes, green CI, and merge. Release publication and runtime downloads remain separate work.

The data source and finite import profiles are documented in [geo-city-data.md](../../geo-city-data.md). Positions are database estimates. Multiple observed IPs may share one position; neither labels nor zoom imply neighborhood or endpoint precision.

## Contracts and implementation

- `GeoModel.hierarchyJson()` version 1 provides countries, composite city keys and canonical observed IPs with verdict counts and known connection facts. `hierarchyChanged` invalidates one coalesced snapshot cache. Wheel, drag and pinch do not request or parse the hierarchy.
- `GeoPlace.city_key()` is the shared opaque identity for city aggregation and filtering. Country-only, local and unknown branches use the documented unavailable-city identity. Missing coordinates stay accessible through the virtual destination list.
- `MapCamera` uses the complete 3600 × 1800 world grid. Focus uses finite bounds; a non-wrapping date-line branch fits both world edges. Coincident points use a bounded focus zoom and retain one shared anchor.
- Cached semantic nodes show countries, cities, then observed IPs as zoom increases, with hysteresis and bounded spatial clusters. Stable keys derive from sorted member identities. Coincident IPs form a selectable stack with a virtual list, without geographic jitter.
- Geographic scope is separate from free-text search and selected-row highlighting. Typed Rust country/city/exact canonical IP predicates are AND-ed with existing filters. World clears only the geographic scope; Back selects the parent. A disappeared branch remains scoped and empty until explicitly changed.
- Live feed geography is prepared before the Qt queue hop, retained with accepted rows, and never lazily decoded by scope filtering on Qt. Missing prepared facts beyond the finite geographic capacity remain available in the unscoped list and are excluded from geographic scopes; the UI labels partial geographic coverage.
- List/app/domain/host selection highlights related destinations without narrowing scope or moving the camera. Existing tree actions and inspector decisions retain their existing ownership. Page-owned destination popups capture stable targets across refreshes.

## Verification and actual status

Implementation is in progress on the map feature branch; City backend is at `b26e2e9`. This document does not claim merged or packaged availability. Standalone Qt tests load production QML and exercise real wheel, drag, pinch, keyboard and marker clicks. They cover semantic scope, coincident IP selection, virtualized stacks, full-world coordinates, antimeridian focus, cached snapshots, fallback branches and camera preservation on live removals. The 31-test standalone Qt suite is green at this checkpoint.

Rust scope predicates, retained prepared-fact behavior after resolver cache overflow, and an in-process production QML + real `ConnectionsModel` probe are the next required gates. Full GUI checks, independent review, remote CI and merge remain pending. Manual AI-ready export remains future roadmap work under its existing security/design gate.
