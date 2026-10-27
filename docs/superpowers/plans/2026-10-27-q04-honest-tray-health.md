# Q04: honest tray and health state

Status: the owner chose option A on 2026-10-26 (GUI-only, no bridge or wire
change); implemented test-first on `feat/tray-health-honest-state`. The actual
process-monitor mode stays "not reported by opensnitchd" until an upstream
daemon change exposes it (tracked with Q05/r16). Roadmap:
`../specs/2026-10-24-competitive-feature-roadmap.md` Q04 and "Dual-mode GUI
requirement"; parent: `2026-10-24-overnight-gui-fixes-and-features.md` Task 2.

## Rules carried from the roadmap
- One GUI for `/proc` and eBPF hosts. `/proc` is supported, never a health
  error. eBPF readiness is never a GUI prerequisite.
- The GUI never writes daemon configuration and never changes the monitor.
- Configured intent is never labelled as the actual state. "Unknown" is a
  valid, honest state.

## What the GUI knows today

| State | Meaning | Source | Actual or intent | Shown today |
|---|---|---|---|---|
| Disconnected | opensnitchd isn't dialing in / its stream ended | bridge `daemon_watchdog.rs` -> `cache.set_daemon_down` -> `TrayState::DaemonDown` (`tray_state.rs:30` priority: down > paused > pending > idle) | Actual | Tray tooltip "opensnitchd not reachable" (+ pause when set), menu token `reconnect`; health check `DaemonReachable` |
| Paused | Bridge auto-allows every `AskRule` until expiry | bridge `filter_pause.rs` -> `FilterPauseState` watch -> `TrayController` | Actual (bridge-owned) | Tooltip "filtering paused until HH:MM", Resume item; issue #78 slot text |
| Pending | Prompts waiting for a verdict | `TrayState::Pending(n)` | Actual | Tooltip "n pending decisions" |
| Prompting ("enforcing") | Connected, not paused: unknown connections are asked | derived: not down, not paused | Actual for the GUI's prompting; says nothing about the firewall | Tooltip "Snitchwatch — filtering" |
| Firewall running | Daemon's nftables interception is up | `ClientConfig.is_firewall_running`, stored on every `subscribe()` (`grpc_server.rs:517`) -> `FirewallRunning` check | Actual, but a snapshot at subscribe time, not live | Health page only; the tray doesn't see it |
| Configured monitor | `ProcMonitorMethod` in the daemon config | `ClientConfig.config` is the config file read from disk (`vendor/.../ui/notifications.go:36,56`); the bridge parses it only for type checks (`daemon_config.rs:148`), and nothing forwards it | Intent only; also misses a `-process-monitor-method` CLI override | Not shown |
| Actual monitor | proc / eBPF / audit actually running, incl. fallback | Not on the wire. `monitor.Init` falls back to proc with only a log line (`procmon/monitor/init.go:116,133`); a fallback alert is sent only on the CLI-override path (`main.go:645`), as generic text | Unavailable | Health page: "opensnitchd does not report the active method to the GUI" (`DaemonHealthPage.qml:46`) |
| Monitor problem | Daemon-reported process-monitor warning/error | `PROC_MONITOR`/`KERNEL_EVENT` alerts and classified generic alerts -> legacy `EbpfSupport` check (`diagnostics/mod.rs:114`) | Actual when present; absence proves nothing | Health page troubleshooting text |

## The gap that blocks Q04's mode requirement
opensnitchd v1.8.0 has no message carrying `procmon.GetMonitorMethod()`. A
bridge change can't recover it either: the only monitor value the bridge
receives is the configured one, and showing that as the mode would break the
"never label intent as actual" rule. So "actual proc/eBPF mode" needs an
upstream daemon change (for example, the running method in `ClientConfig` or a
monitor-state alert on start and fallback), which belongs with Q05/r16.

## Option A as built
1. Tray tooltip and Daemon Health page both carry the fixed line "Process
   monitor: not reported by opensnitchd". It is a constant: no diagnostics
   report, About info (`ebpf_commit`), or daemon config value can turn it into
   a mode. `DaemonHealthPage.qml`'s "does not report the active method" sentence
   stays.
2. Tray tooltip adds a firewall line when the `FirewallRunning` check failed.
   The bridge stores `is_firewall_running` at `subscribe()` and re-sends that
   same value in every later report (alert, recheck, watchdog), so the arrival
   time of a report is not the snapshot's time, and the connect time itself is
   not on the wire. Rule, in `src/tray_health.rs`:
   - the time is stamped only when the GUI sees the status change from a value
     it saw earlier in the same bridge session (for example Unknown after the
     watchdog reset, then Failed): "Firewall not running as of opensnitchd's
     last connection (noticed HH:MM)". The snapshot is from that time or
     earlier; "noticed" says so.
   - an identical re-sent value never re-stamps, so a stale snapshot keeps its
     first time.
   - a failure already present in the first report the tray sees gets no time:
     "... (time not known)".
   - a failure carrying a daemon alert (detail other than the bare snapshot
     text) reads "Firewall issue reported by opensnitchd", not "not running".
   - a new bridge session clears the stamp.
   Local time comes from `QDateTime::current_date_time()` plus
   `set_msecs_since_epoch`, also now used for "paused until", replacing
   `QTimeZone::system_time_zone()` (UTC in the sandbox).
3. Visible "filtering" wording becomes "prompting": idle "Snitchwatch —
   prompting on", "prompting paused [until HH:MM]", "Resume prompting",
   "Prompting is paused, but …", "Snitchwatch — prompting resumed", "Allowed
   once (prompting was paused)". A pause only auto-answers prompts; existing
   rules keep applying. Internal tokens (`pause_filtering`,
   `resume_filtering`, `resumeFiltering`, `SetFilteringPaused`) are contracts
   and stay. The Tauri shell (being replaced) and bridge comments are out of
   scope.

Tests (written first): `tray_health` unit tests for each firewall rule above,
including the stale re-send and first-seen cases; monitor text unchanged for
`EbpfSupport` Ok/Failed/Unknown reports and an `AboutInfo` with an
`ebpf_commit`; tooltip composition never hides down/paused/pending; a source
guard that tray and health sources carry "not reported by opensnitchd" and no
daemon-config reads; updated tray/menu/notice wording tests. Mutation runs are
recorded in the PR.

## What the plan listed as GUI-only (before the decision)
1. Tray: add a monitor line that's always "Process monitor: not reported by
   opensnitchd" until the wire carries it, so the tray never implies a mode.
2. Tray: show "firewall not running" from the `FirewallRunning` check, and say
   it's as of the last daemon connection. Today only the health page shows it.
3. Tray: rename the "Snitchwatch — filtering" idle text so it says prompting is
   on, not that the firewall is enforcing (that's item 2's job).

Tests, written first: pure `tray.rs` derivations for each row of the table
(down beats paused beats pending; the monitor line is never "proc"/"eBPF" with
no evidence; a firewall-off snapshot is labelled as a snapshot); a
`daemon_health_model` fixture where an `EbpfSupport` Ok status from an older
bridge doesn't produce any mode text; and a QML source guard that no tray/health
string says "active"/"running" for the monitor.

## Options for the owner
- A. Ship the GUI-only items 1-3 now (honest "not reported"), and track the
  actual mode under Q05.
- B. Also forward the configured `ProcMonitorMethod` (a bridge plus wire
  change), always labelled "configured", never "active".
- C. Wait for an upstream change that reports the running method, then do Q04
  in full.
