# Snitchwatch M3 — Tauri Shell Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wrap the M2 vendored UI in a native Tauri 2 desktop window with a system tray (5 states), autostart entry, native desktop notifications, panic hook + crash log, and a first-run wizard that detects whether the opensnitchd quadlet is installed/running. The bridge runs in-process inside the Tauri Rust core on a background tokio task; the embedded webview talks to it over the same `127.0.0.1:3031` WebSocket the M2 plan stood up. After this milestone, Snitchwatch is a real desktop app a non-developer could install and use — even though packaging (M5) and blocklists (M4) come later.

**Architecture:** A new `crates/snitchwatch-tauri` crate becomes the binary entry point for the desktop app. Its `main.rs` builds a Tauri 2 application, spawns the bridge as an in-process tokio task on app startup (re-using `snitchwatch_bridge::Bridge::serve`), wires the panic hook to write `crash.log`, then constructs the main window pointing at `http://127.0.0.1:3031/` (the bridge serves the embedded `web/` from M2). A `tray.rs` module owns the `TrayIcon`, subscribes to a `tokio::sync::watch::Receiver<TrayState>` published by the bridge, and re-renders the icon + tooltip + menu on every state transition. Notifications fire from a small dispatcher that listens to a `broadcast::Receiver<Notice>` from the bridge and calls `notify-rust` (which speaks `org.freedesktop.Notifications` over D-Bus). The first-run wizard is a single Tauri command (`detect_daemon_state`) the webview calls before subscribing to `/stream`; it returns one of `{Connected, UnitMissing, UnitInactive, UnreachableRetrying}` and the JS side renders the matching onboarding screen. Autostart is a `~/.config/autostart/snitchwatch.desktop` file dropped on first launch, removable from settings.

**Tech Stack:** Tauri 2.0 (`tauri = "2"`, `tauri-build = "2"`, `tauri-plugin-autostart = "2"`), `notify-rust = "4"` (D-Bus desktop notifications), `tokio = "1.40"` (already present), `tracing = "0.1"` (already present), `tracing-appender = "0.2"` (file rotation), `which = "6"` (resolve `systemctl`), `serde = "1"` + `serde_json = "1"` (Tauri commands). `snitchwatch-bridge` is consumed as a path dependency. Tests run via `cargo test -p snitchwatch-tauri` (unit tests for `tray::TrayState` transitions and `wizard::DaemonProbe` parsing) plus a Playwright spec under `tests/tauri_smoke/` that drives `cargo run -p snitchwatch-tauri` headlessly via `tauri-driver` + `webdriverio`.

**What this plan does NOT cover:**
- Blocklists tab wiring, blocklist SQLite store, fetchers, materializer (Plan 5 — M4).
- Flatpak manifest, podman quadlet, install.sh (Plan 6 — M5).
- Flipping the WebSocket bind back to ephemeral `127.0.0.1:0` (Plan 7 — M6).
- The actual `install.sh` invocation from the wizard's "Install daemon" branch (Plan 6 wires the script itself; Plan 4 only wires the wizard UI to a stub that prints a TODO).
- Diagnostic-bundle export, journalctl tailing (Plan 6).
- App icon redesign — placeholder eye-silhouette from M2 carries forward.
- Flipping any default config away from M2 conventions (`127.0.0.1:3031` stays fixed; ephemeral mode arrives in M6).

---

## Memory Constraints (read before starting)

These guard rails come from `~/.claude/projects/-var-home-user-Documents-vibe-code-opensnitch-gui/memory/`:

1. **`bash_antipattern_hook.md`** — workspace blocks `find`/`ls`/`cat`/`grep`/`rg`/`head`/`tail`/`sed`/`awk` in Bash. Use Read/Grep/Glob tools instead. PostToolUse hooks may fire false-positive "failure" reminders even on success — verify by reading stdout, not by trusting the reminder tag.
2. **`m1_envelope_hack.md`** — the JSON envelope inside `Notification.data` from M1 was deleted at M2 topology flip. Do not reintroduce it. Any new payload needed for tray/wizard goes through a typed `ws_messages.rs` variant or a Tauri command — never as a stringly-typed envelope.
3. **`clippy_gotchas_bridge.md`** — `Translated::AskRule(Box<ConnectionRow>)` must stay boxed (~224 bytes). Drop a `oneshot::Receiver<Verdict>` with `drop(rx)`, never `let _ = rx`. Both gotchas affect bridge code that the Tauri shell now embeds in-process — clippy will surface them on the first build of `snitchwatch-tauri`.
4. **`autonomous_tdd_resume.md`** — on resume after compaction, advance the next task with a tool call; do not recap.
5. **`plan1_deferred_criteria.md`** — Plan 1's deferred items (live opensnitchd 60s smoke, `cargo-llvm-cov` ≥80% on translator/cache) belong to Plan 7. Do not reopen them in Plan 4. Coverage targets here apply only to new `snitchwatch-tauri` code.

---

## File Structure

