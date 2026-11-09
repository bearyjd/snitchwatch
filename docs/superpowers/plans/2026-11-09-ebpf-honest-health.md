# E3: honest process-monitor health when eBPF falls back

Status: **plan + implementation on `feat/ebpf-e3`** (2026-11-09). Parent: `2026-11-08-ebpf-r16-3.md` item 3.
Also closes `2026-10-29-ebpf-q05-q06.md` "Gaps found" 1. Supersedes, for the fallback case only, Q04 option A's
constant monitor line (`2026-10-27-q04-honest-tray-health.md`) and the q05-q06 fallback test's "constant in all
three states". All other states keep `Process monitor: not reported by opensnitchd`.

## Question
When `ProcMonitorMethod` is `ebpf` but the daemon actually attributes connections via `/proc`, the GUI must say
so, and never label intent (config) as actual. This doc lists every signal the bridge has without new
privilege or a daemon change.

## What the daemon really does (v1.8.0 = pin `a1353848` for these files)
`git diff v1.8.0 a1353848` is empty for `procmon/monitor/init.go`, `ui/config_utils.go`, `ui/alerts.go`,
`core/ebpf.go`; `procmon/ebpf/{ebpf,events}.go` differ only in debug log lines. Error strings are identical.

Startup path: `ui.NewClient` starts `alertsDispatcher` (`ui/client.go:85`), then `loadDiskConfiguration(false)`
(`:92`). `c.config.ProcMonitorMethod` is `""`, so `reloadConfiguration` always calls
`monitor.ReconfigureMonitorMethod` (`config_utils.go:208-262`). Config reloads from disk (fsnotify) go the same way.

| Case | `Init` result (`init.go:93-133`) | What runs after | Daemon log (INF) |
|---|---|---|---|
| eBPF ok | `NoError` | eBPF | `Process monitor method ebpf` |
| `opensnitch.o` fails | `EbpfErr`; `ebpf.Stop()`, `startProcMonitors()`, method set to `proc` | `/proc` | `error starting ebpf monitor method` (WAR) then `Process monitor method /proc` |
| `opensnitch-procs.o` / tracefs / perf map fails | `EbpfEventsErr`; `startProcMonitors()`, main module stays loaded; `ReconfigureMonitorMethod` then **rolls the method back** to the old one (`init.go:61-66`), which at startup is the package default `proc` (`procmon/process.go:15`) | `/proc` attribution (`conman/connection.go:91` checks `MethodIsEbpf()`), with `opensnitch.o` loaded and unused | `Process monitor method ebpf` (wrong: intent, not actual) + WAR `opensnitch-procs.o not available` |
| kprobe attach fails (`ebpf.go:145-180`) | `NoError` (only log warnings) | eBPF with missing hooks | `Process monitor method ebpf` |

So even the daemon's own log line labels intent as actual in the procs case. A configured `audit` that changes to
`ebpf` and fails would roll back to `audit`; Snitchwatch never ships `audit`, the GUI text below hedges with
"reported".

## Signals available to the bridge (no new privilege, no daemon change)
1. **`ClientConfig.config`** (Subscribe): `getClientConfig` reads the file from disk (`ui/notifications.go:35`).
   It is the configured method, never the running one. Usable only as "configured" (Q04 option B, not chosen).
2. **Ping `Statistics`** (`ui.proto:79-97`): no monitor field; nothing distinguishes eBPF from `/proc` events.
3. **Startup warning alert**: `loadDiskConfiguration` sends `SendWarningAlert(err.Error())` (`config_utils.go:82`)
   with `err.Msg` of the failed `Init`. `Alert_GENERIC`, `WARNING`, free text. Queued (32 slots, oldest dropped)
   until the bridge connects (`ui/alerts.go:75-118`). Exact v1.8.0 texts:
   - `opensnitch.o`: `[eBPF] Error loading opensnitch.o: ` + `\nunable to load eBPF module (opensnitch.o). Your kernel
     version (...) might not be compatible.\nIf this error persists, change process monitor method to 'proc'`
     (`procmon/ebpf/ebpf.go:134`, `core/ebpf.go:69-71`), or `[eBPF] Error loading opensnitch.o (collection): ...`
     (`ebpf.go:141`).
   - procs: `\nunable to load eBPF module (opensnitch-procs.o). ...` (`events.go:96-98`) or `...\nUnable to access
     debugfs filesystem, needed for eBPF to work, ...` (`utils.go:35-37`). `Assign`/`initPerfMap` errors are bare
     library text and can't be told apart: those make no mode claim.
   - Not the monitor: `EBPF-DNS: Unable to attach ebpf listener: ...` (`main.go:650-660`, `Alert_ERROR`, GENERIC,
     sent from a goroutine, so usually after the monitor alert). Today `classify_generic_alert_text` matches its
   `ebpf` substring and shows it under the process monitor: the same mislabel E3 removes.
4. **Daemon log file** `/var/log/opensnitchd.log`: created 0644 (`log/log.go:211`); logrotate `create 640` is moot
   under `copytruncate`. But the shipped configs set `LogLevel: 2` (IMPORTANT; `log.go:38-41,173` drops lower
   levels), so every `INF Process monitor method ...` line is suppressed in production; the VM evidence
   (`ebpf-vm-test/result/system/pre-{ebpf,proc}.log`) ran at debug level. The WAR lines carry the same text as
   the alert. Also: the system bridge has `ProtectSystem=strict`, Flatpak can't see `/var/log`, the path is
   config-dependent, and parsing a log is a new trust surface. **Rejected.**
