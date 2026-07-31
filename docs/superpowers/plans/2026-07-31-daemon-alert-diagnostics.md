# Daemon Alert → Diagnostics Integration (issue #6)

**Goal:** Close the EbpfSupport blind spot verified live 2026-07-31: the
host has `/sys/kernel/btf/vmlinux` (probe reports Ok) while opensnitchd
v1.8.0 fails to load its bundled eBPF module on kernel 6.19. The host-side
probe cannot see daemon-internal failures — but the daemon *tells us*
about them via the `PostAlert` RPC, which the bridge currently logs and
drops (`grpc_server.rs::post_alert`).

**Upstream facts (vendored v1.8.0, verified):**
- `daemon/ui/alerts.go`: alerts carry `Type` (ERROR/WARNING/INFO), `What`
  (GENERIC/PROC_MONITOR/FIREWALL/CONNECTION/RULE/NETLINK/KERNEL_EVENT),
  priority, and for string payloads `Alert_Text` (`data` oneof).
- eBPF/proc-monitor init failures and nfqueue-creation errors are sent as
  WARNING/ERROR alerts at daemon startup (`daemon/main.go:176,187,645`).

**Design (fold into existing checks — no protocol/QML changes):**
- New `DaemonAlertStore` (bridge): bounded map keyed by `Alert.What`
  holding the most recent ERROR/WARNING alert text per category (INFO
  ignored). Cleared on `subscribe()` — a new daemon session starts clean.
- `post_alert` records into the store (still acks + logs as today), then
  broadcasts a fresh `ServerMessage::DiagnosticsReport` so the GUI banner
  appears push-style without a manual recheck.