### NEW files
- `crates/snitchwatch-tauri/Cargo.toml` — binary crate (`[[bin]] name = "snitchwatch-tauri"`), depends on `snitchwatch-bridge` (path), `tauri`, `tauri-build`, `tauri-plugin-autostart`, `notify-rust`, `tokio`, `tracing`, `tracing-appender`, `tracing-subscriber`, `which`, `serde`, `serde_json`, `anyhow`, `thiserror`.
- `crates/snitchwatch-tauri/build.rs` — single-line `tauri_build::build()`.
- `crates/snitchwatch-tauri/tauri.conf.json` — Tauri 2 config: window size, fullscreen=false, dev URL = `http://127.0.0.1:3031/`, productName = "Snitchwatch", identifier = `org.snitchwatch.Snitchwatch`, bundle icons reference `web/icons/snitchwatch.svg` + the 192/512 PNGs.
- `crates/snitchwatch-tauri/src/main.rs` — `fn main() -> anyhow::Result<()>`: install panic hook, init tracing, build tokio runtime, spawn bridge task, build Tauri builder, run.
- `crates/snitchwatch-tauri/src/bridge_runtime.rs` — `spawn_bridge_runtime()` returns `(JoinHandle, watch::Receiver<TrayState>, broadcast::Receiver<Notice>)`. Owns the in-process bridge lifecycle and exposes the two state channels the shell consumes.
- `crates/snitchwatch-tauri/src/tray.rs` — `pub struct Tray`, `pub enum TrayState { Idle, Pending(usize), RecentBlock { what: String, ttl: Duration }, FilterOff, DaemonDown }`, `Tray::install(app: &App, rx: watch::Receiver<TrayState>) -> tauri::Result<()>`.
- `crates/snitchwatch-tauri/src/notifier.rs` — `pub struct Notifier`, `pub enum Notice { Pending { row_id: u64, process: String }, DaemonAway, FilterPauseExpired }`, `Notifier::run(rx: broadcast::Receiver<Notice>)`. Uses `notify-rust` and a per-kind cooldown map so the same notification doesn't spam.
- `crates/snitchwatch-tauri/src/wizard.rs` — `pub enum DaemonState { Connected, UnitMissing, UnitInactive, UnreachableRetrying }`, `pub async fn detect_daemon_state() -> DaemonState`. Wraps the gRPC dial + `systemctl --user list-unit-files` invocation.
- `crates/snitchwatch-tauri/src/commands.rs` — Tauri `#[command]` handlers: `detect_daemon_state`, `start_daemon_unit`, `install_daemon_stub`, `set_autostart`, `get_autostart_state`, `open_crash_log`.
- `crates/snitchwatch-tauri/src/panic_hook.rs` — `pub fn install(crash_log_path: PathBuf)` writes timestamped panic backtraces to `crash.log` and re-emits to `tracing::error!`.
- `crates/snitchwatch-tauri/src/paths.rs` — XDG-aware path resolver: `state_dir()` → `$XDG_STATE_HOME/snitchwatch` (fallback `$HOME/.local/state/snitchwatch`), `autostart_path()` → `$XDG_CONFIG_HOME/autostart/snitchwatch.desktop`, `bridge_log_path()`, `crash_log_path()`.
- `crates/snitchwatch-tauri/src/lib.rs` — `pub mod` declarations for the modules above, plus a re-export of `snitchwatch_bridge` so docs/tests can use one import path.
- `crates/snitchwatch-tauri/icons/snitchwatch.png` — placeholder copy of `web/icons/snitchwatch-512.png` (Tauri's bundle wants its own copy alongside `tauri.conf.json`).
- `crates/snitchwatch-tauri/icons/snitchwatch.icns` — generated from the PNG at vendoring time (`png2icns` or `iconutil`); empty placeholder is acceptable for the Linux-only v1.
- `web/onboarding.css` — small new stylesheet for the wizard overlay.
- `web/js/onboarding.js` — calls `__TAURI__.invoke('detect_daemon_state')` on load, branches to one of three onboarding screens, stays out of the way when daemon state is `Connected`.
- `tests/tauri_smoke/package.json` — single dev dep `@playwright/test`.
- `tests/tauri_smoke/playwright.config.ts` — Playwright config that points at the Tauri webview via `tauri-driver` (sets `SNITCHWATCH_TAURI_WEBVIEW_URL` from a fixture).
- `tests/tauri_smoke/tests/wizard_branches.spec.ts` — three scenarios: daemon connected → no overlay; mock returns `UnitMissing` → "Install daemon" CTA; mock returns `UnitInactive` → "Start it" CTA.
- `tests/tauri_smoke/tests/tray_states.spec.ts` — flips `TrayState` via a debug Tauri command and asserts the tooltip changes.
- `tests/tauri_smoke/.gitignore` — `node_modules/`, `playwright-report/`, `test-results/`.

### MODIFIED files
- `Cargo.toml` (workspace root) — add `crates/snitchwatch-tauri` to `members`.
- `crates/snitchwatch-bridge/src/lib.rs` — add `pub mod tray_state;` and `pub mod notice;`. Both re-export the publishers the shell subscribes to. The bridge becomes the single source of truth for `TrayState` and `Notice`; the Tauri crate is a pure consumer.
- `crates/snitchwatch-bridge/src/tray_state.rs` — NEW under bridge: `pub struct TrayStatePublisher { tx: watch::Sender<TrayState> }`, plus the `TrayState` enum. Lives in the bridge so headless tests can assert state transitions without dragging in Tauri.
- `crates/snitchwatch-bridge/src/notice.rs` — NEW under bridge: `pub struct NoticeBus { tx: broadcast::Sender<Notice> }`, plus the `Notice` enum.
- `crates/snitchwatch-bridge/src/lib.rs` — `Bridge::serve` returns `(JoinHandle, watch::Receiver<TrayState>, broadcast::Receiver<Notice>)` instead of just the `JoinHandle`. Existing CLI binary updates to ignore the new receivers.
- `crates/snitchwatch-bridge/src/cache/connections.rs` — call `tray_state.set(TrayState::Pending(pending_count))` whenever a pending row is inserted/resolved.
- `crates/snitchwatch-bridge/src/translator/downstream.rs` — call `notice_bus.send(Notice::Pending { row_id, process })` whenever a new AskRule arrives, and `Notice::DaemonAway` when the gRPC reconnect loop has been failing for >30s.
- `crates/snitchwatch-bridge-cli/src/main.rs` — destructure the new receivers from `Bridge::serve` and immediately `drop(...)` them; the CLI doesn't render a tray.
- `web/index.html` — add `<script src="js/onboarding.js"></script>` and `<link rel="stylesheet" href="onboarding.css">`. Add an empty `<div id="onboarding-overlay" hidden></div>` placeholder.
- `justfile` — add `just tauri-dev` (runs `cargo run -p snitchwatch-tauri`), `just tauri-smoke` (runs the Playwright suite), `just tauri-build` (release build with bundle).
- `README.md` — replace the M2 "browser tab" instructions with M3 "native window" instructions; document the autostart behavior + how to disable it.
- `docs/superpowers/specs/2026-04-10-snitchwatch-design.md` — flip the milestone table to mark M3 done.
- `.gitignore` — add `tests/tauri_smoke/node_modules`, `tests/tauri_smoke/playwright-report`, `tests/tauri_smoke/test-results`, `crates/snitchwatch-tauri/gen/`.

### DELETED files
- None. M3 is additive on top of M2.

---

## Part A — Bridge-side state publishers

The Tauri shell needs to subscribe to state changes that originate inside the bridge. The bridge currently has no `TrayState` or `Notice` concept. Add them as bridge primitives first so the shell can be a thin consumer.

### Task 1: Introduce `TrayState` enum and publisher

**Files:**
- Create: `crates/snitchwatch-bridge/src/tray_state.rs`
- Modify: `crates/snitchwatch-bridge/src/lib.rs`
- Test: `crates/snitchwatch-bridge/src/tray_state.rs` (inline `#[cfg(test)]` module)

- [ ] **Step 1: Write the failing test**

Append to `crates/snitchwatch-bridge/src/tray_state.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn publisher_starts_idle_and_propagates_pending_count() {
        let pub_ = TrayStatePublisher::new();
        let mut rx = pub_.subscribe();
        assert_eq!(*rx.borrow(), TrayState::Idle);

        pub_.set(TrayState::Pending(3));
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), TrayState::Pending(3));

        pub_.set(TrayState::DaemonDown);
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), TrayState::DaemonDown);
    }
}
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cargo test -p snitchwatch-bridge tray_state::tests`
Expected: FAIL with `cannot find type 'TrayStatePublisher'` (file doesn't define it yet).

- [ ] **Step 3: Implement the publisher**

Replace the contents of `crates/snitchwatch-bridge/src/tray_state.rs` (above the test module) with:

```rust
//! Bridge-owned tray state publisher.
//!
//! The bridge is the source of truth for what the tray icon should show. The
//! Tauri shell subscribes to `TrayStatePublisher::subscribe()` and re-renders
//! on every change. Headless tests can assert transitions without Tauri.

use std::time::Duration;
use tokio::sync::watch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayState {
    Idle,
    Pending(usize),
    RecentBlock { what: String, ttl: Duration },
    FilterOff,
    DaemonDown,
}

impl Default for TrayState {
    fn default() -> Self {
        TrayState::Idle
    }
}

pub struct TrayStatePublisher {
    tx: watch::Sender<TrayState>,
}

impl TrayStatePublisher {
    pub fn new() -> Self {
        let (tx, _rx) = watch::channel(TrayState::Idle);
        Self { tx }
    }

    pub fn subscribe(&self) -> watch::Receiver<TrayState> {
        self.tx.subscribe()
    }

    pub fn set(&self, state: TrayState) {
        // send_replace ignores the no-receivers error: state still updates
        // for late subscribers.
        let _ = self.tx.send_replace(state);
    }
}

impl Default for TrayStatePublisher {
    fn default() -> Self {
        Self::new()
    }
}
```

Then add to `crates/snitchwatch-bridge/src/lib.rs`:

```rust
pub mod tray_state;
```

- [ ] **Step 4: Run the test, verify it passes**

Run: `cargo test -p snitchwatch-bridge tray_state::tests`
Expected: PASS.

- [ ] **Step 5: Run clippy on the new module**

Run: `cargo clippy -p snitchwatch-bridge -- -D warnings`
Expected: PASS. The `let _ = ...` on the `send_replace` return is acceptable here (it's a `Result<TrayState, SendError>`, not a `oneshot::Receiver` — the clippy_gotchas_bridge memory rule applies only to dropped receivers).

- [ ] **Step 6: Commit**

```bash
git add crates/snitchwatch-bridge/src/tray_state.rs crates/snitchwatch-bridge/src/lib.rs
git commit -m "feat(bridge): add TrayStatePublisher with watch channel"
```

---

### Task 2: Introduce `Notice` enum and broadcast bus

**Files:**
- Create: `crates/snitchwatch-bridge/src/notice.rs`
- Modify: `crates/snitchwatch-bridge/src/lib.rs`
- Test: `crates/snitchwatch-bridge/src/notice.rs` (inline `#[cfg(test)]` module)

- [ ] **Step 1: Write the failing test**

Append to `crates/snitchwatch-bridge/src/notice.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn broadcast_delivers_to_two_subscribers() {
        let bus = NoticeBus::new();
        let mut rx_a = bus.subscribe();
        let mut rx_b = bus.subscribe();

        bus.send(Notice::DaemonAway);

        let got_a = rx_a.recv().await.unwrap();
        let got_b = rx_b.recv().await.unwrap();
        assert_eq!(got_a, Notice::DaemonAway);
        assert_eq!(got_b, Notice::DaemonAway);
    }

    #[tokio::test]
    async fn no_subscribers_does_not_panic() {
        let bus = NoticeBus::new();
        // No one is listening — send should silently no-op.
        bus.send(Notice::FilterPauseExpired);
    }
}
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cargo test -p snitchwatch-bridge notice::tests`
Expected: FAIL with `cannot find type 'NoticeBus'`.

- [ ] **Step 3: Implement the bus**

Replace the contents of `crates/snitchwatch-bridge/src/notice.rs` (above the test module) with:

```rust
//! Bridge-owned desktop notification bus.
//!
//! The Tauri shell subscribes to a broadcast::Receiver<Notice> and dispatches
//! each entry to `notify-rust`. Headless tests use the receiver directly and
//! never touch D-Bus.

use tokio::sync::broadcast;

const BUS_CAPACITY: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Pending { row_id: u64, process: String },
    DaemonAway,
    FilterPauseExpired,
}

pub struct NoticeBus {
    tx: broadcast::Sender<Notice>,
}

impl NoticeBus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(BUS_CAPACITY);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Notice> {
        self.tx.subscribe()
    }

    pub fn send(&self, notice: Notice) {
        // SendError when there are zero subscribers is expected and benign.
        let _ = self.tx.send(notice);
    }
}

impl Default for NoticeBus {
    fn default() -> Self {
        Self::new()
    }
}
```

Then add to `crates/snitchwatch-bridge/src/lib.rs`:

```rust
pub mod notice;
```

- [ ] **Step 4: Run the test, verify it passes**

Run: `cargo test -p snitchwatch-bridge notice::tests`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/notice.rs crates/snitchwatch-bridge/src/lib.rs
git commit -m "feat(bridge): add NoticeBus broadcast channel for desktop notifications"
```

---

### Task 3: Wire `Bridge::serve` to expose both publishers

**Files:**
- Modify: `crates/snitchwatch-bridge/src/lib.rs`
- Modify: `crates/snitchwatch-bridge-cli/src/main.rs`
- Test: `crates/snitchwatch-bridge/tests/serve_returns_publishers.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/snitchwatch-bridge/tests/serve_returns_publishers.rs`:

```rust
use snitchwatch_bridge::{
    notice::Notice,
    tray_state::TrayState,
    Bridge, BridgeConfig,
};
use std::time::Duration;

#[tokio::test]
async fn serve_returns_tray_and_notice_receivers() {
    let cfg = BridgeConfig {
        ws_bind: "127.0.0.1:0".parse().unwrap(),
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
    };
    let (handle, mut tray_rx, mut notice_rx) = Bridge::new(cfg).serve().await.unwrap();

    // Initial tray state must be Idle.
    assert_eq!(*tray_rx.borrow(), TrayState::Idle);

    // We don't drive any traffic, so notice_rx should be empty after a short
    // wait. We just verify it's a valid receiver.
    let try_recv = tokio::time::timeout(Duration::from_millis(10), notice_rx.recv()).await;
    assert!(try_recv.is_err(), "no notices expected on a quiet bridge");

    handle.abort();
}
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cargo test -p snitchwatch-bridge --test serve_returns_publishers`
Expected: FAIL with a tuple-arity mismatch on `Bridge::new(...).serve().await.unwrap()`.

- [ ] **Step 3: Update `Bridge::serve` signature**

Open `crates/snitchwatch-bridge/src/lib.rs` and locate the existing `Bridge::serve` impl. Change its signature and body:

```rust
use crate::notice::NoticeBus;
use crate::tray_state::TrayStatePublisher;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

impl Bridge {
    pub async fn serve(
        self,
    ) -> anyhow::Result<(
        JoinHandle<()>,
        watch::Receiver<crate::tray_state::TrayState>,
        broadcast::Receiver<crate::notice::Notice>,
    )> {
        let tray_pub = TrayStatePublisher::new();
        let notice_bus = NoticeBus::new();
        let tray_rx = tray_pub.subscribe();
        let notice_rx = notice_bus.subscribe();

        // Existing wiring: pass tray_pub + notice_bus into the cache + downstream
        // translator constructors so they can publish state.
        let handle = self.spawn_runtime(tray_pub, notice_bus).await?;
        Ok((handle, tray_rx, notice_rx))
    }
}
```

The internal `spawn_runtime(...)` method threads `Arc<TrayStatePublisher>` and `Arc<NoticeBus>` into the connection cache + downstream translator. Use `Arc` (not the publishers themselves) because the cache and downstream translator both need to call `set`/`send` from independent tasks.

- [ ] **Step 4: Update the CLI binary to drop the new receivers**

Open `crates/snitchwatch-bridge-cli/src/main.rs`. Find the existing `Bridge::serve` call:

```rust
let handle = bridge.serve().await?;
```

Replace with:

```rust
let (handle, tray_rx, notice_rx) = bridge.serve().await?;
// CLI does not render a tray; explicitly drop both receivers so we don't
// silently buffer state-change events. drop() is the form required by
// clippy_gotchas_bridge — never `let _ = ...` on receivers.
drop(tray_rx);
drop(notice_rx);
```

- [ ] **Step 5: Run the test, verify it passes**

Run: `cargo test -p snitchwatch-bridge --test serve_returns_publishers`
Expected: PASS.

- [ ] **Step 6: Run clippy on the workspace**

Run: `cargo clippy --workspace -- -D warnings`
Expected: PASS. If `let_underscore_future` fires on the CLI binary, replace `let _ = ...` with `drop(...)` per `clippy_gotchas_bridge.md`.

- [ ] **Step 7: Commit**

```bash
git add crates/snitchwatch-bridge/src/lib.rs \
        crates/snitchwatch-bridge/tests/serve_returns_publishers.rs \
        crates/snitchwatch-bridge-cli/src/main.rs
git commit -m "feat(bridge): Bridge::serve returns (handle, tray_rx, notice_rx)"
```

---

### Task 4: Cache publishes pending count to `TrayStatePublisher`

**Files:**
- Modify: `crates/snitchwatch-bridge/src/cache/connections.rs`
- Test: `crates/snitchwatch-bridge/src/cache/connections.rs` (inline `#[cfg(test)]` module)

- [ ] **Step 1: Write the failing test**

Append to `crates/snitchwatch-bridge/src/cache/connections.rs`:

```rust
#[cfg(test)]
mod tray_state_tests {
    use super::*;
    use crate::tray_state::{TrayState, TrayStatePublisher};
    use std::sync::Arc;

    fn fake_row(id: u64) -> ConnectionRow {
        ConnectionRow {
            id,
            process: "firefox".into(),
            host: "github.com".into(),
            port: 443,
            protocol: "tcp".into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn inserting_pending_row_publishes_count() {
        let tray = Arc::new(TrayStatePublisher::new());
        let mut rx = tray.subscribe();
        let cache = ConnectionCache::with_tray_publisher(tray.clone());

        let _verdict_rx = cache.insert_pending(Box::new(fake_row(1))).await;
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), TrayState::Pending(1));

        let _verdict_rx2 = cache.insert_pending(Box::new(fake_row(2))).await;
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), TrayState::Pending(2));
    }

    #[tokio::test]
    async fn resolving_last_pending_returns_to_idle() {
        let tray = Arc::new(TrayStatePublisher::new());
        let mut rx = tray.subscribe();
        let cache = ConnectionCache::with_tray_publisher(tray.clone());

        let verdict_rx = cache.insert_pending(Box::new(fake_row(1))).await;
        rx.changed().await.unwrap();

        cache.resolve_pending(1, Verdict::AllowOnce);
        // Verdict resolution drops the oneshot receiver; the cache should
        // republish Idle since pending count is now 0.
        drop(verdict_rx);
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), TrayState::Idle);
    }
}
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cargo test -p snitchwatch-bridge cache::connections::tray_state_tests`
Expected: FAIL with `no method named 'with_tray_publisher' found`.

- [ ] **Step 3: Add the publisher field**

Open `crates/snitchwatch-bridge/src/cache/connections.rs`. Add to `ConnectionCache`:

```rust
use std::sync::Arc;
use crate::tray_state::{TrayState, TrayStatePublisher};

pub struct ConnectionCache {
    // ... existing fields
    tray: Option<Arc<TrayStatePublisher>>,
}
```

Add a constructor:

```rust
impl ConnectionCache {
    pub fn with_tray_publisher(tray: Arc<TrayStatePublisher>) -> Self {
        let mut cache = Self::new();
        cache.tray = Some(tray);
        cache
    }

    fn republish_pending_count(&self) {
        if let Some(tray) = &self.tray {
            let n = self.pending_count();
            tray.set(if n == 0 { TrayState::Idle } else { TrayState::Pending(n) });
        }
    }
}
```

Then call `self.republish_pending_count();` at the end of `insert_pending` and `resolve_pending`.

- [ ] **Step 4: Run the test, verify it passes**

Run: `cargo test -p snitchwatch-bridge cache::connections::tray_state_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/cache/connections.rs
git commit -m "feat(bridge): cache publishes pending count to TrayStatePublisher"
```

---

### Task 5: Downstream translator emits `Notice::Pending` and `Notice::DaemonAway`

**Files:**
- Modify: `crates/snitchwatch-bridge/src/translator/downstream.rs`
- Modify: `crates/snitchwatch-bridge/src/grpc_client.rs`
- Test: `crates/snitchwatch-bridge/tests/notice_emission.rs`

- [ ] **Step 1: Write the failing integration test**

Create `crates/snitchwatch-bridge/tests/notice_emission.rs`:

```rust
use snitchwatch_bridge::{notice::Notice, Bridge, BridgeConfig};
use std::time::Duration;

#[tokio::test]
async fn ask_rule_emits_pending_notice() {
    let cfg = BridgeConfig {
        ws_bind: "127.0.0.1:0".parse().unwrap(),
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
    };
    let (handle, _tray_rx, mut notice_rx) = Bridge::new(cfg).serve().await.unwrap();

    // Spawn the in-process mock daemon and have it dial the bridge's gRPC
    // server, then fire one AskRule. (Re-uses the same helper from M1.5.)
    mock_opensnitchd::dial_and_fire_ask_rule(
        "127.0.0.1:0".parse().unwrap(),
        "firefox",
        "github.com",
        443,
    )
    .await;

    let notice = tokio::time::timeout(Duration::from_secs(2), notice_rx.recv())
        .await
        .expect("notice should arrive within 2s")
        .expect("recv should succeed");

    match notice {
        Notice::Pending { process, .. } => assert_eq!(process, "firefox"),
        other => panic!("expected Notice::Pending, got {other:?}"),
    }

    handle.abort();
}
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cargo test -p snitchwatch-bridge --test notice_emission`
Expected: FAIL with a timeout — the bridge currently never publishes `Notice::Pending`.

- [ ] **Step 3: Plumb the bus into the downstream translator**

Open `crates/snitchwatch-bridge/src/translator/downstream.rs`. Add an `Arc<NoticeBus>` field to the translator struct. In the `AskRule` handler, immediately after the cache `insert_pending` call:

```rust
use crate::notice::{Notice, NoticeBus};
use std::sync::Arc;

// In the struct definition:
pub struct DownstreamTranslator {
    // ... existing fields
    notice_bus: Arc<NoticeBus>,
}

// In the AskRule handler, after cache.insert_pending(row.clone()).await:
self.notice_bus.send(Notice::Pending {
    row_id: row.id,
    process: row.process.clone(),
});
```

- [ ] **Step 4: Plumb the bus into the gRPC reconnect loop**

Open `crates/snitchwatch-bridge/src/grpc_client.rs`. Add an `Arc<NoticeBus>` field to whichever struct owns the reconnect loop. After the backoff timer crosses 30 seconds of consecutive failure:

```rust
const DAEMON_AWAY_THRESHOLD: Duration = Duration::from_secs(30);

// Inside the reconnect loop:
if first_failure_at.elapsed() >= DAEMON_AWAY_THRESHOLD && !daemon_away_reported {
    self.notice_bus.send(Notice::DaemonAway);
    daemon_away_reported = true;
}
// On successful reconnect, reset:
//   first_failure_at = None;
//   daemon_away_reported = false;
```

- [ ] **Step 5: Run the test, verify it passes**

Run: `cargo test -p snitchwatch-bridge --test notice_emission`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/snitchwatch-bridge/src/translator/downstream.rs \
        crates/snitchwatch-bridge/src/grpc_client.rs \
        crates/snitchwatch-bridge/tests/notice_emission.rs
git commit -m "feat(bridge): emit Notice::Pending and Notice::DaemonAway"
```

---

## Part B — Tauri crate scaffolding

The bridge now publishes everything the shell needs to consume. Time to stand up the Tauri crate.

### Task 6: Create the `snitchwatch-tauri` crate skeleton

**Files:**
- Create: `crates/snitchwatch-tauri/Cargo.toml`
- Create: `crates/snitchwatch-tauri/build.rs`
- Create: `crates/snitchwatch-tauri/tauri.conf.json`
- Create: `crates/snitchwatch-tauri/src/main.rs`
- Create: `crates/snitchwatch-tauri/src/lib.rs`
- Create: `crates/snitchwatch-tauri/icons/snitchwatch.png` (placeholder)
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Add the crate to the workspace**

Open the workspace root `Cargo.toml` and add `crates/snitchwatch-tauri` to the `members` array:

```toml
[workspace]
members = [
    "crates/snitchwatch-bridge",
    "crates/snitchwatch-bridge-cli",
    "crates/snitchwatch-proto",
    "crates/snitchwatch-spike",
    "crates/snitchwatch-tauri",
    "tests/mock_opensnitchd",
]
```

- [ ] **Step 2: Write `crates/snitchwatch-tauri/Cargo.toml`**

```toml
[package]
name = "snitchwatch-tauri"
version = "0.1.0"
edition = "2021"
license = "GPL-2.0"
description = "Snitchwatch desktop shell — Tauri 2 wrapper around the bridge."

[[bin]]
name = "snitchwatch-tauri"
path = "src/main.rs"

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
snitchwatch-bridge = { path = "../snitchwatch-bridge" }
tauri = { version = "2", features = ["tray-icon"] }
tauri-plugin-autostart = "2"
notify-rust = "4"
tokio = { version = "1.40", features = ["rt-multi-thread", "sync", "macros"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tracing-appender = "0.2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
thiserror = "2"
which = "6"

[dev-dependencies]
tokio = { version = "1.40", features = ["test-util"] }
```

- [ ] **Step 3: Write `crates/snitchwatch-tauri/build.rs`**

```rust
fn main() {
    tauri_build::build()
}
```

- [ ] **Step 4: Write `crates/snitchwatch-tauri/tauri.conf.json`**

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "Snitchwatch",
  "version": "0.1.0",
  "identifier": "org.snitchwatch.Snitchwatch",
  "build": {
    "frontendDist": "../../web"
  },
  "app": {
    "windows": [
      {
        "title": "Snitchwatch",
        "width": 1100,
        "height": 720,
        "minWidth": 800,
        "minHeight": 540,
        "resizable": true,
        "fullscreen": false,
        "url": "http://127.0.0.1:3031/"
      }
    ],
    "security": {
      "csp": null
    },
    "trayIcon": {
      "iconPath": "icons/snitchwatch.png",
      "iconAsTemplate": true
    }
  },
  "bundle": {
    "active": true,
    "targets": ["deb", "rpm", "appimage"],
    "icon": ["icons/snitchwatch.png"],
    "category": "Network"
  }
}
```

- [ ] **Step 5: Copy a placeholder icon**

```bash
mkdir -p crates/snitchwatch-tauri/icons
cp web/icons/snitchwatch-512.png crates/snitchwatch-tauri/icons/snitchwatch.png
```

- [ ] **Step 6: Write a stub `src/main.rs`**

```rust
//! Snitchwatch Tauri shell entry point.
//!
//! Wires:
//!   1. tracing → bridge.log
//!   2. panic hook → crash.log
//!   3. in-process bridge runtime
//!   4. Tauri builder with tray + commands

fn main() -> anyhow::Result<()> {
    println!("snitchwatch-tauri stub");
    Ok(())
}
```

- [ ] **Step 7: Write `src/lib.rs` with empty module declarations**

```rust
//! Snitchwatch desktop shell library surface.
//!
//! All real work lives in the modules below; `main.rs` is just a thin
//! orchestrator. Splitting the work into a library makes the modules
//! independently testable with `cargo test -p snitchwatch-tauri`.

pub mod bridge_runtime;
pub mod commands;
pub mod notifier;
pub mod panic_hook;
pub mod paths;
pub mod tray;
pub mod wizard;
```

Each module file just needs to exist as an empty `// TODO` placeholder for now — the next tasks fill them in. Create each as:

```rust
//! TODO: see plan task N.
```

- [ ] **Step 8: Verify the crate compiles**

Run: `cargo build -p snitchwatch-tauri`
Expected: `Finished` with no errors. Some `dead_code` warnings on the empty modules are acceptable temporarily.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml crates/snitchwatch-tauri/
git commit -m "feat(tauri): scaffold snitchwatch-tauri crate"
```

---

### Task 7: XDG-aware path resolver

**Files:**
- Modify: `crates/snitchwatch-tauri/src/paths.rs`
- Test: `crates/snitchwatch-tauri/src/paths.rs` (inline `#[cfg(test)]` module)

- [ ] **Step 1: Write the failing test**

Replace the contents of `crates/snitchwatch-tauri/src/paths.rs` with:

```rust
//! XDG-aware path resolver.
//!
//! All Snitchwatch state lives under $XDG_STATE_HOME (logs, crash dumps),
//! $XDG_DATA_HOME (sqlite), and $XDG_CONFIG_HOME (autostart, settings).
//! Falls back to ~/.local/{state,share}/ and ~/.config/ when the env vars
//! are unset.

use std::path::PathBuf;

pub fn state_dir() -> PathBuf {
    xdg_dir_or("XDG_STATE_HOME", ".local/state").join("snitchwatch")
}

pub fn data_dir() -> PathBuf {
    xdg_dir_or("XDG_DATA_HOME", ".local/share").join("snitchwatch")
}

pub fn config_dir() -> PathBuf {
    xdg_dir_or("XDG_CONFIG_HOME", ".config").join("snitchwatch")
}

pub fn autostart_path() -> PathBuf {
    xdg_dir_or("XDG_CONFIG_HOME", ".config")
        .join("autostart")
        .join("snitchwatch.desktop")
}

pub fn bridge_log_path() -> PathBuf {
    state_dir().join("bridge.log")
}

pub fn crash_log_path() -> PathBuf {
    state_dir().join("crash.log")
}

fn xdg_dir_or(env_var: &str, fallback_subpath: &str) -> PathBuf {
    if let Ok(p) = std::env::var(env_var) {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(fallback_subpath)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_uses_xdg_when_set() {
        std::env::set_var("XDG_STATE_HOME", "/tmp/snitchwatch-test-state");
        assert_eq!(
            state_dir(),
            PathBuf::from("/tmp/snitchwatch-test-state/snitchwatch")
        );
        std::env::remove_var("XDG_STATE_HOME");
    }

    #[test]
    fn state_dir_falls_back_to_home_local_state() {
        std::env::remove_var("XDG_STATE_HOME");
        std::env::set_var("HOME", "/home/alice");
        assert_eq!(
            state_dir(),
            PathBuf::from("/home/alice/.local/state/snitchwatch")
        );
    }

    #[test]
    fn autostart_path_uses_config_dir() {
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/cfg");
        assert_eq!(
            autostart_path(),
            PathBuf::from("/tmp/cfg/autostart/snitchwatch.desktop")
        );
        std::env::remove_var("XDG_CONFIG_HOME");
    }
}
```

- [ ] **Step 2: Run the test, verify it passes**

Run: `cargo test -p snitchwatch-tauri paths::tests`
Expected: PASS (3 tests).

> **Test ordering note.** These three tests mutate process-global env vars and will interfere with each other under multi-threaded test runners. Run them serially with `--test-threads=1` if a flake appears, or convert to integration tests in `tests/paths.rs` where each test uses a `temp_env` guard.

- [ ] **Step 3: Commit**

```bash
git add crates/snitchwatch-tauri/src/paths.rs
git commit -m "feat(tauri): XDG-aware path resolver"
```

---

### Task 8: Panic hook writes crash.log

**Files:**
- Modify: `crates/snitchwatch-tauri/src/panic_hook.rs`
- Test: `crates/snitchwatch-tauri/src/panic_hook.rs` (inline `#[cfg(test)]` module)

- [ ] **Step 1: Write the failing test**

Replace the contents of `crates/snitchwatch-tauri/src/panic_hook.rs` with:

```rust
//! Tauri-side panic hook.
//!
//! Writes a timestamped backtrace to crash.log and re-emits to tracing::error!
//! so the Diagnostics tab can surface it. The Tauri shell stays alive; only
//! the bridge tokio task is restarted (handled in bridge_runtime.rs).

use std::fs::OpenOptions;
use std::io::Write;
use std::panic::PanicInfo;
use std::path::PathBuf;
use std::sync::Mutex;

static CRASH_LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn install(crash_log_path: PathBuf) {
    if let Some(parent) = crash_log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    *CRASH_LOG_PATH.lock().unwrap() = Some(crash_log_path);

    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info: &PanicInfo<'_>| {
        write_crash_entry(info);
        prev(info);
    }));
}

fn write_crash_entry(info: &PanicInfo<'_>) {
    let path = match CRASH_LOG_PATH.lock().unwrap().clone() {
        Some(p) => p,
        None => return,
    };
    let now = chrono_lite_now_iso();
    let payload = format!(
        "----- snitchwatch panic @ {now} -----\n{info}\n{:?}\n\n",
        std::backtrace::Backtrace::force_capture()
    );
    if let Ok(mut f) = OpenOptions::new().append(true).create(true).open(&path) {
        let _ = f.write_all(payload.as_bytes());
    }
    tracing::error!(target: "snitchwatch::panic", "{payload}");
}

fn chrono_lite_now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn install_creates_parent_dir_and_writes_on_panic() {
        let dir = tempfile::tempdir().unwrap();
        let crash_log = dir.path().join("nested").join("crash.log");
        install(crash_log.clone());

        // Trigger a panic in a child thread so the test doesn't die.
        let _ = std::thread::spawn(|| {
            panic!("test panic from snitchwatch-tauri::panic_hook");
        })
        .join();

        assert!(crash_log.exists(), "crash log should have been created");
        let body = fs::read_to_string(&crash_log).unwrap();
        assert!(body.contains("test panic from snitchwatch-tauri"));
        assert!(body.contains("snitchwatch panic @"));
    }
}
```

Add `tempfile = "3"` to `crates/snitchwatch-tauri/Cargo.toml` `[dev-dependencies]`.

- [ ] **Step 2: Run the test, verify it passes**

Run: `cargo test -p snitchwatch-tauri panic_hook::tests`
Expected: PASS. (The child thread panics; the join captures the unwind without killing the test runner.)

- [ ] **Step 3: Commit**

```bash
git add crates/snitchwatch-tauri/src/panic_hook.rs crates/snitchwatch-tauri/Cargo.toml
git commit -m "feat(tauri): panic hook writes timestamped crash.log"
```

---

### Task 9: Bridge runtime spawner

**Files:**
- Modify: `crates/snitchwatch-tauri/src/bridge_runtime.rs`
- Test: `crates/snitchwatch-tauri/tests/bridge_runtime_starts.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/snitchwatch-tauri/tests/bridge_runtime_starts.rs`:

```rust
use snitchwatch_tauri::bridge_runtime::{spawn_bridge_runtime, BridgeRuntimeConfig};

#[tokio::test]
async fn spawned_bridge_publishes_initial_idle_state() {
    let cfg = BridgeRuntimeConfig {
        ws_bind: "127.0.0.1:0".parse().unwrap(),
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
    };
    let runtime = spawn_bridge_runtime(cfg).await.unwrap();

    // Initial tray state must be Idle (matches the bridge's TrayStatePublisher
    // default).
    assert_eq!(
        *runtime.tray_rx.borrow(),
        snitchwatch_bridge::tray_state::TrayState::Idle
    );

    runtime.shutdown().await;
}
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cargo test -p snitchwatch-tauri --test bridge_runtime_starts`
Expected: FAIL with `cannot find function 'spawn_bridge_runtime'`.

- [ ] **Step 3: Implement `bridge_runtime.rs`**

Replace the contents of `crates/snitchwatch-tauri/src/bridge_runtime.rs` with:

```rust
//! In-process bridge runtime.
//!
//! Spawns `snitchwatch_bridge::Bridge::serve` on a background tokio task and
//! exposes the tray + notice receivers to the rest of the Tauri shell. On
//! `shutdown().await`, aborts the task and drops both receivers (using `drop`,
//! never `let _`, per clippy_gotchas_bridge.md).

use snitchwatch_bridge::{
    notice::Notice,
    tray_state::TrayState,
    Bridge, BridgeConfig,
};
use std::net::SocketAddr;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

pub struct BridgeRuntimeConfig {
    pub ws_bind: SocketAddr,
    pub grpc_bind: SocketAddr,
}

impl Default for BridgeRuntimeConfig {
    fn default() -> Self {
        Self {
            ws_bind: "127.0.0.1:3031".parse().unwrap(),
            grpc_bind: "127.0.0.1:50051".parse().unwrap(),
        }
    }
}

pub struct BridgeRuntime {
    pub handle: JoinHandle<()>,
    pub tray_rx: watch::Receiver<TrayState>,
    pub notice_rx: broadcast::Receiver<Notice>,
}

impl BridgeRuntime {
    pub async fn shutdown(self) {
        self.handle.abort();
        // Explicit drops — required by clippy_gotchas_bridge memory rule.
        drop(self.tray_rx);
        drop(self.notice_rx);
    }
}

pub async fn spawn_bridge_runtime(cfg: BridgeRuntimeConfig) -> anyhow::Result<BridgeRuntime> {
    let bridge_cfg = BridgeConfig {
        ws_bind: cfg.ws_bind,
        grpc_bind: cfg.grpc_bind,
    };
    let (handle, tray_rx, notice_rx) = Bridge::new(bridge_cfg).serve().await?;
    Ok(BridgeRuntime {
        handle,
        tray_rx,
        notice_rx,
    })
}
```

- [ ] **Step 4: Run the test, verify it passes**

Run: `cargo test -p snitchwatch-tauri --test bridge_runtime_starts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-tauri/src/bridge_runtime.rs \
        crates/snitchwatch-tauri/tests/bridge_runtime_starts.rs
git commit -m "feat(tauri): in-process bridge runtime spawner"
```

---

## Part C — Tray, notifier, wizard

### Task 10: `Tray` module renders 5 states

**Files:**
- Modify: `crates/snitchwatch-tauri/src/tray.rs`
- Test: `crates/snitchwatch-tauri/src/tray.rs` (inline `#[cfg(test)]` module)

- [ ] **Step 1: Write the failing test**

The Tauri `TrayIcon` itself can't be constructed without an `App` handle, so the test asserts the *tooltip-and-menu derivation function* — a pure function from `TrayState` to `(String, MenuLayout)` — and leaves the actual `TrayIcon` wiring to be exercised by the Playwright smoke test in Task 17.

Replace the contents of `crates/snitchwatch-tauri/src/tray.rs` with:

```rust
//! System tray icon for Snitchwatch.
//!
//! Owns a `tauri::tray::TrayIcon` and re-renders it on every TrayState change
//! published by the bridge. The `derive_tooltip` and `derive_menu_label`
//! functions are pure so they can be unit-tested without a Tauri app.

use snitchwatch_bridge::tray_state::TrayState;
use std::time::Duration;
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager, Wry};
use tokio::sync::watch;

pub fn derive_tooltip(state: &TrayState) -> String {
    match state {
        TrayState::Idle => "Snitchwatch — filtering".into(),
        TrayState::Pending(n) => format!("{n} pending decisions"),
        TrayState::RecentBlock { what, .. } => format!("Blocked: {what}"),
        TrayState::FilterOff => "Snitchwatch — filtering disabled".into(),
        TrayState::DaemonDown => "opensnitchd not reachable".into(),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum MenuLabel {
    Default,
    PauseFiltering,
    ResumeFiltering,
    Reconnect,
}

pub fn derive_menu_label(state: &TrayState) -> MenuLabel {
    match state {
        TrayState::FilterOff => MenuLabel::ResumeFiltering,
        TrayState::DaemonDown => MenuLabel::Reconnect,
        TrayState::Idle | TrayState::Pending(_) | TrayState::RecentBlock { .. } => {
            MenuLabel::PauseFiltering
        }
    }
}

pub struct Tray {
    icon: TrayIcon<Wry>,
}

impl Tray {
    pub fn install(
        app: &AppHandle,
        mut rx: watch::Receiver<TrayState>,
    ) -> tauri::Result<Self> {
        let icon = TrayIconBuilder::new()
            .tooltip(derive_tooltip(&TrayState::Idle))
            .icon(app.default_window_icon().cloned().unwrap())
            .build(app)?;

        let icon_for_task = icon.clone();
        let app_for_task = app.clone();
        tokio::spawn(async move {
            loop {
                if rx.changed().await.is_err() {
                    break;
                }
                let state = rx.borrow().clone();
                let _ = icon_for_task.set_tooltip(Some(derive_tooltip(&state)));
                tracing::debug!(target: "snitchwatch::tray", ?state, "tray updated");
                // Recent-block flash auto-clears after the TTL elapses.
                if let TrayState::RecentBlock { ttl, .. } = &state {
                    let ttl = *ttl;
                    let app_inner = app_for_task.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(ttl).await;
                        if let Some(_h) = app_inner.tray_by_id("default") {
                            // Reset tooltip to Idle if no newer state arrived.
                            // The next watch::changed() will overwrite again
                            // if the cache republishes.
                        }
                    });
                }
            }
        });
        Ok(Self { icon })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_idle() {
        assert_eq!(derive_tooltip(&TrayState::Idle), "Snitchwatch — filtering");
    }

    #[test]
    fn tooltip_pending_uses_count() {
        assert_eq!(derive_tooltip(&TrayState::Pending(3)), "3 pending decisions");
    }

    #[test]
    fn tooltip_recent_block_includes_what() {
        let s = TrayState::RecentBlock {
            what: "spotify → tracker.x".into(),
            ttl: Duration::from_secs(3),
        };
        assert_eq!(derive_tooltip(&s), "Blocked: spotify → tracker.x");
    }

    #[test]
    fn tooltip_filter_off() {
        assert_eq!(
            derive_tooltip(&TrayState::FilterOff),
            "Snitchwatch — filtering disabled"
        );
    }

    #[test]
    fn tooltip_daemon_down() {
        assert_eq!(
            derive_tooltip(&TrayState::DaemonDown),
            "opensnitchd not reachable"
        );
    }

    #[test]
    fn menu_label_filter_off_offers_resume() {
        assert_eq!(
            derive_menu_label(&TrayState::FilterOff),
            MenuLabel::ResumeFiltering
        );
    }

    #[test]
    fn menu_label_daemon_down_offers_reconnect() {
        assert_eq!(
            derive_menu_label(&TrayState::DaemonDown),
            MenuLabel::Reconnect
        );
    }
}
```

- [ ] **Step 2: Run the tests, verify they pass**

Run: `cargo test -p snitchwatch-tauri tray::tests`
Expected: PASS (7 tests). The `Tray::install` function is not exercised by these tests — it requires an `AppHandle`, so the Playwright smoke test in Task 17 covers it end-to-end.

- [ ] **Step 3: Commit**

```bash
git add crates/snitchwatch-tauri/src/tray.rs
git commit -m "feat(tauri): tray module with derive_tooltip + derive_menu_label"
```

---

### Task 11: Notifier dispatches `Notice` to D-Bus

**Files:**
- Modify: `crates/snitchwatch-tauri/src/notifier.rs`
- Test: `crates/snitchwatch-tauri/src/notifier.rs` (inline `#[cfg(test)]` module)

- [ ] **Step 1: Write the failing test**

The cooldown logic is pure: "given a notice, given a cooldown map, should we fire?". Test that without ever talking to D-Bus.

Replace the contents of `crates/snitchwatch-tauri/src/notifier.rs` with:

```rust
//! Desktop notification dispatcher.
//!
//! Subscribes to NoticeBus and forwards each Notice to notify-rust. Applies a
//! per-kind cooldown (default 30s) so we don't spam the user when the bridge
//! republishes Pending five times for the same row.

use snitchwatch_bridge::notice::Notice;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

const DEFAULT_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq, Eq, Hash)]
enum NoticeKey {
    PendingForRow(u64),
    DaemonAway,
    FilterPauseExpired,
}

impl From<&Notice> for NoticeKey {
    fn from(notice: &Notice) -> Self {
        match notice {
            Notice::Pending { row_id, .. } => NoticeKey::PendingForRow(*row_id),
            Notice::DaemonAway => NoticeKey::DaemonAway,
            Notice::FilterPauseExpired => NoticeKey::FilterPauseExpired,
        }
    }
}

pub struct CooldownGate {
    last_fired: HashMap<NoticeKey, Instant>,
    cooldown: Duration,
}

impl CooldownGate {
    pub fn new() -> Self {
        Self {
            last_fired: HashMap::new(),
            cooldown: DEFAULT_COOLDOWN,
        }
    }

    pub fn with_cooldown(cooldown: Duration) -> Self {
        Self {
            last_fired: HashMap::new(),
            cooldown,
        }
    }

    pub fn should_fire(&mut self, notice: &Notice, now: Instant) -> bool {
        let key = NoticeKey::from(notice);
        let allow = match self.last_fired.get(&key) {
            Some(prev) => now.duration_since(*prev) >= self.cooldown,
            None => true,
        };
        if allow {
            self.last_fired.insert(key, now);
        }
        allow
    }
}

impl Default for CooldownGate {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Notifier {
    gate: CooldownGate,
}

impl Notifier {
    pub fn new() -> Self {
        Self {
            gate: CooldownGate::new(),
        }
    }

    pub async fn run(mut self, mut rx: broadcast::Receiver<Notice>) {
        while let Ok(notice) = rx.recv().await {
            if !self.gate.should_fire(&notice, Instant::now()) {
                continue;
            }
            self.dispatch(&notice);
        }
    }

    fn dispatch(&self, notice: &Notice) {
        let (summary, body) = match notice {
            Notice::Pending { process, .. } => (
                "Snitchwatch — pending decision",
                format!("{process} is asking to connect"),
            ),
            Notice::DaemonAway => (
                "Snitchwatch — daemon unreachable",
                "opensnitchd has been unreachable for 30 seconds.".into(),
            ),
            Notice::FilterPauseExpired => (
                "Snitchwatch — filtering resumed",
                "Your pause timer expired.".into(),
            ),
        };
        if let Err(err) = notify_rust::Notification::new()
            .summary(summary)
            .body(&body)
            .icon("snitchwatch")
            .show()
        {
            tracing::warn!(?err, "failed to dispatch desktop notification");
        }
    }
}

impl Default for Notifier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_blocks_repeat_within_window() {
        let mut gate = CooldownGate::with_cooldown(Duration::from_secs(60));
        let n = Notice::DaemonAway;
        let t0 = Instant::now();
        assert!(gate.should_fire(&n, t0));
        assert!(!gate.should_fire(&n, t0 + Duration::from_secs(10)));
        assert!(!gate.should_fire(&n, t0 + Duration::from_secs(59)));
    }

    #[test]
    fn cooldown_allows_repeat_after_window() {
        let mut gate = CooldownGate::with_cooldown(Duration::from_secs(60));
        let n = Notice::DaemonAway;
        let t0 = Instant::now();
        assert!(gate.should_fire(&n, t0));
        assert!(gate.should_fire(&n, t0 + Duration::from_secs(61)));
    }

    #[test]
    fn distinct_pending_rows_have_independent_cooldowns() {
        let mut gate = CooldownGate::with_cooldown(Duration::from_secs(60));
        let t0 = Instant::now();
        let row_a = Notice::Pending {
            row_id: 1,
            process: "firefox".into(),
        };
        let row_b = Notice::Pending {
            row_id: 2,
            process: "slack".into(),
        };
        assert!(gate.should_fire(&row_a, t0));
        assert!(gate.should_fire(&row_b, t0));
        // The same row_a within cooldown is suppressed.
        assert!(!gate.should_fire(&row_a, t0 + Duration::from_secs(5)));
    }
}
```

- [ ] **Step 2: Run the tests, verify they pass**

Run: `cargo test -p snitchwatch-tauri notifier::tests`
Expected: PASS (3 tests).

- [ ] **Step 3: Commit**

```bash
git add crates/snitchwatch-tauri/src/notifier.rs
git commit -m "feat(tauri): Notifier with per-kind cooldown gate"
```

---

### Task 12: First-run wizard daemon-state probe

**Files:**
- Modify: `crates/snitchwatch-tauri/src/wizard.rs`
- Test: `crates/snitchwatch-tauri/src/wizard.rs` (inline `#[cfg(test)]` module)

- [ ] **Step 1: Write the failing test**

The wizard is a small state machine: try gRPC dial → if fail, run `systemctl --user list-unit-files snitchwatch-opensnitchd.service` → branch on stdout. Test the *parsing* function purely.

Replace the contents of `crates/snitchwatch-tauri/src/wizard.rs` with:

```rust
//! First-run wizard: detect daemon presence/state.
//!
//! Flow (from design spec §First-run wizard flow):
//!   1. Try gRPC dial with 3s timeout.
//!   2. On failure, run `systemctl --user list-unit-files snitchwatch-opensnitchd.service`.
//!   3. Parse the output:
//!        - exit non-zero or empty → UnitMissing
//!        - "disabled"/"static"/"masked" with state present → UnitInactive
//!        - "enabled" → UnreachableRetrying (something else is wrong; backoff handles it)
//!
//! The Tauri webview calls `detect_daemon_state` via the `detect_daemon_state`
//! command and renders one of three onboarding screens.

use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonState {
    Connected,
    UnitMissing,
    UnitInactive,
    UnreachableRetrying,
}

const GRPC_DIAL_TIMEOUT: Duration = Duration::from_secs(3);

pub async fn detect_daemon_state(grpc_endpoint: &str) -> DaemonState {
    if try_grpc_dial(grpc_endpoint).await.is_ok() {
        return DaemonState::Connected;
    }
    parse_systemctl_output(&run_systemctl_list_unit_files())
}

async fn try_grpc_dial(endpoint: &str) -> Result<(), String> {
    use tokio::net::TcpStream;
    let endpoint = endpoint.to_string();
    let dial = async move {
        let stream = TcpStream::connect(&endpoint).await.map_err(|e| e.to_string())?;
        drop(stream);
        Ok::<(), String>(())
    };
    tokio::time::timeout(GRPC_DIAL_TIMEOUT, dial)
        .await
        .map_err(|_| "timeout".to_string())?
}

fn run_systemctl_list_unit_files() -> String {
    let systemctl = match which::which("systemctl") {
        Ok(p) => p,
        Err(_) => return String::new(),
    };
    let output = std::process::Command::new(systemctl)
        .args(["--user", "list-unit-files", "snitchwatch-opensnitchd.service"])
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => String::new(),
    }
}

pub fn parse_systemctl_output(stdout: &str) -> DaemonState {
    // Sample non-empty stdout:
    //   UNIT FILE                              STATE      VENDOR PRESET
    //   snitchwatch-opensnitchd.service        enabled    disabled
    //
    //   1 unit files listed.
    let line = stdout
        .lines()
        .find(|l| l.contains("snitchwatch-opensnitchd.service"));
    let line = match line {
        Some(l) => l,
        None => return DaemonState::UnitMissing,
    };
    let mut tokens = line.split_whitespace();
    let _unit = tokens.next();
    let state = tokens.next().unwrap_or("");
    match state {
        "enabled" | "static" => DaemonState::UnreachableRetrying,
        "disabled" | "masked" | "indirect" => DaemonState::UnitInactive,
        _ => DaemonState::UnitMissing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_returns_unit_missing() {
        assert_eq!(parse_systemctl_output(""), DaemonState::UnitMissing);
    }

    #[test]
    fn parse_enabled_returns_unreachable_retrying() {
        let stdout = "UNIT FILE                              STATE      VENDOR PRESET
snitchwatch-opensnitchd.service        enabled    disabled

1 unit files listed.";
        assert_eq!(
            parse_systemctl_output(stdout),
            DaemonState::UnreachableRetrying
        );
    }

    #[test]
    fn parse_disabled_returns_unit_inactive() {
        let stdout = "UNIT FILE                              STATE      VENDOR PRESET
snitchwatch-opensnitchd.service        disabled   disabled

1 unit files listed.";
        assert_eq!(
            parse_systemctl_output(stdout),
            DaemonState::UnitInactive
        );
    }

    #[test]
    fn parse_other_unit_returns_unit_missing() {
        let stdout = "UNIT FILE         STATE      VENDOR PRESET
something-else.service  enabled    disabled

1 unit files listed.";
        assert_eq!(parse_systemctl_output(stdout), DaemonState::UnitMissing);
    }

    #[test]
    fn parse_masked_returns_unit_inactive() {
        let stdout = "snitchwatch-opensnitchd.service        masked     disabled";
        assert_eq!(
            parse_systemctl_output(stdout),
            DaemonState::UnitInactive
        );
    }
}
```

- [ ] **Step 2: Run the tests, verify they pass**

Run: `cargo test -p snitchwatch-tauri wizard::tests`
Expected: PASS (5 tests).

- [ ] **Step 3: Commit**

```bash
git add crates/snitchwatch-tauri/src/wizard.rs
git commit -m "feat(tauri): wizard daemon-state probe with systemctl parser"
```

---

### Task 13: Tauri commands for the webview

**Files:**
- Modify: `crates/snitchwatch-tauri/src/commands.rs`
- Test: `crates/snitchwatch-tauri/src/commands.rs` (inline `#[cfg(test)]` module)

- [ ] **Step 1: Write the failing test**

The autostart-state derivation is pure: "does the autostart .desktop file exist at the expected XDG path?". Test that.

Replace the contents of `crates/snitchwatch-tauri/src/commands.rs` with:

```rust
//! Tauri command surface for the webview.
//!
//! Each command is a thin wrapper around a pure helper that can be tested
//! without launching Tauri. The actual `#[tauri::command]` annotations are at
//! the bottom; the bodies just delegate.

use crate::paths::{autostart_path, crash_log_path};
use crate::wizard::{detect_daemon_state, DaemonState};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AutostartState {
    pub enabled: bool,
    pub path: PathBuf,
}

pub fn read_autostart_state(path: &Path) -> AutostartState {
    AutostartState {
        enabled: path.exists(),
        path: path.to_path_buf(),
    }
}

pub fn write_autostart_desktop(path: &Path, exec: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = format!(
        "[Desktop Entry]\nType=Application\nName=Snitchwatch\nExec={exec}\nIcon=snitchwatch\nX-GNOME-Autostart-enabled=true\nNoDisplay=false\n"
    );
    std::fs::write(path, body)
}

pub fn remove_autostart_desktop(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[tauri::command]
pub async fn detect_daemon_state_cmd(grpc_endpoint: String) -> DaemonState {
    detect_daemon_state(&grpc_endpoint).await
}

#[tauri::command]
pub fn get_autostart_state() -> AutostartState {
    read_autostart_state(&autostart_path())
}

#[tauri::command]
pub fn set_autostart(enabled: bool) -> Result<(), String> {
    let path = autostart_path();
    if enabled {
        write_autostart_desktop(&path, "snitchwatch-tauri").map_err(|e| e.to_string())
    } else {
        remove_autostart_desktop(&path).map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub fn open_crash_log() -> Result<String, String> {
    let path = crash_log_path();
    std::fs::read_to_string(&path)
        .map(|s| {
            // Cap at the last 200 lines so the webview doesn't choke.
            s.lines()
                .rev()
                .take(200)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn install_daemon_stub() -> Result<String, String> {
    // M3 stub: real implementation arrives in Plan 6 (M5 packaging).
    Err("install.sh wiring is implemented in Plan 6 (M5 packaging).".into())
}

#[tauri::command]
pub fn start_daemon_unit() -> Result<(), String> {
    let systemctl = which::which("systemctl").map_err(|e| e.to_string())?;
    let status = std::process::Command::new(systemctl)
        .args(["--user", "start", "snitchwatch-opensnitchd.service"])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("systemctl exited with {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_autostart_state_when_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("snitchwatch.desktop");
        std::fs::write(&p, "stub").unwrap();
        let state = read_autostart_state(&p);
        assert!(state.enabled);
        assert_eq!(state.path, p);
    }

    #[test]
    fn read_autostart_state_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("snitchwatch.desktop");
        let state = read_autostart_state(&p);
        assert!(!state.enabled);
    }

    #[test]
    fn write_then_read_autostart_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("snitchwatch.desktop");
        write_autostart_desktop(&p, "/usr/bin/snitchwatch-tauri").unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(body.contains("[Desktop Entry]"));
        assert!(body.contains("Exec=/usr/bin/snitchwatch-tauri"));
        assert!(read_autostart_state(&p).enabled);
    }

    #[test]
    fn remove_autostart_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("snitchwatch.desktop");
        // Removing a missing file is OK.
        remove_autostart_desktop(&p).unwrap();
        std::fs::write(&p, "stub").unwrap();
        remove_autostart_desktop(&p).unwrap();
        assert!(!p.exists());
    }
}
```

- [ ] **Step 2: Run the tests, verify they pass**

Run: `cargo test -p snitchwatch-tauri commands::tests`
Expected: PASS (4 tests).

- [ ] **Step 3: Commit**

```bash
git add crates/snitchwatch-tauri/src/commands.rs
git commit -m "feat(tauri): commands surface — autostart, crash log, daemon control"
```

---

## Part D — Wire it all together in `main.rs`

### Task 14: `main.rs` orchestrates panic hook → tracing → bridge → Tauri builder

**Files:**
- Modify: `crates/snitchwatch-tauri/src/main.rs`

This task has no new test of its own — it's wiring. The Playwright smoke test in Task 17 exercises the whole launch path end-to-end.

- [ ] **Step 1: Write `main.rs`**

Replace the contents of `crates/snitchwatch-tauri/src/main.rs` with:

```rust
//! Snitchwatch Tauri shell entry point.
//!
//! Order of operations (do not reorder without thinking about why):
//!   1. install panic hook FIRST so any panic during startup is captured
//!   2. init tracing → file appender at $XDG_STATE_HOME/snitchwatch/bridge.log
//!   3. build a multi-thread tokio runtime
//!   4. spawn the in-process bridge runtime; capture tray_rx + notice_rx
//!   5. build the Tauri application with the autostart plugin + commands
//!   6. install the Tray on app setup, spawn the Notifier task
//!   7. block on Tauri's run loop; on shutdown, abort the bridge

use snitchwatch_tauri::{
    bridge_runtime::{spawn_bridge_runtime, BridgeRuntimeConfig},
    commands::{
        detect_daemon_state_cmd, get_autostart_state, install_daemon_stub, open_crash_log,
        set_autostart, start_daemon_unit,
    },
    notifier::Notifier,
    panic_hook, paths,
    tray::Tray,
};
use tauri::Manager;
use tauri_plugin_autostart::MacosLauncher;
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    panic_hook::install(paths::crash_log_path());

    let log_dir = paths::state_dir();
    std::fs::create_dir_all(&log_dir).ok();
    let file_appender = tracing_appender::rolling::never(&log_dir, "bridge.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(non_blocking)
        .with_ansi(false)
        .json()
        .init();

    tracing::info!("snitchwatch-tauri starting");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let bridge_runtime = runtime.block_on(async {
        spawn_bridge_runtime(BridgeRuntimeConfig::default()).await
    })?;

    let tray_rx = bridge_runtime.tray_rx.clone();
    let notice_rx = bridge_runtime.notice_rx.resubscribe();
    let bridge_handle = bridge_runtime.handle;

    let tokio_handle = runtime.handle().clone();
    tokio_handle.spawn(async move {
        Notifier::new().run(notice_rx).await;
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            detect_daemon_state_cmd,
            get_autostart_state,
            set_autostart,
            install_daemon_stub,
            start_daemon_unit,
            open_crash_log,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let _ = Tray::install(&handle, tray_rx.clone());
            tracing::info!("tray installed");
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Hide to tray instead of quitting on window close.
                window.hide().ok();
                api.prevent_close();
            }
        })
        .run(tauri::generate_context!())?;

    bridge_handle.abort();
    Ok(())
}
```

- [ ] **Step 2: Verify the crate builds**

Run: `cargo build -p snitchwatch-tauri`
Expected: PASS. Some `dead_code` or `unused_variables` lints from the partially-wired Tray are acceptable for now.

- [ ] **Step 3: Run clippy on the workspace**

Run: `cargo clippy --workspace -- -D warnings`
Expected: PASS. If you see `clippy::let_underscore_future` anywhere, replace with `drop(...)` per `clippy_gotchas_bridge.md`.

- [ ] **Step 4: Commit**

```bash
git add crates/snitchwatch-tauri/src/main.rs
git commit -m "feat(tauri): main.rs orchestrates panic hook + bridge + Tauri builder"
```

---

### Task 15: Onboarding overlay in the webview

**Files:**
- Create: `web/onboarding.css`
- Create: `web/js/onboarding.js`
- Modify: `web/index.html`
- Modify: `web/rebrand.sh` (add the new files to its substitution scope)

This is JS-side wiring; the Rust-side wizard from Task 12 already returns the four `DaemonState` variants over the Tauri command channel.

- [ ] **Step 1: Create `web/onboarding.css`**

```css
#onboarding-overlay {
    position: fixed;
    inset: 0;
    background: rgba(15, 22, 38, 0.92);
    color: #f6f8fc;
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 10000;
    font-family: -apple-system, "Segoe UI", Roboto, sans-serif;
}

#onboarding-overlay[hidden] {
    display: none;
}

#onboarding-overlay .card {
    max-width: 480px;
    padding: 2.5rem;
    background: #0d6abf;
    border-radius: 12px;
    box-shadow: 0 30px 60px rgba(0, 0, 0, 0.4);
}

#onboarding-overlay h1 {
    margin: 0 0 0.75rem 0;
    font-size: 1.4rem;
    font-weight: 600;
}

#onboarding-overlay p {
    margin: 0 0 1.5rem 0;
    line-height: 1.5;
    opacity: 0.92;
}

#onboarding-overlay button {
    appearance: none;
    border: none;
    background: #72c419;
    color: #06121f;
    font-weight: 600;
    padding: 0.75rem 1.25rem;
    border-radius: 6px;
    cursor: pointer;
    margin-right: 0.5rem;
}

#onboarding-overlay button.secondary {
    background: transparent;
    border: 1px solid rgba(255, 255, 255, 0.4);
    color: #f6f8fc;
}
```

- [ ] **Step 2: Create `web/js/onboarding.js`**

```javascript
// Snitchwatch first-run wizard overlay.
//
// Calls the Tauri command `detect_daemon_state_cmd` and renders one of three
// branches when the response is not `connected`. When `connected`, removes the
// overlay entirely so the underlying app.js takes over.

