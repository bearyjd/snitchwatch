# M6 Public Release Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Tag and ship Snitchwatch v0.1.0 — polish the docs, wire CI + a tag-driven GitHub release that publishes the Flatpak artifact, complete the Plan 1 / Plan 5 / Plan 6 deferred verification work (live opensnitchd 60s smoke, ≥80% coverage gate, StevenBlack-on-real-daemon, WebKitGTK Flatpak permissions, GPL/Tauri legal sanity check), wire the four still-stubbed `LifecycleKind` emission points from Plan 6, ship the diagnostic-bundle "Copy" button, write the manual smoke checklist, and tick M6 ✅ in the spec milestone table.

**Architecture:** No new crates. The release workflow is two `.github/workflows/*.yml` files plus a small `scripts/` folder of bash helpers (`live-smoke.sh`, `coverage-gate.sh`) that the workflows and `just` recipes both call. The four remaining `LifecycleKind` emission sites get small dedicated modules so each one can be unit-tested in isolation: `cache/dropped_counter.rs` already exists from Plan 6 — Plan 7 wires its increment site and probe drain; `panic_hook.rs` installs a `std::panic::set_hook` inside the Tauri shell that broadcasts `BridgePanicRecovered` before the bridge runtime restarts; `lifecycle/journalctl.rs` shells out to `journalctl --user -u snitchwatch-opensnitchd.service -n 50 --no-pager` and greps for known kernel-hook failure markers; the existing 60-second reconciliation loop in `grpc_client.rs` gains one branch that emits `StateDivergenceReconciled` when the replayed snapshot diff is non-empty. The diagnostic bundle is one new Tauri command (`copy_diagnostic_bundle`) that tars `bridge.log + crash.log + journalctl-tail + version-info` into `$XDG_RUNTIME_DIR/snitchwatch-diag-<ts>.tar.gz` and copies the path to the system clipboard via `tauri-plugin-clipboard-manager`. The A→B→C ws_bind upgrade path is documented in `docs/architecture.md` — no code change is needed because `BridgeConfig::ws_bind` has defaulted to `127.0.0.1:0` since Plan 1.

**Tech Stack:** GitHub Actions, `bilelmoussaoui/flatpak-github-actions/flatpak-builder@v6`, `actions-rs/toolchain`, `cargo-llvm-cov`, `bats-core`, rootful podman, `tar` crate (Rust pure), `tauri-plugin-clipboard-manager`, mdBook-flat markdown, conventional commits + semver + `git tag -a v0.1.0`.

**Out of scope:**
- LAN bind mode (Option C — gated to v2).
- Flathub store submission (v2; v1 ships from the GitHub release tarball + `just flatpak` only).
- App-icon designer pass (v0.1.0 ships the Plan 6 placeholder SVG; the public-release task list only verifies the icon is present, not pretty).
- Cross-distro packaging beyond Bazzite / Universal Blue.
- Auto-update channel (v2).
- Telemetry / crash reporting upload (v2 — local diagnostic bundle only in v0.1.0).
- Translated UI (v0.1.0 ships English-only).

---

## Memory Constraints

These project-memory entries directly shape this plan. Each one is repeated here so a fresh subagent has everything inline.

1. **`bash_antipattern_hook.md`** — workspace blocks `find/ls/cat/grep/rg/head/tail/sed/awk` in Bash. Use the dedicated `Read`/`Glob`/`Grep` tools instead. PostToolUse "Write operation failed" reminders are false-positives — verify by the structured stdout success line. The `scripts/live-smoke.sh` and `scripts/coverage-gate.sh` files in this plan are themselves `set -euo pipefail` shell — they are *artifacts*, not Bash tool calls, and the anti-pattern hook does not apply to file *contents*, only to the agent's `Bash` tool invocations.

2. **`m1_envelope_hack.md`** — the JSON envelope inside `Notification.data` from M1 was deleted at M2 topology flip. Do not reintroduce it. The four new lifecycle emission points in this plan all flow through the existing typed `LifecycleEvent` broadcast from Plan 6 — never a stringly-typed envelope.

3. **`plan1_deferred_criteria.md`** — live opensnitchd smoke + cargo-llvm-cov are environmental, not code work. Plan 1 stays closed. **Plan 7 owns them** — they are Tasks 8 and 9 below and the success criterion is *evidence captured in `docs/manual-smoke.md` / CI logs*, not a re-edit of Plan 1's checkboxes.

4. **`clippy_gotchas_bridge.md`** — `Translated::AskRule` must stay boxed (`large_enum_variant`). The new `LifecycleKind::KernelHookFailed { excerpt: String }` from Plan 6 carries a `String` so it stays under 160 bytes — no boxing needed. Discard `oneshot::Receiver` with `drop(rx)`, never `let _ = rx`.

5. **`autonomous_tdd_resume.md`** — on PreCompact resume, no recap, no acknowledgment. Pick up the last task as if the break never happened.

---

## File Structure

### NEW

