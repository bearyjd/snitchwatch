# Overnight GUI Fixes and Bounded Features Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish the owner-requested GUI repairs, then add three bounded clarity improvements, with TDD, independent adversarial review, green CI and separately reviewable packaging.

**Architecture:** Preserve the existing OpenSnitch bridge contract and live connection snapshots. Own menus at page lifetime, keep optional GeoIP and scanning behind explicit capability boundaries, and build the proposed extras from observed metadata without changing enforcement semantics.

**Tech Stack:** Rust workspace, CXX-Qt, Qt 6/Kirigami QML, private D-Bus tests, OpenSnitch, Flatpak, polkit and MMDB.

**Spec:** [Competitive roadmap and owner requirements](../specs/2026-10-24-competitive-feature-roadmap.md); [existing r16 plan](2026-10-22-r16-platform-upgrade.md); [existing manual-export plan](2026-10-22-log-export-for-analysis.md). This plan narrows the candidate backlog; it does not approve all 64 features.

**Status:** Owner-approved on 2026-10-24, including the three additions and test
boundaries. Execute the agreed PR, adversarial review, fix, re-review, CI and
merge gates autonomously. Record progress in this plan's SDD ledger.

## Global Constraints

- Component A: Kirigami GUI + bridge on OpenSnitch, including host/image dependencies, plus the owner-requested repair to the existing GUI/host-scanner bridge in Task 4. No broader Component B scanner redesign, app merge or replacement firewall engine.
- “The same GUI must support both `/proc` and eBPF hosts.” The GUI must not change daemon monitoring configuration.
- “Unavailable data stays unavailable.” Source presence, tests, reviewed commits, merged source and installed artifacts are separate evidence.
- “Aggregate daemon statistics are not per-app byte counters.” GeoIP context is not a threat verdict.
- Manual AI-ready export remains subject to its existing security gate; no provider routing or automatic policy application tonight.
- Existing authorization covers the requested repairs and the three additions below. Repairs remain first priority.
- Use the owner's explicit PR/push authorization already supplied in this session; do not request it again. Never push tower-owned candidate refs or change candidate variables.
- Do not install host policies, replace the running host service, publish a release, or enable eBPF without explicit authorization for that action.
- Preserve file-size limits, capability checks, permission boundaries and unrelated dirty work; fix structure rather than weakening guards.

## Review Focus

1. Recreated/reordered rows and daemon reconnects: an open menu retains its original scope and cannot dispatch into another session (Tasks 1 and 6).
2. Missing/contradictory mode evidence: `/proc` stays supported; configured eBPF is never presented as proven active eBPF (Task 2).
3. Malformed, incompatible, oversized or interrupted database imports: preserve the last working database and give actionable errors (Task 3).
4. Denied, missing, hung or spoofed scanner service: bounded failure with no caller-supplied executable, arguments or privileged output path (Task 4).
5. Hostile/absent metadata and stale previews: plain-text explanation, exact scope, honest unknowns and no changed-target action (Tasks 5–7).

## Priority and delivery shape

**Must finish first:** persistent menus; truthful dual-mode diagnostics; working GeoIP setup with the owner's DB-IP preference; constrained Deep Scan integration; current CI failures; source/artifact traceability.

**Approved additions, in order:** D01 offline “What is this?” explanation; P03 exact decision-scope preview; copy executable/host/IP/port details as a supporting C01/D01 usability addition. These reuse current data and do not add an engine or a service.

**Preserve and verify:** R05 already has matched-rule display and “Show rule” navigation in `InspectorPanel.qml`; do not sell that existing path as a new feature or infer event-time rule revisions.

**Stretch only after all mandatory gates:** source-backed C03 verdict counts with an explicitly defined cache/time window, or an X01/X02 export schema and fixtures under the existing security design. Neither is promised tonight; do not start both.

**Deferred:** persistent history, AI/provider integration, traffic-byte accounting, quotas, remote control, automatic database updater, new firewall engine, automatic eBPF enablement and a general host-deployment redesign. O01 freshness work waits for verified source timestamps and enforcement evidence.

## Task 0: Establish ownership, baseline and evidence ledger

