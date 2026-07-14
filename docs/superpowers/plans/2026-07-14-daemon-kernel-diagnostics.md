# Daemon/Kernel Diagnostics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Detect and surface, live inside the Snitchwatch GUI, four connection/kernel-readiness problems (opensnitchd unreachable, firewall not running, eBPF unsupported, nftables unsupported), each with actionable troubleshooting text — replacing the current "nothing visible happens" failure mode.

**Architecture:** `snitchwatch-bridge` gains a `diagnostics` module that combines a local kernel probe (eBPF/BTF, nftables) with existing daemon-reachability (`daemon_watchdog`) and newly-captured firewall status (`ClientConfig.is_firewall_running`, read on `Subscribe()`) into a `Vec<DiagnosticCheck>`. This is pushed to the GUI over a new `ServerMessage::DiagnosticsReport` WS message (existing broadcast channel, existing WS transport — no new transport). The GUI adds a `DaemonHealthModel` (cxx-qt QObject) following the existing `bridge_dispatch`/`interests_*`/`spawn_feed` model pattern, a warning banner in `main.qml` (mirroring the existing `bridgeBanner` `Kirigami.InlineMessage`), and a new `DaemonHealthPage.qml` listing all four checks with troubleshooting text and a manual recheck button.

**Tech Stack:** Rust (tokio, tonic, serde), cxx-qt, QML/Kirigami — all matching this repo's existing stack, no new dependencies.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-07-14-daemon-kernel-diagnostics-design.md`.
- No new external crate dependencies — kernel probing uses only `std::fs`/`std::path`/`std::env` (matches `SystemInspector`'s `RealSystem` impl, which also avoids extra deps).
- `CheckStatus::Failed`'s `detail` field must be an owned `String` (not `&'static str`) — the same `ServerMessage` type is `Deserialize`d GUI-side from arbitrary runtime JSON strings, not just `Serialize`d bridge-side, so a `'static` borrow won't round-trip.
- Follow this repo's existing trait-based DI test-double convention exactly (`SystemInspector`/`MockInspector`, `SystemFacts`/`SyntheticFacts`/`FakeFacts`): a `KernelProbe` trait, a real impl, a unit-test-visible fake in `#[cfg(test)] mod testing`, and a **separate** fake struct defined directly inside any integration test file that needs one (integration tests can't see `#[cfg(test)]` doubles from the library crate).
- `DaemonHealthPage.qml` is the correct new filename — `DiagnosticsPage.qml` already exists (unrelated "Settings & Diagnostics" / `SettingsController`) and must not be touched or renamed.
- Test everything via `tests/mock_opensnitchd` / `MockOpensnitchd`, never a real `opensnitchd` — per `CLAUDE.md`'s "Reproduction paths" convention.
- `just check` (workspace default-members) and `just test-bridge` must pass after every bridge-side task; Kirigami-side tasks are verified with `cargo test -p snitchwatch-kirigami` under `QT_QPA_PLATFORM=offscreen` (not part of default-members, per `CLAUDE.md`).

---

### Task 1: `CheckKind`/`CheckStatus`/`DiagnosticCheck` types + new protocol messages

**Files:**
- Modify: `crates/snitchwatch-bridge/src/ws_messages.rs`
- Test: same file, `#[cfg(test)] mod tests` (existing module in this file)

**Interfaces:**
- Produces: `pub enum CheckKind { DaemonReachable, FirewallRunning, EbpfSupport, NftablesSupport }`, `pub enum CheckStatus { Ok, Failed { detail: String }, Unknown }`, `pub struct DiagnosticCheck { pub kind: CheckKind, pub status: CheckStatus }`, `ServerMessage::DiagnosticsReport { checks: Vec<DiagnosticCheck> }`, `ClientMessage::RecheckDiagnostics`.

- [ ] **Step 1: Write the failing test**

Add to `ws_messages.rs`'s existing `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn diagnostics_report_round_trips() {
        let msg = ServerMessage::DiagnosticsReport {
            checks: vec![
                DiagnosticCheck {
                    kind: CheckKind::DaemonReachable,
                    status: CheckStatus::Ok,
                },
                DiagnosticCheck {
                    kind: CheckKind::EbpfSupport,
                    status: CheckStatus::Failed {
                        detail: "no BTF".to_string(),
                    },
                },
                DiagnosticCheck {
                    kind: CheckKind::FirewallRunning,
                    status: CheckStatus::Unknown,
                },
            ],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"action\":\"diagnosticsReport\""));
        let round_tripped: ServerMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped, msg);
    }

    #[test]
    fn recheck_diagnostics_round_trips() {
        let msg = ClientMessage::RecheckDiagnostics;
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"action\":\"recheckDiagnostics\""));
        let round_tripped: ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped, msg);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge --lib ws_messages -- diagnostics_report_round_trips recheck_diagnostics_round_trips`
Expected: FAIL with "cannot find type `CheckKind`" (or similar — the types don't exist yet).

- [ ] **Step 3: Write minimal implementation**

Add near `ConnectionsStatus` in `ws_messages.rs` (same file, module-level, before the `ServerMessage`/`ClientMessage` enums):

```rust
/// Which readiness/connectivity property a diagnostic check covers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    DaemonReachable,
    FirewallRunning,
    EbpfSupport,
    NftablesSupport,
}

/// Result of one diagnostic check. `Unknown` covers "can't assess yet"
/// (e.g. opensnitchd connected but hasn't sent a `ClientConfig` yet) —
/// never reported as `Ok` or `Failed` when the bridge genuinely doesn't
/// know.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Failed { detail: String },
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticCheck {
    pub kind: CheckKind,
    pub status: CheckStatus,
}
```

Add a new variant to `ServerMessage` (insert alongside the other variants, e.g. after `SetAboutInfo`):

```rust
    DiagnosticsReport {
        checks: Vec<DiagnosticCheck>,
    },
```

Add a new variant to `ClientMessage` (insert alongside the other variants, e.g. after `SetFilteringPaused`):

```rust
    RecheckDiagnostics,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge --lib ws_messages -- diagnostics_report_round_trips recheck_diagnostics_round_trips`
Expected: PASS (2 passed).

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/ws_messages.rs
git commit -m "feat(bridge): add DiagnosticsReport/RecheckDiagnostics protocol messages"
```

---

### Task 2: `KernelProbe` trait + local eBPF/nftables checks

**Files:**
- Create: `crates/snitchwatch-bridge/src/diagnostics/mod.rs`
- Create: `crates/snitchwatch-bridge/src/diagnostics/kernel_probe.rs`
- Modify: `crates/snitchwatch-bridge/src/lib.rs` (add `pub mod diagnostics;`)