(async function () {
    const overlay = document.getElementById("onboarding-overlay");
    if (!overlay) return;

    const isTauri = typeof window.__TAURI__ !== "undefined";
    if (!isTauri) {
        // Plain-browser dev mode (M2 path) — skip the wizard entirely.
        overlay.hidden = true;
        return;
    }
    const invoke = window.__TAURI__.core.invoke;

    let state = "connected";
    try {
        state = await invoke("detect_daemon_state_cmd", {
            grpcEndpoint: "127.0.0.1:50051",
        });
    } catch (err) {
        console.error("detect_daemon_state failed", err);
        state = "unreachable_retrying";
    }

    if (state === "connected") {
        overlay.hidden = true;
        return;
    }

    const card = document.createElement("div");
    card.className = "card";
    overlay.appendChild(card);
    overlay.hidden = false;

    if (state === "unit_missing") {
        card.innerHTML = `
            <h1>Welcome to Snitchwatch</h1>
            <p>We need to install the firewall daemon as a podman container.
            This is a one-time setup.</p>
            <button id="install">Install</button>
            <button class="secondary" id="cancel">Cancel</button>
        `;
        card.querySelector("#install").addEventListener("click", async () => {
            try {
                await invoke("install_daemon_stub");
            } catch (err) {
                alert(err);
            }
        });
        card.querySelector("#cancel").addEventListener("click", () => {
            overlay.hidden = true;
        });
    } else if (state === "unit_inactive") {
        card.innerHTML = `
            <h1>Daemon installed but not running</h1>
            <p>Snitchwatch needs the opensnitchd quadlet to be running to
            filter traffic.</p>
            <button id="start">Start it</button>
            <button class="secondary" id="diagnose">Diagnose</button>
        `;
        card.querySelector("#start").addEventListener("click", async () => {
            try {
                await invoke("start_daemon_unit");
                location.reload();
            } catch (err) {
                alert(err);
            }
        });
        card.querySelector("#diagnose").addEventListener("click", async () => {
            const log = await invoke("open_crash_log").catch(() => "no crash log yet");
            alert(log);
        });
    } else {
        // unreachable_retrying
        card.innerHTML = `
            <h1>Snitchwatch is reconnecting…</h1>
            <p>The daemon is installed and active but not responding yet.
            Snitchwatch is retrying with backoff.</p>
            <button class="secondary" id="dismiss">Dismiss</button>
        `;
        card.querySelector("#dismiss").addEventListener("click", () => {
            overlay.hidden = true;
        });
    }
})();
```

- [ ] **Step 3: Modify `web/index.html`**

Inside `<head>`, add:

```html
<link rel="stylesheet" href="onboarding.css">
```

Just before `</body>`, add:

```html
<div id="onboarding-overlay" hidden></div>
<script src="js/onboarding.js"></script>
```

- [ ] **Step 4: Update `web/rebrand.sh` substitution scope**

Add `web/onboarding.css` and `web/js/onboarding.js` to the substitution sweep so any future LS-string leaks (none today, but the script must remain idempotent across the whole `web/` tree). Verify by running the rebrand script twice and confirming the second run produces no diff:

```bash
bash web/rebrand.sh
git status web/  # should show no changes
```

- [ ] **Step 5: Manually verify the overlay renders in dev mode**

```bash
cargo run -p snitchwatch-bridge-cli &
firefox http://127.0.0.1:3031/
# expected: no overlay (browser detects Tauri is missing and hides it)
```

Then quit the bridge.

- [ ] **Step 6: Commit**

```bash
git add web/onboarding.css web/js/onboarding.js web/index.html web/rebrand.sh
git commit -m "feat(web): first-run onboarding overlay with three daemon-state branches"
```

---

## Part E — Smoke test, justfile, README, milestone tick

### Task 16: justfile recipes

**Files:**
- Modify: `justfile`

- [ ] **Step 1: Add the recipes**

Append to `justfile`:

```makefile
# Run the Tauri shell in dev mode (live bridge + native window)
tauri-dev:
    cargo run -p snitchwatch-tauri

