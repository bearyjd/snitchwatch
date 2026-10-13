# Little Snitch-style redesign — Phase 2 (Network Monitor)

Parent plan: bazzite-tower `.omc/plans/snitchwatch-ls-redesign-2026-10-12.md` (Phase 2); Phase 0+1:
`2026-10-12-ls-redesign-phase01.md`. Interaction model only: no Little Snitch logo, icons, artwork or copy.
Paths below are under `crates/snitchwatch-kirigami/` unless stated. Plan only; no code in this PR.

## Goals

1. One nested **program > domain > host** tree replacing today's connections list.
2. Per-row colored **rule button** (allow green, deny red, partial gray, needs-rule blue, pending orange —
   `qml/Theme.qml` colors from Phase 0) that creates or deletes a rule at that row's level.
3. Inspector as a **side panel**; the OverlaySheet inspector is removed.
4. Token search, "From now" time filter, denied-only filter.
5. **World map** (featured, see Layout): offline country shapes, markers colored by allowed/denied/pending,
   pulse on connection events, clustered; clicking a marker filters the list. Country-level markers only
   (RESOLVED Q1).

## Layout (RESOLVED Q3, 2026-10-13)

The map is a featured part of the Monitor view, **visible by default**, not behind a toggle: a hero band
above the tree in a vertical `SplitView` (inspector stays the right-hand side panel, step 5), resizable by
dragging the handle. The band may be collapsed (a header chevron / drag to zero) and remembers that for the
session, but a fresh start shows it. When there is no data yet it shows the empty state, never nothing.
It must degrade on narrow windows: the band shrinks to a minimum height before the tree does.

## Non-goals

- Byte/throughput charts or "traffic volume" pulses: opensnitchd has no byte counters, so pulses mean
  "a connection event happened", never "data flowed".
- Online tiles, QtLocation, any network lookup, any geo-IP database or download. Rules window and sidebar shell (Phases 3, 4).
- Changing the bridge wire protocol unless step 3 proves it necessary.

## Current state (evidence)

- Grouping is already a Qt-free two-level tree, process > registrable domain, with incremental counts and a
  flattened `VisibleEntry` projection (`src/connections/grouping.rs:1-43`, `VisibleEntry` at :194, `GroupTree`
  :217, `build_projection` :401). The model exposes it as a **flat list with `depth`/`isGroupHeader`/`groupKey`
  roles** (`src/connections_model.rs:44-94`); a grouped-mode or filter change is a full model reset.
  Leaves are single connections (`dst_host` per row), so the **host level is new**: a host node aggregating
  its connections, between domain and leaf. Domain is a last-two-labels heuristic (`grouping.rs:40`).
- `qml/ConnectionsPage.qml` is **799 lines** (`wc -l`); `tests/simulate_connection_qml.rs:327` asserts
  `lines < 800`. It holds search/pending-only/grouped switches (:205-236), ~30 `inspect*` properties
  (:71-121) and the inspector `SizedOverlaySheet` (:678). It cannot grow; Phase 2 splits it (step 1).
- Filters live in Rust: `ConnectionFilter { query, pending_only }` (`src/connections/filter.rs:27-60`); QML
  only forwards input.
- Per-level rules are **already expressible**: bridge `VerdictScope::{ThisHost, AnyHostOnDomain, AnyHost}`
  (`snitchwatch-bridge/src/ws_messages/verdict_wire.rs:32-40`). `AnyHostOnDomain` wildcards the leftmost label
  and also covers the apex (`translator/verdict.rs:388-396`, built at :564); `AnyHost` drops the host operator
  (program-wide); all are bound to the program path (`bind_to_process`). It degrades to `ThisHost` for bare
  IPs / public-suffix parents (`grpc_server/tests.rs:545`). So: host row = ThisHost, domain row = AnyHostOnDomain,
  program row = AnyHost.
- Rule creation after the fact: `make_rule::add_rule_message(row, choice, scope, duration, ...)`
  (`src/make_rule.rs:71`) already takes a scope token, but is **gated** by `offers_make_rule` =
  deferred or default-decided rows only (:61). The delete path exists (`RulesModel.delete_rule(name)`,
  `src/rules_model.rs:159`; `DeleteRule` at `bridge_dispatch.rs:559`). Missing: mapping a tree node to "the
  rule(s) that exist at this level" for the button state.
- Inspector today: `SizedOverlaySheet` with FormLayout (Host/IP/Port ...) and verdict actions
  (`ConnectionsPage.qml:676-700`).

## Data sources and licenses