**Interfaces:**
- Consumes: `CheckKind`, `CheckStatus`, `DiagnosticCheck` from `crate::ws_messages` (Task 1).
- Produces: `pub trait KernelProbe: Send + Sync { fn btf_vmlinux_exists(&self) -> bool; fn nft_on_path(&self) -> bool; fn nf_tables_module_loaded(&self) -> bool; }`, `pub struct RealKernelProbe;` (impls the trait), `pub fn local_checks(probe: &dyn KernelProbe) -> Vec<DiagnosticCheck>` (returns exactly the `EbpfSupport` and `NftablesSupport` checks — daemon/firewall checks are Task 3/4's job).

- [ ] **Step 1: Write the failing test**

Create `crates/snitchwatch-bridge/src/diagnostics/kernel_probe.rs`:

```rust
//! Local kernel-readiness probes for opensnitchd's eBPF process monitor and
//! nftables firewall backend. Pure host inspection, no opensnitchd
//! involvement — this is what lets diagnostics work even when opensnitchd
//! never connects at all.

use std::path::Path;

pub trait KernelProbe: Send + Sync {
    /// BTF (BPF Type Format) availability — required for the eBPF CO-RE
    /// approach opensnitchd's default `ProcMonitorMethod: ebpf` uses.
    fn btf_vmlinux_exists(&self) -> bool;
    /// Whether the `nft` binary is reachable on `$PATH`.
    fn nft_on_path(&self) -> bool;
    /// Whether `nf_tables` appears in `/proc/modules` (loaded or built-in
    /// modules both show up there).
    fn nf_tables_module_loaded(&self) -> bool;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RealKernelProbe;

impl KernelProbe for RealKernelProbe {
    fn btf_vmlinux_exists(&self) -> bool {
        Path::new("/sys/kernel/btf/vmlinux").exists()
    }

    fn nft_on_path(&self) -> bool {
        let Some(paths) = std::env::var_os("PATH") else {
            return false;
        };
        std::env::split_paths(&paths).any(|dir| dir.join("nft").is_file())
    }

    fn nf_tables_module_loaded(&self) -> bool {
        std::fs::read_to_string("/proc/modules")
            .map(|contents| contents.lines().any(|line| line.starts_with("nf_tables ")))
            .unwrap_or(false)
    }
}

#[cfg(test)]
pub mod testing {
    use super::KernelProbe;

    /// Unit-test-visible fake. Mirrors `scanner-core`'s `MockInspector` /
    /// `scanner-privileged`'s `SyntheticFacts` pattern — builder-style,
    /// every field explicit so a test can't accidentally rely on an
    /// unset-but-truthy default.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct FakeKernelProbe {
        pub btf: bool,
        pub nft_on_path: bool,
        pub nf_tables_loaded: bool,
    }

    impl FakeKernelProbe {
        pub fn all_ok() -> Self {
            Self {
                btf: true,
                nft_on_path: true,
                nf_tables_loaded: true,
            }
        }
    }

    impl KernelProbe for FakeKernelProbe {
        fn btf_vmlinux_exists(&self) -> bool {
            self.btf
        }
        fn nft_on_path(&self) -> bool {
            self.nft_on_path
        }
        fn nf_tables_module_loaded(&self) -> bool {
            self.nf_tables_loaded
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakeKernelProbe;
    use super::*;

    #[test]
    fn all_ok_probe_reports_all_true() {
        let probe = FakeKernelProbe::all_ok();
        assert!(probe.btf_vmlinux_exists());
        assert!(probe.nft_on_path());
        assert!(probe.nf_tables_module_loaded());
    }

    #[test]
    fn default_probe_reports_all_false() {
        let probe = FakeKernelProbe::default();
        assert!(!probe.btf_vmlinux_exists());
        assert!(!probe.nft_on_path());
        assert!(!probe.nf_tables_module_loaded());
    }
}
```

Create `crates/snitchwatch-bridge/src/diagnostics/mod.rs` with the assembly function and its test:

```rust
//! Daemon/kernel readiness diagnostics — combines local kernel probing
//! (this module's `local_checks`) with daemon-reachability and
//! firewall-status signals (assembled by `DiagnosticsCtx` in this same
//! module, wired up in Task 3/4) into the `DiagnosticCheck` list the GUI
//! renders.

pub mod kernel_probe;

use crate::ws_messages::{CheckKind, CheckStatus, DiagnosticCheck};
use kernel_probe::KernelProbe;

pub const EBPF_TROUBLESHOOTING: &str = "This kernel doesn't expose BTF \
    (/sys/kernel/btf/vmlinux missing), which opensnitchd's default \
    ProcMonitorMethod: ebpf requires. Either upgrade to a kernel built with \
    CONFIG_DEBUG_INFO_BTF=y, or set ProcMonitorMethod to proc in \
    opensnitchd's config as a fallback (slower, more overhead, but works on \
    any kernel).";

pub const NFTABLES_TROUBLESHOOTING: &str = "The nft firewall backend \
    opensnitchd depends on isn't available on this host. Install the \
    nftables package, and confirm the kernel wasn't built without \
    CONFIG_NF_TABLES.";

/// Runs the two local (opensnitchd-independent) checks: eBPF/BTF support
/// and nftables support. Always returns exactly these two checks, in this
/// order.
pub fn local_checks(probe: &dyn KernelProbe) -> Vec<DiagnosticCheck> {
    let ebpf_status = if probe.btf_vmlinux_exists() {
        CheckStatus::Ok
    } else {
        CheckStatus::Failed {
            detail: EBPF_TROUBLESHOOTING.to_string(),
        }
    };
    let nftables_status = if probe.nft_on_path() && probe.nf_tables_module_loaded() {
        CheckStatus::Ok
    } else {
        CheckStatus::Failed {
            detail: NFTABLES_TROUBLESHOOTING.to_string(),
        }
    };
    vec![
        DiagnosticCheck {
            kind: CheckKind::EbpfSupport,
            status: ebpf_status,
        },
        DiagnosticCheck {
            kind: CheckKind::NftablesSupport,
            status: nftables_status,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::kernel_probe::testing::FakeKernelProbe;
    use super::*;

    #[test]
    fn all_ok_probe_yields_two_ok_checks() {
        let checks = local_checks(&FakeKernelProbe::all_ok());
        assert_eq!(checks.len(), 2);
        assert!(checks.iter().all(|c| c.status == CheckStatus::Ok));
    }

    #[test]
    fn missing_btf_yields_failed_ebpf_check() {
        let probe = FakeKernelProbe {
            btf: false,
            nft_on_path: true,
            nf_tables_loaded: true,
        };
        let checks = local_checks(&probe);
        let ebpf = checks
            .iter()
            .find(|c| c.kind == CheckKind::EbpfSupport)
            .unwrap();
        assert!(matches!(ebpf.status, CheckStatus::Failed { .. }));
    }

    #[test]
    fn missing_nft_binary_yields_failed_nftables_check() {
        let probe = FakeKernelProbe {
            btf: true,
            nft_on_path: false,
            nf_tables_loaded: true,
        };
        let checks = local_checks(&probe);
        let nft = checks
            .iter()
            .find(|c| c.kind == CheckKind::NftablesSupport)
            .unwrap();
        assert!(matches!(nft.status, CheckStatus::Failed { .. }));
    }
}
```

Add `pub mod diagnostics;` to `crates/snitchwatch-bridge/src/lib.rs`'s module list (alongside `daemon_watchdog`, `grpc_server`, etc — insert alphabetically after `cache`, before `error`).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge --lib diagnostics -- --list`
Expected: FAIL to compile until Step 1's files exist — run this after creating the files but before Step 3 isn't applicable here since Step 1 already contains the implementation (probe logic is simple enough that test-first would just be testing `std::fs`/`std::env` directly). Instead: after creating both files verbatim as in Step 1, run the tests directly.

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p snitchwatch-bridge --lib diagnostics`
Expected: PASS (5 tests: 2 in `kernel_probe::tests`, 3 in `diagnostics::tests`).

- [ ] **Step 4: Commit**

```bash
git add crates/snitchwatch-bridge/src/diagnostics/ crates/snitchwatch-bridge/src/lib.rs
git commit -m "feat(bridge): add KernelProbe trait and local eBPF/nftables checks"
```

---

### Task 3: Capture firewall status on `Subscribe()`

**Files:**
- Modify: `crates/snitchwatch-bridge/src/grpc_server.rs`
- Test: same file, existing `#[cfg(test)] mod tests` (or create one if none exists at the bottom of this file — check first with `grep -n "mod tests" crates/snitchwatch-bridge/src/grpc_server.rs`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `UiService::firewall_status_handle(&self) -> Arc<StdMutex<Option<bool>>>` (mirrors the existing `last_ping_handle()` exactly).

- [ ] **Step 1: Write the failing test**

Add to `grpc_server.rs`'s test module:

```rust
    #[tokio::test]
    async fn subscribe_captures_firewall_status() {
        let (broadcast_tx, _rx) = tokio::sync::broadcast::channel(16);
        let cache = Arc::new(tokio::sync::Mutex::new(ConnectionCache::new(64)));
        let tray_pub = Arc::new(TrayStatePublisher::new());
        let notice_bus = Arc::new(NoticeBus::new());
        let filtering_paused = Arc::new(AtomicBool::new(false));
        let service = UiService::new(
            cache,
            broadcast_tx,
            tray_pub,
            notice_bus,
            filtering_paused,
        );

        let handle = service.firewall_status_handle();
        assert_eq!(*handle.lock().unwrap(), None);

        let cfg = ClientConfig {
            is_firewall_running: true,
            ..Default::default()
        };
        let _ = service.subscribe(Request::new(cfg)).await.unwrap();

        assert_eq!(*handle.lock().unwrap(), Some(true));
    }
```

(If `ConnectionCache::new` or `TrayStatePublisher::new` differ from these exact calls elsewhere in this file's existing tests, copy the construction pattern already used by a neighboring test in this same file instead — the point of this step is exercising `subscribe()` + `firewall_status_handle()`, not re-deriving unrelated setup boilerplate.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge --lib grpc_server -- subscribe_captures_firewall_status`
Expected: FAIL with "no method named `firewall_status_handle`".

- [ ] **Step 3: Write minimal implementation**

In `grpc_server.rs`, add a field to `UiService`:

```rust
    firewall_status: Arc<StdMutex<Option<bool>>>,
```

In `UiService::new`, initialize it:

```rust
            firewall_status: Arc::new(StdMutex::new(None)),
```

Add the handle getter next to `last_ping_handle`:

```rust
    pub fn firewall_status_handle(&self) -> Arc<StdMutex<Option<bool>>> {
        self.firewall_status.clone()
    }
```

Replace the `subscribe` handler body:

```rust
    async fn subscribe(
        &self,
        request: Request<ClientConfig>,
    ) -> Result<Response<ClientConfig>, Status> {
        let cfg = request.into_inner();
        info!(client = %cfg.name, version = %cfg.version, "client subscribed");
        {
            let mut guard = self
                .firewall_status
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *guard = Some(cfg.is_firewall_running);
        }
        Ok(Response::new(cfg))
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge --lib grpc_server -- subscribe_captures_firewall_status`
Expected: PASS.

- [ ] **Step 5: Run full bridge test suite to check for regressions**

Run: `just test-bridge`
Expected: all existing tests still PASS (the `UiService::new` call signature didn't change — only an internal field was added — so no other call site needs updating).

- [ ] **Step 6: Commit**

```bash
git add crates/snitchwatch-bridge/src/grpc_server.rs
git commit -m "feat(bridge): capture firewall status from opensnitchd's Subscribe()"
```

---

### Task 4: `DiagnosticsCtx` — combine daemon/firewall/kernel signals into a report

**Files:**
- Modify: `crates/snitchwatch-bridge/src/diagnostics/mod.rs`

**Interfaces:**
- Consumes: `daemon_watchdog::{is_daemon_down, DAEMON_DOWN_TIMEOUT}`, `UiService::last_ping_handle()`/`firewall_status_handle()` (Task 3), `kernel_probe::KernelProbe`/`local_checks` (Task 2).
- Produces: `pub struct DiagnosticsCtx { ... }` with `pub fn new(last_ping: Arc<StdMutex<Instant>>, firewall_status: Arc<StdMutex<Option<bool>>>, probe: Arc<dyn KernelProbe>) -> Self` and `pub fn report(&self) -> Vec<DiagnosticCheck>` — this is what Task 5 wires into the bridge's startup/watchdog/recheck triggers.

- [ ] **Step 1: Write the failing test**

Add to `diagnostics/mod.rs`'s test module:

```rust
    use crate::daemon_watchdog::DAEMON_DOWN_TIMEOUT;
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::Instant;

    #[test]
    fn report_reflects_daemon_reachable_and_firewall_running() {
        let last_ping = Arc::new(StdMutex::new(Instant::now()));
        let firewall_status = Arc::new(StdMutex::new(Some(true)));
        let probe: Arc<dyn kernel_probe::KernelProbe> =
            Arc::new(kernel_probe::testing::FakeKernelProbe::all_ok());
        let ctx = DiagnosticsCtx::new(last_ping, firewall_status, probe);

        let checks = ctx.report();
        assert_eq!(checks.len(), 4);
        let daemon = checks
            .iter()
            .find(|c| c.kind == CheckKind::DaemonReachable)
            .unwrap();
        assert_eq!(daemon.status, CheckStatus::Ok);
        let firewall = checks
            .iter()
            .find(|c| c.kind == CheckKind::FirewallRunning)
            .unwrap();
        assert_eq!(firewall.status, CheckStatus::Ok);
    }

    #[test]
    fn report_reflects_stale_ping_as_daemon_unreachable() {
        let stale = Instant::now() - (DAEMON_DOWN_TIMEOUT + std::time::Duration::from_secs(1));
        let last_ping = Arc::new(StdMutex::new(stale));
        let firewall_status = Arc::new(StdMutex::new(None));
        let probe: Arc<dyn kernel_probe::KernelProbe> =
            Arc::new(kernel_probe::testing::FakeKernelProbe::all_ok());
        let ctx = DiagnosticsCtx::new(last_ping, firewall_status, probe);

        let checks = ctx.report();
        let daemon = checks
            .iter()
            .find(|c| c.kind == CheckKind::DaemonReachable)
            .unwrap();
        assert!(matches!(daemon.status, CheckStatus::Failed { .. }));
        let firewall = checks
            .iter()
            .find(|c| c.kind == CheckKind::FirewallRunning)
            .unwrap();
        assert_eq!(firewall.status, CheckStatus::Unknown);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge --lib diagnostics -- report_reflects`
Expected: FAIL with "cannot find type `DiagnosticsCtx`".

- [ ] **Step 3: Write minimal implementation**

Add to `diagnostics/mod.rs` (below `local_checks`, above the test module):

```rust
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

pub const DAEMON_UNREACHABLE_TROUBLESHOOTING: &str = "opensnitchd isn't \
    dialing in. Confirm it's installed and running (systemctl status \
    opensnitchd), and that its Server.Address in \
    /etc/opensnitchd/default-config.json matches the bridge's \
    SNITCHWATCH_GRPC_BIND (default 127.0.0.1:50051). Check \
    /var/log/opensnitchd.log for dial errors.";

pub const FIREWALL_NOT_RUNNING_TROUBLESHOOTING: &str = "opensnitchd \
    connected but its firewall backend isn't active. Check \
    /var/log/opensnitchd.log for nftables errors; confirm nftables is \
    enabled and not conflicting with iptables/firewalld rules already on \
    the host.";

/// Combines daemon-reachability (watchdog's `last_ping` staleness),
/// opensnitchd-reported firewall status, and local kernel probes into the
/// full four-check `DiagnosticCheck` list the GUI renders.
pub struct DiagnosticsCtx {
    last_ping: Arc<StdMutex<Instant>>,
    firewall_status: Arc<StdMutex<Option<bool>>>,
    probe: Arc<dyn kernel_probe::KernelProbe>,
}

impl DiagnosticsCtx {
    pub fn new(
        last_ping: Arc<StdMutex<Instant>>,
        firewall_status: Arc<StdMutex<Option<bool>>>,
        probe: Arc<dyn kernel_probe::KernelProbe>,
    ) -> Self {
        Self {
            last_ping,
            firewall_status,
            probe,
        }
    }

    pub fn report(&self) -> Vec<DiagnosticCheck> {
        let last_ping = {
            let guard = self.last_ping.lock().unwrap_or_else(|e| e.into_inner());
            *guard
        };
        let daemon_status = if crate::daemon_watchdog::is_daemon_down(
            last_ping,
            Instant::now(),
            crate::daemon_watchdog::DAEMON_DOWN_TIMEOUT,
        ) {
            CheckStatus::Failed {
                detail: DAEMON_UNREACHABLE_TROUBLESHOOTING.to_string(),
            }
        } else {
            CheckStatus::Ok
        };

        let firewall_status = {
            let guard = self
                .firewall_status
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            match *guard {
                Some(true) => CheckStatus::Ok,
                Some(false) => CheckStatus::Failed {
                    detail: FIREWALL_NOT_RUNNING_TROUBLESHOOTING.to_string(),
                },
                None => CheckStatus::Unknown,
            }
        };

        let mut checks = vec![
            DiagnosticCheck {
                kind: CheckKind::DaemonReachable,
                status: daemon_status,
            },
            DiagnosticCheck {
                kind: CheckKind::FirewallRunning,
                status: firewall_status,
            },
        ];
        checks.extend(local_checks(self.probe.as_ref()));
        checks
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge --lib diagnostics -- report_reflects`
Expected: PASS (2 passed).

- [ ] **Step 5: Run full diagnostics module test suite**

Run: `cargo test -p snitchwatch-bridge --lib diagnostics`
Expected: PASS (7 tests total: 2 `kernel_probe::tests`, 3 `local_checks` tests, 2 `DiagnosticsCtx` tests).

- [ ] **Step 6: Commit**

```bash
git add crates/snitchwatch-bridge/src/diagnostics/mod.rs
git commit -m "feat(bridge): assemble full DiagnosticsCtx report from daemon/firewall/kernel signals"
```

---

### Task 5: Wire `DiagnosticsCtx` into `RunningBridge` — startup, watchdog, recheck, snapshot

**Files:**
- Modify: `crates/snitchwatch-bridge/src/daemon_watchdog.rs`
- Modify: `crates/snitchwatch-bridge-cli/src/lib.rs`

**Interfaces:**
- Consumes: `DiagnosticsCtx` (Task 4), `RealKernelProbe` (Task 2), `UiService::firewall_status_handle()` (Task 3), `ServerMessage::DiagnosticsReport`/`ClientMessage::RecheckDiagnostics` (Task 1).
- Produces: `daemon_watchdog::run`'s new signature (adds `diagnostics_ctx` + `broadcast_tx` params) — the next task/plan touching this function must use the new signature, not the old three-argument one.

- [ ] **Step 1: Write the failing test**

Modify `daemon_watchdog.rs`'s existing test(s) that call `run(...)` to use the new signature (the exact existing test names weren't captured verbatim during exploration — locate them with `grep -n "async fn.*test\|fn run(" crates/snitchwatch-bridge/src/daemon_watchdog.rs` and update each `run(...)` call site). Add one new test asserting the diagnostics broadcast fires on a down-transition:

```rust
    #[tokio::test]
    async fn watchdog_broadcasts_diagnostics_report_on_down_transition() {
        let stale = Instant::now() - (DAEMON_DOWN_TIMEOUT + Duration::from_secs(1));
        let last_ping = Arc::new(StdMutex::new(stale));
        let tray_pub = Arc::new(TrayStatePublisher::new());
        let cache = Arc::new(TokioMutex::new(ConnectionCache::new(64)));
        let (broadcast_tx, mut broadcast_rx) = tokio::sync::broadcast::channel(16);
        let firewall_status = Arc::new(StdMutex::new(None));
        let probe: Arc<dyn crate::diagnostics::kernel_probe::KernelProbe> =
            Arc::new(crate::diagnostics::kernel_probe::testing::FakeKernelProbe::all_ok());
        let diagnostics_ctx = Arc::new(crate::diagnostics::DiagnosticsCtx::new(
            last_ping.clone(),
            firewall_status,
            probe,
        ));

        let handle = tokio::spawn(run(
            last_ping,
            tray_pub,
            cache,
            diagnostics_ctx,
            broadcast_tx,
        ));

        let msg = tokio::time::timeout(Duration::from_secs(3), broadcast_rx.recv())
            .await
            .expect("timed out waiting for DiagnosticsReport")
            .unwrap();
        assert!(matches!(msg, ServerMessage::DiagnosticsReport { .. }));

        handle.abort();
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge --lib daemon_watchdog -- watchdog_broadcasts_diagnostics_report_on_down_transition`
Expected: FAIL to compile — `run` doesn't accept 5 arguments yet.

- [ ] **Step 3: Write minimal implementation**

Replace `daemon_watchdog.rs`'s `run` function:

```rust
pub async fn run(
    last_ping: Arc<StdMutex<Instant>>,
    tray_pub: Arc<TrayStatePublisher>,
    cache: Arc<TokioMutex<ConnectionCache>>,
    diagnostics_ctx: Arc<crate::diagnostics::DiagnosticsCtx>,
    broadcast_tx: broadcast::Sender<crate::ws_messages::ServerMessage>,
) {
    let mut interval = tokio::time::interval(WATCHDOG_TICK);
    let mut was_down = false;
    loop {
        interval.tick().await;

        let last_ping_ts = {
            let guard = last_ping.lock().unwrap_or_else(|e| e.into_inner());
            *guard
        };
        let down_now = is_daemon_down(last_ping_ts, Instant::now(), DAEMON_DOWN_TIMEOUT);

        if down_now && !was_down {
            tray_pub.set(TrayState::DaemonDown);
            let _ = broadcast_tx.send(crate::ws_messages::ServerMessage::DiagnosticsReport {
                checks: diagnostics_ctx.report(),
            });
        } else if !down_now && was_down {
            cache.lock().await.resync_tray_state();
            let _ = broadcast_tx.send(crate::ws_messages::ServerMessage::DiagnosticsReport {
                checks: diagnostics_ctx.report(),
            });
        }
        was_down = down_now;
    }
}
```

Add `use tokio::sync::broadcast;` to this file's imports if not already present (it already imports `tokio::sync::Mutex as TokioMutex`, so add `broadcast` alongside it).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge --lib daemon_watchdog`
Expected: PASS for all tests in this file, including the new one and the updated pre-existing ones.

- [ ] **Step 5: Wire into `RunningBridge::run()`**

In `crates/snitchwatch-bridge-cli/src/lib.rs`, after the gRPC server's `UiService::new(...)` construction (step 8 in the orchestration order) and before the watchdog spawn (step 9), add:

```rust
    let firewall_status = ui_service_inner.firewall_status_handle();
    let kernel_probe: Arc<dyn snitchwatch_bridge::diagnostics::kernel_probe::KernelProbe> =
        Arc::new(snitchwatch_bridge::diagnostics::kernel_probe::RealKernelProbe);
    let diagnostics_ctx = Arc::new(snitchwatch_bridge::diagnostics::DiagnosticsCtx::new(
        last_ping.clone(),
        firewall_status,
        kernel_probe,
    ));
    if broadcast_tx.receiver_count() > 0 {
        let _ = broadcast_tx.send(ServerMessage::DiagnosticsReport {
            checks: diagnostics_ctx.report(),
        });
    }
```

(`last_ping` and `broadcast_tx` are already in scope at this point in `run()` per the existing orchestration — `last_ping` is captured right after `UiService::new` via `ui_service_inner.last_ping_handle()`, per the file's existing pattern; `broadcast_tx` was created at step 1.)

Update the watchdog spawn call site to the new signature:

```rust
    let watchdog_handle = tokio::spawn(snitchwatch_bridge::daemon_watchdog::run(
        last_ping,
        tray_pub.clone(),
        cache.clone(),
        diagnostics_ctx.clone(),
        broadcast_tx.clone(),
    ));
```

In the inbound pump loop (`while let Some(msg) = inbound_rx.recv().await { ... }`), add a special-case branch before the generic `upstream::apply` handling, following the exact style of the existing `SetFilteringPaused` special-case:

```rust
            if let ClientMessage::RecheckDiagnostics = msg {
                let _ = broadcast_tx.send(ServerMessage::DiagnosticsReport {
                    checks: diagnostics_ctx.report(),
                });
                continue;
            }
```

(This closure/loop needs `diagnostics_ctx` moved/cloned into it — clone `diagnostics_ctx` before the pump task's `tokio::spawn`, same pattern already used for `broadcast_tx.clone()` into that task.)

In the `SnapshotRequested` branch (`Ok(UpstreamEffect::SnapshotRequested) => { ... }`), add the diagnostics send alongside the existing `ClearConnectionRows`/`InsertConnectionRows`/`SetBlocklists`/`SetProfiles` sends:

```rust
                    let _ = broadcast_tx.send(ServerMessage::DiagnosticsReport {
                        checks: diagnostics_ctx.report(),
                    });
```

- [ ] **Step 6: Run full bridge integration suite**

Run: `just test-bridge && cargo build -p snitchwatch-bridge-cli`
Expected: all tests PASS, crate builds cleanly.

- [ ] **Step 7: Commit**

```bash
git add crates/snitchwatch-bridge/src/daemon_watchdog.rs crates/snitchwatch-bridge-cli/src/lib.rs
git commit -m "feat(bridge): wire DiagnosticsReport into startup, watchdog transitions, recheck, and snapshot"
```

---

### Task 6: `MockOpensnitchd::subscribe_with_config` + end-to-end integration test

**Files:**
- Modify: `tests/mock_opensnitchd/src/lib.rs`
- Modify: `tests/bridge_protocol_test.rs`

**Interfaces:**
- Consumes: `snitchwatch_bridge_cli::{run, BridgeConfig}`, `ServerMessage::DiagnosticsReport` (Task 1), `MockOpensnitchd::connect` (existing).
- Produces: `MockOpensnitchd::subscribe_with_config(&mut self, cfg: ClientConfig) -> Result<ClientConfig, MockError>` — a new method other tests can also use to script arbitrary `ClientConfig` values.

- [ ] **Step 1: Write the failing test**

Add to `tests/bridge_protocol_test.rs`:

```rust
#[tokio::test]
async fn diagnostics_report_reflects_firewall_down_after_subscribe() {
    let socket_dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: socket_dir.path().join("bridge.sock"),
        cache_capacity: 1024,
    };
    let bridge = run(cfg).await.expect("bridge run failed");

    let mut ws = connect_stream(&bridge.ws_socket_path, bridge.ws_token.as_str()).await;

    let grpc_addr = bridge.grpc_addr;
    let subscribe_handle = tokio::spawn(async move {
        let mut mock = MockOpensnitchd::connect(grpc_addr).await.unwrap();
        mock.subscribe_with_config(snitchwatch_proto::protocol::ClientConfig {
            id: 1,
            name: "mock".to_string(),
            version: "mock-1.6.0".to_string(),
            is_firewall_running: false,
            ..Default::default()
        })
        .await
        .unwrap();
    });
    subscribe_handle.await.unwrap();

    ws.send(Message::Text(
        json!({ "action": "requestSnapshot" }).to_string(),
    ))
    .await
    .expect("send requestSnapshot failed");

    let mut saw_firewall_failed = false;
    for _ in 0..20 {
        let Some(Ok(Message::Text(text))) =
            tokio::time::timeout(Duration::from_secs(3), ws.next())
                .await
                .expect("timed out waiting for a WS message")
        else {
            continue;
        };
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        if v.get("action").and_then(|a| a.as_str()) == Some("diagnosticsReport") {
            let checks = v["checks"].as_array().unwrap();
            saw_firewall_failed = checks.iter().any(|c| {
                c["kind"] == "firewall_running" && c["status"]["status"] == "failed"
            });
            break;
        }
    }
    assert!(
        saw_firewall_failed,
        "expected a diagnosticsReport with a failed firewall_running check"
    );

    bridge.shutdown();
}
```

(The exact JSON key path for `status`, e.g. `c["status"]["status"]`, depends on Task 1's `#[serde(tag = "status", ...)]` choice on `CheckStatus` — verify against the actual serialized JSON with a quick `dbg!(json)` if the assertion doesn't match on first run, since internally-tagged enum serde output shape is easy to get subtly wrong on paper.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-bridge --test bridge_protocol_test -- diagnostics_report_reflects_firewall_down_after_subscribe`
Expected: FAIL to compile — `subscribe_with_config` doesn't exist yet.

- [ ] **Step 3: Write minimal implementation**

In `tests/mock_opensnitchd/src/lib.rs`, add next to the existing `subscribe`:

```rust
    /// Like [`Self::subscribe`], but takes a full `ClientConfig` the
    /// caller controls — used by tests that need to drive a specific
    /// `is_firewall_running`/`config` value through the bridge.
    pub async fn subscribe_with_config(
        &mut self,
        cfg: ClientConfig,
    ) -> Result<ClientConfig, MockError> {
        let echoed = self.client.subscribe(cfg).await?.into_inner();
        Ok(echoed)
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p snitchwatch-bridge --test bridge_protocol_test -- diagnostics_report_reflects_firewall_down_after_subscribe`
Expected: PASS. If the JSON assertion fails on shape, inspect the actual serialized `CheckStatus` output and adjust the assertion path (not the production serde attributes, unless the shape is genuinely awkward — in which case adjust Task 1's `#[serde(...)]` attribute and re-run Task 1's round-trip tests too).

- [ ] **Step 5: Run full bridge test suite**

Run: `just test-bridge`
Expected: all PASS, no regressions in existing `bridge_protocol_test.rs` scenarios.

- [ ] **Step 6: Commit**

```bash
git add tests/mock_opensnitchd/src/lib.rs tests/bridge_protocol_test.rs
git commit -m "test(bridge): add end-to-end diagnostics report coverage via MockOpensnitchd"
```

---

### Task 7: `interests_diagnostics` predicate (bridge_dispatch routing)

**Files:**
- Modify: `crates/snitchwatch-kirigami/src/bridge_dispatch.rs`

**Interfaces:**
- Consumes: `ServerMessage` (from `snitchwatch_bridge::ws_messages`, already imported in this file).
- Produces: `pub fn interests_diagnostics(msg: &ServerMessage) -> bool` — Task 8's `DaemonHealthModel` uses this as its `spawn_feed` interest predicate.

- [ ] **Step 1: Write the failing test**

Add to `bridge_dispatch.rs`'s existing `#[cfg(test)] mod tests`, following the same style as the other `*_route_only_to_*` tests in this file:

```rust
    #[test]
    fn diagnostics_route_only_to_diagnostics_report() {
        let diagnostics_msg = ServerMessage::DiagnosticsReport { checks: vec![] };
        assert!(interests_diagnostics(&diagnostics_msg));

        let other_msg = ServerMessage::ClearConnectionRows;
        assert!(!interests_diagnostics(&other_msg));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p snitchwatch-kirigami --lib bridge_dispatch -- diagnostics_route_only_to_diagnostics_report`
Expected: FAIL — `interests_diagnostics` doesn't exist. (Note: this crate isn't in `default-members`; run with `QT_QPA_PLATFORM=offscreen` set if the test binary links Qt — `bridge_dispatch.rs`'s existing `interests_*` tests are pure-Rust and shouldn't need it, but set it anyway for consistency: `QT_QPA_PLATFORM=offscreen cargo test -p snitchwatch-kirigami --lib bridge_dispatch`.)

- [ ] **Step 3: Write minimal implementation**

Add next to the other `interests_*` functions in `bridge_dispatch.rs`:

```rust
pub fn interests_diagnostics(msg: &ServerMessage) -> bool {
    matches!(msg, ServerMessage::DiagnosticsReport { .. })
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `QT_QPA_PLATFORM=offscreen cargo test -p snitchwatch-kirigami --lib bridge_dispatch`
Expected: PASS for all `interests_*` tests including the new one.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-kirigami/src/bridge_dispatch.rs
git commit -m "feat(kirigami): add interests_diagnostics bridge_dispatch predicate"
```

---

### Task 8: `DaemonHealthModel` QObject

**Files:**
- Create: `crates/snitchwatch-kirigami/src/daemon_health_model.rs`
- Modify: `crates/snitchwatch-kirigami/src/lib.rs` (register the new module — find the existing `pub mod rules_model;`-style line list and add `pub mod daemon_health_model;` alongside it, plus whatever QML-registration wiring the other models use — check `crates/snitchwatch-kirigami/src/main.rs` or wherever `rules_model`'s module is referenced for QML type registration, e.g. a `cxx_qt::init_qml_module!` or similar, and add the equivalent line for this new type; if none is found, mirror however `RulesModel` is registered by grepping `RulesModel` outside its own file first)

**Interfaces:**
- Consumes: `ServerMessage::DiagnosticsReport`, `CheckKind`, `CheckStatus` (`snitchwatch_bridge::ws_messages`), `crate::bridge_dispatch::{interests_diagnostics, spawn_feed}`, `crate::bridge_runtime::handles`, `ClientMessage::RecheckDiagnostics`.
- Produces: QML-visible `DaemonHealthModel` type with `#[qproperty(bool, has_problem)]`, `#[qproperty(QString, status_summary)]`, `#[qproperty(QString, troubleshooting_text)]`, `#[qinvokable] applyServerMessageJson(json: &QString)`, `#[qinvokable] startBridgeFeed()`, `#[qinvokable] recheck()`.

- [ ] **Step 1: Write the failing test**

Before implementing, run `grep -n "pub mod\|cxx_qt::init_qml_module\|register" crates/snitchwatch-kirigami/src/lib.rs crates/snitchwatch-kirigami/src/main.rs` to find the exact registration mechanism used for existing models (this varies by cxx-qt version/setup and wasn't captured verbatim during exploration — confirm it before writing Step 3's registration line so it matches this codebase's actual pattern instead of guessing).

Add a pure-logic unit test to the new file (testable without Qt, since it only exercises the `derive_*` helper, not the QObject macro machinery):

```rust
    #[test]
    fn derive_status_summary_ok_when_all_checks_pass() {
        let checks = vec![
            DiagnosticCheck {
                kind: CheckKind::DaemonReachable,
                status: CheckStatus::Ok,
            },
            DiagnosticCheck {
                kind: CheckKind::FirewallRunning,
                status: CheckStatus::Ok,
            },
            DiagnosticCheck {
                kind: CheckKind::EbpfSupport,
                status: CheckStatus::Ok,
            },
            DiagnosticCheck {
                kind: CheckKind::NftablesSupport,
                status: CheckStatus::Ok,
            },
        ];
        assert!(!has_problem(&checks));
        assert_eq!(troubleshooting_text(&checks), "");
    }

    #[test]
    fn derive_status_summary_flags_failed_check_with_its_detail() {
        let checks = vec![DiagnosticCheck {
            kind: CheckKind::EbpfSupport,
            status: CheckStatus::Failed {
                detail: "no BTF".to_string(),
            },
        }];
        assert!(has_problem(&checks));
        assert!(troubleshooting_text(&checks).contains("no BTF"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `QT_QPA_PLATFORM=offscreen cargo test -p snitchwatch-kirigami --lib daemon_health_model`
Expected: FAIL — module/functions don't exist yet.

- [ ] **Step 3: Write minimal implementation**

Create `crates/snitchwatch-kirigami/src/daemon_health_model.rs`:

```rust
//! Live daemon/kernel readiness status, driven by
//! `ServerMessage::DiagnosticsReport`. Mirrors `rules_model.rs`'s
//! bridge-feed wiring pattern, but exposes scalar properties (not a list
//! model) since this is a fixed four-check summary, not a growing
//! collection.

use cxx_qt_lib::QString;
use snitchwatch_bridge::ws_messages::{CheckStatus, DiagnosticCheck, ServerMessage};
use std::pin::Pin;

/// True if any check in the report is `Failed`.
fn has_problem(checks: &[DiagnosticCheck]) -> bool {
    checks
        .iter()
        .any(|c| matches!(c.status, CheckStatus::Failed { .. }))
}

/// Joins every failed check's troubleshooting detail, one per line. Empty
/// string when nothing has failed.
fn troubleshooting_text(checks: &[DiagnosticCheck]) -> String {
    checks
        .iter()
        .filter_map(|c| match &c.status {
            CheckStatus::Failed { detail } => Some(detail.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn status_summary(checks: &[DiagnosticCheck]) -> String {
    if has_problem(checks) {
        "Connection or kernel problem detected — see Daemon Health for details".to_string()
    } else {
        "Everything looks healthy".to_string()
    }
}

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    unsafe extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(bool, has_problem)]
        #[qproperty(QString, status_summary)]
        #[qproperty(QString, troubleshooting_text)]
        type DaemonHealthModel = super::DaemonHealthModelRust;

        #[qinvokable]
        #[cxx_name = "applyServerMessageJson"]
        fn apply_server_message_json(self: Pin<&mut DaemonHealthModel>, json: &QString);

        #[qinvokable]
        #[cxx_name = "startBridgeFeed"]
        fn start_bridge_feed(self: Pin<&mut DaemonHealthModel>);

        #[qinvokable]
        fn recheck(self: Pin<&mut DaemonHealthModel>);
    }

    impl cxx_qt::Threading for DaemonHealthModel {}
}

#[derive(Default)]
pub struct DaemonHealthModelRust {
    has_problem: bool,
    status_summary: QString,
    troubleshooting_text: QString,
}

impl qobject::DaemonHealthModel {
    fn apply_server_message_json(self: Pin<&mut Self>, json: &QString) {
        let text = json.to_string();
        let Ok(ServerMessage::DiagnosticsReport { checks }) =
            serde_json::from_str::<ServerMessage>(&text)
        else {
            return;
        };
        let mut this = self;
        this.as_mut().set_has_problem(has_problem(&checks));
        this.as_mut()
            .set_status_summary(QString::from(&status_summary(&checks)));
        this.as_mut()
            .set_troubleshooting_text(QString::from(&troubleshooting_text(&checks)));
    }

    fn start_bridge_feed(self: Pin<&mut Self>) {
        let Some(handles) = crate::bridge_runtime::handles() else {
            tracing::warn!("DaemonHealthModel: bridge not running; live feed disabled");
            return;
        };
        let qt_thread = self.qt_thread();
        crate::bridge_dispatch::spawn_feed(
            &handles,
            "DaemonHealthModel",
            crate::bridge_dispatch::interests_diagnostics,
            move |_msg, json| {
                let _ = qt_thread.queue(move |qobject| {
                    qobject.apply_server_message_json(&QString::from(&json));
                });
            },
        );
    }

    fn recheck(self: Pin<&mut Self>) {
        let Some(handles) = crate::bridge_runtime::handles() else {
            return;
        };
        let inbound = handles.inbound_tx();
        handles.runtime().spawn(async move {
            let _ = inbound
                .send(snitchwatch_bridge::ws_messages::ClientMessage::RecheckDiagnostics)
                .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::ws_messages::CheckKind;

    #[test]
    fn derive_status_summary_ok_when_all_checks_pass() {
        let checks = vec![
            DiagnosticCheck {
                kind: CheckKind::DaemonReachable,
                status: CheckStatus::Ok,
            },
            DiagnosticCheck {
                kind: CheckKind::FirewallRunning,
                status: CheckStatus::Ok,
            },
            DiagnosticCheck {
                kind: CheckKind::EbpfSupport,
                status: CheckStatus::Ok,
            },
            DiagnosticCheck {
                kind: CheckKind::NftablesSupport,
                status: CheckStatus::Ok,
            },
        ];
        assert!(!has_problem(&checks));
        assert_eq!(troubleshooting_text(&checks), "");
    }

    #[test]
    fn derive_status_summary_flags_failed_check_with_its_detail() {
        let checks = vec![DiagnosticCheck {
            kind: CheckKind::EbpfSupport,
            status: CheckStatus::Failed {
                detail: "no BTF".to_string(),
            },
        }];
        assert!(has_problem(&checks));
        assert!(troubleshooting_text(&checks).contains("no BTF"));
    }
}
```

**Note for the implementer:** the `#[cxx_qt::bridge]` macro block above follows `rules_model.rs`'s structure as closely as possible from what was verified, but `rules_model.rs` is a `QAbstractListModel` (has `#[base = QAbstractListModel]` and list-model plumbing this type doesn't need) — cross-check this file's exact macro syntax (the `unsafe extern "C++"`/`unsafe extern "RustQt"` block shape, `include!` paths, whether `#[qml_element]` needs a module/URI argument in this cxx-qt version) against a **second**, non-list existing model if one exists in this codebase (e.g. `TrayController`, referenced during exploration as scalar-`qproperty`-based) before treating this as final — `rules_model.rs` alone confirms the list-model shape, not the plain-QObject shape.

- [ ] **Step 4: Register the module**

Add `pub mod daemon_health_model;` to `crates/snitchwatch-kirigami/src/lib.rs`'s module list, and add whatever QML type registration line the exploration in Step 1 found (matching exactly how `RulesModel`/`TrayController` are registered — e.g. a `cxx-qt-build` `build.rs` entry, or automatic via `#[qml_element]` alone, depending on this project's cxx-qt setup).

- [ ] **Step 5: Run test to verify it passes**

Run: `QT_QPA_PLATFORM=offscreen cargo test -p snitchwatch-kirigami --lib daemon_health_model`
Expected: PASS (2 pure-logic tests; the `#[cxx_qt::bridge]` macro portion is exercised by Task 9's QML test, not here).

- [ ] **Step 6: Full crate build check**

Run: `cargo build -p snitchwatch-kirigami`
Expected: builds cleanly (requires system Qt6/KF6 dev packages per `CLAUDE.md` — if this environment lacks them, this step is deferred to a real-hardware/CI check per this repo's existing convention, not a blocker for merging the Rust-only logic).

- [ ] **Step 7: Commit**

```bash
git add crates/snitchwatch-kirigami/src/daemon_health_model.rs crates/snitchwatch-kirigami/src/lib.rs
git commit -m "feat(kirigami): add DaemonHealthModel QObject driven by DiagnosticsReport"
```

---

### Task 9: Banner + `DaemonHealthPage.qml`

**Files:**
- Modify: `crates/snitchwatch-kirigami/qml/main.qml`
- Create: `crates/snitchwatch-kirigami/qml/DaemonHealthPage.qml`

**Interfaces:**
- Consumes: `DaemonHealthModel` (Task 8) — `hasProblem` (bool), `statusSummary` (string), `troubleshootingText` (string), `startBridgeFeed()`, `recheck()`.

- [ ] **Step 1: Instantiate the model and start its feed in `main.qml`**

Near wherever other models (`rulesModel`, `trafficModel`, etc.) are instantiated at the top level of `main.qml`, add:

```qml
    DaemonHealthModel {
        id: daemonHealthModel
        Component.onCompleted: startBridgeFeed()
    }
```

- [ ] **Step 2: Add the warning banner**

Immediately after the existing `bridgeBanner` `Kirigami.InlineMessage` block (lines ~195-210), add:

```qml
    // Daemon/kernel readiness banner — distinct from bridgeBanner above:
    // bridgeBanner covers the bridge PROCESS failing to start; this covers
    // the bridge running fine but opensnitchd/kernel prerequisites not
    // being met. See docs/superpowers/specs/2026-07-14-daemon-kernel-diagnostics-design.md.
    Kirigami.InlineMessage {
        id: daemonHealthBanner
        z: 999
        anchors {
            top: bridgeBanner.visible ? bridgeBanner.bottom : parent.top
            left: parent.left
            right: parent.right
            margins: Kirigami.Units.smallSpacing
        }
        type: Kirigami.MessageType.Warning
        visible: daemonHealthModel.hasProblem
        text: daemonHealthModel.statusSummary
        actions: [
            Kirigami.Action {
                text: "Details"
                onTriggered: root.pageStack.replace(daemonHealthPageComponent)
            }
        ]
    }
```

- [ ] **Step 3: Register the page component**

Near the other `Component { id: xPageComponent; XPage {} }` declarations, add:

```qml
    Component {
        id: daemonHealthPageComponent
        DaemonHealthPage {
            model: daemonHealthModel
        }
    }
```

- [ ] **Step 4: Add a drawer navigation entry**

In the `Kirigami.GlobalDrawer.actions` list (lines ~382-424), add a new `Kirigami.Action` entry — pick a `text`/icon that doesn't collide with the existing "Settings & Diagnostics" entry, e.g.:

```qml
                Kirigami.Action {
                    text: "Daemon Health"
                    icon.name: "network-connect"
                    onTriggered: root.pageStack.replace(daemonHealthPageComponent)
                }
```

- [ ] **Step 5: Create `DaemonHealthPage.qml`**

```qml
import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

Kirigami.ScrollablePage {
    id: page
    title: "Daemon Health"

    required property var model

    ColumnLayout {
        width: page.width
        spacing: Kirigami.Units.largeSpacing

        Kirigami.InlineMessage {
            Layout.fillWidth: true
            type: Kirigami.MessageType.Warning
            visible: page.model.hasProblem
            text: page.model.statusSummary
        }

        Controls.Label {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            visible: page.model.hasProblem
            text: page.model.troubleshootingText
        }

        Controls.Label {
            Layout.fillWidth: true
            visible: !page.model.hasProblem
            text: "opensnitchd is reachable, its firewall is running, and this \
                   host's kernel supports eBPF process monitoring and \
                   nftables — everything opensnitchd needs."
        }

        Controls.Button {
            text: "Recheck"
            onClicked: page.model.recheck()
        }
    }
}
```

- [ ] **Step 6: Manual verification**

Run: `just kirigami-dev` (requires Qt6/KF6 dev packages — same real-hardware caveat as elsewhere in this repo). Confirm:
- With no bridge/daemon issues, no banner appears and "Daemon Health" page shows the all-clear message.
- Stopping `opensnitchd` (or not starting it) surfaces the banner within one watchdog tick (`WATCHDOG_TICK`, 2s) plus `DAEMON_DOWN_TIMEOUT` (10s), and the "Daemon Health" page shows the `DAEMON_UNREACHABLE_TROUBLESHOOTING` text.
- Clicking "Recheck" doesn't crash and re-populates the page.

This step can't be automated in this sandbox (no `opensnitchd`, no systemd, and `snitchwatch-kirigami` needs system Qt6/KF6) — flag as a real-hardware verification item in the same bucket as this repo's existing Phase 2 runbook items, not a blocker for the automated tasks above.

- [ ] **Step 7: Commit**

```bash
git add crates/snitchwatch-kirigami/qml/main.qml crates/snitchwatch-kirigami/qml/DaemonHealthPage.qml
git commit -m "feat(kirigami): add daemon health banner and DaemonHealthPage"
```

---

### Task 10: QML integration test for `DaemonHealthModel`

**Files:**
- Create: `crates/snitchwatch-kirigami/tests/daemon_health_model_qml.rs`

**Interfaces:**
- Consumes: `DaemonHealthModel`'s QML-registered type (Task 8/9), following `tests/rules_model_qml.rs`'s exact template and `tests/smoke.rs`'s headless-setup boilerplate.

- [ ] **Step 1: Write the test**

```rust
use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[allow(unused_imports)]
use snitchwatch_kirigami::daemon_health_model as _;

#[test]
fn daemon_health_model_registers_and_applies_report_json() {
    if std::env::var_os("QT_QPA_PLATFORM").is_none() {
        std::env::set_var("QT_QPA_PLATFORM", "offscreen");
    }
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }
    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));
    let qml = r#"
import QtQuick
import com.snitchwatch.shell
QtObject {
    property DaemonHealthModel model: DaemonHealthModel {}
    Component.onCompleted: {
        model.applyServerMessageJson(JSON.stringify({
            action: "diagnosticsReport",
            checks: [
                { kind: "ebpf_support", status: { status: "failed", detail: "no BTF" } }
            ]
        }));
        if (!model.hasProblem) {
            throw new Error("expected hasProblem to be true after a failed check");
        }
    }
}
"#;
    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });
    if let Some(engine) = engine.as_mut() {
        engine.load_data(
            &QByteArray::from(qml),
            &QUrl::from("qrc:/inline_daemon_health_probe.qml"),
        );
    }
    drop(guard);
    assert!(
        root_ok.load(Ordering::SeqCst),
        "DaemonHealthModel QML probe failed: root object was null"
    );
    let _ = app.as_mut();
}
```

(The inline JSON's `kind`/`status` string values — `"ebpf_support"`, `"failed"` — must match whatever Task 1's `#[serde(rename_all = "snake_case")]`/`#[serde(tag = "status", ...)]` attributes actually produce; verify against Task 1's round-trip test output before finalizing this string, same caveat as Task 6.)

- [ ] **Step 2: Run test to verify it fails, then passes**

Run: `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic cargo test -p snitchwatch-kirigami --test daemon_health_model_qml`
Expected: FAILs first if `DaemonHealthModel` isn't correctly QML-registered (confirms Task 8's registration step actually worked end-to-end, which Task 8's own unit tests couldn't prove), then PASSes once correct.

- [ ] **Step 3: Run the full Kirigami test suite**

Run: `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic cargo test -p snitchwatch-kirigami`
Expected: PASS, no regressions in the other QML smoke tests.

- [ ] **Step 4: Commit**

```bash
git add crates/snitchwatch-kirigami/tests/daemon_health_model_qml.rs
git commit -m "test(kirigami): add QML integration test for DaemonHealthModel"
```

---

### Task 11: Update `HANDOFF.md`

**Files:**
- Modify: `HANDOFF.md`

**Interfaces:** none (documentation only).

- [ ] **Step 1: Fold the new in-app surface into the existing troubleshooting section**

In the "Troubleshooting: GUI runs, but no visible connection to opensnitchd" section added 2026-07-13, add a note at the top (before the "Symptom hit 2026-07-13..." paragraph):

```markdown
**Update (2026-07-14):** this class of problem now surfaces *inside the
running GUI* — a warning banner plus a "Daemon Health" page — instead of
requiring the manual `/var/log/opensnitchd.log` triage below. See
`docs/superpowers/specs/2026-07-14-daemon-kernel-diagnostics-design.md`
for the design and `docs/superpowers/plans/2026-07-14-daemon-kernel-diagnostics.md`
for the implementation. The manual steps below remain accurate as a deeper
fallback (the in-app troubleshooting text is deliberately terser) and as
the only option before this feature shipped.
```

- [ ] **Step 2: Commit**

```bash
git add HANDOFF.md
git commit -m "docs: note the new in-app daemon/kernel diagnostics surface in HANDOFF.md"
```

---

## Self-Review Notes

- **Spec coverage:** all four checks (Task 2, 4), troubleshooting text per check (Task 2, 4), protocol (Task 1), banner (Task 9), Diagnostics page (Task 9), manual recheck (Task 5, 8, 9), testing via `MockOpensnitchd`/`FakeKernelProbe` doubles (Task 2, 6) are each covered by a task.
- **Naming collision resolved:** `DaemonHealthPage.qml`/`DaemonHealthModel`/"Daemon Health" used throughout, not `Diagnostics*`, to avoid the existing `DiagnosticsPage.qml`/`SettingsController`.
- **Type consistency checked:** `DiagnosticCheck`/`CheckKind`/`CheckStatus` (Task 1) are used identically in Tasks 2, 3, 4, 5, 6, 8, 10. `DiagnosticsCtx::new`'s three-argument signature (Task 4) matches its two call sites (Task 5's `RunningBridge::run()`, and the watchdog test in Task 5). `daemon_watchdog::run`'s five-argument signature (Task 5) is used consistently in its own test and its `RunningBridge::run()` call site.
- **Known residual uncertainty, flagged inline rather than guessed away:** Task 8's exact `#[cxx_qt::bridge]` macro shape for a *non-list* QObject wasn't independently verified (only the list-model `RulesModel` shape was) — Task 8 Step 3 explicitly tells the implementer to cross-check a second existing scalar-property model before treating the macro block as final. Task 6/10's exact serde JSON shape for the internally-tagged `CheckStatus` enum should be verified against Task 1's own round-trip test output before finalizing the string literals that depend on it — both are flagged inline at the point they matter, not treated as settled.