**Files:** current dirty paths from `git status --short`; `.github/workflows/ci.yml`; this plan.
**Interfaces:** produces a path/commit ownership map and a short gate ledger; no product API change.

- [ ] Record branch, HEAD, PR #206 state and its latest exact commit/check results; reconcile newer work before editing.
- [ ] Classify staged/unstaged/untracked paths into menu, diagnostics, GeoIP, scanner, packaging and unrelated documentation; never blanket-add the working tree.
- [ ] Record inherited proof honestly: reported menu follow-up 15 tests, health 24 bridge + 5 GUI + QML, Geo installer 4 unit tests; rerun relevant gates against the assembled head.
- [ ] Record known PR #206 `ConnectionsPage.qml` budget failure (403 lines versus a limit below 400). The current follow-up is 383 lines after replacing obsolete task-history comments with a concise behavior description; rerun the existing guard without relaxing its threshold.
- [ ] Treat `dist/snitchwatch-system-r15.1-fixed.flatpak` built from `4c966b7` as historical proof only; it predates follow-up fixes.
- [ ] Keep completed work separate from pending proof. For new behavior, record failing test before implementation; never invent red-test evidence for inherited edits.

## Task 1: Stabilize menus and all action entry points (C01/Q01)

**Files:** `qml/{ConnectionsPage,ConnectionActions,ConnectionRow,RuleButton,LevelRuleActions}.qml` under `crates/snitchwatch-kirigami/`; `src/connections_model{.rs,/queries.rs}`; `tests/{connections_row_click_qml,level_rule_actions_qml,qml_source_guards}.rs` and `tests/qml/`.
**Interfaces:** existing model action functions and `currentSourceSession()`; page-owned immutable action target containing level, program, key and source session.

- [ ] Add failing regressions for any uncovered path: right-click, Actions and RuleButton through real pointer events, then inject model reset, reorder and row removal while the menu is open.
- [ ] Assert original scope survives updates; reconnect/session change invalidates menu and confirmation; activation dispatches once; Escape/focus restoration and unavailable actions behave consistently.
- [ ] Run each new test and save the expected failure. Preserve the existing regression evidence for already-implemented behavior.
- [ ] Complete the smallest page-owned menu/session fix and extract a focused QML component if needed to satisfy the existing file budget.
- [ ] Run `cargo test -p snitchwatch-kirigami --test connections_row_click_qml --test level_rule_actions_qml --test qml_source_guards --locked` with the headless Qt environment used in CI, then `just qml-test`.
- [ ] Reviewer checks actual production-page wiring separately from structural QML mirrors. In-process pointer tests catch wiring/reset bugs; they do not prove real Wayland/compositor behavior.
- [ ] Commit only this task's source/tests once focused gates pass; do not merge while any menu follow-up remains pending.

## Task 2: Finish dual-mode diagnostics without changing host configuration (Q04)

**Files:** `crates/snitchwatch-bridge/src/diagnostics/{mod,kernel_probe}.rs`; GUI `src/daemon_health_model.rs`; `qml/{DaemonHealthPage,DiagnosticsPage}.qml`; `tests/daemon_health_model_qml.rs`.
**Interfaces:** existing structured diagnostic result and GUI health projection. The current protocol does not report the active monitor: retain unknown rather than infer it from kernel probes or configuration.

- [ ] Add missing red fixtures for healthy core checks with unknown monitor, older reports containing an eBPF-capability check, real process-monitor warnings, disconnected service and incomplete core health.
- [ ] Assert absent optional eBPF/BTF readiness does not mark a functioning `/proc` host unsupported. Preserve actual daemon-reported issues; do not claim a successful fallback or active method without evidence.
- [ ] Implement only missing branches and copy, preserving shared connection/rule behavior and read-only diagnostics.
- [ ] Run `cargo test -p snitchwatch-bridge diagnostics --locked` and `cargo test -p snitchwatch-kirigami --test daemon_health_model_qml --locked`; run existing external-text guards on daemon-provided messages.
- [ ] Commit diagnostic changes with their focused test evidence. Host mode switching is outside this task.

## Task 3: Complete GeoIP import and plan redistributable DB-IP data (D03 prerequisite)

