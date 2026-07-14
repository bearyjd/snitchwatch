# Daemon/kernel diagnostics — design

**Date:** 2026-07-14
**Status:** approved for planning
**Component:** A (Snitchwatch)

## Problem

Snitchwatch is a friendly frontend over `opensnitchd`, not the interception
engine itself. When `opensnitchd` isn't reachable, or is reachable but can't
actually intercept traffic because the host kernel doesn't support the
mode it's configured for (eBPF process monitoring, nftables firewall), the
GUI currently shows **nothing**: no dialog, no error, nothing actionable.
The only existing signal is the system tray's `DaemonDown` tooltip
("opensnitchd not reachable") — invisible unless the user happens to check
the tray, and carrying no guidance on what to do about it.

This was hit directly verifying on real hardware (see `HANDOFF.md`'s
"Troubleshooting: GUI runs, but no visible connection to opensnitchd"
section, added 2026-07-13) — the fix at the time was a doc, written after
the fact from first-hand debugging. This design moves that same guidance
into the app itself, live, plus extends it to a class of failure the doc
didn't cover: a kernel that can't satisfy opensnitchd's configured
prerequisites at all.

## Goals

- Detect and surface, inside the running GUI, without requiring a
  real-hardware debugging session:
  1. opensnitchd unreachable (already detected via the watchdog; not
     currently surfaced beyond the tray).
  2. opensnitchd reachable but its firewall backend isn't running.
  3. Host kernel lacks eBPF/BTF support opensnitchd's default
     `ProcMonitorMethod: ebpf` needs.
  4. Host kernel/userspace lacks nftables support opensnitchd's default
     `Firewall: nftables` needs.
- Each surfaced problem carries concrete, actionable troubleshooting text,
  not just a status label.
- Detection to work even when opensnitchd never connects at all (kernel
  checks are the bridge's own local probe, independent of the daemon
  link).

## Non-goals

- Auto-fixing the host (installing packages, editing opensnitchd's config,
  changing `ProcMonitorMethod`) — this is diagnosis only, the user acts on
  the guidance manually.
- Replacing `opensnitchd`'s own logging — `/var/log/opensnitchd.log`
  remains the source of truth for daemon-side detail; Snitchwatch surfaces
  a summary and points there when relevant.
- General host health/security scanning — that's Component B's job, and
  explicitly out of scope per this repo's Component A/B split.

## Architecture

The GUI runs Flatpak-sandboxed with no filesystem access beyond
`$XDG_RUNTIME_DIR/snitchwatch` (settled decision, see `CLAUDE.md`). All
probing — both the opensnitchd link and local kernel checks — happens in
`snitchwatch-bridge`, which already runs unprivileged, host-side. The GUI
only ever renders what the bridge reports over the existing WS protocol.

### 1. Checks (bridge-side, new `diagnostics` module)

| Check | How | Source |
|---|---|---|
| opensnitchd reachable | reuse existing watchdog staleness detection (`daemon_watchdog.rs`) | already computed, just newly surfaced |
| Firewall running | `ClientConfig.isFirewallRunning`, read off `Subscribe()` | opensnitchd self-report, once connected |
| eBPF support | presence of `/sys/kernel/btf/vmlinux` (BTF, required for eBPF CO-RE); kernel version as a secondary signal if BTF path itself is inconclusive | local probe |
| nftables support | `nft` binary on `$PATH` **and** `nf_tables` present in `/proc/modules` | local probe |

Each check reports one of:

```rust
enum CheckStatus {
    Ok,
    Failed { detail: &'static str },
    Unknown,
}
```

`Unknown` covers "can't assess without more data" (e.g. opensnitchd
connected but hasn't sent a `ClientConfig` yet) — never guessed as `Ok` or
`Failed`.

### 2. Troubleshooting text (fixed, per check, bridge-owned)

The bridge owns the wording — same pattern as tray tooltip text — so the
GUI never duplicates troubleshooting copy:

- **opensnitchd unreachable**: "opensnitchd isn't dialing in. Confirm it's
  installed and running (`systemctl status opensnitchd`), and that its
  `Server.Address` in `/etc/opensnitchd/default-config.json` matches the
  bridge's `SNITCHWATCH_GRPC_BIND` (default `127.0.0.1:50051`). Check
  `/var/log/opensnitchd.log` for dial errors."
- **Firewall not running**: "opensnitchd connected but its firewall
  backend isn't active. Check `/var/log/opensnitchd.log` for nftables
  errors; confirm nftables is enabled and not conflicting with
  iptables/firewalld rules already on the host."
- **eBPF unsupported**: "This kernel doesn't expose BTF
  (`/sys/kernel/btf/vmlinux` missing), which opensnitchd's default
  `ProcMonitorMethod: ebpf` requires. Either upgrade to a kernel built with
  `CONFIG_DEBUG_INFO_BTF=y`, or set `ProcMonitorMethod` to `proc` in
  opensnitchd's config as a fallback (slower, more overhead, but works on
  any kernel)."