**GeoModel** (`src/geo_model.rs`, `src/geo/`): per-country aggregate only (roles countryCode, countryName,
flag, total, allowed, denied, pending; :175-184). Country comes from an **operator-supplied MaxMind-format
`.mmdb`** read with the `maxminddb` crate (`Cargo.toml:68`; `src/geo/resolver.rs:169-190`); discovery in
`src/geo/paths.rs:1-47` (`$SNITCHWATCH_GEOIP_DB`, `$XDG_DATA_HOME/snitchwatch/GeoLite2-Country.mmdb`,
`/usr/share/GeoIP`, `/var/lib/GeoIP`). Nothing is vendored or downloaded ("we never download or vendor a
GeoIP database", `paths.rs:3`); no database means a setup placeholder. Private/loopback IPs are bucketed
without lookup. Lookups are already fully offline. No RDAP.

| Source | License | Use | Notes |
|---|---|---|---|
| Natural Earth admin-0 countries | Public domain (naturalearthdata.com/about/terms-of-use; verify wording at step 8) | Map shapes | 110m zip is 210 KB (naturalearthdata.com downloads page, v5.1.1); 50m size to measure. Ship the **110m** (small countries become markers anyway), simplified to one path file. |
| DB-IP City Lite (.mmdb) | CC BY 4.0, attribution link to db-ip.com required (db-ip.com/db/download/ip-to-city-lite) | Marker coordinates + country | Monthly updates. Official City Lite MMDB listed at ~121 MB (Oct 2026); Country Lite is far smaller. City-level, approximate. Same `maxminddb` format, so `resolver.rs` is reused. |
| MaxMind GeoLite2-City/Country | MaxMind EULA: free account + license key, no redistribution of the DB, must update/delete old copies | Today's de-facto source | Cannot be bundled or Flatpak extra-data'd without each user's key. |
| IPtoASN / ip2location LITE | Not evaluated in depth (ip2location LITE is CC BY-SA 4.0 per vendor, share-alike; IPtoASN is ASN-only, no coordinates) | Rejected | Share-alike and no coordinates. |

**Owner decision (RESOLVED Q1, 2026-10-13): country-level markers only.** No city database, no download
button, no attribution UI: markers sit at the country centroid taken from the bundled Natural Earth shapes,
and the existing operator-supplied Country `.mmdb` (`geo/paths.rs`) stays the only IP source. The DB-IP
text below is kept as the rejected alternative; step 8 shrinks accordingly.

**Rejected alternative (was the recommendation):** DB-IP City Lite, **not bundled in the image or Flatpak**;
the user chooses to fetch it once with an explicit button in the Geography/Map empty state, saved at the
existing XDG path. Obligations: CC BY 4.0 credit "IP geolocation by DB-IP" with link in the map's footer and
About text; no endorsement claim; keep it unmodified or state changes. Fetch cadence monthly, manual (no
background updater). Flatpak: the sandbox has no `--share=network` by design (`packaging/flatpak/
org.snitchwatch.Snitchwatch.yml:23-26`), so the fetch must go via the bridge/host side or the user drops the
file in; **do not add network permission to the GUI**. Alternative if the owner wants zero attribution UI and
no fetch: keep country-level markers (country centroid from bundled Natural Earth), no city DB at all.
Privacy: every lookup is a local file read; the monitor never sends destination IPs anywhere.

## Risks

- **Tree scale (thousands of rows).** Keep the flat model + depth role and `ListView` (Qt `TreeView` needs a
  true `QAbstractItemModel` with parent/index, a rewrite of `connections_model.rs` at 1647 lines). Cost to
  fix: today every filter/expand is a full reset; replace with row-range insert/remove on expand/collapse and
  `dataChanged` on counts. Gate: a test building 5k rows asserts a collapse touches O(children) signals.
  Hosts are lazily materialized only when their domain is expanded.
- **Honest-UI guards** (`tests/honest_ui_*`, `qml_source_guards.rs`) pin wording. Map text must say markers
  are the *approximate* location of the destination IP (CDN/anycast skews), pulses mean connection events,
  colors are counts of decisions. Update guards to the new intent and say so in the PR; never delete one.
- **Accessibility.** Color is never the only cue: each button state also has a text label/tooltip and a
  distinct glyph (check, cross, half, plus, clock); markers get a shape per verdict and an Accessible.name
  ("3 denied, 1 pending, Germany"); the map has a keyboard-reachable country list (existing GeoPage list).
- **Rule button safety.** Deny at program level (`AnyHost`) is broad: require confirm text naming scope and
  duration; unbindable programs (no bindable process path; see `make_rule.rs:69`, issue #44) get a disabled button with a reason.
- **Pulse rate.** Throttle to one animation per marker per 500 ms; honour reduced-motion.

## Work breakdown (small PRs, in order; each TDD, `just check`, cargo test/clippy offscreen, `just qml-test`)

1. **Split `ConnectionsPage.qml`** (no behavior change). *Landed in `feat/monitor-tree-step1`, which also
   carries the host level and ranged signals from step 2 (see the note there).* Extract `ConnectionRow.qml` (delegate :251-610),
   `InspectorState.qml`/inspector body, `ConnectionsHeader.qml`. Tests: existing
   `tests/*connections*qml.rs` pass untouched; move the 800 pin in `simulate_connection_qml.rs:327` to
   a per-file budget (page < 400, each new file < 400) and say so. Add `include_str!` guards for new files.
2. **Host level in the tree** (Rust only). *Partly landed with step 1: `GroupTree` tracks hosts,
   `build_host_projection` emits `HostHeader` rows, the model maps them (depth 2, connections depth 3) and
   grouped expand/collapse and filter changes use ranged signals (`connections/projection_diff.rs`, 5k-row
   test). The shipped model still uses the two-level `build_projection`, so the look is unchanged; what
   remains is the `groupLevel` role, a `toggleHostGroup` invokable and switching the model to the host
   projection together with the UI that renders host rows.* Extend `VisibleEntry` with `HostHeader`, `GroupTree` with a host
   key, new role `groupLevel` (0 program/1 domain/2 host/3 connection). Tests first in `grouping.rs`:
   aggregation, expand state, filter forces path open, pending badge unchanged, incremental updates.
   Replace the reset-per-change with ranged signals (perf gate above).
3. **Level rule state** (Rust). Program-level button = `AnyHost` (program-bound) behind a confirm dialog that
   names scope and duration; the duration is the Settings `default_duration` (RESOLVED Q2). New `LevelRules` projection: for a node (program path, scope, host/domain)
   find existing rules from `RulesModel` data and return {allow, deny, partial, none, pending}; roles
   `levelRuleState`, `levelRuleName`. Add `add_level_rule_message` that drops the `offers_make_rule` gate for
   the button path only (`make_rule.rs:61` stays for the sheet). Tests: each scope maps to the verdict scope
   above; degraded scope (bare IP, `co.uk`) reports the degraded level, never a broader one.
4. **Rule button** (QML + controller). `RuleButton.qml`: state color + glyph + text, click cycles
   create/delete via `make_rule_controller` and `RulesModel.delete_rule`; outcome shown from the
   `RuleCommandResult`, never assumed (honest-UI pattern in `make_rule.rs:24-30`). Tests: QML state table,
   disabled-with-reason for unbindable, guard that the label text is not color-only.
5. **Inspector side panel.** `InspectorPanel.qml` in a `SplitView`/right column (collapses under a width
   threshold to a stacked page, not a sheet); delete the `SizedOverlaySheet` inspector. Update
   `stuck_pending_inspector_qml.rs`, `simulate_connection_qml.rs` and `sheet_*` guards to the panel. Keep all
   verdict actions and the "Simulate this connection" fixed text.
6. **Filters.** Extend `ConnectionFilter` (`filter.rs`): token search (`app:`, `host:`, `port:`, `country:`,
   free text AND-ed), `since_ms` ("From now" = filter start time, session-only), `denied_only`. Tests first
   in `filter.rs`, including blank/whitespace tokens and the "filter forces path open" rule. QML: three
   controls in `ConnectionsHeader.qml`, forwarding only.
7. **Map data build step.** Script (Python or Rust in `xtask`-style, offline, run at dev time) converts the
   Natural Earth 110m GeoJSON to a compact path file committed under `qml/map/` with a hash + source note in
   `docs/`. Tests: file under a size budget (target < 150 KB, measure), every ISO-A2 in the file, round-trip
   check of polygon count. Rendering: `Shape`/`ShapePath` from the preprocessed `SVG path` strings in QtQuick.Shapes
   (in the KDE runtime already; QtLocation would need plugins and tiles, so not used).
8. **Country markers** (Rust, reduced by Q1). No `PointLookup`, City DB, attribution or download. Marker
   position = country centroid computed at map-build time (step 7) and stored in the map file; `GeoModel`
   already supplies per-country counts. Tests: every country the resolver can return has a centroid.
9. **Map view** (featured band, see Layout; built after the tree, not in step 1). `MapView.qml`: shapes, markers by verdict (shape + color), clustering by grid bucket at
   current zoom (Rust-side, tested), pulse on events (throttled, reduced-motion aware), a country/cluster
   click calls `ConnectionsModel.setFilterQuery("country:XX")` or a geo filter. Tests: cluster math, marker
   click filter, honest-text guard, accessible names.
10. **Docs.** Update `docs/CODEMAPS/*` and README in the same PR as each step that changes its subject.

## Open questions for the owner

All three RESOLVED by the owner on 2026-10-13 (recorded in the step-1 PR).

1. **Geo-IP source and attribution.** RESOLVED: **country-level markers only for now.** No geo-IP database,
   no download, no DB-IP attribution; centroids come from the bundled map data.
2. **Rule scope on the button.** RESOLVED: the program-level button creates **`AnyHost` program-bound rules
   behind a confirm dialog**; the duration is the **Settings `default_duration`** (Phase 1), not Forever.
3. **Map placement.** RESOLVED: the map is **featured**: visible by default as a prominent, resizable band
   above the tree (splitter) in the Monitor view, collapsible but starting shown; not a hidden tab or
   toggle. See Layout.