**Files:** GUI `src/geo/{mod.rs,install.rs,install/}` and `src/geo_model.rs`; `qml/{GeoPage,GeoDatabaseSettings}.qml`; `tests/geo_qml.rs`; packaged-data manifests belong to Task 8.
**Interfaces:** existing installer API; user-writable database override first, packaged country database fallback second; no credential requirement for the default DB-IP option.

- [ ] Verify official DB-IP Country Lite monthly MMDB source, actual metadata/type compatibility, current license and required attribution before choosing build inputs. Do not assume a MaxMind-only database-type check accepts it.
- [ ] Add red import tests for real compatible DB-IP metadata, supported MaxMind country metadata, wrong MMDB type, corrupt bytes, oversized input, failed copy and interrupted/failed replacement.
- [ ] Assert failed import preserves the previous file, private app-data destination, understandable failure, and truthful restart/reload state.
- [ ] Keep official browser-download guidance plus a file chooser/import path; support DB-IP and existing MaxMind users. A missing database offers setup rather than implying a healthy geolocation source.
- [ ] Preserve explicit environment/user overrides and existing supported system database paths, then add `/app/share/snitchwatch` and `/usr/share/snitchwatch` packaged fallbacks. Record selected source and dataset age when known; bundled data must not silently override a user's existing database.
- [ ] Run existing installer unit tests and `cargo test -p snitchwatch-kirigami --test geo_qml --locked`; add external-text and restart-path coverage where missing.
- [ ] Commit installer behavior. Task 8 owns the pinned monthly HTTPS input, SHA-256, build timestamp, CC BY 4 attribution and release-based update instructions.
- [ ] No background updater/network broker tonight. Do not commit a database binary to git or invent a distribution policy; use reviewed packaging inputs and honor owner storage decisions.

## Task 4: Complete constrained Deep Scan behavior and failure handling

**Files:** `crates/scanner-broker/{Cargo.toml,src/lib.rs,src/main.rs}`; `crates/scanner-privileged/src/main.rs`; GUI `src/{scanner,scanner_controller}.rs`; `qml/ScannerPage.qml`; `tests/{scanner_controller_qml,scanner_status_qml}.rs`.
**Interfaces:** fixed no-argument session-bus broker method, fixed privileged helper invocation, bounded typed result; packaging/activation is Task 8.

- [ ] Add red tests for helper absent, authorization denied/cancelled, broker absent, invalid result, child timeout, output overflow, disconnect and forged/unexpected request parameters.
- [ ] Verify the broker never accepts caller-controlled commands/arguments; the privileged helper restricts database paths, validates inputs and has a root-side watchdog.
- [ ] Finish typed error mapping and meaningful unavailable/denied/failed UI states; do not report a fake execution as a completed scan.
- [ ] Run `cargo test -p scanner-broker -p scanner-privileged --locked` and the two focused scanner QML suites; inspect exact workspace package names before execution.
- [ ] Use fake execution/private D-Bus to prove transport, constraints and failure propagation. These tests cannot prove desktop polkit authentication or real host installation.
- [ ] Commit scanner logic independently of installation policies. Obtain separate adversarial security review before any host integration is approved.

## Task 5: Add offline “What is this?” inspector explanation (proposed D01)

**Files:** GUI `qml/{InspectorPanel,InspectorState,ConnectionsPage}.qml`; create `qml/ConnectionExplanation.qml` and register in `build.rs` if a component is needed; add `tests/connection_explanation_qml.rs`.
**Interfaces:** consumes the existing inspect snapshot (program/path, host/IP, port/protocol, verdict and recorded matched rule); produces read-only plain-text explanation.

- [ ] Inventory existing inspector and “Show rule” behavior first; extend the surface rather than adding a duplicate page.
- [ ] Write red tests: observed fields produce a factual app-to-destination sentence; missing process/DNS/rule data stays unknown; HTML-like names remain literal; private/local addresses are described only when verifiable.
- [ ] Assert no reputation verdict, inferred provider ownership, invented intent or network request; cached verdict explanation does not claim current policy was replayed.
- [ ] Implement minimal explanation with field provenance/limitations and preserve the existing matched-rule jump only when an identifier exists.
- [ ] Run the new QML suite, existing connection inspector/navigation tests and external-text guards; record focused pass before committing.

## Task 6: Add exact scope preview using the actual action payload (proposed P03)