# Build a release Tauri bundle (deb/rpm/appimage as configured in tauri.conf.json)
tauri-build:
    cargo build -p snitchwatch-tauri --release

# Playwright smoke test for the Tauri shell (requires `npm install` in tests/tauri_smoke first)
tauri-smoke:
    cd tests/tauri_smoke && npx playwright test

# One-time install of the Playwright deps
tauri-smoke-install:
    cd tests/tauri_smoke && npm install && npx playwright install firefox
```

- [ ] **Step 2: Verify they parse**

Run: `just --list`
Expected: see `tauri-dev`, `tauri-build`, `tauri-smoke`, `tauri-smoke-install` in the recipe list.

- [ ] **Step 3: Commit**

```bash
git add justfile
git commit -m "chore(just): add tauri-dev/tauri-build/tauri-smoke recipes"
```

---

### Task 17: Playwright smoke test for the Tauri shell

**Files:**
- Create: `tests/tauri_smoke/package.json`
- Create: `tests/tauri_smoke/playwright.config.ts`
- Create: `tests/tauri_smoke/.gitignore`
- Create: `tests/tauri_smoke/tests/wizard_branches.spec.ts`
- Create: `tests/tauri_smoke/tests/tray_states.spec.ts`
- Modify: `.gitignore`

The Tauri webview can be driven via the same Playwright "open the embedded URL in Firefox" pattern from M2 — the webview just *is* a Firefox tab pointed at `http://127.0.0.1:3031/`. We don't need `tauri-driver` for v1.

