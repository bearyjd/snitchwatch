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

Camera navigation merged in PR #217; optional City data and the bounded off-thread geographic hierarchy merged in PR #218 (`61e7f9b` on main). Semantic map navigation and typed scope integration are implemented on the map feature branch; this document does not claim merged or packaged availability for that portion.

Test first verification recorded the expected missing-scope and missing-model-command failures, then five pure Rust geographic predicate tests and the in-process production QML + real `ConnectionsModel` integration passed. The latter proves canonical exact-IP scope AND preserved search, filtered leaf counts independent of group headers, list/app highlight IDs, invalid-scope rejection, and IP map activation revealing/selecting/inspecting the matching connection. A regression compares list/map session-qualified prepared IDs and rejects stale preparer sessions.

Standalone Qt tests load production QML and exercise real wheel, drag, pinch, keyboard and marker clicks. Independent visual review reproduced and verified fixes for compact-country stage skipping, invisible keyboard target highlighting, adjacent-cell marker overlap, large stack glyph overflow, and long city/IPv6 label overlap. The independent 39-test full standalone suite and dedicated drilldown/count/label probes passed; the subsequent country-context label refinement has its own red/green regression. Anchors are never jittered. Country-only, local, unknown and missing-coordinate destinations remain reachable in the virtual list, including keyboard input; disappeared branches remain explicitly scoped.

Full native GUI checks, all-target Clippy, independent source review and remote CI remain pending at this checkpoint. Manual AI-ready export remains future roadmap work under its existing security/design gate. No provider API integration or automatic rule application is included.