- `DiagnosticsCtx::report()` overlays stored alerts onto the existing
  checks:
  - `PROC_MONITOR` or `KERNEL_EVENT` alert present → `EbpfSupport` is
    `Failed` with detail = `EBPF_TROUBLESHOOTING` + the daemon's own
    alert text appended (even when the BTF probe passes — the daemon's
    word wins over the host heuristic).
  - `FIREWALL` alert present → `FirewallRunning` is `Failed` with the
    alert text appended to `FIREWALL_NOT_RUNNING_TROUBLESHOOTING`.
  - Other `What` values: recorded + logged only, no check mapping (a
    dedicated generic-alerts surface is future work; noted in issue #6).
- Rationale for folding vs. a new `CheckKind`: no ws_messages/QML surface
  change, the blind spot is closed exactly where users look for it, and
  the existing four-check page semantics stay intact. A new kind can be
  added later without conflicting with this.

**Mock gap to close:** `MockOpensnitchd` gains `post_alert(type, what,
text)`; integration test drives a PROC_MONITOR error alert through a live
bridge and asserts (a) an unsolicited `diagnosticsReport` broadcast
arrives with `ebpf_support: failed` carrying the alert text, (b) a
subsequent daemon re-`subscribe()` clears it back to Ok on recheck.

## Tasks

1. `DaemonAlertStore` in a new `crates/snitchwatch-bridge/src/daemon_alerts.rs`
   (bounded, per-`What` latest ERROR/WARNING text; `record()`, `clear()`,
   `snapshot()`; unit-TDD).
2. `grpc_server.rs`: `UiService` holds the store + a `broadcast_tx` clone
   (or a callback) — `post_alert` records ERROR/WARNING alerts with an
   `Alert_Text` payload and triggers a `DiagnosticsReport` broadcast;
   `subscribe` clears the store. Unit tests via `spawn_test_service`.
   NOTE: `UiService` today has no `broadcast_tx` for reports — pass the
   `DiagnosticsCtx` (or a closure) in at construction; check
   `snitchwatch-bridge-cli` wiring order (DiagnosticsCtx is currently
   built *after* UiService — invert or use a late-bound
   `OnceLock`/`SetOnce` handle, pick whichever reads cleaner and test it).
3. `DiagnosticsCtx::report()` overlay per the mapping above; unit tests
   for both mappings + precedence (probe-Ok + alert → Failed).
4. `MockOpensnitchd::post_alert(...)` + integration test in
   `tests/bridge_protocol_test.rs` per "Mock gap" above.
5. Runbook Step 6c: add the now-testable real-hardware scenario — with
   the v1.8.0 RPM configured `ProcMonitorMethod: ebpf` on kernel ≥6.19
   the daemon's own alert must surface as a failed `ebpf_support` check
   with the daemon's message visible; update issue-#6 references.

Gates per task: `just check`, `just test-bridge`; full `just test` at the
end. Kirigami crates untouched (no protocol change).

## Review amendments (2026-07-31)

Whole-branch review (`fix/daemon-alert-diagnostics`) returned REQUEST
CHANGES against the first pass of tasks 1-5 above. The store/late-bind/
broadcast infrastructure was judged sound and kept as-is; two parts of the
original design were wrong and got corrected. This section documents what
actually shipped, superseding the corresponding bullets above.

**GENERIC is the real shape, not an edge case.** The plan's design assumed
`PROC_MONITOR`/`FIREWALL`-tagged alerts would be what a real daemon sends.
Checking `daemon/ui/alerts.go:64-70` shows `SendWarningAlert`/
`SendErrorAlert` — the functions every issue-#6-relevant call site
(`daemon/main.go:176,187,645`, `daemon/ui/config_utils.go:82`) actually
calls — hardcode `Alert_GENERIC`. The one call site that does tag a specific
`What` (`daemon/main.go:307`, `KERNEL_EVENT`) sends a `Proc` payload, not
`Text`, so it can't reach this store at all. In practice, essentially every
real v1.8.0 alert is `GENERIC` + free text. `DiagnosticsCtx::report()`
(`diagnostics/mod.rs`) now runs a `classify_generic_alert_text` text
classifier over `GENERIC` alerts — "process monitor"/"ebpf" (case-
insensitive) → `EbpfSupport`; "queue #"/"nfqueue"/"nftables"/"firewall" →
`FirewallRunning`; anything else is recorded but left unmapped. The
`What`-tagged mapping from the original design (`PROC_MONITOR`/
`KERNEL_EVENT`/`FIREWALL`) is kept as forward-compat and takes precedence
over the classifier when present, in case a future daemon version starts
tagging alerts properly.

**Alerts persist until explicitly cleared — `subscribe()` does NOT clear
the store.** The original "cleared on subscribe()" design turned out to
have two problems: it erases a still-true alert on a reconnect that fixed
nothing, and it races the daemon's own alert delivery
(`daemon/ui/client.go:236-243`'s `onStatusChange` fires `go c.Subscribe()`
concurrently with unblocking the queued-alert flush — ordering between the
two is undefined). Instead, `ClientMessage::RecheckDiagnostics` clears the
store (via `DiagnosticsCtx::clear_alerts`) immediately before re-running
`report()` — a user-driven "re-baseline": a persisting problem re-alerts on
the daemon's next restart, so a stale positive is recoverable, but a
silently-dropped real alert (the old design's failure mode) is not. Each
stored alert also carries a `recorded_at` timestamp; the overlay appends a
coarse age ("Ns"/"Nm ago") and the alert's severity ("error:"/"warning:")
to the detail text.

**Self-contradiction guard for the eBPF overlay.** When a daemon alert maps
to `EbpfSupport` but the local probe says BTF is actually present, the
overlay now uses a new `EBPF_DAEMON_REPORTED_TROUBLESHOOTING` constant
instead of `EBPF_TROUBLESHOOTING` — the latter's "kernel doesn't expose
BTF" claim would be false in that case (issue #6's exact scenario:
`opensnitchd` can fail its eBPF init on kernel 6.19+ regardless of BTF).
`EBPF_TROUBLESHOOTING` is still used when the probe *also* failed (both
signals agree BTF is the problem).

Also fixed: unrecognized `Alert.What` values are skipped (not coerced to
`Generic`, now that `Generic` is meaningful); non-`Text` alert payloads are
debug-logged when dropped; stored alert text is truncated to 512 bytes;
`grpc_server.rs`'s test module moved to `grpc_server/tests.rs` (the file had
grown past the 800-line convention).