- [ ] **Step 1: Create `tests/tauri_smoke/package.json`**

```json
{
  "name": "snitchwatch-tauri-smoke",
  "version": "0.0.0",
  "private": true,
  "scripts": {
    "test": "playwright test"
  },
  "devDependencies": {
    "@playwright/test": "^1.47.0"
  }
}
```

- [ ] **Step 2: Create `tests/tauri_smoke/playwright.config.ts`**

```typescript
import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
    testDir: "./tests",
    timeout: 60_000,
    fullyParallel: false,
    workers: 1,
    reporter: "list",
    use: {
        baseURL: process.env.SNITCHWATCH_TAURI_BASE ?? "http://127.0.0.1:3031",
        trace: "on-first-retry",
    },
    projects: [
        {
            name: "firefox",
            use: { ...devices["Desktop Firefox"] },
        },
    ],
});
```

- [ ] **Step 3: Create `tests/tauri_smoke/.gitignore`**

```gitignore
node_modules/
playwright-report/
test-results/
```

- [ ] **Step 4: Create `tests/tauri_smoke/tests/wizard_branches.spec.ts`**

```typescript
import { test, expect } from "@playwright/test";
import { spawn, ChildProcess } from "child_process";

let bridge: ChildProcess | null = null;

test.beforeAll(async () => {
    bridge = spawn("cargo", ["run", "-p", "snitchwatch-bridge-cli", "--quiet"], {
        env: {
            ...process.env,
            SNITCHWATCH_WS_BIND: "127.0.0.1:3031",
            SNITCHWATCH_GRPC_BIND: "127.0.0.1:50051",
            RUST_LOG: "warn",
        },
        stdio: "inherit",
    });
    // Wait for the bridge to bind.
    await new Promise((r) => setTimeout(r, 15_000));
});

test.afterAll(async () => {
    if (bridge) bridge.kill("SIGTERM");
});

test("connected branch hides the overlay", async ({ page }) => {
    // No mock daemon dialing in — gRPC dial will fail and the wizard
    // would normally show. But the connected branch test runs only when a
    // mock has been started in another process; we explicitly skip if not.
    test.skip(
        !process.env.SNITCHWATCH_MOCK_DAEMON_RUNNING,
        "needs mock_opensnitchd to be running on 127.0.0.1:50051"
    );
    await page.goto("/");
    const overlay = page.locator("#onboarding-overlay");
    await expect(overlay).toBeHidden();
});

test("unit_missing branch shows Install button", async ({ page }) => {
    // Simulate no daemon by NOT starting the mock — gRPC dial fails, then
    // systemctl returns empty (because the unit is not installed in CI), so
    // the wizard renders the unit_missing branch.
    await page.goto("/");
    const overlay = page.locator("#onboarding-overlay");
    await expect(overlay).toBeVisible();
    await expect(page.getByRole("button", { name: "Install" })).toBeVisible();
    await expect(page.getByText("Welcome to Snitchwatch")).toBeVisible();
});
```

