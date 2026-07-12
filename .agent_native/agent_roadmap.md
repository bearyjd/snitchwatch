# Agent-Native Roadmap — Snitchwatch

Audit date: 2026-07-07. This repo is **original work**, not a vendored
upstream clone: `vendor/opensnitch` is a pinned git submodule (v1.8.0,
read-only reference for protocol/config shape) and `Cargo.toml` shows the
actual product is a 10-crate Rust workspace ("Snitchwatch") that bridges
opensnitchd's gRPC protocol to a Little-Snitch-style WebSocket UI, plus two
unrelated scanner crates (`scanner-core`, `scanner-privileged`) for a
separate "Bazzite security scanner" component. It is pre-alpha, already has
~3,000+ lines of Rust tests, an in-process mock-daemon test harness, and a
mature `docs/superpowers/{specs,plans}/` paper trail of prior design
decisions. No `.github/` CI and no `CLAUDE.md` existed before this audit.

Items are ranked by **Human-Attention-Saved per Unit of Effort**. Top 5 are
immediately actionable with no further human input needed.

## Top 5 — do these first

1. **Write `CLAUDE.md`** (done as part of this audit — see repo root). Cost:
   one read-through. Payoff: every future agent session stops re-deriving
   the default-members/Qt6 quirk, the plan-doc convention, and the three
   already-settled architecture decisions (GUI stack, socket transport,
   fail-open→deny) that are otherwise buried across `AUDIT.md`/`HANDOFF.md`/
   `IMPLEMENTATION_PROMPT.md`. Highest ratio in this list — pure transcription
   of decisions already made, zero new judgment required.

2. **DONE — Add a CI workflow (`.github/workflows/ci.yml`)** running
   `cargo check`/`cargo clippy --all-targets -- -D warnings`/`cargo test`,
   scoped to `default-members` (no Qt6 needed) on `ubuntu-latest`, with the
   Tauri Linux build deps (`libwebkit2gtk-4.1-dev` etc.) installed since
   `snitchwatch-tauri` is a default member. Deliberately does **not** shell
   out to `just check`/`just test` verbatim, because those pass `--workspace`
   and would pull in `kirigami-spike`/`snitchwatch-kirigami`, which need
   Qt6/KF6 dev packages not provisioned on the runner and whose CI behavior
   is unverified (see item 6) — this is called out in a comment at the top
   of the workflow. YAML syntax validated locally
   (`python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"`)
   and the underlying commands (`cargo check --locked`,
   `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked`)
   were run locally and pass; the workflow itself has not been run through
   actual GitHub Actions from this environment.

3. **DONE — Document (and script) the one-time Playwright setup** for
   `tests/web_smoke` and `tests/tauri_smoke`. Added a "Playwright smoke
   suites (one-time setup)" section to `README.md` pointing at
   `just web-smoke-install` / `just tauri-smoke-install`, plus a new
   `just doctor` target that checks for `tests/web_smoke/node_modules` and
   `tests/tauri_smoke/node_modules` and prints a fix hint (exit 1) if
   missing. Verified locally: `just doctor` correctly reported
   `tests/web_smoke/node_modules` present and `tests/tauri_smoke/node_modules`
   missing in this environment (exit code 1 with the install hint).

4. **DONE — Note the `vendor/opensnitch` submodule's expected dirtiness.**
   The one-sentence note in CLAUDE.md was already in place. Additionally
   excluded the stray `.omc/` path via
   `.git/modules/vendor/opensnitch/info/exclude` — a local-only, uncommitted
   git config file (never pushed/shared), so this doesn't touch the tracked
   `vendor/opensnitch` working tree or its own `.gitignore` (still out of
   scope, per the original note). Verified: `git status --short` no longer
   lists `vendor/opensnitch` at all in this checkout.

5. **Point future agents at the existing test-double pattern
   (`SystemInspector`/`MockInspector` in `scanner-core/src/testkit.rs`,
   `SystemFacts`/fake in `scanner-privileged/tests/end_to_end.rs`, and
   `MockOpensnitchd` in `tests/mock_opensnitchd`) as the mandatory
   reproduction path for anything touching real system state or a real
   daemon.** This already exists and is well-built — the gap is discoverability,
   not missing infrastructure. Effort: a short "Reproduction paths" section
   in CLAUDE.md (done). Payoff: stops a future agent from reaching for `sudo`,
   a real opensnitchd, or a live Bazzite host to reproduce a scanner/bridge
   bug when a deterministic in-process double already does the job in
   milliseconds.

## Further out (real effort, defer until above lands)