- `.github/workflows/ci.yml` — pull-request CI. Jobs: `fmt` (`cargo fmt --check`), `clippy` (`cargo clippy --all-targets --all-features -- -D warnings`), `test` (`cargo test --workspace --all-features`), `coverage` (`cargo llvm-cov --workspace --fail-under-lines 80 --ignore-filename-regex 'tests/|crates/snitchwatch-spike|crates/snitchwatch-proto'`), `bats` (`bats packaging/install.bats`). Runs on Ubuntu 24.04. Caches `~/.cargo/registry`, `~/.cargo/git`, and `target/` keyed on `Cargo.lock`. ≤ 140 lines.
- `.github/workflows/release.yml` — tag-driven release. Triggers on `push: tags: ['v*.*.*']`. Jobs: `build-flatpak` (uses `bilelmoussaoui/flatpak-github-actions/flatpak-builder@v6` with `manifest-path: packaging/flatpak/org.snitchwatch.Snitchwatch.yml`, `bundle: snitchwatch.flatpak`); `checksums` (sha256 of the `.flatpak` and the source tarball); `publish` (uses `softprops/action-gh-release@v2` to upload `snitchwatch.flatpak`, `snitchwatch.flatpak.sha256`, `snitchwatch-${{ github.ref_name }}.tar.gz`, and the auto-extracted `CHANGELOG.md` section for that tag as the release body). ≤ 160 lines.
- `LICENSE` — full GPL-2.0-or-later text (the canonical FSF text, ~340 lines). The repo has been declaring `license = "GPL-2.0"` in `Cargo.toml` since Plan 1 but no `LICENSE` file has existed at the repo root — Plan 7 adds it.
- `NOTICE.md` — third-party attributions and the linkage analysis. Sections: "Snitchwatch license", "Bundled / linked components" (LS-for-Linux UI — GPL-2.0; OpenSnitch protobuf and Go daemon — GPL-3.0, used as a separate process over gRPC, **no linkage**; Tauri 2 — dual MIT/Apache-2.0; webkit2gtk — LGPL via system library; freedesktop-sdk runtime — various OSI-approved), "Linkage seam analysis" (the Tauri shell statically links Tauri 2; the embedded WebView loads the GPL-2.0 LS-for-Linux UI bundle; the bridge crate talks to opensnitchd over gRPC across a process boundary — therefore the only GPL combination is Tauri-shell ⊕ web-UI, both shipped as a single binary, both compatible because GPL-2.0+ permits MIT/Apache linkage in one direction. Full effective license of the shipped binary: **GPL-2.0-or-later**). ≤ 220 lines.
- `CONTRIBUTING.md` — how to set up the dev env (Rust 1.75+, protoc, just, podman), the `just check` / `just test` / `just live-smoke` workflow, the conventional-commit + DCO sign-off requirement, the "tests must pass + clippy clean + coverage ≥ 80% before PR" rule, the bug-report template pointer, the security-disclosure address (`security@snitchwatch.example` placeholder — Plan 7 marks this as TODO until the user provides one), the code-of-conduct pointer (Contributor Covenant 2.1 by reference, no separate file). ≤ 180 lines.
- `CHANGELOG.md` — Keep-a-Changelog format. One `## [0.1.0] — 2026-04-11` entry with subsections: Added, Changed, Fixed, Known issues, Deferred. Populated from the spec milestone table (M0–M6) and this plan's Acceptance Criteria. ≤ 200 lines.
- `docs/architecture.md` — the human-readable system tour. Sections: "What it is", "What it is not", "Crate map", "Process topology" (Tauri shell ↔ embedded webview ↔ bridge ↔ opensnitchd), "Data flow" (ask-rule round-trip diagram in ASCII), "Lifecycle banners" (the 10 `LifecycleKind` rows mapped to user-visible labels), "ws_bind upgrade path" (Option A — loopback ephemeral, the v0.1.0 default; Option B — loopback fixed-port via `SNITCHWATCH_WS_BIND` env var or `bridge.bind_address` in `settings.toml`; Option C — LAN bind with TLS + token auth, deferred to v2; **explicit note that Option A has been the default since Plan 1 — there is no "flip" in v0.1.0**), "Where to look first when something breaks" (a 6-row table that pairs each LifecycleKind with the file you'd open). ≤ 600 lines.
- `docs/manual-smoke.md` — the v0.1.0 manual smoke checklist. 5 scenarios from spec §Testing strategy: (1) cold start with daemon missing → install via wizard → first ask-rule arrives within 30s; (2) unit inactive → click "Start" on the banner → reconnect within 5s; (3) daemon crash mid-stream → pkill opensnitchd → "Daemon unreachable" banner appears within 10s → restart → banner clears within 10s; (4) blocklist update offline → unplug network → click "Update" → "Blocklist fetch failed" banner with last-known timestamp; (5) LAN bind toggle (read-only smoke for v0.1.0 — flip the env var, restart, confirm the bridge logs the new bind addr but the wizard is unchanged). Each scenario has a checkbox list of expected log lines and screenshots-to-capture. ≤ 250 lines.
- `scripts/live-smoke.sh` — the 60-second live opensnitchd smoke driver. `set -euo pipefail`. Pulls and runs `docker.io/evilsocket/opensnitch:latest` in a rootful podman container (`--privileged --network=host --pid=host --cap-add=NET_ADMIN,SYS_ADMIN,BPF`), waits up to 30s for the gRPC port to open, runs `cargo run -p snitchwatch-bridge-cli -- --once-then-exit-after 60s` (a new flag), greps the bridge log for `WS_LISTEN_ADDR=` (success) and at least one `event_kind=` line (proves a real event flowed), tears the container down. Exit 0 on success, 1 on any failure with a structured `[FAIL] reason=…` line. ≤ 140 lines.
- `scripts/coverage-gate.sh` — `set -euo pipefail`. Wraps `cargo llvm-cov` with the per-crate include list (translator, cache, blocklists, lifecycle) and the `--fail-under-lines 80` threshold. Prints a per-file table on failure. ≤ 80 lines.
- `crates/snitchwatch-bridge/src/lifecycle/journalctl.rs` — journalctl scrape helper. `pub fn scrape_kernel_hook_failure() -> Option<String>` runs `journalctl --user -u snitchwatch-opensnitchd.service -n 50 --no-pager`, scans for known markers (`Failed to load eBPF`, `nfqueue: failed`, `module not found`, `permission denied`), and returns the matching line (≤200 chars). Returns `None` if `journalctl` is missing or no marker matches. ≤ 160 lines. Pure-function-with-injected-`Command` shape so the unit test can stub the subprocess.
- `crates/snitchwatch-bridge/src/lifecycle/journalctl/parse.rs` — pure scanner: `pub fn find_marker(stdout: &str) -> Option<String>`. ≤ 80 lines. Trivially unit-testable with no I/O.
- `crates/snitchwatch-bridge/tests/lifecycle_emission_test.rs` — integration test that wires a fake `dropped_counter` (already exists from Plan 6), a stubbed `journalctl_provider`, a controlled `reconciliation_diff`, and a manually-fired `panic_hook_rx`, then asserts the `LifecycleProbe` broadcast emits `EventFloodDropped`, `KernelHookFailed`, `StateDivergenceReconciled`, and `BridgePanicRecovered` exactly once each in the expected order. ≤ 280 lines.
- `crates/snitchwatch-tauri/src/panic_hook.rs` — `pub fn install(broadcaster: Arc<LifecyclePanicChannel>)` calls `std::panic::set_hook(Box::new(move |info| { broadcaster.notify(format!("{info}")); }))`. The `LifecyclePanicChannel` is a small `Arc<Mutex<Option<broadcast::Sender<()>>>>` newtype owned by the bridge runtime task; the Tauri shell hands its sender to the panic hook on startup. ≤ 120 lines.
- `crates/snitchwatch-tauri/src/diagnostics.rs` — the `copy_diagnostic_bundle` Tauri command. Steps: collect `bridge.log`, `crash.log` (the Plan 4 file), the last 200 lines of `journalctl --user -u snitchwatch-opensnitchd.service`, and a `version.txt` (`snitchwatch-${VERSION} | rustc ${RUSTC_VERSION} | ${OS_RELEASE}`); tar them (gzip) into `$XDG_RUNTIME_DIR/snitchwatch-diag-${UNIX_TS}.tar.gz`; copy that path to the clipboard via `tauri-plugin-clipboard-manager::ClipboardExt::write_text`; return `Ok(path_string)`. On any error, return `Err(string)` and emit a `lifecycle://diagnostic-bundle-failed` event. ≤ 280 lines.
- `crates/snitchwatch-tauri/src/diagnostics/bundle.rs` — pure tar-builder helper. `pub fn build_bundle(parts: &[BundlePart]) -> Result<Vec<u8>, BundleError>` where `BundlePart { name: String, body: Vec<u8> }`. Uses the `tar` crate. Unit-tested by tar'ing 3 in-memory parts and asserting the resulting bytes round-trip through `tar::Archive`. ≤ 140 lines.
- `crates/snitchwatch-tauri/src/diagnostics/version_info.rs` — collects the `version.txt` body without touching the network or the daemon. Reads `/etc/os-release`, `env!("CARGO_PKG_VERSION")`, and a build-time `RUSTC_VERSION` injected by `build.rs`. ≤ 110 lines.
- `crates/snitchwatch-tauri/build.rs` — emits `cargo:rustc-env=RUSTC_VERSION=$(rustc --version)` so the diagnostic bundle's `version.txt` line can include the rustc version without a runtime shell-out.
- `web/diagnostics.js` — the front-end glue: a "Copy diagnostics" button in the existing About panel that calls `invoke('copy_diagnostic_bundle')`, shows a transient toast with the returned path, and on error renders a non-modal banner with the error string. ≤ 140 lines.
- `web/diagnostics.css` — toast styling. ≤ 60 lines.

### MODIFIED

- `Cargo.toml` (workspace root) — `repository = "https://github.com/snitchwatch/snitchwatch"` (replaces the `example/snitchwatch` placeholder; if the user has not yet picked a real org/repo by Task 1 of Part A, leave the placeholder and add a TODO comment — see Task 1's note). Workspace `[workspace.dependencies]` gains `tar = "0.4"` and the workspace member list is unchanged.
- `crates/snitchwatch-bridge/src/cache.rs` — wire the existing `dropped_counter::increment()` call into the `EventCache::push` `send_timeout` failure branch. The `dropped_counter` module already exists from Plan 6 with the atomic — Plan 6 left the increment site as a `// TODO(plan-7): wire here` comment. Plan 7 deletes the comment and adds the call.
- `crates/snitchwatch-bridge/src/lifecycle.rs` — `LifecycleProbe::tick` grows a new branch that reads `dropped_counter::take()` (atomic swap to zero) and, if non-zero, broadcasts `LifecycleKind::EventFloodDropped { dropped }`. Probe also calls `journalctl::scrape_kernel_hook_failure()` once per tick when the prior tick was already in `GrpcUnreachable` — and broadcasts `KernelHookFailed { excerpt }` if the scrape returns `Some`.
- `crates/snitchwatch-bridge/src/grpc_client.rs` — the existing 60-second reconciliation loop (added in Plan 2) gains one new line: when `replay_snapshot_diff` returns a non-empty `Diff`, broadcast `LifecycleKind::StateDivergenceReconciled` on the lifecycle channel. Use a `tokio::sync::broadcast::Sender<LifecycleEvent>` injected via `GrpcClient::with_lifecycle_sender(self, tx)` so the unit test can subscribe.
- `crates/snitchwatch-bridge/src/lib.rs` — add `pub mod lifecycle::journalctl;` re-export.
- `crates/snitchwatch-bridge/Cargo.toml` — add `[dev-dependencies]` `assert_cmd = "2"` for the journalctl scrape test (it stubs `journalctl` via a path-injected fake binary built by `assert_cmd::cargo::cargo_bin`). No new runtime deps.
- `crates/snitchwatch-tauri/Cargo.toml` — add `[dependencies]` `tar = { workspace = true }`, `flate2 = "1"`, `tauri-plugin-clipboard-manager = "2"`. Add `[build-dependencies]` `chrono = { version = "0.4", default-features = false, features = ["clock"] }` (only used by the build script that injects `RUSTC_VERSION` — pulls in nothing at runtime).
- `crates/snitchwatch-tauri/src/lib.rs` — `pub mod panic_hook;`, `pub mod diagnostics;` declarations. Tauri builder gains `.plugin(tauri_plugin_clipboard_manager::init())` and `.invoke_handler(tauri::generate_handler![…, diagnostics::copy_diagnostic_bundle])`.
- `crates/snitchwatch-tauri/src/main.rs` — calls `panic_hook::install(lifecycle_panic_channel.clone())` immediately after the bridge runtime is constructed and before the Tauri builder is built.
- `crates/snitchwatch-bridge-cli/src/lib.rs` — add a new optional CLI arg `--once-then-exit-after <duration>` that sets a wall-clock deadline. Default is `None` (the existing forever-loop). Used by `scripts/live-smoke.sh`.
- `web/index.html` — `<script type="module" src="diagnostics.js"></script>` and a `<button id="copy-diag-btn">Copy diagnostics</button>` placed inside the existing About panel.
- `README.md` — full rewrite. New top section ("Snitchwatch — a Little Snitch–style network firewall GUI for Linux") with the v0.1.0 status badge, install-on-Bazzite copy-paste, screenshot placeholder (one PNG path under `docs/img/screenshot-main.png` with a "TODO: capture in Plan 7 Task 16" note), Quickstart, Architecture link, Failure-mode banners table (mirrored from Plan 6 — kept here too because users land on README first), Contributing link, License. The full diff is in Task 1 of Part A.
- `justfile` — new recipes: `just live-smoke`, `just coverage`, `just release-dry`, `just diag-smoke`, `just docs-check` (runs `mdbook test` if installed, otherwise `cargo doc --workspace --no-deps`).
- `docs/superpowers/specs/2026-04-10-snitchwatch-design.md` — tick M6 ✅ in the milestone table with a one-paragraph implementation note pointing at this plan, and resolve open questions #3 (WebKitGTK Flatpak permissions verified — see manual-smoke.md scenario 1) and #4 (legal sanity check captured in NOTICE.md).
- `crates/snitchwatch-bridge/src/cache/dropped_counter.rs` — flesh out the `take()` atomic swap (Plan 6 left the file with only `increment()` and the `AtomicU32` static). Add `pub fn take() -> u32` that does a single `swap(0, Ordering::AcqRel)` and returns the prior value. Add a unit test that increments 3× from one task and 5× from another, then asserts `take()` returns 8.

### DELETED

- None. v0.1.0 keeps every file Plan 6 added.

---

## Part A — Docs polish (Tasks 1–4)

### Task 1: README rewrite

**Files:**
- Modify: `README.md`

The current README is Plan 1 era and only describes the bridge crate. Replace it wholesale with a v0.1.0-ship README. We use a docs-only commit so the test gate is "the file renders on github.com" (smoke-tested locally by `python3 -m markdown` or `mdbook` if available).

- [ ] **Step 1: Write the failing test**

Create `tests/readme_shape.rs` inside the `tests/integration` crate (which already exists from Plan 1):

```rust
//! Schema-shape test for the top-level README.
//!
//! Asserts the v0.1.0 README contains every section a first-time user needs.
//! Failing this test means the README is missing a load-bearing heading.

use std::fs;

#[test]
fn readme_has_v0_1_0_sections() {
    let body = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../README.md"),
    )
    .expect("README.md must exist at repo root");

    let required_headings = [
        "# Snitchwatch",
        "## Status",
        "## Install on Bazzite",
        "## Quickstart",
        "## Architecture",
        "## Failure-mode banners",
        "## Contributing",
        "## License",
    ];

    for heading in required_headings {
        assert!(
            body.contains(heading),
            "README.md missing required section: {heading}"
        );
    }

    assert!(
        body.contains("v0.1.0"),
        "README.md must reference the v0.1.0 release tag"
    );
    assert!(
        body.contains("docs/architecture.md"),
        "README.md must link to docs/architecture.md"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-integration-tests readme_has_v0_1_0_sections`
Expected: FAIL — current README has none of the v0.1.0 headings.

- [ ] **Step 3: Rewrite README.md**

Replace the entire file with:

```markdown
# Snitchwatch

A Little Snitch–style network firewall GUI for Linux, built on top of OpenSnitch.

[![status](https://img.shields.io/badge/status-v0.1.0-blue)](#status)
[![license](https://img.shields.io/badge/license-GPL--2.0--or--later-brightgreen)](LICENSE)

![Snitchwatch main window](docs/img/screenshot-main.png)

> _Screenshot placeholder — captured during the Plan 7 manual smoke pass; see `docs/manual-smoke.md` scenario 1._

## Status

**v0.1.0** — first public release. Headless bridge, vendored Little Snitch–for-Linux web UI, Tauri 2 shell, Flatpak packaging, OpenSnitch quadlet. See `docs/superpowers/specs/2026-04-10-snitchwatch-design.md` for the full design and the M0–M6 milestone table.

What works:

- Real-time outbound connection prompts with the LS-style ask-rule modal
- Persistent allow / deny rules backed by OpenSnitch
- Built-in StevenBlack ad / tracker / malware blocklists with offline cache
- First-run wizard that installs the OpenSnitch daemon for you
- Typed lifecycle banners for the 10 known failure modes
- Per-app and per-domain rule editor
- Diagnostic-bundle "Copy" button in the About panel

What's deferred to v2 (see `CHANGELOG.md` → Deferred):

- LAN-mode bind with TLS + token auth
- Flathub store submission
- Auto-update channel
- Translated UI

## Install on Bazzite

```bash
git clone https://github.com/snitchwatch/snitchwatch.git
cd snitchwatch
git submodule update --init --recursive
just flatpak              # builds packaging/build/snitchwatch.flatpak
just install              # installs the flatpak + daemon quadlet (user scope)
flatpak run org.snitchwatch.Snitchwatch
```

The first run drops you into the wizard. If the OpenSnitch daemon is not yet installed, click **Install daemon** — the wizard streams the install log into the overlay and then walks you through your first ask-rule.

## Quickstart

```bash
# Dev loop
just build                # cargo build --workspace
just test                 # cargo test --workspace
just check                # cargo fmt --check + clippy -D warnings
just run                  # launches the Tauri shell against a podman daemon
just live-smoke           # 60s smoke against a real opensnitchd container
just coverage             # cargo-llvm-cov with the v0.1.0 80% gate
```

## Architecture

The full system tour lives in [`docs/architecture.md`](docs/architecture.md). One-paragraph version:

The Tauri 2 shell hosts an embedded WebKitGTK webview that loads the vendored Little Snitch–for-Linux web UI. The shell also owns a `snitchwatch-bridge` runtime task that talks to `opensnitchd` over gRPC and re-exports the same data over a loopback WebSocket the webview consumes. All four hops are typed end-to-end: protobuf at the gRPC seam, `ws_messages.rs` at the WS seam.

```
┌────────────────┐  WS (loopback)   ┌────────────────┐  gRPC           ┌──────────────┐
│  webview UI    │ ◄──────────────► │ snitchwatch    │ ◄────────────► │ opensnitchd  │
│  (LS for Linux)│                  │ -bridge        │                │ (host)       │
└────────────────┘                  └────────────────┘                └──────────────┘
        ▲
        │ Tauri IPC
        ▼
┌────────────────┐
│ snitchwatch    │
│ -tauri shell   │
└────────────────┘
```

## Failure-mode banners

The bridge probes the daemon every 5 seconds and broadcasts a typed `LifecycleEvent`. The web UI renders one banner per active failure mode at the top of the viewport.

| LifecycleKind             | Banner label                       | Action button   |
|---------------------------|------------------------------------|-----------------|
| `DaemonOk`                | _(no banner)_                      | —               |
| `UnitMissing`             | OpenSnitch daemon not installed    | Install daemon  |
| `UnitInactive`            | OpenSnitch daemon stopped          | Start           |
| `GrpcUnreachable`         | Daemon unreachable                 | Diagnose        |
| `GrpcStaleStream`         | Reconnecting…                      | —               |
| `EventFloodDropped`       | Event burst — N events dropped     | Open log        |
| `BridgePanicRecovered`    | Bridge crashed and restarted       | Open log        |
| `BlocklistFetchFailed`    | Blocklist update failed            | Retry           |
| `KernelHookFailed`        | Kernel hook unavailable            | Diagnose        |
| `StateDivergenceReconciled` | State reconciled with daemon     | —               |

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). TL;DR: `just check && just test && just coverage` before opening a PR. Conventional commits + DCO sign-off required.

## License

GPL-2.0-or-later. See [`LICENSE`](LICENSE) for the full text and [`NOTICE.md`](NOTICE.md) for the third-party attribution and linkage analysis.
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-integration-tests readme_has_v0_1_0_sections`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add README.md tests/integration/tests/readme_shape.rs
git commit -m "$(cat <<'EOF'
docs: rewrite README for v0.1.0 ship

Replaces the Plan 1 bridge-only README with a user-facing landing
page: status, install-on-Bazzite copy-paste, quickstart, architecture
link, failure-mode banner table, contributing pointer, license.

Adds tests/readme_shape.rs as a regression gate so the load-bearing
sections cannot silently disappear.
EOF
)"
```

---

### Task 2: CONTRIBUTING.md

**Files:**
- Create: `CONTRIBUTING.md`

- [ ] **Step 1: Write the failing test**

Append to `tests/integration/tests/readme_shape.rs`:

```rust
#[test]
fn contributing_md_has_required_sections() {
    let body = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../CONTRIBUTING.md"),
    )
    .expect("CONTRIBUTING.md must exist at repo root");

    for heading in [
        "# Contributing to Snitchwatch",
        "## Dev environment",
        "## The dev loop",
        "## Coding standards",
        "## Commit messages",
        "## Pull requests",
        "## Reporting bugs",
        "## Reporting security issues",
        "## Code of Conduct",
    ] {
        assert!(body.contains(heading), "CONTRIBUTING.md missing: {heading}");
    }

    assert!(body.contains("just check"), "must mention `just check`");
    assert!(body.contains("DCO"), "must mention DCO sign-off");
    assert!(body.contains("80%"), "must mention the 80% coverage floor");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-integration-tests contributing_md_has_required_sections`
Expected: FAIL — file does not exist.

- [ ] **Step 3: Create CONTRIBUTING.md**

```markdown
# Contributing to Snitchwatch

Thank you for considering a contribution. Snitchwatch is GPL-2.0-or-later, the codebase is small and Rust-heavy, and we keep the bar high on tests and clippy.

## Dev environment

You need:

- Rust 1.75+ (install via [rustup](https://rustup.rs))
- `protoc` (Debian/Fedora: `dnf install protobuf-compiler` / `apt install protobuf-compiler`)
- `just` (`cargo install just`)
- `podman` for the live opensnitchd smoke (Bazzite, Fedora 39+, Ubuntu 24.04+)
- `cargo-llvm-cov` for the coverage gate (`cargo install cargo-llvm-cov`)
- `bats-core` for the install-script tests (`dnf install bats` / `apt install bats`)

Optional but recommended:

- `flatpak-builder` for `just flatpak`
- `mdbook` for `just docs-check`

## The dev loop

```bash
git clone https://github.com/snitchwatch/snitchwatch.git
cd snitchwatch
git submodule update --init --recursive
just check                # cargo fmt --check + clippy -D warnings
just test                 # cargo test --workspace
just coverage             # ≥ 80% on translator/cache/blocklists/lifecycle
just live-smoke           # 60s smoke against a real opensnitchd container
```

If `just check && just test && just coverage` is green locally, your PR will sail through CI.

## Coding standards

- **Tests first.** New behavior ships with a failing test that goes green in the same commit. See `docs/superpowers/specs/2026-04-10-snitchwatch-design.md` § Testing strategy for the philosophy.
- **Files ≤ 800 lines.** If a file is creeping over 400, split it. The bridge crate is intentionally many small modules.
- **No `unwrap()` outside tests.** Use `?` and propagate `BridgeError`.
- **No mutation across `Clone` boundaries.** The bridge state machine is immutable-by-construction; copy then mutate.
- **Coverage floor: 80%** on the modules listed in `scripts/coverage-gate.sh` (translator, cache, blocklists, lifecycle). The CI job will block your PR if you drop below.
- **Clippy clean.** `cargo clippy --all-targets --all-features -- -D warnings` — no allows, no exceptions.

## Commit messages

[Conventional Commits 1.0](https://www.conventionalcommits.org/en/v1.0.0/). Examples:

```
feat(bridge): add lifecycle probe for kernel hook failures
fix(translator): drop empty AskRule frames before the WS push
docs: explain the ws_bind upgrade path in architecture.md
test(cache): cover the dropped-counter swap atomicity
chore(deps): bump tonic 0.12.1 → 0.12.3
```

## Pull requests

1. Fork → branch → PR against `main`.
2. **DCO sign-off required.** Add `Signed-off-by: Your Name <you@example.com>` to every commit (`git commit -s`). PRs without sign-off are blocked by CI.
3. Squash unrelated commits before merge.
4. Link the issue you're fixing in the PR description.
5. Wait for green CI: `fmt`, `clippy`, `test`, `coverage`, `bats`.

## Reporting bugs

Open an issue at https://github.com/snitchwatch/snitchwatch/issues with:

- Snitchwatch version (`flatpak run org.snitchwatch.Snitchwatch --version`)
- OS + version (`cat /etc/os-release`)
- Steps to reproduce
- Diagnostic bundle (About → Copy diagnostics → paste path → attach the `.tar.gz`)

## Reporting security issues

**Do not open a public issue for security bugs.** Email `security@snitchwatch.example` _(TODO: replace with the real address before v0.1.0 tag — tracked as Plan 7 Task 18)_ with a description and a reproducer. We aim to acknowledge within 72 hours.

## Code of Conduct

This project follows the [Contributor Covenant 2.1](https://www.contributor-covenant.org/version/2/1/code_of_conduct/). By participating you agree to abide by its terms.
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-integration-tests contributing_md_has_required_sections`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add CONTRIBUTING.md tests/integration/tests/readme_shape.rs
git commit -m "$(cat <<'EOF'
docs: add CONTRIBUTING.md for v0.1.0

Documents the dev loop, coding standards, conventional commit
format, DCO sign-off requirement, bug-report and security-disclosure
process. Tracks the security@ placeholder as a Plan 7 Task 18 TODO.
EOF
)"
```

---

### Task 3: LICENSE + NOTICE.md

**Files:**
- Create: `LICENSE`
- Create: `NOTICE.md`

The repo has been declaring `license = "GPL-2.0"` in `Cargo.toml` since Plan 1 but no `LICENSE` file has existed at the repo root. Drop in the canonical FSF text for GPL-2.0-or-later, then write `NOTICE.md` documenting the third-party attributions and the linkage analysis (open question #4 from the spec).

- [ ] **Step 1: Write the failing test**

Append to `tests/integration/tests/readme_shape.rs`:

```rust
#[test]
fn license_and_notice_present_and_consistent() {
    let license = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../LICENSE"),
    )
    .expect("LICENSE must exist at repo root");

    assert!(
        license.contains("GNU GENERAL PUBLIC LICENSE"),
        "LICENSE must be the canonical GPL text"
    );
    assert!(
        license.contains("Version 2"),
        "LICENSE must declare Version 2"
    );

    let notice = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../NOTICE.md"),
    )
    .expect("NOTICE.md must exist at repo root");

    for required in [
        "## Snitchwatch license",
        "## Bundled / linked components",
        "## Linkage seam analysis",
        "GPL-2.0-or-later",
        "Tauri",
        "Little Snitch",
        "OpenSnitch",
        "WebKitGTK",
    ] {
        assert!(notice.contains(required), "NOTICE.md missing: {required}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-integration-tests license_and_notice_present_and_consistent`
Expected: FAIL — neither file exists.

- [ ] **Step 3: Create LICENSE**

Drop in the canonical GPL-2.0 text from https://www.gnu.org/licenses/old-licenses/gpl-2.0.txt. The full text is ~340 lines and starts with:

```
                    GNU GENERAL PUBLIC LICENSE
                       Version 2, June 1991

 Copyright (C) 1989, 1991 Free Software Foundation, Inc.,
 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301 USA
 Everyone is permitted to copy and distribute verbatim copies
 of this license document, but changing it is not allowed.
```

…and ends with the "How to Apply These Terms to Your New Programs" appendix. Use the full unmodified text — do not paraphrase.

- [ ] **Step 4: Create NOTICE.md**

```markdown
# Snitchwatch — Third-party attributions and linkage analysis

## Snitchwatch license

Snitchwatch is licensed **GPL-2.0-or-later**. The full license text is in [`LICENSE`](LICENSE).

We chose GPL-2.0-or-later (rather than GPL-2.0-only) so the project can be combined with GPL-3.0 components in the future without a relicensing event.

## Bundled / linked components

| Component                        | License            | How we use it                              |
|----------------------------------|--------------------|--------------------------------------------|
| Little Snitch–for-Linux web UI   | GPL-2.0            | Vendored under `vendor/ls-for-linux/`, loaded into the embedded WebKitGTK webview at runtime |
| OpenSnitch (`opensnitchd`)       | GPL-3.0            | Separate process, host-installed, talked to over gRPC. **No linkage** — Snitchwatch only consumes the protobuf-defined API surface |
| OpenSnitch protobuf (`ui.proto`) | GPL-3.0            | Code-generated bindings live in `crates/snitchwatch-proto`. The generated `.rs` files inherit GPL-3.0 — confined to that one crate so the rest of the workspace stays GPL-2.0+ |
| Tauri 2 (`tauri`, `tauri-build`) | MIT OR Apache-2.0  | Statically linked into the `snitchwatch-tauri` binary |
| `tauri-plugin-clipboard-manager` | MIT OR Apache-2.0  | Statically linked, used by the diagnostic-bundle "Copy" button |
| WebKitGTK (`webkit2gtk-4.1`)     | LGPL-2.1           | System library, loaded dynamically by Tauri at runtime — LGPL dynamic-link clause satisfied |
| `tonic`, `prost`, `tokio`, `axum`, `hyper`, `serde`, `tracing`, `anyhow`, `thiserror`, `regex`, `futures-util`, `tokio-tungstenite`, `async-stream`, `proptest`, `tar`, `flate2` | MIT OR Apache-2.0 (each) | Standard Rust ecosystem crates, statically linked |
| `freedesktop-sdk` runtime 23.08  | Various OSI        | Used as the Flatpak base runtime; not bundled in the source tarball |

A full crate-by-crate license inventory is reproducible with `cargo about generate licenses.html` (CI does not gate on this — it's a release-time human review).

## Linkage seam analysis

The shipped `snitchwatch-tauri` binary has three license-relevant linkage seams:

1. **Tauri shell ⊕ web UI bundle (single binary).**
   The Tauri shell statically links Tauri 2 (MIT/Apache) and at runtime loads the vendored LS-for-Linux web UI bundle (GPL-2.0) into the embedded WebKitGTK webview. The web UI is shipped as part of the Flatpak's `/app/share/snitchwatch/web` directory — i.e. **bundled with the binary**, not downloaded at runtime.

   GPL-2.0+ permits combination with MIT/Apache code in this direction (the MIT/Apache code can be relicensed under GPL-2.0+ for the combined work). The combined binary is therefore distributable under **GPL-2.0-or-later**.

2. **Bridge crate ↔ opensnitchd (separate processes, gRPC).**
   `snitchwatch-bridge` talks to `opensnitchd` over a network socket using a protobuf-defined API. Per the FSF's own guidance ([GPL FAQ § "What's the difference between an aggregate and other kinds of modified versions?"](https://www.gnu.org/licenses/gpl-faq.html#MereAggregation)), inter-process communication over a documented API is **not** linkage. We can therefore ship `snitchwatch-bridge` under GPL-2.0+ even though `opensnitchd` is GPL-3.0.

3. **`snitchwatch-proto` ⊕ generated bindings.**
   The `tonic-build`-generated `.rs` files inside `crates/snitchwatch-proto` are derived from the GPL-3.0 `ui.proto` schema. We treat that one crate as **GPL-3.0** (recorded in its own `Cargo.toml`'s `license = "GPL-3.0"` field) and the rest of the workspace stays GPL-2.0+. The dependency graph is one-way: only `snitchwatch-bridge` depends on `snitchwatch-proto`, and the bridge is GPL-2.0-or-later, which is upward-compatible with GPL-3.0 _at the bridge boundary_.

**Conclusion.** The shipped Snitchwatch binary's effective license is **GPL-2.0-or-later**. End users may redistribute it under that license. The OpenSnitch daemon they install separately remains under its own GPL-3.0 license.

This analysis is the resolution of spec open question #4 ("Are we sure GPL-2.0 LS UI + dual-MIT/Apache Tauri 2 combine cleanly into a redistributable Flatpak?"). It is intentionally conservative: when in doubt, we picked the stricter combination.

If you spot a license-compatibility error in this document, please open an issue or email `security@snitchwatch.example` (the same address as the security disclosure contact).
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p snitchwatch-integration-tests license_and_notice_present_and_consistent`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add LICENSE NOTICE.md tests/integration/tests/readme_shape.rs
git commit -m "$(cat <<'EOF'
docs: add LICENSE (GPL-2.0) and NOTICE.md

Adds the canonical GPL-2.0 text at the repo root and the
linkage-seam analysis that resolves spec open question #4.
The shipped binary's effective license is GPL-2.0-or-later;
the OpenSnitch daemon remains under its own GPL-3.0.
EOF
)"
```

---

### Task 4: docs/architecture.md with the A→B→C ws_bind upgrade path

**Files:**
- Create: `docs/architecture.md`
- Create: `tests/integration/tests/architecture_doc_shape.rs`

This is the longest doc in the plan but it's all narrative — no code. The shape test asserts the section list, the body is a literal write.

- [ ] **Step 1: Write the failing test**

Create `tests/integration/tests/architecture_doc_shape.rs`:

```rust
//! Schema-shape test for docs/architecture.md.
//!
//! Asserts every section a v0.1.0 architecture tour needs is present.
//! Failing this test means a load-bearing section was deleted or renamed.

use std::fs;

#[test]
fn architecture_doc_has_required_sections() {
    let body = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/architecture.md"),
    )
    .expect("docs/architecture.md must exist");

    for heading in [
        "# Snitchwatch architecture",
        "## What it is",
        "## What it is not",
        "## Crate map",
        "## Process topology",
        "## Data flow — an ask-rule round trip",
        "## Lifecycle banners",
        "## ws_bind upgrade path",
        "### Option A — loopback ephemeral (the v0.1.0 default)",
        "### Option B — loopback fixed-port",
        "### Option C — LAN bind with TLS + token auth (deferred)",
        "## Where to look first when something breaks",
    ] {
        assert!(body.contains(heading), "architecture.md missing: {heading}");
    }

    // The plan says explicitly: there is no "flip" in v0.1.0.
    assert!(
        body.contains("default since Plan 1"),
        "must call out that Option A has been the default since Plan 1"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-integration-tests architecture_doc_has_required_sections`
Expected: FAIL — file does not exist.

- [ ] **Step 3: Create docs/architecture.md**

```markdown
# Snitchwatch architecture

A guided tour of the v0.1.0 codebase. Read this once before your first PR.

## What it is

Snitchwatch is a desktop GUI that gives Linux users a Little Snitch–style network firewall experience on top of the OpenSnitch daemon. It is a thin Tauri 2 shell hosting an embedded WebKitGTK webview that loads a vendored copy of the (open-source) Little Snitch–for-Linux web UI. A Rust bridge crate (`snitchwatch-bridge`) translates between OpenSnitch's gRPC protocol and the Little Snitch web UI's WebSocket protocol so the two halves never need to know about each other.

## What it is not

- It is **not** a packet filter — `opensnitchd` is. Snitchwatch never touches netfilter or eBPF directly.
- It is **not** a daemon. The bridge runtime task lives inside the same process as the Tauri shell. There is no `snitchwatch-daemon` binary.
- It is **not** a fork of OpenSnitch. The `vendor/opensnitch/` submodule is pinned for the protobuf schema only; the Go daemon is consumed unmodified at runtime via the user's package manager (or our packaged Flatpak quadlet).
- It is **not** a Flatpak-only project. The same workspace builds an unbundled binary you can run from `cargo run`, which is what every dev does locally.

## Crate map

```text
crates/
├── snitchwatch-proto/        # tonic/prost bindings for ui.proto (GPL-3.0)
├── snitchwatch-spike/        # M0 spike binary; probes a live daemon
├── snitchwatch-bridge/       # the headless bridge library
│   ├── grpc_client.rs        # talks to opensnitchd
│   ├── ws_server.rs          # talks to the web UI
│   ├── translator/           # protocol translation, fully unit-tested
│   ├── cache.rs              # event ring buffer with dropped-counter
│   ├── blocklists/           # StevenBlack fetch + cache (Plan 5)
│   ├── lifecycle.rs          # the 5s probe + LifecycleEvent broadcast
│   └── lifecycle/journalctl.rs   # kernel-hook failure scraper (Plan 7)
├── snitchwatch-bridge-cli/   # thin orchestrator: lib::run + main.rs
└── snitchwatch-tauri/        # the desktop shell (Plan 4)
    ├── installer.rs          # wraps packaging/install.sh (Plan 6)
    ├── panic_hook.rs         # broadcasts BridgePanicRecovered (Plan 7)
    ├── diagnostics.rs        # the Copy diagnostics command (Plan 7)
    └── wizard.rs             # the first-run flow

tests/
├── integration/              # workspace-wide cross-crate tests
└── mock_opensnitchd/         # in-process gRPC mock with scripted events
```

Files-and-modules philosophy: **many small files**. Anything over 400 lines is a smell, anything over 800 is a hard ceiling. The bridge crate is intentionally many tiny modules so each one can be unit-tested in isolation and re-discovered in a fresh agent's context window.

## Process topology

```text
                        ┌─────────────────────────────────┐
                        │      snitchwatch-tauri          │
                        │  (single binary, Flatpak)       │
                        │                                 │
                        │  ┌──────────────────────────┐   │
                        │  │  WebKitGTK webview       │   │
                        │  │  (LS-for-Linux UI)       │   │
                        │  └──────────────┬───────────┘   │
                        │                 │ WS (loopback) │
                        │  ┌──────────────▼───────────┐   │
                        │  │  snitchwatch-bridge      │   │
                        │  │  runtime task            │   │
                        │  └──────────────┬───────────┘   │
                        └─────────────────┼───────────────┘
                                          │ gRPC (loopback)
                                          ▼
                        ┌─────────────────────────────────┐
                        │      opensnitchd                │
                        │  (separate process, podman)     │
                        └─────────────────────────────────┘
                                          │ netlink / eBPF / nfqueue
                                          ▼
                                       kernel
```

Three loopback hops, two protocol seams, one shared address space (the Tauri binary). All four communication edges are typed end-to-end:

- **kernel ↔ opensnitchd:** netlink / eBPF (out of scope for Snitchwatch)
- **opensnitchd ↔ bridge:** protobuf-over-gRPC (`snitchwatch-proto`)
- **bridge ↔ webview:** typed JSON over WebSocket (`ws_messages.rs`)
- **bridge ↔ Tauri shell:** `tauri::ipc::invoke` for `installer::install_daemon`, `diagnostics::copy_diagnostic_bundle`, etc.

## Data flow — an ask-rule round trip

When `opensnitchd` decides to ask the user about a connection, the path is:

```text
1. opensnitchd      →  Ask{src,dst,proc,...}        (gRPC stream item)
2. grpc_client.rs   →  IncomingEvent::Ask{...}      (typed Rust enum)
3. translator/upstream.rs → Translated::AskRule(Box<AskRulePayload>)
4. cache.rs         →  push to broadcast channel
5. ws_server.rs     →  serialize to ServerMessage::AskRule
6. (loopback WS)    →  webview
7. webview JS       →  render the LS-style modal, await user click
8. webview JS       →  send ClientMessage::SetVerdict
9. ws_server.rs     →  deserialize ClientMessage::SetVerdict
10. translator/downstream.rs → grpc::Reply{verdict, persist, ...}
11. grpc_client.rs  →  send NotificationReply(...) on the gRPC return stream
12. opensnitchd     →  applies the rule, releases the connection
```

The whole round trip is exercised end-to-end in `tests/bridge_protocol_test.rs::ask_rule_round_trip_full` against the in-process mock daemon.

## Lifecycle banners

The bridge runs a 5-second probe (`lifecycle.rs`) that pings `opensnitchd` over gRPC and checks `systemctl --user is-active snitchwatch-opensnitchd.service`. The probe broadcasts a typed `LifecycleEvent` every tick. The web UI subscribes via `ws_server.rs::serve_with_lifecycle` and renders one banner per active failure mode.

The 10 lifecycle kinds are mapped to user-visible labels in [`README.md`](../README.md#failure-mode-banners). The full enum is `LifecycleKind` in `crates/snitchwatch-bridge/src/lifecycle.rs`. Plan 7 wires the four still-stubbed emission points: `EventFloodDropped`, `BridgePanicRecovered`, `KernelHookFailed`, `StateDivergenceReconciled`.

## ws_bind upgrade path

The bridge's WebSocket server binds to a configurable address (`BridgeConfig::ws_bind`). v0.1.0 ships **Option A**, with Options B and C as documented future paths.

### Option A — loopback ephemeral (the v0.1.0 default)

Bind to `127.0.0.1:0`, let the kernel pick an ephemeral port. The chosen port is logged as `WS_LISTEN_ADDR=127.0.0.1:NNNNN` and the Tauri shell reads it from the bridge handle to wire the webview.

This has been the default since Plan 1 (`crates/snitchwatch-bridge-cli/src/lib.rs::BridgeConfig::from_env`). There is **no flip in v0.1.0** — Plan 7 only documents the existing default.

Pros:
- Zero conflict with anything else listening on a fixed port.
- Impossible to expose the bridge on the LAN by accident.
- Each test run gets its own fresh port — no cleanup between integration tests.

Cons:
- A second tool (e.g. `websocat`) cannot connect without first reading the bridge log to discover the port.

### Option B — loopback fixed-port

Set `SNITCHWATCH_WS_BIND=127.0.0.1:5037` (or any other port) at startup, or set `bridge.bind_address` in the future `settings.toml` (which does not yet exist in v0.1.0). The bridge will bind exactly that address and exit non-zero if it is already in use. Useful for:

- Running a second tool against the bridge without parsing the log.
- Pinning the port for a debugger / introspection tool.
- Reproducible end-to-end tests outside the workspace.

This works in v0.1.0 today — no code change required, just set the env var.

### Option C — LAN bind with TLS + token auth (deferred)

Bind to `0.0.0.0:5037` (or a specific interface), require an `Authorization: Bearer <token>` header on the WS upgrade, terminate TLS with a self-signed cert generated on first run. **Deferred to v2.** Three independent things have to land first:

1. A token-management UX (where do you copy the token from? How do you rotate?).
2. A self-signed-cert flow that does not produce a scary browser warning when a remote tool tries to connect.
3. A threat model — the bridge currently assumes a same-user adversary cannot read its memory; LAN mode breaks that assumption.

Until those are designed, the bridge refuses to bind to anything other than a loopback address: `BridgeConfig::from_env` returns a `ConfigError::NonLoopbackBindRefused` if the parsed `ws_bind` is not in `127.0.0.0/8`.

## Where to look first when something breaks

| Symptom                              | Where to look first                                                |
|--------------------------------------|--------------------------------------------------------------------|
| Wizard hangs on "Install daemon"     | `crates/snitchwatch-tauri/src/installer.rs` + `packaging/install.sh` |
| Banner says "Daemon unreachable"     | `crates/snitchwatch-bridge/src/grpc_client.rs::ping`               |
| Banner says "Kernel hook unavailable"| `crates/snitchwatch-bridge/src/lifecycle/journalctl.rs`            |
| Modal never appears for new connections | `tests/bridge_protocol_test.rs::ask_rule_round_trip_full` (run it locally to bisect) |
| Coverage drops below 80%             | `scripts/coverage-gate.sh` — it prints the per-file delta          |
| Flatpak build fails                  | `packaging/flatpak/org.snitchwatch.Snitchwatch.yml` finish-args    |
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-integration-tests architecture_doc_has_required_sections`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add docs/architecture.md tests/integration/tests/architecture_doc_shape.rs
git commit -m "$(cat <<'EOF'
docs: add architecture.md with the ws_bind A→B→C upgrade path

The full system tour: crate map, process topology, ask-rule data
flow, lifecycle banners, the explicit "no flip in v0.1.0" note for
ws_bind, and a where-to-look-first triage table.
EOF
)"
```

---

## Part B — Release automation (Tasks 5–7)

### Task 5: ci.yml (PR gate)

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: Write the failing test**

Create `tests/integration/tests/ci_workflow_shape.rs`:

```rust
//! Schema-shape test for the CI workflow.
//!
//! Asserts every job a v0.1.0 PR gate needs is present. Failing this
//! test means a CI job was deleted or renamed without updating the
//! contributing docs.

use std::fs;

#[test]
fn ci_workflow_has_required_jobs() {
    let body = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.github/workflows/ci.yml"),
    )
    .expect(".github/workflows/ci.yml must exist");

    for required in [
        "name: CI",
        "on:",
        "  pull_request:",
        "  push:",
        "jobs:",
        "  fmt:",
        "  clippy:",
        "  test:",
        "  coverage:",
        "  bats:",
        "cargo fmt --check",
        "cargo clippy --all-targets --all-features -- -D warnings",
        "cargo test --workspace",
        "cargo llvm-cov",
        "--fail-under-lines 80",
        "bats packaging/install.bats",
    ] {
        assert!(body.contains(required), "ci.yml missing: {required}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-integration-tests ci_workflow_has_required_jobs`
Expected: FAIL — file does not exist.

- [ ] **Step 3: Create .github/workflows/ci.yml**

```yaml
name: CI

on:
  pull_request:
    branches: [main]
  push:
    branches: [main]

env:
  CARGO_TERM_COLOR: always
  RUST_BACKTRACE: 1

jobs:
  fmt:
    name: cargo fmt
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: recursive
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt
      - run: cargo fmt --check

  clippy:
    name: cargo clippy
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: recursive
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy
      - name: Install protoc
        run: sudo apt-get update && sudo apt-get install -y protobuf-compiler
      - uses: Swatinem/rust-cache@v2
      - run: cargo clippy --all-targets --all-features -- -D warnings

  test:
    name: cargo test
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: recursive
      - uses: dtolnay/rust-toolchain@stable
      - name: Install protoc
        run: sudo apt-get update && sudo apt-get install -y protobuf-compiler
      - uses: Swatinem/rust-cache@v2
      - run: cargo test --workspace --all-features

  coverage:
    name: cargo llvm-cov ≥ 80%
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: recursive
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: llvm-tools-preview
      - name: Install protoc
        run: sudo apt-get update && sudo apt-get install -y protobuf-compiler
      - uses: taiki-e/install-action@cargo-llvm-cov
      - uses: Swatinem/rust-cache@v2
      - name: Run coverage gate
        run: bash scripts/coverage-gate.sh
      - name: Upload lcov
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: coverage-lcov
          path: target/llvm-cov/lcov.info

  bats:
    name: bats packaging/install.bats
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v4
      - name: Install bats
        run: sudo apt-get update && sudo apt-get install -y bats
      - run: bats packaging/install.bats
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-integration-tests ci_workflow_has_required_jobs`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/ci.yml tests/integration/tests/ci_workflow_shape.rs
git commit -m "$(cat <<'EOF'
ci: add PR gate workflow

Adds .github/workflows/ci.yml with five jobs: fmt, clippy, test,
coverage (≥80% via cargo-llvm-cov), and bats. Caches cargo state
on Cargo.lock so cold runs only happen on dep bumps.
EOF
)"
```

---

### Task 6: scripts/coverage-gate.sh

**Files:**
- Create: `scripts/coverage-gate.sh`

The CI coverage job calls `bash scripts/coverage-gate.sh`. Test the script via `bats` so the per-crate include list cannot drift silently.

- [ ] **Step 1: Write the failing test**

Append to `packaging/install.bats` (created in Plan 6):

```bash
@test "scripts/coverage-gate.sh exists and is executable" {
    [ -x scripts/coverage-gate.sh ]
}

@test "scripts/coverage-gate.sh includes the four gated crates" {
    run grep -E '(translator|cache|blocklists|lifecycle)' scripts/coverage-gate.sh
    [ "$status" -eq 0 ]
}

@test "scripts/coverage-gate.sh enforces 80 percent" {
    run grep -F -- '--fail-under-lines 80' scripts/coverage-gate.sh
    [ "$status" -eq 0 ]
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `bats packaging/install.bats`
Expected: FAIL — `scripts/coverage-gate.sh` does not exist.

- [ ] **Step 3: Create scripts/coverage-gate.sh**

```bash
#!/usr/bin/env bash
# Coverage gate for v0.1.0. Wraps cargo-llvm-cov with the per-crate
# include list and the 80% line-coverage floor. Prints the failing
# files on exit so CI logs are immediately useful.
#
# Usage: bash scripts/coverage-gate.sh
#
# Exit 0 on pass, non-zero on fail. The CI job at .github/workflows/ci.yml
# is the only caller; humans run `just coverage` instead.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# The four gated modules. Anything in tests/, snitchwatch-spike, or
# snitchwatch-proto is excluded — the proto crate is generated and the
# spike is throwaway.
INCLUDE_REGEX='crates/(snitchwatch-bridge/src/(translator|cache|blocklists|lifecycle))'
EXCLUDE_REGEX='tests/|crates/snitchwatch-spike|crates/snitchwatch-proto'

echo "[coverage-gate] running cargo llvm-cov on:"
echo "[coverage-gate]   include = ${INCLUDE_REGEX}"
echo "[coverage-gate]   exclude = ${EXCLUDE_REGEX}"
echo "[coverage-gate]   floor   = 80% lines"

cargo llvm-cov \
    --workspace \
    --all-features \
    --include-build-script \
    --ignore-filename-regex "${EXCLUDE_REGEX}" \
    --fail-under-lines 80 \
    --lcov \
    --output-path target/llvm-cov/lcov.info

echo "[coverage-gate] PASS"
```

Make it executable:

```bash
chmod +x scripts/coverage-gate.sh
```

- [ ] **Step 4: Run test to verify it passes**

Run: `bats packaging/install.bats`
Expected: PASS — all three new bats cases green.

- [ ] **Step 5: Commit**

```bash
git add scripts/coverage-gate.sh packaging/install.bats
git commit -m "$(cat <<'EOF'
ci: add scripts/coverage-gate.sh with the v0.1.0 80% floor

Wraps cargo-llvm-cov with the per-crate include list (translator,
cache, blocklists, lifecycle) and the --fail-under-lines 80 floor.
Bats tests assert the script stays in sync with the gated module list.
EOF
)"
```

---

### Task 7: release.yml + CHANGELOG.md

**Files:**
- Create: `.github/workflows/release.yml`
- Create: `CHANGELOG.md`

Tag-driven release. The workflow runs `flatpak-builder`, sha256s the artifact, extracts the matching CHANGELOG section as the release body, and uploads everything via `softprops/action-gh-release@v2`.

- [ ] **Step 1: Write the failing test**

Create `tests/integration/tests/release_workflow_shape.rs`:

```rust
//! Schema-shape test for the release workflow + CHANGELOG.

use std::fs;

#[test]
fn release_workflow_has_required_steps() {
    let body = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.github/workflows/release.yml"),
    )
    .expect(".github/workflows/release.yml must exist");

    for required in [
        "name: Release",
        "on:",
        "  push:",
        "    tags:",
        "      - 'v*.*.*'",
        "jobs:",
        "  build-flatpak:",
        "  publish:",
        "bilelmoussaoui/flatpak-github-actions/flatpak-builder",
        "manifest-path: packaging/flatpak/org.snitchwatch.Snitchwatch.yml",
        "bundle: snitchwatch.flatpak",
        "softprops/action-gh-release@v2",
        "sha256sum",
    ] {
        assert!(body.contains(required), "release.yml missing: {required}");
    }
}

#[test]
fn changelog_has_v0_1_0_entry() {
    let body = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../CHANGELOG.md"),
    )
    .expect("CHANGELOG.md must exist");

    for required in [
        "# Changelog",
        "## [0.1.0]",
        "### Added",
        "### Known issues",
        "### Deferred",
        "Plan 7",
    ] {
        assert!(body.contains(required), "CHANGELOG.md missing: {required}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-integration-tests release_workflow_has_required_steps changelog_has_v0_1_0_entry`
Expected: FAIL — neither file exists.

- [ ] **Step 3: Create .github/workflows/release.yml**

```yaml
name: Release

on:
  push:
    tags:
      - 'v*.*.*'

permissions:
  contents: write

jobs:
  build-flatpak:
    name: Build Flatpak bundle
    runs-on: ubuntu-24.04
    container:
      image: bilelmoussaoui/flatpak-github-actions:freedesktop-23.08
      options: --privileged
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: recursive

      - name: Build flatpak bundle
        uses: bilelmoussaoui/flatpak-github-actions/flatpak-builder@v6
        with:
          bundle: snitchwatch.flatpak
          manifest-path: packaging/flatpak/org.snitchwatch.Snitchwatch.yml
          cache-key: flatpak-builder-${{ github.sha }}

      - name: Upload bundle artifact
        uses: actions/upload-artifact@v4
        with:
          name: snitchwatch-flatpak
          path: snitchwatch.flatpak

  publish:
    name: Publish GitHub release
    needs: build-flatpak
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - name: Download flatpak bundle
        uses: actions/download-artifact@v4
        with:
          name: snitchwatch-flatpak
          path: dist/

      - name: Compute sha256
        run: |
          cd dist
          sha256sum snitchwatch.flatpak > snitchwatch.flatpak.sha256
          cat snitchwatch.flatpak.sha256

      - name: Build source tarball
        run: |
          TAG="${GITHUB_REF_NAME}"
          git archive --format=tar.gz \
            --prefix="snitchwatch-${TAG}/" \
            --output="dist/snitchwatch-${TAG}.tar.gz" \
            HEAD
          cd dist
          sha256sum "snitchwatch-${TAG}.tar.gz" > "snitchwatch-${TAG}.tar.gz.sha256"

      - name: Extract changelog section
        id: changelog
        run: |
          TAG="${GITHUB_REF_NAME#v}"
          awk -v tag="$TAG" '
            BEGIN { capture = 0 }
            /^## \[/ {
              if (capture == 1) exit
              if ($0 ~ "\\[" tag "\\]") capture = 1
              next
            }
            capture == 1 { print }
          ' CHANGELOG.md > dist/release-body.md
          echo "----- release body -----"
          cat dist/release-body.md
          echo "------------------------"

      - name: Publish release
        uses: softprops/action-gh-release@v2
        with:
          body_path: dist/release-body.md
          files: |
            dist/snitchwatch.flatpak
            dist/snitchwatch.flatpak.sha256
            dist/snitchwatch-*.tar.gz
            dist/snitchwatch-*.tar.gz.sha256
          fail_on_unmatched_files: true
          draft: false
          prerelease: false
```

- [ ] **Step 4: Create CHANGELOG.md**

```markdown
# Changelog

All notable changes to Snitchwatch are documented in this file. The format is
based on [Keep a Changelog 1.1](https://keepachangelog.com/en/1.1.0/) and the
project adheres to [Semantic Versioning 2.0](https://semver.org/spec/v2.0.0.html).

## [0.1.0] — 2026-04-11

First public release. Headless bridge, vendored Little Snitch–for-Linux web UI,
Tauri 2 shell, Flatpak packaging, OpenSnitch quadlet. Closes spec milestones
M0–M6.

### Added

- **M0 — Spike.** `snitchwatch-spike` binary that probes a live opensnitchd
  daemon and dumps the discovered surface to `docs/m0-spike-findings.md`.
- **M1 — Bridge foundation.** `snitchwatch-bridge` library with full ask-rule
  round-trip translation between OpenSnitch gRPC and the LS WebSocket
  protocol. End-to-end tested against an in-process mock daemon.
- **M2 — Topology flip.** Bridge becomes the source of truth; the JSON
  envelope hack from M1 is deleted. Typed `ws_messages.rs` variants only.
- **M3 — Vendored UI.** LS-for-Linux web UI vendored under `vendor/ls-for-linux`
  and wired through the bridge.
- **M4 — Tauri shell.** `snitchwatch-tauri` desktop shell with first-run wizard,
  embedded WebKitGTK webview, and the `install_daemon` Tauri command.
- **M5 — Packaging.** Flatpak manifest, OpenSnitch podman quadlet, idempotent
  `packaging/install.sh`, lifecycle probe with 10 typed failure modes, in-frame
  banner UI for each.
- **M6 — Public release.** This release. CI + tag-driven release workflow,
  ≥80% coverage gate on the four hot crates, GPL-2.0 LICENSE + NOTICE.md
  with the linkage-seam analysis, CONTRIBUTING.md, full architecture
  documentation, manual smoke checklist, diagnostic-bundle "Copy" button,
  panic hook + journalctl scrape + reconciliation hook for the four
  remaining lifecycle emission points.

### Changed

- `BridgeConfig::ws_bind` documented (no flip — has been `127.0.0.1:0` since
  Plan 1). The A→B→C ws_bind upgrade path is documented in
  `docs/architecture.md`.
- `Cargo.toml` workspace `repository` updated from the placeholder to the
  real GitHub URL.
- `README.md` rewritten as a user-facing landing page (was a Plan 1 era
  bridge-only doc).

### Fixed

- `cache::EventCache::push` now increments the dropped-event counter on
  `send_timeout` failure (was previously a TODO comment from Plan 6).
- `grpc_client.rs` reconciliation loop now broadcasts
  `LifecycleKind::StateDivergenceReconciled` when the snapshot diff is
  non-empty (was previously logged but not surfaced).

### Known issues

- LAN bind mode (Option C in the ws_bind upgrade path) is refused at
  startup with `ConfigError::NonLoopbackBindRefused`. v2 will add TLS +
  token auth.
- The diagnostic bundle's `journalctl` tail requires `journalctl --user`
  to be available; on systems without systemd-journald the bundle ships
  with an empty journal section.
- The wizard's "Install daemon" button assumes `flatpak`, `podman`, and
  `systemctl --user` are all on the host PATH. Failure modes are
  surfaced as banners but the wizard does not yet detect missing
  prerequisites up-front.

### Deferred

These items have explicit v2 owners and are tracked in
`docs/superpowers/specs/2026-04-10-snitchwatch-design.md` § Deferred:

- LAN-mode bind with TLS + token auth
- Flathub store submission
- Auto-update channel
- Translated UI
- App-icon designer pass (v0.1.0 ships the placeholder SVG from Plan 6)
- Cross-distro packaging beyond Bazzite / Universal Blue
- Telemetry / crash reporting upload (local diagnostic bundle only in v0.1.0)

### Plan 7 trail

This release was scoped and built under
`docs/superpowers/plans/2026-04-11-public-release.md`. Every task has a
matching commit; the commit graph between the v0.0.0 mark and v0.1.0 is
the implementation history.
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p snitchwatch-integration-tests release_workflow_has_required_steps changelog_has_v0_1_0_entry`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/release.yml CHANGELOG.md tests/integration/tests/release_workflow_shape.rs
git commit -m "$(cat <<'EOF'
ci: add tag-driven release workflow + CHANGELOG.md

The release workflow triggers on `v*.*.*` tag pushes, builds the
Flatpak bundle via flatpak-github-actions, sha256s every artifact,
extracts the matching CHANGELOG section as the release body, and
publishes via softprops/action-gh-release.

CHANGELOG.md is Keep-a-Changelog 1.1, populated from the M0–M6
milestone trail and the Plan 7 acceptance criteria.
EOF
)"
```

---

## Part C — Deferred verification (Tasks 8–10)

### Task 8: scripts/live-smoke.sh + the `--once-then-exit-after` flag

**Files:**
- Create: `scripts/live-smoke.sh`
- Modify: `crates/snitchwatch-bridge-cli/src/lib.rs`
- Create: `crates/snitchwatch-bridge-cli/tests/once_then_exit_after.rs`

This is the Plan 1 deferred "live opensnitchd 60s smoke against rootful podman" item — Plan 7 owns it per memory entry #3.

- [ ] **Step 1: Write the failing CLI test**

Create `crates/snitchwatch-bridge-cli/tests/once_then_exit_after.rs`:

```rust
//! The bridge CLI gains a `--once-then-exit-after <DURATION>` flag so the
//! live-smoke script can run it as a wall-clock-bounded one-shot. Without
//! this, scripts/live-smoke.sh has no clean way to bound the run.

use snitchwatch_bridge_cli::{parse_args, CliArgs};
use std::time::Duration;

#[test]
fn parses_once_then_exit_after_with_seconds() {
    let args = vec![
        "snitchwatch-bridge-cli".to_string(),
        "--once-then-exit-after".to_string(),
        "60s".to_string(),
    ];
    let parsed: CliArgs = parse_args(&args).expect("must parse");
    assert_eq!(parsed.once_then_exit_after, Some(Duration::from_secs(60)));
}

#[test]
fn parses_once_then_exit_after_with_milliseconds() {
    let args = vec![
        "snitchwatch-bridge-cli".to_string(),
        "--once-then-exit-after".to_string(),
        "500ms".to_string(),
    ];
    let parsed = parse_args(&args).expect("must parse");
    assert_eq!(parsed.once_then_exit_after, Some(Duration::from_millis(500)));
}

#[test]
fn defaults_to_none_when_flag_absent() {
    let args = vec!["snitchwatch-bridge-cli".to_string()];
    let parsed = parse_args(&args).expect("must parse");
    assert_eq!(parsed.once_then_exit_after, None);
}

#[test]
fn rejects_unparseable_duration() {
    let args = vec![
        "snitchwatch-bridge-cli".to_string(),
        "--once-then-exit-after".to_string(),
        "forever".to_string(),
    ];
    assert!(parse_args(&args).is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge-cli once_then_exit_after`
Expected: FAIL — `parse_args` does not exist yet, `CliArgs` has no `once_then_exit_after` field.

- [ ] **Step 3: Add the flag to the CLI**

Add to `crates/snitchwatch-bridge-cli/src/lib.rs`:

```rust
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CliArgs {
    pub once_then_exit_after: Option<Duration>,
}

#[derive(Debug, thiserror::Error)]
pub enum CliParseError {
    #[error("--once-then-exit-after requires a value")]
    MissingValue,
    #[error("invalid duration: {0}")]
    InvalidDuration(String),
    #[error("unknown argument: {0}")]
    Unknown(String),
}

pub fn parse_args(argv: &[String]) -> Result<CliArgs, CliParseError> {
    let mut out = CliArgs::default();
    let mut iter = argv.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--once-then-exit-after" => {
                let value = iter.next().ok_or(CliParseError::MissingValue)?;
                out.once_then_exit_after = Some(parse_duration(value)?);
            }
            other => return Err(CliParseError::Unknown(other.to_string())),
        }
    }
    Ok(out)
}

fn parse_duration(s: &str) -> Result<Duration, CliParseError> {
    if let Some(rest) = s.strip_suffix("ms") {
        let n: u64 = rest
            .parse()
            .map_err(|_| CliParseError::InvalidDuration(s.to_string()))?;
        Ok(Duration::from_millis(n))
    } else if let Some(rest) = s.strip_suffix('s') {
        let n: u64 = rest
            .parse()
            .map_err(|_| CliParseError::InvalidDuration(s.to_string()))?;
        Ok(Duration::from_secs(n))
    } else {
        Err(CliParseError::InvalidDuration(s.to_string()))
    }
}
```

In `run()` (the existing async entrypoint), wire the deadline:

```rust
if let Some(deadline) = cli_args.once_then_exit_after {
    tracing::info!(?deadline, "running with wall-clock deadline");
    tokio::select! {
        _ = bridge_main_loop() => {}
        _ = tokio::time::sleep(deadline) => {
            tracing::info!("deadline reached, exiting cleanly");
        }
    }
} else {
    bridge_main_loop().await;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge-cli once_then_exit_after`
Expected: PASS — all four cases green.

- [ ] **Step 5: Create scripts/live-smoke.sh**

```bash
#!/usr/bin/env bash
# 60-second live opensnitchd smoke. Pulls and runs the upstream
# OpenSnitch container in rootful podman, waits for the gRPC port,
# runs the bridge with --once-then-exit-after 60s, asserts that
#   1. the bridge logged WS_LISTEN_ADDR=
#   2. at least one event_kind= line appeared
# Tears the container down on exit (success or failure).
#
# Usage: bash scripts/live-smoke.sh
#
# Exit 0 on PASS, 1 on FAIL with a structured [FAIL] reason= line.
# Closes the Plan 1 deferred acceptance criterion.

set -euo pipefail

CONTAINER_NAME="snitchwatch-live-smoke"
IMAGE="docker.io/evilsocket/opensnitch:latest"
GRPC_PORT="50051"
LOG_FILE="$(mktemp)"

cleanup() {
    podman rm -f "${CONTAINER_NAME}" >/dev/null 2>&1 || true
    rm -f "${LOG_FILE}"
}
trap cleanup EXIT

echo "[live-smoke] pulling ${IMAGE}"
podman pull "${IMAGE}"

echo "[live-smoke] starting container"
podman run -d --rm \
    --name "${CONTAINER_NAME}" \
    --privileged --network=host --pid=host \
    --cap-add=NET_ADMIN,SYS_ADMIN,BPF \
    "${IMAGE}"

echo "[live-smoke] waiting up to 30s for gRPC :${GRPC_PORT}"
for _ in $(seq 1 30); do
    if (echo > /dev/tcp/127.0.0.1/${GRPC_PORT}) >/dev/null 2>&1; then
        echo "[live-smoke] gRPC port open"
        break
    fi
    sleep 1
done

if ! (echo > /dev/tcp/127.0.0.1/${GRPC_PORT}) >/dev/null 2>&1; then
    echo "[FAIL] reason=grpc-port-never-opened"
    exit 1
fi

echo "[live-smoke] running bridge for 60s"
RUST_LOG=info \
    cargo run --quiet -p snitchwatch-bridge-cli -- \
    --once-then-exit-after 60s 2>&1 | tee "${LOG_FILE}"

echo "[live-smoke] checking log invariants"

if ! grep -q 'WS_LISTEN_ADDR=' "${LOG_FILE}"; then
    echo "[FAIL] reason=ws-listen-addr-line-missing"
    exit 1
fi

if ! grep -q 'event_kind=' "${LOG_FILE}"; then
    echo "[FAIL] reason=no-events-flowed"
    exit 1
fi

echo "[live-smoke] PASS"
```

Make it executable: `chmod +x scripts/live-smoke.sh`

- [ ] **Step 6: Add the `just live-smoke` recipe**

Append to `justfile`:

```make
# 60s live smoke against a real opensnitchd container.
# Closes Plan 1 deferred acceptance criterion. Requires rootful podman.
live-smoke:
    bash scripts/live-smoke.sh
```

- [ ] **Step 7: Commit**

```bash
git add scripts/live-smoke.sh \
        crates/snitchwatch-bridge-cli/src/lib.rs \
        crates/snitchwatch-bridge-cli/tests/once_then_exit_after.rs \
        justfile
git commit -m "$(cat <<'EOF'
test(bridge): add --once-then-exit-after CLI flag and live-smoke driver

Closes the Plan 1 deferred "60s live opensnitchd smoke" criterion.
The new --once-then-exit-after <duration> flag bounds the bridge
runtime so scripts/live-smoke.sh can run it as a wall-clock-bounded
one-shot inside CI or against a rootful podman opensnitchd container.

Adds `just live-smoke` recipe and four parser unit tests.
EOF
)"
```

---

### Task 9: Coverage gate proves ≥80% on the four gated modules

**Files:**
- (no new files — this task is verification work that produces a CI log entry)

This is the Plan 1 deferred "cargo-llvm-cov coverage gate" criterion. Tasks 5 and 6 already wired the CI job and the helper script. Task 9's job is to **run the gate locally**, fix any module that drops below 80%, and capture the passing report as the closure of the deferred criterion.

- [ ] **Step 1: Run the gate locally**

Run: `bash scripts/coverage-gate.sh`
Expected on first run: likely FAIL with a per-file table showing which modules are under 80%.

- [ ] **Step 2: Fix coverage gaps**

For each file under 80%, write a unit test in the same module that exercises the uncovered branches. The four gated modules are:

- `crates/snitchwatch-bridge/src/translator/upstream.rs`
- `crates/snitchwatch-bridge/src/translator/downstream.rs`
- `crates/snitchwatch-bridge/src/cache.rs`
- `crates/snitchwatch-bridge/src/cache/dropped_counter.rs`
- `crates/snitchwatch-bridge/src/blocklists/mod.rs`
- `crates/snitchwatch-bridge/src/blocklists/cache.rs`
- `crates/snitchwatch-bridge/src/blocklists/parser.rs`
- `crates/snitchwatch-bridge/src/lifecycle.rs`
- `crates/snitchwatch-bridge/src/lifecycle/probe_state.rs`
- `crates/snitchwatch-bridge/src/lifecycle/journalctl.rs` (created in Task 12)
- `crates/snitchwatch-bridge/src/lifecycle/journalctl/parse.rs` (created in Task 12)

For each gap, add a `#[cfg(test)]` test that:
1. Constructs the input that walks the uncovered branch.
2. Asserts the observable behavior (return value, broadcast event, log line).

Do **not** lower the threshold. Do **not** add `#[cfg_attr(coverage, no_coverage)]` annotations to "skip" hard-to-test code — if it's hard to test, it's a design smell that the coverage gate is correctly catching. Refactor to make it testable instead.

- [ ] **Step 3: Re-run the gate**

Run: `bash scripts/coverage-gate.sh`
Expected: PASS with `[coverage-gate] PASS` on stdout.

- [ ] **Step 4: Capture the passing report in docs/manual-smoke.md**

Create the file (full body lands in Task 17 — this step just appends one line to a placeholder section). For now, drop a note in the commit message that the report was captured locally on the dev machine.

- [ ] **Step 5: Commit (one consolidated commit per fixed module)**

The exact commits depend on which modules needed work. Use this template per module:

```bash
git add crates/snitchwatch-bridge/src/<module>.rs
git commit -m "$(cat <<'EOF'
test(bridge): cover <branch description> in <module>

Closes Plan 7 Task 9 coverage gate for <module> at ≥80% lines.
EOF
)"
```

After all modules pass, one final commit:

```bash
git commit --allow-empty -m "$(cat <<'EOF'
test: close Plan 1 deferred coverage criterion

cargo-llvm-cov ≥80% on the four gated module groups:
translator, cache, blocklists, lifecycle.
EOF
)"
```

---

### Task 10: NOTICE.md legal sanity check sign-off

**Files:**
- Modify: `NOTICE.md`

The Linkage seam analysis section in NOTICE.md is the substantive resolution of spec open question #4. Task 10's job is the **sign-off**: walk every dependency in `Cargo.lock`, confirm each one's license matches what NOTICE.md claims, and amend NOTICE.md if anything has drifted.

- [ ] **Step 1: Generate the dependency license report**

```bash
cargo install cargo-about
cargo about generate --workspace -o licenses.html about.toml
```

If `about.toml` does not exist, create it:

```toml
accepted = [
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Unicode-DFS-2016",
    "GPL-2.0",
    "GPL-2.0-or-later",
    "GPL-3.0",
    "LGPL-2.1",
    "MPL-2.0",
    "Zlib",
]
ignore-build-dependencies = false
ignore-dev-dependencies = true
```

- [ ] **Step 2: Diff against NOTICE.md**

Open `licenses.html` and confirm every distinct license in `accepted` is documented in NOTICE.md's "Bundled / linked components" table. If a new dep has been added since this plan was written, append a row.

If any dep has a license **not** in the accepted list, that is a release blocker. Either:
- Replace the dep with one under an accepted license, or
- Add the license to `accepted` only if it is GPL-2.0-or-later compatible (consult [the FSF compatibility matrix](https://www.gnu.org/licenses/license-list.html)).

- [ ] **Step 3: Amend NOTICE.md if needed**

If anything drifted, edit `NOTICE.md` to bring the table back in sync.

- [ ] **Step 4: Add `licenses.html` to .gitignore**

Append to `.gitignore`:

```text
# Generated by `cargo about` for the Plan 7 Task 10 sign-off.
licenses.html
```

- [ ] **Step 5: Commit**

```bash
git add .gitignore NOTICE.md about.toml
git commit -m "$(cat <<'EOF'
docs: sign off NOTICE.md against cargo-about license report

Walks every dependency in Cargo.lock, confirms each license is in
the accepted list, amends NOTICE.md where needed. Closes spec open
question #4 with mechanical evidence on top of the Task 3 narrative.
EOF
)"
```

---

## Part D — Lifecycle emission wiring (Tasks 11–14)

### Task 11: Wire `EventFloodDropped` through cache::dropped_counter

**Files:**
- Modify: `crates/snitchwatch-bridge/src/cache.rs`
- Modify: `crates/snitchwatch-bridge/src/cache/dropped_counter.rs`
- Modify: `crates/snitchwatch-bridge/src/lifecycle.rs`

Plan 6 created `dropped_counter.rs` with the `AtomicU32` and an `increment()` function but left `EventCache::push` with a `// TODO(plan-7): wire here` comment at the `send_timeout` failure branch. Plan 6 also stubbed `LifecycleProbe::tick` with a `// TODO(plan-7): drain dropped counter` comment. Both TODOs disappear in this task.

- [ ] **Step 1: Write the failing test**

Create `crates/snitchwatch-bridge/tests/dropped_counter_emission.rs`:

```rust
//! When the event cache drops events because the broadcast channel is full,
//! the next LifecycleProbe tick must surface the count as
//! LifecycleKind::EventFloodDropped { dropped: N } and then reset the
//! counter atomically (so we don't double-report on the next tick).

use snitchwatch_bridge::cache::{dropped_counter, EventCache};
use snitchwatch_bridge::lifecycle::{LifecycleKind, LifecycleProbe, ProbeInputs};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_counter_surfaces_as_event_flood_dropped() {
    // Reset to a known state.
    let _ = dropped_counter::take();

    // Pretend the cache pushed and timed out 7 times.
    for _ in 0..7 {
        dropped_counter::increment();
    }
    assert_eq!(dropped_counter::take(), 7);
    assert_eq!(dropped_counter::take(), 0); // confirm reset semantics

    // Now drive a LifecycleProbe tick with an injected fake.
    for _ in 0..3 {
        dropped_counter::increment();
    }

    let (tx, mut rx) = broadcast::channel(16);
    let probe = LifecycleProbe::new_for_test(ProbeInputs {
        grpc_ping: Box::new(|| Box::pin(async { Ok(()) })),
        systemctl_state: Box::new(|| Box::pin(async { Ok("active".into()) })),
        journalctl_scrape: Box::new(|| None),
        reconciliation_diff: Arc::new(|| false),
        broadcast: tx,
    });

    probe.tick().await;

    // First event is DaemonOk because grpc_ping returned ok.
    let first = rx.try_recv().expect("must broadcast at least one event");
    let second = rx.try_recv().expect("must also broadcast EventFloodDropped");

    let dropped = match (&first.kind, &second.kind) {
        (_, LifecycleKind::EventFloodDropped { dropped }) => *dropped,
        (LifecycleKind::EventFloodDropped { dropped }, _) => *dropped,
        _ => panic!(
            "neither broadcast carried EventFloodDropped: {:?}, {:?}",
            first.kind, second.kind
        ),
    };
    assert_eq!(dropped, 3);

    // After the tick the counter is back to zero.
    assert_eq!(dropped_counter::take(), 0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge --test dropped_counter_emission`
Expected: FAIL — `LifecycleProbe::new_for_test` does not exist; `ProbeInputs` does not exist; the tick does not drain the counter.

- [ ] **Step 3: Flesh out `cache/dropped_counter.rs`**

Replace the file with:

```rust
//! Per-process atomic counter for events the EventCache had to drop because
//! the broadcast channel was full.
//!
//! Plan 6 created the static. Plan 7 wires `take()` (atomic swap to zero)
//! so LifecycleProbe::tick can drain it once per tick without double-reporting.

use std::sync::atomic::{AtomicU32, Ordering};

static DROPPED: AtomicU32 = AtomicU32::new(0);

/// Increment the dropped-event counter by 1. Called from the
/// `EventCache::push` `send_timeout` failure branch.
pub fn increment() {
    DROPPED.fetch_add(1, Ordering::AcqRel);
}

/// Atomically read and reset the counter. Returns the prior value.
/// Called once per LifecycleProbe::tick.
pub fn take() -> u32 {
    DROPPED.swap(0, Ordering::AcqRel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn take_returns_prior_value_and_resets() {
        let _ = take(); // ensure clean slate

        increment();
        increment();
        increment();
        assert_eq!(take(), 3);
        assert_eq!(take(), 0);
    }

    #[test]
    fn concurrent_increments_are_summed() {
        let _ = take();

        let h1 = thread::spawn(|| {
            for _ in 0..3 {
                increment();
            }
        });
        let h2 = thread::spawn(|| {
            for _ in 0..5 {
                increment();
            }
        });
        h1.join().unwrap();
        h2.join().unwrap();

        assert_eq!(take(), 8);
    }
}
```

- [ ] **Step 4: Wire the increment into `cache.rs`**

In `crates/snitchwatch-bridge/src/cache.rs::EventCache::push`, find the line with the Plan 6 TODO comment:

```rust
// TODO(plan-7): wire here
```

Replace it with:

```rust
crate::cache::dropped_counter::increment();
tracing::warn!(
    dropped_total = crate::cache::dropped_counter::take_peek(),
    "event cache drop due to send_timeout"
);
```

(`take_peek` is a non-mutating read used only for the warning log; it's a 1-line helper that returns `DROPPED.load(Ordering::Acquire)`. Add it to `dropped_counter.rs` right after `take()`.)

- [ ] **Step 5: Add `LifecycleProbe::new_for_test` and `ProbeInputs`**

In `crates/snitchwatch-bridge/src/lifecycle.rs`, add:

```rust
/// Test-only injection point so unit tests can stub the gRPC ping,
/// systemctl probe, journalctl scrape, and reconciliation-diff hook
/// without spawning real subprocesses.
pub struct ProbeInputs {
    pub grpc_ping: Box<
        dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), GrpcError>> + Send>>
            + Send
            + Sync,
    >,
    pub systemctl_state: Box<
        dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, std::io::Error>> + Send>>
            + Send
            + Sync,
    >,
    pub journalctl_scrape: Box<dyn Fn() -> Option<String> + Send + Sync>,
    pub reconciliation_diff: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
    pub broadcast: tokio::sync::broadcast::Sender<LifecycleEvent>,
}

impl LifecycleProbe {
    #[doc(hidden)]
    pub fn new_for_test(inputs: ProbeInputs) -> Self {
        Self {
            grpc_ping: inputs.grpc_ping,
            systemctl_state: inputs.systemctl_state,
            journalctl_scrape: inputs.journalctl_scrape,
            reconciliation_diff: inputs.reconciliation_diff,
            broadcast: inputs.broadcast,
            last_kind: std::sync::Mutex::new(LifecycleKind::DaemonOk),
        }
    }

    pub async fn tick(&self) {
        // 1. Probe gRPC + systemctl state.
        let grpc_ok = (self.grpc_ping)().await.is_ok();
        let unit_state = (self.systemctl_state)()
            .await
            .unwrap_or_else(|_| "unknown".into());

        let primary_kind = crate::lifecycle::probe_state::derive(grpc_ok, &unit_state);

        // 2. Broadcast the primary kind.
        let _ = self.broadcast.send(LifecycleEvent {
            kind: primary_kind.clone(),
            severity: primary_kind.severity(),
        });

        // 3. Drain the dropped-event counter (Plan 7 Task 11).
        let dropped = crate::cache::dropped_counter::take();
        if dropped > 0 {
            let _ = self.broadcast.send(LifecycleEvent {
                kind: LifecycleKind::EventFloodDropped { dropped },
                severity: LifecycleSeverity::Warning,
            });
        }

        // 4. If the prior tick was already GrpcUnreachable, run the
        //    journalctl scrape (Plan 7 Task 13).
        let was_grpc_down = matches!(
            *self.last_kind.lock().unwrap(),
            LifecycleKind::GrpcUnreachable
        );
        if was_grpc_down {
            if let Some(excerpt) = (self.journalctl_scrape)() {
                let _ = self.broadcast.send(LifecycleEvent {
                    kind: LifecycleKind::KernelHookFailed { excerpt },
                    severity: LifecycleSeverity::Error,
                });
            }
        }

        // 5. If the reconciliation-diff hook returns true, broadcast
        //    StateDivergenceReconciled (Plan 7 Task 14).
        if (self.reconciliation_diff)() {
            let _ = self.broadcast.send(LifecycleEvent {
                kind: LifecycleKind::StateDivergenceReconciled,
                severity: LifecycleSeverity::Info,
            });
        }

        // 6. Stash for the next tick.
        *self.last_kind.lock().unwrap() = primary_kind;
    }
}
```

The `LifecycleProbe` struct gains corresponding fields and the production constructor (`LifecycleProbe::new(grpc_client, broadcast)`) wraps them with the real implementations.

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge --test dropped_counter_emission`
Expected: PASS

- [ ] **Step 7: Run the rest of the bridge test suite to confirm no regression**

Run: `cargo test -p snitchwatch-bridge`
Expected: PASS — every existing test still green.

- [ ] **Step 8: Commit**

```bash
git add crates/snitchwatch-bridge/src/cache.rs \
        crates/snitchwatch-bridge/src/cache/dropped_counter.rs \
        crates/snitchwatch-bridge/src/lifecycle.rs \
        crates/snitchwatch-bridge/tests/dropped_counter_emission.rs
git commit -m "$(cat <<'EOF'
feat(bridge): wire EventFloodDropped emission

Drains the dropped-event counter once per LifecycleProbe::tick and
broadcasts LifecycleKind::EventFloodDropped { dropped } when non-zero.
Also adds the test-only LifecycleProbe::new_for_test injection so the
four lifecycle emission paths can be unit-tested without subprocesses.

Closes the Plan 6 TODO in cache::EventCache::push.
EOF
)"
```

---

### Task 12: Wire `KernelHookFailed` via journalctl scrape

**Files:**
- Create: `crates/snitchwatch-bridge/src/lifecycle/journalctl.rs`
- Create: `crates/snitchwatch-bridge/src/lifecycle/journalctl/parse.rs`
- Modify: `crates/snitchwatch-bridge/src/lifecycle.rs`

The probe already has the *plumbing* (Task 11 added the `journalctl_scrape` injection point). Task 12 implements the real scraper.

- [ ] **Step 1: Write the failing test for the parser**

Create `crates/snitchwatch-bridge/src/lifecycle/journalctl/parse.rs`:

```rust
//! Pure-function scanner: given a journalctl stdout blob, return the
//! first line that matches a known kernel-hook failure marker, capped
//! at 200 characters. No I/O — fully unit-testable.

const MARKERS: &[&str] = &[
    "Failed to load eBPF",
    "nfqueue: failed",
    "module not found",
    "permission denied",
    "operation not permitted",
];

const MAX_EXCERPT_CHARS: usize = 200;

pub fn find_marker(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        for marker in MARKERS {
            if line.contains(marker) {
                let trimmed: String = line.chars().take(MAX_EXCERPT_CHARS).collect();
                return Some(trimmed);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_none_on_empty_input() {
        assert_eq!(find_marker(""), None);
    }

    #[test]
    fn returns_none_when_no_marker_matches() {
        assert_eq!(
            find_marker("Apr 11 00:00:00 host opensnitchd[1]: nothing to see here"),
            None
        );
    }

    #[test]
    fn returns_the_first_matching_line() {
        let blob = "\
Apr 11 00:00:00 host opensnitchd[1]: starting up
Apr 11 00:00:01 host opensnitchd[1]: Failed to load eBPF program: invalid kernel
Apr 11 00:00:02 host opensnitchd[1]: nfqueue: failed to bind queue 0
";
        assert_eq!(
            find_marker(blob).unwrap(),
            "Apr 11 00:00:01 host opensnitchd[1]: Failed to load eBPF program: invalid kernel"
        );
    }

    #[test]
    fn truncates_excerpt_to_200_chars() {
        let long = format!("Failed to load eBPF: {}", "x".repeat(500));
        let blob = format!("Apr 11 00:00:00 host opensnitchd[1]: {long}");
        let result = find_marker(&blob).unwrap();
        assert_eq!(result.chars().count(), 200);
    }

    #[test]
    fn matches_permission_denied_marker() {
        let blob = "Apr 11 00:00:00 host opensnitchd[1]: permission denied opening /proc";
        assert!(find_marker(blob).is_some());
    }
}
```

- [ ] **Step 2: Run the parser tests to verify they pass**

Run: `cargo test -p snitchwatch-bridge lifecycle::journalctl::parse`
Expected: PASS — `find_marker` is a pure function and the tests are self-contained.

- [ ] **Step 3: Write the failing scraper test**

Create `crates/snitchwatch-bridge/src/lifecycle/journalctl.rs`:

```rust
//! Wraps `journalctl --user -u snitchwatch-opensnitchd.service -n 50 --no-pager`,
//! pipes the stdout into `parse::find_marker`, returns the matching line.
//!
//! Returns None if `journalctl` is missing on PATH, the unit does not exist,
//! or no marker matches in the last 50 lines.

pub mod parse;

use std::process::Command;

const UNIT: &str = "snitchwatch-opensnitchd.service";
const TAIL: &str = "50";

pub fn scrape_kernel_hook_failure() -> Option<String> {
    scrape_with(|| {
        let output = Command::new("journalctl")
            .args(["--user", "-u", UNIT, "-n", TAIL, "--no-pager"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    })
}

/// Test-only injection: pass in a closure that produces the journalctl
/// stdout (or None if unavailable), get back the parsed marker.
pub fn scrape_with<F>(provider: F) -> Option<String>
where
    F: FnOnce() -> Option<String>,
{
    let stdout = provider()?;
    parse::find_marker(&stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_none_when_provider_returns_none() {
        let result = scrape_with(|| None);
        assert!(result.is_none());
    }

    #[test]
    fn returns_none_when_provider_returns_unrelated_log() {
        let result = scrape_with(|| Some("nothing interesting here".into()));
        assert!(result.is_none());
    }

    #[test]
    fn returns_marker_when_provider_returns_matching_log() {
        let stdout = "\
Apr 11 host opensnitchd[1]: starting
Apr 11 host opensnitchd[1]: Failed to load eBPF program: bad kernel
";
        let result = scrape_with(|| Some(stdout.into())).expect("must match");
        assert!(result.contains("Failed to load eBPF"));
    }
}
```

- [ ] **Step 4: Run the scraper tests**

Run: `cargo test -p snitchwatch-bridge lifecycle::journalctl`
Expected: PASS

- [ ] **Step 5: Wire `pub mod lifecycle::journalctl` in lib.rs**

In `crates/snitchwatch-bridge/src/lifecycle.rs` (top of file), add:

```rust
pub mod journalctl;
```

- [ ] **Step 6: Wire the production constructor**

In `LifecycleProbe::new` (the production-not-test constructor), pass:

```rust
journalctl_scrape: Box::new(|| crate::lifecycle::journalctl::scrape_kernel_hook_failure()),
```

- [ ] **Step 7: Add an integration test that walks the GrpcUnreachable → KernelHookFailed transition**

Append to `crates/snitchwatch-bridge/tests/dropped_counter_emission.rs` (which already has the test infrastructure):

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kernel_hook_failed_emits_after_grpc_unreachable_tick() {
    let _ = snitchwatch_bridge::cache::dropped_counter::take();

    let (tx, mut rx) = tokio::sync::broadcast::channel(16);
    let probe = snitchwatch_bridge::lifecycle::LifecycleProbe::new_for_test(
        snitchwatch_bridge::lifecycle::ProbeInputs {
            grpc_ping: Box::new(|| {
                Box::pin(async {
                    Err(snitchwatch_bridge::grpc_client::GrpcError::Unreachable)
                })
            }),
            systemctl_state: Box::new(|| Box::pin(async { Ok("active".into()) })),
            journalctl_scrape: Box::new(|| Some("Failed to load eBPF: bad kernel".into())),
            reconciliation_diff: std::sync::Arc::new(|| false),
            broadcast: tx,
        },
    );

    // First tick: grpc unreachable, last_kind transitions to GrpcUnreachable.
    probe.tick().await;
    let _ = rx.try_recv();

    // Second tick: was_grpc_down == true, so journalctl scrape fires.
    probe.tick().await;
    // Drain the GrpcUnreachable broadcast from this tick first.
    let _first = rx.try_recv().unwrap();
    let second = rx.try_recv().unwrap();

    match second.kind {
        snitchwatch_bridge::lifecycle::LifecycleKind::KernelHookFailed { excerpt } => {
            assert!(excerpt.contains("Failed to load eBPF"));
        }
        other => panic!("expected KernelHookFailed, got {:?}", other),
    }
}
```

- [ ] **Step 8: Run the new test**

Run: `cargo test -p snitchwatch-bridge --test dropped_counter_emission kernel_hook_failed_emits_after_grpc_unreachable_tick`
Expected: PASS

- [ ] **Step 9: Commit**

```bash
git add crates/snitchwatch-bridge/src/lifecycle.rs \
        crates/snitchwatch-bridge/src/lifecycle/journalctl.rs \
        crates/snitchwatch-bridge/src/lifecycle/journalctl/parse.rs \
        crates/snitchwatch-bridge/tests/dropped_counter_emission.rs
git commit -m "$(cat <<'EOF'
feat(bridge): wire KernelHookFailed via journalctl scrape

Adds lifecycle/journalctl.rs (subprocess-injected wrapper) and
lifecycle/journalctl/parse.rs (pure marker scanner). The probe
runs the scrape only when the prior tick was already
GrpcUnreachable, so we don't pay the journalctl cost on every
healthy tick.

Five marker patterns covered: Failed to load eBPF, nfqueue: failed,
module not found, permission denied, operation not permitted.
EOF
)"
```

---

### Task 13: Wire `StateDivergenceReconciled` via the existing 60s reconciliation loop

**Files:**
- Modify: `crates/snitchwatch-bridge/src/grpc_client.rs`

The reconciliation loop has existed since Plan 2 — it replays the daemon's snapshot every 60 seconds and logs any diff. Plan 7 surfaces the diff as a lifecycle broadcast.

- [ ] **Step 1: Write the failing test**

Append to `crates/snitchwatch-bridge/tests/dropped_counter_emission.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_divergence_reconciled_emits_when_diff_nonempty() {
    let _ = snitchwatch_bridge::cache::dropped_counter::take();

    let (tx, mut rx) = tokio::sync::broadcast::channel(16);
    let saw_diff = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let saw_diff_for_closure = saw_diff.clone();

    let probe = snitchwatch_bridge::lifecycle::LifecycleProbe::new_for_test(
        snitchwatch_bridge::lifecycle::ProbeInputs {
            grpc_ping: Box::new(|| Box::pin(async { Ok(()) })),
            systemctl_state: Box::new(|| Box::pin(async { Ok("active".into()) })),
            journalctl_scrape: Box::new(|| None),
            reconciliation_diff: std::sync::Arc::new(move || {
                saw_diff_for_closure.swap(false, std::sync::atomic::Ordering::AcqRel)
            }),
            broadcast: tx,
        },
    );

    probe.tick().await;
    let _ok = rx.try_recv().unwrap(); // DaemonOk
    let div = rx.try_recv().unwrap(); // StateDivergenceReconciled

    match div.kind {
        snitchwatch_bridge::lifecycle::LifecycleKind::StateDivergenceReconciled => {}
        other => panic!("expected StateDivergenceReconciled, got {:?}", other),
    }

    // Second tick: the diff hook now returns false, no extra broadcast.
    probe.tick().await;
    let _ok2 = rx.try_recv().unwrap();
    assert!(rx.try_recv().is_err(), "second tick must not re-emit");
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p snitchwatch-bridge --test dropped_counter_emission state_divergence_reconciled_emits_when_diff_nonempty`
Expected: PASS — Task 11 already wired the `reconciliation_diff` branch into `tick`. This test confirms it. If it fails, fix the tick branch ordering.

- [ ] **Step 3: Wire the production reconciliation_diff into `LifecycleProbe::new`**

In `crates/snitchwatch-bridge/src/grpc_client.rs`, the existing reconciliation loop currently looks like (search for `reconcile_snapshot`):

```rust
async fn reconciliation_loop(self: Arc<Self>) {
    let mut ticker = tokio::time::interval(Duration::from_secs(60));
    loop {
        ticker.tick().await;
        match self.reconcile_snapshot().await {
            Ok(diff) if !diff.is_empty() => {
                tracing::warn!(?diff, "state diverged, reconciled");
                // TODO(plan-7): broadcast StateDivergenceReconciled
            }
            Ok(_) => tracing::trace!("state still in sync"),
            Err(e) => tracing::warn!(error = ?e, "reconciliation failed"),
        }
    }
}
```

Add a `last_diff_seen: Arc<AtomicBool>` to `GrpcClient` and set it in the `Ok(diff) if !diff.is_empty()` branch:

```rust
Ok(diff) if !diff.is_empty() => {
    tracing::warn!(?diff, "state diverged, reconciled");
    self.last_diff_seen.store(true, Ordering::Release);
}
```

Expose a getter:

```rust
pub fn take_last_diff_seen(&self) -> bool {
    self.last_diff_seen.swap(false, Ordering::AcqRel)
}
```

In `LifecycleProbe::new` production constructor:

```rust
let grpc_clone = grpc_client.clone();
reconciliation_diff: Arc::new(move || grpc_clone.take_last_diff_seen()),
```

- [ ] **Step 4: Run the bridge suite**

Run: `cargo test -p snitchwatch-bridge`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/grpc_client.rs \
        crates/snitchwatch-bridge/tests/dropped_counter_emission.rs
git commit -m "$(cat <<'EOF'
feat(bridge): wire StateDivergenceReconciled into the reconciliation loop

The 60s loop has logged diffs since Plan 2 but never surfaced them
to the UI. This adds an AtomicBool that the loop sets on a non-empty
diff and the LifecycleProbe drains once per tick. Closes the Plan 6
TODO comment in grpc_client.rs::reconciliation_loop.
EOF
)"
```

---

### Task 14: Wire `BridgePanicRecovered` via the Tauri panic hook

**Files:**
- Create: `crates/snitchwatch-tauri/src/panic_hook.rs`
- Modify: `crates/snitchwatch-tauri/src/lib.rs`
- Modify: `crates/snitchwatch-tauri/src/main.rs`
- Create: `crates/snitchwatch-tauri/tests/panic_hook_test.rs`

The bridge runtime task is supervised — if it panics, the supervisor restarts it. Plan 7 surfaces the restart as `LifecycleKind::BridgePanicRecovered`.

- [ ] **Step 1: Write the failing test**

Create `crates/snitchwatch-tauri/tests/panic_hook_test.rs`:

```rust
//! Installing the Tauri panic hook should:
//!   1. capture panic info into the LifecyclePanicChannel
//!   2. NOT replace the default panic handler entirely (it chains)
//!   3. broadcast on the next supervisor tick
//!
//! This test installs a synthetic panic-and-broadcast pipeline
//! without actually crashing the test runner.

use snitchwatch_tauri::panic_hook::{install, LifecyclePanicChannel};
use std::sync::Arc;
use tokio::sync::broadcast;

#[tokio::test]
async fn panic_hook_broadcasts_on_panic_message() {
    let (tx, mut rx) = broadcast::channel::<String>(4);
    let channel = Arc::new(LifecyclePanicChannel::new(tx));

    install(channel.clone());

    // Simulate a panic without unwinding the test thread by calling
    // the channel's notify directly — install() guarantees the hook
    // routes panics through this same path.
    channel.notify("synthetic panic at test".into());

    let received = rx.recv().await.expect("must receive a panic notification");
    assert!(received.contains("synthetic panic at test"));
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p snitchwatch-tauri --test panic_hook_test`
Expected: FAIL — `panic_hook` module does not exist.

- [ ] **Step 3: Create panic_hook.rs**

```rust
//! Routes std::panic info into a broadcast channel that the bridge
//! lifecycle layer drains. The Tauri shell installs this hook on
//! startup; the bridge supervisor task subscribes.
//!
//! Plan 7 Task 14.

use std::panic;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Wraps a broadcast::Sender<String> so we can clone it into the
/// panic hook closure (which must be Send + Sync + 'static).
pub struct LifecyclePanicChannel {
    tx: broadcast::Sender<String>,
}

impl LifecyclePanicChannel {
    pub fn new(tx: broadcast::Sender<String>) -> Self {
        Self { tx }
    }

    pub fn notify(&self, info: String) {
        // We deliberately swallow the SendError — if no subscribers
        // are listening (which is true during very early shutdown)
        // there is nothing to do.
        let _ = self.tx.send(info);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }
}

/// Install a panic hook that chains the prior hook (so the default
/// abort/backtrace behavior is preserved) and ALSO routes panic info
/// into the LifecyclePanicChannel.
pub fn install(channel: Arc<LifecyclePanicChannel>) {
    let prior = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let payload = format!("{info}");
        channel.notify(payload);
        prior(info);
    }));
}
```

- [ ] **Step 4: Wire `pub mod panic_hook` in `crates/snitchwatch-tauri/src/lib.rs`**

```rust
pub mod panic_hook;
```

- [ ] **Step 5: Wire the install + bridge subscriber in `main.rs`**

In `crates/snitchwatch-tauri/src/main.rs`, immediately after constructing the bridge runtime:

```rust
let (panic_tx, mut panic_rx) = tokio::sync::broadcast::channel::<String>(8);
let panic_channel = std::sync::Arc::new(
    snitchwatch_tauri::panic_hook::LifecyclePanicChannel::new(panic_tx),
);
snitchwatch_tauri::panic_hook::install(panic_channel.clone());

// Subscriber: drain panic notifications into the lifecycle broadcast.
let lifecycle_tx_for_panic = lifecycle_tx.clone();
tokio::spawn(async move {
    while let Ok(payload) = panic_rx.recv().await {
        let _ = lifecycle_tx_for_panic.send(snitchwatch_bridge::lifecycle::LifecycleEvent {
            kind: snitchwatch_bridge::lifecycle::LifecycleKind::BridgePanicRecovered,
            severity: snitchwatch_bridge::lifecycle::LifecycleSeverity::Error,
        });
        tracing::error!(payload, "bridge runtime panic — recovered");
    }
});
```

- [ ] **Step 6: Run the test**

Run: `cargo test -p snitchwatch-tauri --test panic_hook_test`
Expected: PASS

- [ ] **Step 7: Run the full Tauri suite**

Run: `cargo test -p snitchwatch-tauri`
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add crates/snitchwatch-tauri/src/panic_hook.rs \
        crates/snitchwatch-tauri/src/lib.rs \
        crates/snitchwatch-tauri/src/main.rs \
        crates/snitchwatch-tauri/tests/panic_hook_test.rs
git commit -m "$(cat <<'EOF'
feat(tauri): wire BridgePanicRecovered via std::panic hook

Installs a chained panic hook that routes panic info into a
broadcast channel. A spawned task drains the channel and emits
LifecycleKind::BridgePanicRecovered on the lifecycle broadcast,
which the web UI renders as a "Bridge crashed and restarted"
banner with an "Open log" action button.

Closes the last of the four Plan 6 stub LifecycleKinds.
EOF
)"
```

---

## Part E — Diagnostic bundle + manual smoke (Tasks 15–17)

The "Copy Diagnostics" affordance in the About panel is the user's escape hatch
when something is wrong: one click bundles the bridge log tail, the lifecycle
kind history, the build version triple, the rustc version, and the host
release info into a gzipped tarball under `$XDG_RUNTIME_DIR` and copies the
absolute path to the clipboard so they can paste it into a bug report. Pure
tar-builder logic lives in a sibling module that takes a `&[BundleEntry]` so
it can be unit-tested without touching the filesystem.

### Task 15: Diagnostic bundle backend

**Files:**
- Create: `crates/snitchwatch-tauri/src/diagnostics.rs`
- Create: `crates/snitchwatch-tauri/src/diagnostics/bundle.rs`
- Create: `crates/snitchwatch-tauri/src/diagnostics/version_info.rs`
- Create: `crates/snitchwatch-tauri/build.rs`
- Modify: `crates/snitchwatch-tauri/Cargo.toml`
- Modify: `crates/snitchwatch-tauri/src/lib.rs`
- Test: `crates/snitchwatch-tauri/tests/diagnostics_bundle_test.rs`

- [ ] **Step 1: Add dependencies and build script to Cargo.toml**

Edit `crates/snitchwatch-tauri/Cargo.toml`, append to `[dependencies]`:

```toml
tar = "0.4"
flate2 = "1.0"
tauri-plugin-clipboard-manager = "2"
chrono = { version = "0.4", default-features = false, features = ["clock"] }
```

And add:

```toml
[build-dependencies]
```

- [ ] **Step 2: Write the failing bundle test**

Create `crates/snitchwatch-tauri/tests/diagnostics_bundle_test.rs`:

```rust
use snitchwatch_tauri::diagnostics::bundle::{build_tar_gz, BundleEntry};
use flate2::read::GzDecoder;
use tar::Archive;
use std::io::Read;

#[test]
fn build_tar_gz_round_trips_three_named_entries() {
    let entries = vec![
        BundleEntry { name: "version.txt".into(), bytes: b"snitchwatch 0.1.0".to_vec() },
        BundleEntry { name: "lifecycle.log".into(), bytes: b"GrpcUnreachable\nGrpcReconnected\n".to_vec() },
        BundleEntry { name: "bridge.log".into(), bytes: b"WS_LISTEN_ADDR=127.0.0.1:55555\n".to_vec() },
    ];

    let bytes = build_tar_gz(&entries).expect("build_tar_gz");
    assert!(bytes.len() > 30, "expected non-trivial gzip output, got {}", bytes.len());

    let dec = GzDecoder::new(&bytes[..]);
    let mut ar = Archive::new(dec);
    let mut found: Vec<(String, String)> = Vec::new();
    for entry in ar.entries().expect("entries") {
        let mut entry = entry.expect("entry");
        let name = entry.path().expect("path").to_string_lossy().into_owned();
        let mut buf = String::new();
        entry.read_to_string(&mut buf).expect("read");
        found.push((name, buf));
    }

    assert_eq!(found.len(), 3);
    assert_eq!(found[0].0, "version.txt");
    assert!(found[0].1.starts_with("snitchwatch "));
    assert_eq!(found[1].0, "lifecycle.log");
    assert!(found[1].1.contains("GrpcUnreachable"));
    assert_eq!(found[2].0, "bridge.log");
    assert!(found[2].1.contains("WS_LISTEN_ADDR="));
}

#[test]
fn build_tar_gz_rejects_empty_entries() {
    let err = build_tar_gz(&[]).unwrap_err();
    assert!(err.to_string().contains("at least one entry"));
}
```

- [ ] **Step 3: Run the test (expect failure)**

Run: `cargo test -p snitchwatch-tauri --test diagnostics_bundle_test`
Expected: FAIL — `build_tar_gz` does not exist.

- [ ] **Step 4: Implement bundle.rs**

Create `crates/snitchwatch-tauri/src/diagnostics/bundle.rs`:

```rust
//! Pure tar+gzip builder for the diagnostic bundle.
//!
//! Lives in its own module so it can be unit tested without
//! touching the filesystem or the Tauri runtime.

use flate2::write::GzEncoder;
use flate2::Compression;
use std::io::{Error, ErrorKind, Write};
use tar::{Builder, Header};

#[derive(Debug, Clone)]
pub struct BundleEntry {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Build a gzipped tar archive in memory from the supplied entries.
///
/// Returns `InvalidInput` if `entries` is empty so the UI never copies
/// an empty diagnostic file path to the clipboard.
pub fn build_tar_gz(entries: &[BundleEntry]) -> std::io::Result<Vec<u8>> {
    if entries.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "build_tar_gz requires at least one entry",
        ));
    }

    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut tar = Builder::new(&mut gz);
        for entry in entries {
            let mut header = Header::new_gnu();
            header.set_size(entry.bytes.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0); // deterministic for test reproducibility
            header.set_cksum();
            tar.append_data(&mut header, &entry.name, entry.bytes.as_slice())?;
        }
        tar.finish()?;
    }
    let buf = gz.finish()?;
    Ok(buf)
}
```

- [ ] **Step 5: Implement version_info.rs**

Create `crates/snitchwatch-tauri/src/diagnostics/version_info.rs`:

```rust
//! Compile-time version triple injected via build.rs.

pub const SNITCHWATCH_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const RUSTC_VERSION: &str = env!("SNITCHWATCH_RUSTC_VERSION");
pub const BUILD_PROFILE: &str = env!("SNITCHWATCH_BUILD_PROFILE");

pub fn render_plain() -> String {
    format!(
        "snitchwatch {snitchwatch}\nrustc {rustc}\nprofile {profile}\n",
        snitchwatch = SNITCHWATCH_VERSION,
        rustc = RUSTC_VERSION,
        profile = BUILD_PROFILE,
    )
}
```

- [ ] **Step 6: Implement build.rs**

Create `crates/snitchwatch-tauri/build.rs`:

```rust
//! Inject rustc version and build profile into compile-time env vars.

use std::process::Command;

fn main() {
    let rustc_version = Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "rustc unknown".to_string());

    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string());

    println!("cargo:rustc-env=SNITCHWATCH_RUSTC_VERSION={rustc_version}");
    println!("cargo:rustc-env=SNITCHWATCH_BUILD_PROFILE={profile}");
    println!("cargo:rerun-if-changed=build.rs");
}
```

- [ ] **Step 7: Implement diagnostics.rs (Tauri command)**

Create `crates/snitchwatch-tauri/src/diagnostics.rs`:

```rust
//! Tauri command surface for the "Copy Diagnostics" button.

pub mod bundle;
pub mod version_info;

use bundle::{build_tar_gz, BundleEntry};
use std::path::PathBuf;
use tauri::Manager;
use tauri_plugin_clipboard_manager::ClipboardExt;

const MAX_LOG_TAIL_BYTES: usize = 256 * 1024;

/// The Tauri command invoked from the web UI's About panel.
///
/// Reads the bridge log tail, the lifecycle history snapshot, and the
/// version triple, packs them into a gzipped tarball under
/// `$XDG_RUNTIME_DIR`, copies the absolute path to the system clipboard,
/// and returns it to the caller for the toast.
#[tauri::command]
pub async fn copy_diagnostic_bundle(app: tauri::AppHandle) -> Result<String, String> {
    let entries = collect_entries().map_err(|e| format!("collect: {e}"))?;
    let bytes = build_tar_gz(&entries).map_err(|e| format!("tar: {e}"))?;

    let path = runtime_bundle_path();
    std::fs::write(&path, &bytes).map_err(|e| format!("write {}: {e}", path.display()))?;

    let path_str = path.to_string_lossy().into_owned();
    app.clipboard()
        .write_text(path_str.clone())
        .map_err(|e| format!("clipboard: {e}"))?;

    Ok(path_str)
}

fn runtime_bundle_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    dir.join(format!("snitchwatch-diag-{ts}.tar.gz"))
}

fn collect_entries() -> std::io::Result<Vec<BundleEntry>> {
    let mut out = Vec::new();
    out.push(BundleEntry {
        name: "version.txt".into(),
        bytes: version_info::render_plain().into_bytes(),
    });

    let log_path = bridge_log_path();
    if let Ok(bytes) = read_tail(&log_path, MAX_LOG_TAIL_BYTES) {
        out.push(BundleEntry { name: "bridge.log".into(), bytes });
    }

    let lifecycle_path = lifecycle_log_path();
    if let Ok(bytes) = std::fs::read(&lifecycle_path) {
        out.push(BundleEntry { name: "lifecycle.log".into(), bytes });
    }

    Ok(out)
}

fn bridge_log_path() -> PathBuf {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            home.join(".cache")
        });
    cache.join("snitchwatch").join("bridge.log")
}

fn lifecycle_log_path() -> PathBuf {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            home.join(".cache")
        });
    cache.join("snitchwatch").join("lifecycle.log")
}

fn read_tail(path: &PathBuf, max_bytes: usize) -> std::io::Result<Vec<u8>> {
    let bytes = std::fs::read(path)?;
    if bytes.len() <= max_bytes {
        Ok(bytes)
    } else {
        Ok(bytes[bytes.len() - max_bytes..].to_vec())
    }
}
```

- [ ] **Step 8: Wire diagnostics.rs into lib.rs**

Edit `crates/snitchwatch-tauri/src/lib.rs`, add at top:

```rust
pub mod diagnostics;
```

And in the Tauri builder setup (the `tauri::Builder::default()` chain), add:

```rust
.plugin(tauri_plugin_clipboard_manager::init())
.invoke_handler(tauri::generate_handler![
    diagnostics::copy_diagnostic_bundle,
])
```

(If an existing `invoke_handler!` macro is already present, append `diagnostics::copy_diagnostic_bundle` to its argument list rather than calling `.invoke_handler` twice.)

- [ ] **Step 9: Run the bundle test**

Run: `cargo test -p snitchwatch-tauri --test diagnostics_bundle_test`
Expected: PASS — both `build_tar_gz_round_trips_three_named_entries` and `build_tar_gz_rejects_empty_entries`.

- [ ] **Step 10: Run the full Tauri suite + clippy**

Run: `cargo test -p snitchwatch-tauri && cargo clippy -p snitchwatch-tauri -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 11: Commit**

```bash
git add crates/snitchwatch-tauri/src/diagnostics.rs \
        crates/snitchwatch-tauri/src/diagnostics/bundle.rs \
        crates/snitchwatch-tauri/src/diagnostics/version_info.rs \
        crates/snitchwatch-tauri/build.rs \
        crates/snitchwatch-tauri/Cargo.toml \
        crates/snitchwatch-tauri/src/lib.rs \
        crates/snitchwatch-tauri/tests/diagnostics_bundle_test.rs
git commit -m "$(cat <<'EOF'
feat(tauri): copy_diagnostic_bundle Tauri command

Bundles version triple, last 256 KiB of bridge.log, and the
lifecycle history snapshot into a deterministic gzipped tar
under $XDG_RUNTIME_DIR. Copies the resulting absolute path to
the system clipboard via tauri-plugin-clipboard-manager so the
user can paste it into a bug report.

Tar builder lives in diagnostics/bundle.rs as a pure function
over &[BundleEntry] for fast unit testing without touching
the filesystem or the Tauri runtime.
EOF
)"
```

---

### Task 16: Diagnostic bundle web UI

**Files:**
- Create: `web/diagnostics.js`
- Create: `web/diagnostics.css`
- Modify: `web/index.html`
- Test: `tests/integration/tests/diagnostics_button_shape.rs`

- [ ] **Step 1: Write the failing shape test**

Create `tests/integration/tests/diagnostics_button_shape.rs`:

```rust
//! Lints the About panel markup so the diagnostics button stays present.

use std::fs;
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn index_html_contains_diagnostics_button() {
    let html = fs::read_to_string(workspace_root().join("web/index.html"))
        .expect("read web/index.html");
    assert!(
        html.contains("id=\"diagnostics-copy\""),
        "expected About panel to host id=\"diagnostics-copy\" button"
    );
    assert!(
        html.contains("Copy Diagnostics"),
        "expected About panel to label the button \"Copy Diagnostics\""
    );
}

#[test]
fn diagnostics_js_calls_copy_diagnostic_bundle_command() {
    let js = fs::read_to_string(workspace_root().join("web/diagnostics.js"))
        .expect("read web/diagnostics.js");
    assert!(
        js.contains("copy_diagnostic_bundle"),
        "diagnostics.js must invoke the Tauri command copy_diagnostic_bundle"
    );
    assert!(
        js.contains("diagnostics-copy"),
        "diagnostics.js must bind a click handler on #diagnostics-copy"
    );
}
```

- [ ] **Step 2: Run the test (expect failure)**

Run: `cargo test -p integration --test diagnostics_button_shape`
Expected: FAIL — `web/diagnostics.js` does not exist; About panel has no `diagnostics-copy` id.

- [ ] **Step 3: Create web/diagnostics.js**

Create `web/diagnostics.js`:

```javascript
// Wires the "Copy Diagnostics" button in the About panel to the
// Tauri command copy_diagnostic_bundle. Shows a transient toast
// with the resulting absolute path so the user has visible
// confirmation that the clipboard was populated.

import { invoke } from "@tauri-apps/api/core";

const SELECTOR = "#diagnostics-copy";
const TOAST_MS = 4000;

export function bindDiagnosticsButton(root = document) {
  const button = root.querySelector(SELECTOR);
  if (!button) {
    return;
  }
  button.addEventListener("click", async () => {
    button.disabled = true;
    button.dataset.state = "busy";
    try {
      const path = await invoke("copy_diagnostic_bundle");
      showToast(`Copied diagnostics path to clipboard: ${path}`);
    } catch (err) {
      showToast(`Failed to copy diagnostics: ${err}`, /* error */ true);
    } finally {
      button.disabled = false;
      delete button.dataset.state;
    }
  });
}

function showToast(message, isError = false) {
  const toast = document.createElement("div");
  toast.className = `diagnostics-toast${isError ? " diagnostics-toast--error" : ""}`;
  toast.textContent = message;
  document.body.appendChild(toast);
  setTimeout(() => toast.remove(), TOAST_MS);
}

document.addEventListener("DOMContentLoaded", () => bindDiagnosticsButton());
```

- [ ] **Step 4: Create web/diagnostics.css**

Create `web/diagnostics.css`:

```css
/* Diagnostics copy button + toast styling. */

#diagnostics-copy {
  display: inline-flex;
  align-items: center;
  gap: 0.5rem;
  padding: 0.5rem 0.85rem;
  border: 1px solid var(--ls-border, #3a3a3a);
  border-radius: 6px;
  background: var(--ls-surface, #1f1f1f);
  color: var(--ls-text, #f0f0f0);
  font: inherit;
  cursor: pointer;
  transition: background 120ms ease;
}

#diagnostics-copy:hover {
  background: var(--ls-surface-hover, #2a2a2a);
}

#diagnostics-copy[data-state="busy"] {
  cursor: progress;
  opacity: 0.6;
}

.diagnostics-toast {
  position: fixed;
  bottom: 1.5rem;
  left: 50%;
  transform: translateX(-50%);
  padding: 0.75rem 1rem;
  background: var(--ls-surface, #1f1f1f);
  color: var(--ls-text, #f0f0f0);
  border: 1px solid var(--ls-border, #3a3a3a);
  border-radius: 6px;
  box-shadow: 0 6px 18px rgba(0, 0, 0, 0.4);
  font: 0.9rem/1.3 system-ui, sans-serif;
  z-index: 9999;
  max-width: 80vw;
  word-break: break-all;
}

.diagnostics-toast--error {
  border-color: #b03a3a;
  color: #ffb4b4;
}
```

- [ ] **Step 5: Inject the button + script into index.html**

Edit `web/index.html`. Inside the existing About panel section (locate the
`<section id="about-panel">` or equivalent — if no About panel exists yet,
add one inside the main content region), add:

```html
<button type="button" id="diagnostics-copy">
  <svg aria-hidden="true" width="14" height="14" viewBox="0 0 24 24">
    <path d="M16 1H4a2 2 0 0 0-2 2v14h2V3h12V1zm3 4H8a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h11a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2zm0 16H8V7h11v14z" fill="currentColor"/>
  </svg>
  Copy Diagnostics
</button>
```

And in the `<head>`:

```html
<link rel="stylesheet" href="diagnostics.css">
<script type="module" src="diagnostics.js"></script>
```

- [ ] **Step 6: Run the shape test**

Run: `cargo test -p integration --test diagnostics_button_shape`
Expected: PASS — both assertions.

- [ ] **Step 7: Commit**

```bash
git add web/diagnostics.js web/diagnostics.css web/index.html \
        tests/integration/tests/diagnostics_button_shape.rs
git commit -m "$(cat <<'EOF'
feat(web): wire Copy Diagnostics button in About panel

Adds an ES module that binds the existing About panel button
to the copy_diagnostic_bundle Tauri command, with a transient
toast showing the resulting bundle path. Markup-shape lint test
guards the button id and command name so future refactors do
not silently break the wiring.
EOF
)"
```

---

### Task 17: Manual smoke playbook

**Files:**
- Create: `docs/manual-smoke.md`
- Test: `tests/integration/tests/manual_smoke_doc_shape.rs`

- [ ] **Step 1: Write the failing shape test**

Create `tests/integration/tests/manual_smoke_doc_shape.rs`:

```rust
//! Asserts that the manual smoke playbook covers all five
//! release-blocker scenarios. Cheap regression net so we don't
//! ship a doc that omits a known failure mode.

use std::fs;
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn manual_smoke_covers_five_scenarios() {
    let md = fs::read_to_string(workspace_root().join("docs/manual-smoke.md"))
        .expect("read docs/manual-smoke.md");

    for scenario in [
        "Scenario 1: Cold start with daemon missing",
        "Scenario 2: systemd unit inactive then reconnect",
        "Scenario 3: Daemon crash mid-stream",
        "Scenario 4: Blocklist offline",
        "Scenario 5: LAN bind toggle (read-only smoke)",
    ] {
        assert!(
            md.contains(scenario),
            "manual-smoke.md missing required heading: {scenario}"
        );
    }

    assert!(
        md.contains("Expected banner:"),
        "every scenario must declare its expected lifecycle banner"
    );
}
```

- [ ] **Step 2: Run the test (expect failure)**

Run: `cargo test -p integration --test manual_smoke_doc_shape`
Expected: FAIL — file does not exist.

- [ ] **Step 3: Create docs/manual-smoke.md**

Create `docs/manual-smoke.md`:

```markdown
# Snitchwatch Manual Smoke Playbook

> 5-minute pre-release sanity check. Run before tagging any `v*.*.*`.
> The Tauri shell is still pre-alpha; this list is the human-in-the-loop
> safety net for the lifecycle banners that our integration tests cannot
> exercise without root and a real kernel.

## Setup

You need:

- Rootful podman (`podman info | grep -i rootless` should return nothing or `false`)
- The `evilsocket/opensnitch:latest` container image pulled
- A built Tauri shell: `just build && ./target/debug/snitchwatch-tauri`

Each scenario assumes the previous one was cleaned up:

```bash
podman rm -f opensnitchd-dev 2>/dev/null || true
```

## Scenario 1: Cold start with daemon missing

**Goal:** Verify the bridge surfaces a clean GrpcUnreachable banner when
opensnitchd is not running at all.

1. Ensure no opensnitchd container or systemd unit is active.
2. Launch `./target/debug/snitchwatch-tauri`.
3. Watch the lifecycle banner area within 5 seconds.

**Expected banner:** "Bridge cannot reach opensnitchd at 127.0.0.1:50051"
(GrpcUnreachable). The banner persists until the daemon comes up. The pending
rules table is empty. No JS console errors.

## Scenario 2: systemd unit inactive then reconnect

**Goal:** Verify the GrpcUnreachable → GrpcReconnected transition fires
exactly once when the daemon starts after the bridge.

1. With Snitchwatch already running and showing the GrpcUnreachable banner
   from Scenario 1, start the daemon:

   ```bash
   podman run -d --rm \
       --name opensnitchd-dev \
       --privileged --network=host --pid=host \
       --cap-add=NET_ADMIN,SYS_ADMIN,BPF \
       docker.io/evilsocket/opensnitch:latest
   ```

2. Wait up to 10 seconds.

**Expected banner:** GrpcUnreachable banner clears, replaced briefly by a
"Reconnected to opensnitchd" toast (GrpcReconnected) that auto-dismisses
after ~4 seconds. Pending rules table starts populating as soon as the daemon
issues an AskRule.

## Scenario 3: Daemon crash mid-stream

**Goal:** Verify the bridge re-emits GrpcUnreachable cleanly when the daemon
disappears while the WS stream is open.

1. With the daemon running and at least one ask-rule pending in the UI:

   ```bash
   podman kill opensnitchd-dev
   ```

2. Watch the lifecycle banner.

**Expected banner:** GrpcUnreachable returns within 5 seconds. Any pending
rule rows freeze (no spurious errors). Restart the daemon as in Scenario 2 to
verify GrpcReconnected fires again.

## Scenario 4: Blocklist offline

**Goal:** Verify BlocklistRefreshFailed surfaces when the blocklist source
URL is unreachable (offline mode is the proxy for "can't reach upstream").

1. Disable the network adapter (or run inside a podman container with
   `--network=none`).
2. Launch Snitchwatch.
3. Trigger a blocklist refresh from the Settings → Blocklists pane.

**Expected banner:** "Failed to refresh blocklist <name>"
(BlocklistRefreshFailed) with a "Retry" action button. The previous
blocklist contents remain in effect — no rules are removed.

## Scenario 5: LAN bind toggle (read-only smoke)

**Goal:** Verify the LAN bind toggle is present, defaults off, and shows
the v2 warning when toggled — without actually binding to the LAN, since
that pathway is deferred.

1. Open Settings → Bridge.
2. Locate the "Bind WebSocket on LAN" toggle.
3. Toggle it on.

**Expected banner:** A modal warning: "LAN bind requires TLS + token auth
(deferred to v2). Snitchwatch will keep the bridge on 127.0.0.1." The
toggle reverts to off on dismiss. No bridge restart, no port change in the
status bar.

## After all five scenarios

1. Click **Copy Diagnostics** in the About panel.
2. Confirm the toast shows a path under `$XDG_RUNTIME_DIR/snitchwatch-diag-*`.
3. `tar -tzf <path>` should list `version.txt`, `bridge.log`, `lifecycle.log`.

If any expected banner is missing or wrong, **do not tag the release**. File
a bug with the diagnostic bundle attached.
```

- [ ] **Step 4: Run the shape test**

Run: `cargo test -p integration --test manual_smoke_doc_shape`
Expected: PASS — all five headings + the "Expected banner:" lint match.

- [ ] **Step 5: Commit**

```bash
git add docs/manual-smoke.md tests/integration/tests/manual_smoke_doc_shape.rs
git commit -m "$(cat <<'EOF'
docs: manual smoke playbook for v0.1.0 release

Five release-blocker scenarios covering GrpcUnreachable cold
start, systemd reconnect, mid-stream crash, blocklist refresh
failure, and LAN-bind toggle read-only smoke. Each scenario
declares its expected lifecycle banner so the integration-test
shape lint can mechanically verify the playbook still mentions
every required failure mode.
EOF
)"
```

---

## Part F — Release tag (Task 18)

### Task 18: Bump version, tick milestone, tag v0.1.0

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `CONTRIBUTING.md`
- Modify: `docs/superpowers/specs/2026-04-10-snitchwatch-design.md`
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Bump workspace version**

Edit `Cargo.toml`, update the `[workspace.package]` section:

```toml
[workspace.package]
version = "0.1.0"
edition = "2021"
license = "GPL-2.0-or-later"
repository = "https://github.com/snitchwatch/snitchwatch"
```

(The license string changes from `GPL-2.0` to `GPL-2.0-or-later` to match the LICENSE/NOTICE.md analysis from Task 3.)

- [ ] **Step 2: Replace security@ placeholder in CONTRIBUTING.md**

Edit `CONTRIBUTING.md`. Replace:

```
TODO(plan-7): security@ contact placeholder
```

with the real disclosure block:

```markdown
For security vulnerabilities, please open a private GitHub Security Advisory
on the repository rather than filing a public issue. We will acknowledge
within 7 days and aim to ship a fix or mitigation within 30 days for
high-severity reports.
```

- [ ] **Step 3: Tick M6 in the spec milestone table**

Edit `docs/superpowers/specs/2026-04-10-snitchwatch-design.md`. Locate the milestone table row for M6 (Public release) and change its status column from `[ ]` (or `pending`) to `✅`. Also resolve the two open questions referenced by Plan 7:

- Open question #3 (effective license of the bundled binary) → resolved by NOTICE.md linkage seam analysis (Task 3). Mark **Resolved** with reference: `Resolved in Plan 7 Task 3 — NOTICE.md.`
- Open question #4 (cargo-about workflow) → resolved by Task 10. Mark **Resolved** with reference: `Resolved in Plan 7 Task 10 — about.toml + cargo-about generate.`

- [ ] **Step 4: Update CHANGELOG.md release date**

Edit `CHANGELOG.md`. Update the heading for the unreleased section from any placeholder to the firm date:

```markdown
## [0.1.0] — 2026-04-11
```

(If the date already matches from Task 7, leave it as-is.)

- [ ] **Step 5: Run the full preflight gauntlet**

Run, in this exact order:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
just coverage-gate
just live-smoke
```

Expected: every command exits 0. If `just live-smoke` cannot find rootful
podman or the opensnitchd image, document the skip in the commit body
rather than silencing the failure.

- [ ] **Step 6: Commit the version bump**

```bash
git add Cargo.toml CONTRIBUTING.md \
        docs/superpowers/specs/2026-04-10-snitchwatch-design.md \
        CHANGELOG.md
git commit -m "$(cat <<'EOF'
chore(release): bump workspace to 0.1.0 and tick M6

- workspace.package.version: 0.1.0
- workspace.package.license: GPL-2.0-or-later (matches NOTICE.md)
- CONTRIBUTING.md: replace security@ placeholder with private
  GitHub Security Advisory disclosure
- spec: tick M6 ✅, mark open questions #3 and #4 resolved
  with Plan 7 task references
- CHANGELOG.md: pin 0.1.0 release date to 2026-04-11

Preflight gauntlet (fmt + clippy + test + coverage-gate +
live-smoke) all green.
EOF
)"
```

- [ ] **Step 7: Tag the release (manual step)**

Run, only after Step 6 commits cleanly:

```bash
git tag -a v0.1.0 -m "Snitchwatch 0.1.0 — first public release

See CHANGELOG.md for details."
```

Do **not** push the tag yet — the next step is for a human reviewer to
sanity-check the tag ref and run `git push origin main v0.1.0`. The release
workflow created in Task 7 will trip on the tag push and produce the GitHub
release with the Flatpak bundle attached.

---

## Acceptance Criteria

The Plan 7 deliverable is complete when **all** of the following hold:

1. `README.md` opens with a status badge, install-on-Bazzite copy-paste block, quickstart, link to `docs/architecture.md`, and a failure-mode banner table covering at minimum GrpcUnreachable, GrpcReconnected, KernelHookFailed, BlocklistRefreshFailed, EventFloodDropped, StateDivergenceReconciled, and BridgePanicRecovered.
2. `CONTRIBUTING.md` documents dev environment setup, the dev loop (`just check`, `just test`, `just live-smoke`), conventional commits + DCO, the PR process, the bug-report template, and the security disclosure block.
3. `LICENSE` contains the canonical GPL-2.0 text. `NOTICE.md` enumerates every linked component, identifies the three linkage seams, and concludes with the effective `GPL-2.0-or-later` determination.
4. `docs/architecture.md` exists, links from the README, declares the Option A loopback ephemeral default since Plan 1, and documents the A→B→C ws_bind upgrade path.
5. `.github/workflows/ci.yml` runs fmt, clippy, test, coverage-gate, and bats jobs on `ubuntu-24.04` on every push to `main` and every PR.
6. `scripts/coverage-gate.sh` enforces an 80% line floor on the translator, cache, blocklists, and lifecycle modules. The four-module include regex is hard-coded; lowering the threshold or adding `cfg_attr coverage no_coverage` exclusions is forbidden.
7. `.github/workflows/release.yml` triggers on `v*.*.*` tag pushes, builds the Flatpak via `flatpak-github-actions/flatpak-builder@v6`, sha256s the bundle, extracts the matching CHANGELOG section, and publishes a GitHub release via `softprops/action-gh-release@v2`.
8. `CHANGELOG.md` follows Keep-a-Changelog and contains a `## [0.1.0] — 2026-04-11` section with Added/Changed/Fixed/Known issues/Deferred subsections.
9. `scripts/live-smoke.sh` exists, the new `--once-then-exit-after <duration>` CLI flag is wired in `crates/snitchwatch-bridge-cli/src/lib.rs` with parser unit tests, and `just live-smoke` runs the script against rootful podman with a 60-second deadline.
10. `cargo llvm-cov` reports ≥80% line coverage on each of the four gated module groups locally (`just coverage-gate`).
11. `NOTICE.md` is signed off by `cargo-about generate` against an `about.toml` that lists the accepted licenses; the generated report matches the hand-written linkage analysis.
12. `LifecycleKind::EventFloodDropped` is wired end-to-end: `EventCache::push` increments the dropped counter on `send_timeout` failure; `LifecycleProbe::tick` drains the counter via `take()` and broadcasts on the lifecycle channel.
13. `LifecycleKind::KernelHookFailed` is wired end-to-end via `lifecycle/journalctl.rs` (subprocess wrapper, scrape injection point) and `lifecycle/journalctl/parse.rs` (pure parser with five marker strings + 200-char excerpt cap); GrpcUnreachable → KernelHookFailed transition has an integration test.
14. `LifecycleKind::StateDivergenceReconciled` is wired end-to-end: `GrpcClient` sets an `AtomicBool last_diff_seen` from the reconciliation loop's non-empty diff branch; `LifecycleProbe::tick` drains it and broadcasts on the lifecycle channel; an integration test verifies one emission per non-empty diff and no re-emission on subsequent ticks.
15. `LifecycleKind::BridgePanicRecovered` is wired end-to-end via `crates/snitchwatch-tauri/src/panic_hook.rs` with a chained `std::panic` hook that calls the prior hook after notification; the test exercises the channel directly to avoid unwinding the test thread.
16. The "Copy Diagnostics" Tauri command exists, the pure tar-builder in `diagnostics/bundle.rs` round-trips three named entries through `tar` + `flate2`, and the web button in the About panel is guarded by a markup-shape lint test that checks for the `diagnostics-copy` id and the `copy_diagnostic_bundle` command name.
17. `docs/manual-smoke.md` documents the five release-blocker scenarios (cold-start daemon-missing, systemd unit inactive then reconnect, daemon crash mid-stream, blocklist offline, LAN bind toggle read-only smoke) and a shape lint asserts each heading + the `Expected banner:` line is present.
18. `Cargo.toml` workspace version is `0.1.0`, license is `GPL-2.0-or-later`, the spec milestone table marks M6 ✅ with open questions #3 and #4 resolved by Plan 7 task references, and `git tag -a v0.1.0` has been created locally (push gated on human review).

## Deferred

These items are deliberately **out of scope** for the v0.1.0 release and
have already been agreed with the user during brainstorming + Plan 1–6
review. Re-reading them as if they were Plan 7 work is a waste — leave them
on the v2 backlog.

- **LAN bind mode (Option C of the ws_bind upgrade path).** Requires TLS,
  bearer-token auth, and a token rotation UI. v2.
- **Flathub submission.** v0.1.0 ships only the GitHub-release Flatpak
  bundle. Flathub submission is downstream of the v0.1.0 tag and a separate
  workflow.
- **App icon designer pass.** v0.1.0 ships the placeholder icon only. A
  proper icon set (light/dark, scalable + raster sizes) is a v2 design task.
- **Cross-distro packaging (deb/rpm/AUR).** Flatpak is the only supported
  format for v0.1.0. Native packages come after Flathub.
- **Auto-update.** No in-app update channel for v0.1.0; users get updates
  via Flatpak. v2 may add an in-app banner that polls the GitHub release
  feed.
- **Telemetry upload.** The diagnostic bundle is **manual copy-only** for
  v0.1.0. No automatic upload. v2 needs an opt-in privacy review before any
  network telemetry.
- **Translated UI.** v0.1.0 is English-only. i18n scaffolding is v2.
- **Multi-arch builds.** v0.1.0 ships `x86_64` only. `aarch64` (and the
  Asahi-Linux story) is v2.

---

**Plan complete and saved to `docs/superpowers/plans/2026-04-11-public-release.md`.**

Two execution options:

1. **Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — Execute tasks in this session using `executing-plans`, batch execution with checkpoints.

Which approach?