**Files:** GUI `qml/{LevelRuleActions,ConnectionActions,PendingDecisionSheet,MakeRuleSheet}.qml` as required by the existing scope surfaces; `tests/level_rule_actions_qml.rs` plus relevant pending/make-rule suites.
**Interfaces:** preview derives from the same captured level/program/key/duration/session used by dispatch; existing `createLevelRule(...)` remains authoritative.

- [ ] Inventory current app-wide confirmation and narrower action labels; do not add duplicate confirmation steps where existing wording can show the exact scope.
- [ ] Write red tests for app-wide/all-hosts, domain and exact-host scope, duration, and a port-limited action only where the existing operand actually carries a port restriction.
- [ ] Assert absent port restriction reads “any port”; missing app binding is explicit; domain text is not misrepresented as an exact host; capability refusal stays visible.
- [ ] Add red changed-duration/session tests: an open preview must not submit a scope or duration different from what was shown; require reopening when its contract changes.
- [ ] Implement a shared scope description from the dispatched payload, without creating unsupported operands or changing rule defaults.
- [ ] Run focused interaction tests and `just qml-test`; verify cancel dispatches nothing and acceptance dispatches exactly the displayed target; commit.

## Task 7: Add explicit copy-details actions (proposed C01/D01 support)

**Files:** GUI `qml/{ConnectionActions,InspectorPanel}.qml`; existing clipboard bridge if present, otherwise a narrow GUI clipboard helper registered in `build.rs`; `tests/connections_row_click_qml.rs` and a focused copy-details test.
**Interfaces:** copy only the captured executable path, host, destination IP or port; no policy mutation and no hidden network lookup. Initial scoped search found no existing clipboard helper, so confirm before choosing a minimal Qt clipboard bridge.

- [ ] Locate existing clipboard support; reuse it. Write red tests asserting each action copies exactly its displayed full value, including spaces, IPv6 and literal markup.
- [ ] Assert missing values disable the corresponding action; a row reorder does not change the copied target; stale-session behavior matches the owning menu contract.
- [ ] Implement named copy actions with accessible labels; do not silently copy unrelated fields or raw command-line/environment data.
- [ ] Run copy/interaction and external-text suites; use a headless clipboard seam for exact bytes and reserve desktop clipboard proof for Task 9; commit.

## Task 8: Package host integration and country data in separate PRs (Q08)

**Files:** `packaging/scanner/{stage.sh,test-staging.py,README.md,snitchwatch-scanner.conf,org.snitchwatch.Scanner.service}`; scanner polkit policy; both Flatpak manifests; only necessary Cargo/build metadata.
**Interfaces:** activation launches the fixed broker/helper; GUI capability detection explains missing host integration; country data is a pinned read-only build input with attribution.

- [ ] Add failing staging/shape tests for executable paths, activation names, permissions, narrowly allowed bus access, denied arbitrary arguments and missing helper behavior.
- [ ] Add packaging tests for pinned DB-IP monthly source/hash, attribution installation, lookup paths and offline runtime behavior; official-source verification must precede pin selection.
- [ ] Bundle the hash-pinned monthly MMDB as a Flatpak build asset at `/app/share/snitchwatch`; native packaging uses `/usr/share/snitchwatch`. Do not place a large database blob in git.
- [ ] Finish deterministic staging and manifests without broad filesystem/session-bus permissions; keep scanner host packages distinct from the Flatpak application.
- [ ] Run `python3 packaging/scanner/test-staging.py`, existing packaging shape tests, `just package-check` and relevant Flatpak manifest validation.
- [ ] Open a separate scanner-host packaging PR and a country-data packaging PR if data bundling is approved; keep feature logic reviewable without asserting host deployment occurred.
- [ ] Record install/rollback instructions and missing manual proof. No actual polkit/system service installation occurs merely because this plan exists.

## Test boundaries for the proposed scope