- **nftables unsupported**: "The `nft` firewall backend opensnitchd
  depends on isn't available on this host. Install the `nftables`
  package, and confirm the kernel wasn't built without
  `CONFIG_NF_TABLES`."

### 3. Protocol

New `ws_messages.rs` types:

```rust
enum CheckKind {
    DaemonReachable,
    FirewallRunning,
    EbpfSupport,
    NftablesSupport,
}

struct DiagnosticCheck {
    kind: CheckKind,
    status: CheckStatus,
}

// ServerMessage variant:
DiagnosticsReport { checks: Vec<DiagnosticCheck> }

// ClientMessage variant:
RecheckDiagnostics
```

Trigger points for a (re-)send, no new polling loop:

- Once at bridge startup (local checks run immediately; daemon-dependent
  checks start `Unknown` until the first `Subscribe()`/watchdog tick).
- Included in the existing rebroadcast-on-connect snapshot for
  (re)connecting GUI clients.
- Whenever the watchdog flips `DaemonDown` in either direction (local
  checks are cheap — `stat`/`PATH` lookup — so re-running them on every
  flip is negligible).
- On `ClientMessage::RecheckDiagnostics` (GUI-initiated manual recheck).

### 4. GUI

- **Banner**: persistent, dismissible `Kirigami.InlineMessage` (warning)
  at the top of the main window, visible whenever any check is `Failed`.
  Short summary text + "Details" button navigating to the Diagnostics
  page. Dismissal hides it until the next status *change* — not a
  permanent mute, so a real ongoing problem doesn't get lost, but a
  session where the user already knows and is mid-workaround doesn't get
  renagged every message.
- **Diagnostics page**: new navigation entry alongside Connections/Rules/
  etc. Auto-navigated-to the first time any check goes `Failed` in a
  session (not on every launch). Lists all four checks with a status icon
  and, for any `Failed` entry, its troubleshooting text inline. Includes a
  manual "Recheck" button wired to `ClientMessage::RecheckDiagnostics`.
- New `DiagnosticsModel` QObject in `bridge_runtime.rs`, same shape as the
  existing `TrafficModel`/`RulesModel` pattern — updated on
  `DiagnosticsReport` receipt, exposed to QML.

  **Descoped in the implementation plan:** dismissal and auto-navigation
  were dropped when this design was broken into tasks (see the plan's
  Task 9) in favor of the simpler always-visible-while-`hasProblem`
  banner and a plain drawer entry — a real ongoing problem staying
  visible was judged more important than avoiding a re-nag, and
  auto-navigation added session-state tracking for a feature not yet
  proven necessary. The shipped `DaemonHealthModel`/`DaemonHealthPage.qml`
  (renamed from `DiagnosticsModel`/Diagnostics page to avoid colliding
  with the pre-existing unrelated `DiagnosticsPage.qml`) reflect this
  simplification; both listed behaviors above remain a reasonable future
  enhancement, not a bug.

## Testing

Per this repo's convention (`CLAUDE.md` "Reproduction paths"), all of this
is testable via `tests/mock_opensnitchd` — no real daemon or host needed:

- Bridge unit tests: local eBPF/nftables probes, using dependency
  injection (a `KernelProbe` trait, mirroring `SystemInspector`/
  `SystemFacts` doubles already used by Component B) so tests don't
  depend on the sandbox's actual kernel.
- Bridge integration tests (`tests/bridge_protocol_test.rs` pattern): boot
  a real bridge, connect a scripted `MockOpensnitchd`, assert
  `DiagnosticsReport` contents for reachable/unreachable/firewall-down
  scenarios.
- Kirigami QML tests: `DiagnosticsModel` update behavior, banner
  visibility toggling, auto-navigation-once-per-session logic — following
  the existing per-model QML test file pattern
  (`crates/snitchwatch-kirigami/tests/`).
- Real-kernel eBPF/nftables detection itself (does the probe correctly
  read a *real* `/sys/kernel/btf/vmlinux` and `/proc/modules`) is a
  real-hardware verification item, same bucket as the existing Phase 2
  runbook — the `KernelProbe` trait's production implementation isn't
  exercised by CI, only its double.

## Open questions

None — scope, checks, wording, protocol, and UX were confirmed
conversationally before writing this doc.