5. **journald**: `LogFile` is set, so the journal has nothing (`docs/packaging/system-bridge-integration.md:122`);
   reading other units' journal needs `systemd-journal`/`wheel`. **Rejected.**
6. **Notification round-trip** (e.g. re-enable interception): restarts the monitor; the GUI never drives daemon
   config. **Rejected.**

VM evidence (owner run 2026-11-09): eBPF-ok and configured-`proc` runs only. **No fallback has been seen on a
host**; the alert texts above come from source.

## Recommendation (built)
Signal 3 is the only real "actual" evidence. It is positive-only: its absence proves nothing (bridge restarted
after the daemon's first connection, queue overflow, `RecheckDiagnostics`). So:
- Known fallback text present: say the daemon *reported* eBPF did not start and fell back to `/proc`, with the
  time it was reported. Not a health failure (owner invariant: `/proc` is supported, never an error).
- Anything else: keep `Process monitor: not reported by opensnitchd`. Never "eBPF active", never the configured
  method.

### Bridge
- `daemon_alerts`: GENERIC alerts are stored per topic (`MonitorFallback`, `ProcessMonitor`, `Firewall`,
  `EbpfDns`, `Unclassified`), so a later DNS or firewall alert no longer evicts the fallback alert. The text
  classifier moves there from `diagnostics/mod.rs`. `EBPF-DNS:` is its own topic, overlaid on no check.
- A `MonitorFallback` alert does **not** mark `EbpfSupport` failed (that check stays the local kernel probe plus
  other eBPF alerts, as before).
- `RecheckDiagnostics` clears it like every alert (unchanged re-baseline semantics; the daemon re-alerts on its
  next start if still true). Subscribe does not clear it (the race in `daemon_alerts.rs` module doc).
- Review fix (2026-11-09): `process_monitor()` is `None` while liveness says the daemon is down, and the
  watchdog clears the fallback slot on the down transition, so an alert from a daemon process that went away
  never describes the next one. The GUI health page resets its line when no bridge session is live.

### Wire (additive; older GUIs ignore it)
```rust
ServerMessage::DiagnosticsReport {
    checks: Vec<DiagnosticCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    process_monitor: Option<ProcessMonitorReport>,
}
pub struct ProcessMonitorReport { pub fallback: MonitorFallback, pub reported_secs_ago: u64, pub daemon_text: String }
pub enum MonitorFallback { EbpfDidNotStart, EbpfEventsDidNotStart } // snake_case on the wire
```
JSON: `"processMonitor": {"fallback": "ebpf_did_not_start", "reportedSecsAgo": 12, "daemonText": "..."}`
(field casing follows the enclosing message's serde attrs).

### GUI text (tray tooltip + Daemon Health monitor line)
- none: `Process monitor: not reported by opensnitchd`
- `ebpf_did_not_start`: `Process monitor: eBPF did not start; opensnitchd reported it at HH:MM`
- `ebpf_events_did_not_start`: `Process monitor: eBPF process events did not start; opensnitchd reported it at HH:MM`
  Neither line names `/proc` (codex review): it is certain only at daemon startup, and the alert text can't tell
  startup from a live config reload, where `ReconfigureMonitorMethod` rolls back to the old method, eBPF or audit
  (`init.go:53-67,102-108`). The plan's "/proc" wording above is the startup behaviour, not GUI text.

`HH:MM` is GUI local time minus `reportedSecsAgo`. The Daemon Health page is not marked as a problem by it.

## Tests (written first)
- `tests/bridge_protocol_test`: (a) verbatim `opensnitch.o` alert then an `EBPF-DNS` alert: the report keeps
  `processMonitor.fallback == ebpf_did_not_start`, `ebpf_support` is not failed by it; (b) procs text gives
  `ebpf_events_did_not_start`; (c) an unclassifiable text gives no `processMonitor`; (d) recheck clears it.
- `daemon_alerts` unit: per-topic slots; classifier table on every text above.
- `tray_health` unit: the three lines; the existing "never names a mode" guard narrowed to the no-report case.

## Unverified / open
- Daemon restarts (codex review): `onStatusChange` fires `go c.Subscribe()` and the queued-alert flush from
  two goroutines on the same connect (`ui/client.go:235-247`, `ui/alerts.go:96-108`), so a new process's
  startup alert lands just before or after its Subscribe; a plain reconnect never re-sends it. So every
  Subscribe drops a stored fallback older than `FALLBACK_SUBSCRIBE_GRACE` (3 s) and keeps a newer one or any
  alert after it (`DaemonAlertStore::on_daemon_subscribe`). Residual: a daemon that alerts, then is replaced by
  one that reconnects within 3 s and starts eBPF fine, keeps the old line; a reconnect of the same process
  drops a still-true fallback (shows "not reported", the weaker claim). Other alerts still survive Subscribe.
- The health page's disconnect reset has no automated test (needs a live bridge link in a QML test).
- Real fallback alert text on a host (needs a forced fallback on the VM, never on the production host).
- Kirigami tests segfault in this sandbox; CI's `kirigami` job is the evidence for the GUI part.
- Upstream "active method" field (Q04 option C) would replace this text matching; not requested here.