| Boundary | What the tests catch | What they do not establish |
|---|---|---|
| In-process real QML and model messages | Click wiring, live resets, captured scope, session changes, explanation and preview text | Real Wayland grabs, desktop focus and hardware behavior |
| In-process parsers and health projections | Missing/malformed metadata, honest unknowns, exact scope and error mapping | Actual daemon mode, completeness of captured traffic |
| Locally substitutable files and subprocesses | Atomic import, permissions, path/size limits, timeout, bounded output and fixed arguments | A privileged scan on the deployed host |
| Locally substitutable private D-Bus | Activation interface, no-input enforcement, concurrency and typed replies | The installed desktop's real polkit agent and authorization |
| True-external official DB-IP input | Actual MMDB compatibility, pinned bytes, source identity and attribution | Country accuracy or future availability of the next release |
| Owned-remote GitHub CI | Required checks on the reviewed immutable commit | Installed Flatpak or host-helper acceptance |

## Task 9: Adversarial review, full gates, PRs and final artifact

**PR order:** repair logic on #206 or a clean scoped successor; approved extras in their own PR; scanner packaging separately; country data packaging separately. Preserve unrelated roadmap/export edits outside these diffs.

- [ ] Run scoped gates first, then `cargo fmt --all -- --check`, the CI-equivalent locked check/clippy/test jobs, full headless Kirigami tests, QML input tests and packaging gates appropriate to each PR. Log missing prerequisites as missing proof.
- [ ] Open the PR before independent adversarial code review, as requested; attach exact diff/head, scope and test evidence. Never treat a locally green run as remote CI success.
- [ ] Assign an independent code reviewer; scanner/database trust boundaries also get an independent security reviewer. Ask for stale-session, UI lifecycle, untrusted text, privilege, import atomicity and packaging attacks.
- [ ] Fix every finding, including lower-severity findings; add regression tests and rerun affected gates. Disputed findings require explicit evidence and reviewer agreement, not silent dismissal.
- [ ] Request another independent review of the updated exact head. Repeat until all findings are resolved and no mandatory gate remains pending; author never self-approves.
- [ ] Collect CI green for the exact reviewed commit, after the final fix. Any rebase, edit or merge conflict resolution invalidates the affected review/CI evidence and restarts those gates.
- [ ] Perform real desktop tests where authorized: each menu under live traffic/reconnect, clipboard, Geo import/restart and `/proc`-host use. Test actual eBPF only on an already authorized available host.
- [ ] Record the remaining Deep Scan deployment acceptance: real host installation plus actual polkit authorization/denial. Fake/private-bus tests do not replace that proof. A reviewed implementation PR may merge after its automated gates pass; do not claim an installed working flow or deploy host policies without the corresponding authorization and acceptance.
- [ ] Merge only PRs with final independent approval, all required CI green and no unresolved findings, within existing merge authorization. Dependency-related packaging PRs wait for their prerequisites.
- [ ] Rebuild the final Flatpak from the merged/reviewed commit(s), record commit, manifest/data/helper versions, artifact SHA-256 and smoke results. Prior `4c966b7` artifact is not final release proof.
- [ ] Report merged PRs, exact source/artifact mapping, tests, blocked manual checks and deferred scope. Building/install-testing does not authorize release publishing.

## Stop boundaries and morning acceptance

- Mandatory repairs outrank extras. Do not begin an extra while the repair gate is red or its approval is missing; do not expand an extra into history, identity services or new enforcement operands.
- Stop a lane for missing authorization, incompatible official database format/license, an unbounded privileged interface, unresolved security findings or infrastructure failures that prevent meaningful validation. Missing desktop proof blocks a deployment claim, not an otherwise reviewed source merge.
- Continue independent safe lanes; report the exact blocked gate. Do not mark tests green because they skipped, weaken assertions, merge with failing CI or silently reduce scope to claim completion.
- Accept menus only with captured-target/reset/reconnect interaction evidence; accept health only with both modes supported and unknown evidence qualified.
- Accept GeoIP with validated import, old-data preservation and honest source/restart state; accept bundled data only with official pinned source, compatibility proof, attribution and update instructions.
- Accept Deep Scan only at its proven boundary: constrained implementation plus packaging proof; claim a working installed flow only after real host/polkit testing.
- Accept each approved extra only with observed-field correctness, accessibility and no enforcement/privacy expansion. Unstarted stretch work remains a backlog item.
- Completion means every merged PR meets TDD evidence, independent review → fix all → independent re-review → exact-head CI green → merge, with an honest final artifact and manual-test ledger.
