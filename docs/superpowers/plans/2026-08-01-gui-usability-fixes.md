# GUI usability fixes — issues #15, #18, #19, #20 + scanner preflight

**Date:** 2026-08-01
**Context:** the 2026-07-31/08-01 real-hardware session
(`docs/superpowers/HANDOFF-2026-08-01-gui-usability.md`) surfaced four UI
defects and one unfiled DX papercut. Three were blocked on owner design
calls; those calls were made 2026-08-01:

| Issue | Decision |
|---|---|
| #18 | **Inline Allow/Deny buttons on pending rows**, plus per-process batch actions |
| #19 | **Aggregate view from `Ping.Statistics`** — rework the Traffic tab around what the daemon actually reports |
| #20 | **Pending-count badge** — never force-expand; manual collapse always wins; collapsed groups show pending count |

Also in scope: #15 (tray tooltip sanitization), the scanner-binary
preflight check, and one QML interaction-shaped test to start closing the
verification gap the handoff doc describes.

## Batch 1 — issue #20: collapse override + pending badge
Branch: `fix/collapse-pending-badge` (this doc rides along).

- `crates/snitchwatch-kirigami/src/connections/grouping.rs`: drop the
  `|| pg.counts.pending > 0` / `|| dg.counts.pending > 0` clauses from the
  expansion decisions in `build_projection` (lines ~402, ~417). Manual
  toggle state (`expanded_processes`/`expanded_domains`) and
  `filter_active` remain the only expansion inputs. Update the module-header
  design note (lines ~30-38, "pending rows always visible") to record the
  new contract: *pending work is signalled by the header badge, never by
  forced expansion*.
- `qml/ConnectionsPage.qml`: the header already renders `groupPending` as
  "N pending" — make it a visually distinct badge (highlight color when
  > 0) since it is now the only signal for hidden pending rows.
- Fix the latent wiring bug found during exploration: the "Grouped" switch
  (ConnectionsPage.qml:135) calls the auto-generated `setGrouped` property
  setter instead of the `setGroupedMode` invokable, skipping the model
  reset bracket. Route it through `setGroupedMode`.
- Tests: invert `pending_row_forces_group_expansion_even_when_collapsed`
  (grouping.rs:774) into a "collapse survives pending descendants, counts
  still update" test; keep
  `expand_collapse_toggles_control_non_pending_groups` green.

## Batch 2 — issue #15: tray tooltip sanitization
Branch: `fix/tray-tooltip-sanitize`.

- Fix at the source: `crates/snitchwatch-bridge/src/grpc_server.rs:269`
  builds the raw `what = "{process} → {dst_host}"` for `TrayState::
  RecentBlock`; build it from `sanitize_for_display(&row.process, 64)` /
  `sanitize_for_display(&row.dst_host, 64)` instead (the adjacent
  `safe_what` at lines 283-287 already does exactly this for
  notifications). Bridge-side fix covers both shells.
- Update `tray.rs` test `tooltip_recent_block_includes_what` (asserts the
  raw pass-through today) and add a grpc_server test feeding a
  control-char/bidi-laden process name and asserting the tray state's
  `what` is sanitized.

## Batch 3 — scanner preflight (unfiled papercut)
Branch: `fix/scanner-preflight`.

- `crates/snitchwatch-kirigami/src/scanner.rs::run_deep_scan`: before
  invoking pkexec, check the resolved scanner binary path exists; if not,
  return an actionable error naming the missing path and the
  `SNITCHWATCH_SCANNER_BIN` override (dev builds currently die with a bare
  exit 127 after the polkit prompt).
- Test alongside the existing `run_deep_scan_never_panics_*` test.

## Batch 4 — issue #19: Traffic tab → daemon aggregate stats
Branch: `feat/traffic-daemon-stats`.

The `Connection` proto has no byte counters, so the per-connection byte
chart can never populate (`translator/connection.rs:56-57` hardcodes 0).
`Statistics` (ui.proto:79-97) arrives on every `Ping` and is dropped except
for `events` (grpc_server.rs:204). Plumb the scalars through:

- `ws_messages.rs`: new `ServerMessage::DaemonStatistics` carrying
  `daemon_version`, `uptime`, `rules`, `connections`, `ignored`,
  `accepted`, `dropped`, `rule_hits`, `rule_misses` (skip the `by_*` maps
  for now — tiles first, breakdowns later if wanted).
- `grpc_server.rs::ping` (~204): emit it whenever `req.stats` is present.
- `bridge_dispatch.rs`: extend `interests_traffic` (or add
  `interests_statistics`) so `TrafficModel` receives it.
- `traffic_model.rs`: new qproperties for the stat scalars +
  `apply_server_message` arm.
- `TrafficPage.qml`: replace the dead byte-rate presentation with stat
  tiles (connections, accepted, dropped, rule hits, uptime, daemon
  version). Keep the `TrafficEvents` plumbing intact — if a real byte
  source ever appears the chart can return.
- Rewrite `connection_to_row_populates_all_visible_fields`'s byte
  assertions (connection.rs:142-143) so zeros are documented as
  "proto has no byte source", not asserted as desired behavior.
- Tests: grpc_server test that a `Ping` with stats broadcasts
  `DaemonStatistics`; traffic_model test folding the message into
  properties.

## Batch 5 — issue #18: inline verdicts + batch actions + interaction test
Branch: `feat/inline-verdict-buttons`. **Depends on Batch 1** (same QML
delegate); branch after it merges.

- `ConnectionsPage.qml` delegate: on `!row.isGroupHeader && row.pending`
  rows, add compact Allow/Deny buttons that call the existing
  `PendingDecision.submit(rowId, choice, scope, duration)` →
  `bridgeFeed.sendClientJson` chain (mirror PendingDecisionSheet.qml's
  default scope/duration tokens so inline and sheet decisions match).
  Buttons must accept the click so the row's `openInspector` doesn't fire.
- Batch action: new `ConnectionsModel` invokable returning the pending row
  ids for a process group key (JSON array, from the group tree /
  `RowStore`); QML "Allow all"/"Deny all" on process headers with
  `groupPending > 0` loops `submit` per id. No new WS protocol — N
  `SetVerdict` messages.
- Interaction test: extend the `connections_page_diagnostics_qml.rs`
  page-probe pattern — instantiate the real ConnectionsPage, drive a
  pending row in, invoke the inline-button handler, and assert on the
  Rust side (model/store state or emitted verdict JSON) rather than in
  QML, honoring the "QML asserts are not load-bearing" constraint. Goal:
  a click-path (handler → submit → verdict message) is exercised
  end-to-end for the first time.

## Acceptance

- [x] #20: collapsing a group with pending descendants stays collapsed;
      badge shows the pending count; unit tests updated.
- [x] #15: `RecentBlock` tooltip text is sanitized at construction.
- [x] Scanner: missing binary yields an error naming the path and the env
      override, before any polkit prompt.
- [x] #19: Traffic page shows live daemon aggregates from `Ping`.
- [x] #18: pending rows expose Allow/Deny inline; process headers expose
      batch actions; one interaction-path test exists.
- [x] All batches: CI was green for PRs #24–#28 before merge. The focused
      Kirigami suite passes locally with `CCACHE_DISABLE=1`, except for its
      socket-owning live-loop test, which this managed sandbox rejects with
      `EPERM`; the same sandbox restriction also prevents the workspace
      socket/listener tests. This is environmental, not a product-test
      failure.
- [ ] Real-hardware visual re-check of #18/#19/#20 remains a human step —
      record in the phase2 runbook convention.
      **Attempted 2026-08-02 from the managed development container:** the
      mounted host has a live `opensnitchd` process and bridge runtime
      artifacts, but the sandbox rejects host D-Bus and Unix-socket access
      with `EPERM`; it cannot operate the service or observe the GUI. The
      host also has neither the image-baked binaries nor the Flatpak
      installed. Later host-terminal access confirmed the bridge/daemon
      service path, but no Flatpak or compositor-visible Kirigami shell was
      installed, so the visual re-check remains open.