6. **PARTIALLY DONE (2026-07-11).** A later session's sandbox turned out to
   have Qt6 + KF6 Kirigami dev packages installed after all (Fedora:
   `qt6-qtbase-devel`, `qt6-qtdeclarative-devel`, `qt6-qttools-devel`,
   `kf6-kirigami-devel`) — confirmed `cargo build -p snitchwatch-kirigami`
   and `cargo test -p snitchwatch-kirigami` (246 lib unit tests + 13
   integration test files) pass headless there
   (`QT_QPA_PLATFORM=offscreen`), including a real manual GUI verification
   of Task 7's fullscreen-focus safety behavior on a live Plasma Wayland
   session in that same sandbox. A `kirigami` job was added to
   `.github/workflows/ci.yml` to make this a standing, automated check
   rather than a one-off finding — **but its Ubuntu apt package names are
   still unverified against a live run** (translated from the Fedora
   package list above, no outbound network from the authoring sandbox to
   check apt). Remaining effort: small — watch the first real CI run of
   that job and fix any apt package-name mismatches.

7. **No raw daemon-traffic recording/replay fixture yet.** The bridge's
   reproduction path (mock daemon + WS client) is script-driven and
   excellent for *known* protocol sequences, but there's no captured-pcap or
   recorded-gRPC-transcript fixture for reproducing a bug reported against a
   *specific* real opensnitchd version's quirky behavior. Worth a follow-up
   if/when a real bug report references daemon-specific behavior the mock
   doesn't model. Effort: medium-high (needs a real daemon to capture
   against). Payoff: currently speculative — no such bug report exists yet.

8. **Component B (security scanner) baseline classification and privileged
   checks are real design surfaces, not chokepoints to codify** — they're
   already resolved in `docs/superpowers/specs/2026-07-04-scanner-*.md` and
   implemented with the testkit pattern from item 5. No action needed beyond
   linking those docs from CLAUDE.md (done).

9. **RESOLVED 2026-07-12. Tray state (`DaemonDown`/`RecentBlock`/
   `FilterOff`) was display-only — nothing in production transitioned the
   bridge's `TrayStatePublisher` into those three states.** Found
   2026-07-12 while investigating a `#[allow(dead_code)]` in
   `grpc_server.rs`. All four variants (including `Idle`/`Pending(n)`) are
   now wired end to end:
   - `Idle`/`Pending(n)`: production wires `ConnectionCache::
     with_tray_publisher`, which was always fully implemented and tested,
     just never called (`b902a2a`).
   - `DaemonDown`/`RecentBlock`: see
     `docs/superpowers/plans/2026-07-12-tray-daemon-down-recent-block.md`.
     `DaemonDown` is a staleness watchdog off `ping()`'s recency
     (`daemon_watchdog.rs`); `RecentBlock` publishes on a Deny verdict in
     `ask_rule` with a generation-guarded revert timer.
   - `FilterOff`: owner decision (2026-07-12) — pausing keeps
     `opensnitchd`'s `DefaultAction: deny` untouched; the bridge itself
     auto-resolves every `AskRule` as Allow-Once while paused instead of
     prompting, so a genuine bridge outage still fails closed. See
     `docs/superpowers/plans/2026-07-12-tray-filter-off.md` for the full
     design: a new `ClientMessage::SetFilteringPaused`, `UiService` gaining
     a genuine `filtering_paused` constructor parameter (unlike
     `last_ping`/`block_generation`, this one must be externally writable),
     an inbound-pump toggle handler, and Kirigami shell wiring
     (`TrayController::toggleFiltering` + a real tray-menu item — the
     `menuLabel` tokens existed before this but nothing ever displayed or
     triggered them).
   Full workspace test suite green throughout (`cargo test --workspace`,
   0 failures); `snitchwatch-kirigami`'s 247 tests include a passing
   `main.qml` load, confirming the new tray menu item doesn't break QML
   parsing. No unbuilt tray-state variants remain.

## What's already good (don't touch)

- The mock-daemon (`tests/mock_opensnitchd`) + `bridge_protocol_test.rs`
  round-trip test is a genuinely strong reproduction harness — new bridge
  bugs should almost always get a new scripted scenario here before a fix.
- `scanner-core::testkit::MockInspector` / `scanner-privileged`'s
  `SystemFacts` fake are the correct abstraction boundary for testing
  host-dependent code without root or a real Bazzite box.
- The `docs/superpowers/plans/2026-MM-DD-<slug>.md` convention is already
  consistently followed and gives an agent a ready-made template for
  documenting new work before implementing it.