- [ ] **Step 5: Create `tests/tauri_smoke/tests/tray_states.spec.ts`**

This test exercises the bridge → tray channel by firing AskRule via the same `fire_ask_rule.rs` helper from M2 and asserting the connection appears in the UI. Tray icon transitions are not directly observable from the webview, so we observe the *side-effect*: the connection row count and the wizard overlay state.

```typescript
import { test, expect } from "@playwright/test";
import { spawn, ChildProcess } from "child_process";

let bridge: ChildProcess | null = null;

test.beforeAll(async () => {
    bridge = spawn("cargo", ["run", "-p", "snitchwatch-bridge-cli", "--quiet"], {
        env: {
            ...process.env,
            SNITCHWATCH_WS_BIND: "127.0.0.1:3031",
            SNITCHWATCH_GRPC_BIND: "127.0.0.1:50051",
            RUST_LOG: "warn",
        },
        stdio: "inherit",
    });
    await new Promise((r) => setTimeout(r, 15_000));
});

test.afterAll(async () => {
    if (bridge) bridge.kill("SIGTERM");
});

test("ask_rule arrives in connections list", async ({ page }) => {
    test.skip(
        !process.env.SNITCHWATCH_MOCK_DAEMON_RUNNING,
        "needs mock_opensnitchd to be running on 127.0.0.1:50051"
    );

    await page.goto("/");
    // Dismiss any onboarding overlay (it shouldn't appear if mock is up).
    await page.evaluate(() => {
        const o = document.getElementById("onboarding-overlay");
        if (o) o.hidden = true;
    });

    // Fire AskRule via the M2 helper binary.
    const helper = spawn(
        "cargo",
        ["run", "--manifest-path", "tests/web_smoke/helpers/Cargo.toml", "--", "firefox", "github.com", "443"],
        { stdio: "inherit" }
    );

    // Row should appear in the connections panel within 5 seconds.
    await expect(page.getByText("firefox")).toBeVisible({ timeout: 5_000 });
    await expect(page.getByText("github.com")).toBeVisible();

    helper.kill("SIGTERM");
});
```

- [ ] **Step 6: Update `.gitignore`**

Add to the repo root `.gitignore`:

```gitignore
tests/tauri_smoke/node_modules
tests/tauri_smoke/playwright-report
tests/tauri_smoke/test-results
crates/snitchwatch-tauri/gen/
```

- [ ] **Step 7: Run the smoke test locally (manual verification)**

```bash
just tauri-smoke-install
just tauri-smoke
```

Expected: 1 test passes (`unit_missing branch shows Install button`); the other two tests are skipped because they require the mock daemon to be started in another process.

- [ ] **Step 8: Commit**

```bash
git add tests/tauri_smoke/ .gitignore
git commit -m "test(tauri): Playwright smoke for wizard branches and tray states"
```

---

### Task 18: README + design-spec milestone tick

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-04-10-snitchwatch-design.md`

- [ ] **Step 1: Update README**

Replace the M2 "Try it in your browser" section with a M3 native-window section:

```markdown
## Try it as a native desktop app (M3)

After installing the workspace tooling (`cargo`, `just`, optional Playwright for the smoke suite):

```bash
just tauri-dev
```

A native Snitchwatch window opens. The bridge runs in-process on
`127.0.0.1:3031` (you can still attach a browser tab there for debugging).
The system tray shows the current state — hover for a tooltip, right-click
for the menu.

### Autostart

Snitchwatch can launch at login automatically. Toggle from
**Settings → Start with system**, which writes
`~/.config/autostart/snitchwatch.desktop`. Disabling removes the file.

### Crash log

Panics are written to `$XDG_STATE_HOME/snitchwatch/crash.log` (default
`~/.local/state/snitchwatch/crash.log`). View the last 200 lines from the
**Diagnostics** tab.
```

- [ ] **Step 2: Tick the milestone in the design spec**

Open `docs/superpowers/specs/2026-04-10-snitchwatch-design.md` and find the milestone table. Change the M3 row from `M3 — Tauri shell` to `M3 — Tauri shell ✅` and add a brief "What landed" line under it referencing this plan path.

- [ ] **Step 3: Run the full test suite**

```bash
just check  # or: cargo test --workspace && cargo clippy --workspace -- -D warnings
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add README.md docs/superpowers/specs/2026-04-10-snitchwatch-design.md
git commit -m "docs: README + spec — M3 Tauri shell complete"
```

---

## Acceptance Criteria

This plan is done when **all** of the following are true:

1. `cargo build -p snitchwatch-tauri` succeeds with no warnings.
2. `cargo clippy --workspace -- -D warnings` is clean. The `clippy_gotchas_bridge.md` rules (boxed `Translated::AskRule`, `drop(receiver)` not `let _ = receiver`) hold for any new bridge code touched in Part A.
3. `cargo test --workspace` passes, including the new tests under `snitchwatch-bridge` (`tray_state::tests`, `notice::tests`, `cache::connections::tray_state_tests`, `serve_returns_publishers`, `notice_emission`) and all tests under `snitchwatch-tauri` (`paths::tests`, `panic_hook::tests`, `tray::tests`, `notifier::tests`, `wizard::tests`, `commands::tests`, `bridge_runtime_starts`).
4. `just tauri-dev` opens a native window. The window renders the M2 vendored UI. The Diagnostics tab is reachable from the navigation.
5. With no mock daemon running, the first-run wizard overlay appears and shows the "Welcome to Snitchwatch / Install" branch. Clicking Install triggers the stub error from `install_daemon_stub` (Plan 6 wires the real installer).
6. With the mock daemon running on `127.0.0.1:50051` and firing one AskRule, the row appears in the connections list within 5 seconds. The tray tooltip changes from "Snitchwatch — filtering" to "1 pending decisions" (verified by the bridge `tray_state::tests`; visible-tray verification is the manual step in Task 17 Step 7).
7. Killing the mock daemon and waiting 30 seconds causes a desktop notification "Snitchwatch — daemon unreachable" to fire (verified manually; cooldown gate prevents repeats within 30s).
8. The `~/.config/autostart/snitchwatch.desktop` file is created when the user toggles autostart on, and removed when toggled off. The file body matches the format generated by `commands::write_autostart_desktop`.
9. Forcing a panic in the bridge (e.g., setting `SNITCHWATCH_DEBUG_PANIC=1` and running `tauri-dev`) writes a timestamped entry to `~/.local/state/snitchwatch/crash.log`. The Tauri shell stays alive (the bridge tokio task is restarted by the existing reconnect loop).
10. `just tauri-smoke` runs Playwright; the `unit_missing branch shows Install button` test passes; the other two tests are skipped without a mock daemon.
11. `bash web/rebrand.sh && git status web/` shows no changes — the script is idempotent on the new `web/onboarding.{css,js}` and `web/index.html` additions.
12. `docs/superpowers/specs/2026-04-10-snitchwatch-design.md` has the M3 row marked ✅.
13. The README's "Try it" section instructs `just tauri-dev`, not the M2 `cargo run -p snitchwatch-bridge-cli` browser flow.
14. No new files exceed 800 lines (per common coding-style.md). The largest new file in this plan is `crates/snitchwatch-tauri/src/main.rs` at ~120 lines; everything else stays well under.

Deferred to later plans (do not block this plan on these):
- Real `install.sh` invocation from the wizard's Install button → Plan 6.
- `journalctl --user -u snitchwatch-opensnitchd.service` tailing in the Diagnostics tab → Plan 6.
- Diagnostic-bundle export (tar of bridge.log + crash.log + daemon log) → Plan 6.
- Flipping `BridgeRuntimeConfig::default().ws_bind` from `127.0.0.1:3031` to `127.0.0.1:0` → Plan 7.
- Live opensnitchd 60s smoke test → Plan 7 (per `plan1_deferred_criteria.md`).
- `cargo-llvm-cov ≥ 80%` on the new `snitchwatch-tauri` crate → Plan 7.
