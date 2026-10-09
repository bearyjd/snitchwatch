# N3: a bridge restart judged from the daemon's counters, not always a gap

**Date:** 2026-10-09
**Owner decision:** N3 in issue #117 (recommendation approved): "on bridge
start, judge the gap from the daemon's own counters".
**Baseline:** `origin/main` @ `ac446e2` (branch `fix/n3-unused-window`).
**Size:** S–M. Bridge only: `cache/rule_hits.rs`, `cache/rule_hits_file.rs`,
`cache/rule_hits_handle.rs`, one line in `grpc_server.rs`
(`with_daemon_transport`), and the bridge-cli shutdown call. No wire change,
no Kirigami change.
**Related:** #94 / #101 (hit counts, "Unused"), plan
`2026-10-08-rule-insights.md` ("Part 1 as built", "Part 2 as built"), E3
plan `2026-10-08-default-applied-events.md`, #82 / #90 (file hardening),
#35 (TCP transport).

## The problem

`RuleHits::restore` notes a gap on every bridge start ("the bridge was
down"). Kirigami's "Unused" needs 14 days since the latest of counting
start, rule creation and the **last gap**, so it needs 14 days of continuous
bridge uptime. A desktop that reboots to apply updates may never get there.

## What the daemon gives us (stock v1.8.0, `vendor/`, read-only)

- `statistics/stats.go` `onConnection`: a matched connection does
  `RuleHits++` and appends an `Event` (the batch is capped at `MaxEvents`,
  dropping the oldest); a missed one does `RuleMisses++` and appends
  nothing. `nolog` rules reach neither (`main.go` `onPacket`).
- `Serialize` returns `nil` unless a matched event was appended since the
  last call (`newEvents`), and `ui/client.go` `ping` then sends **no Ping at
  all**. So the daemon pings (about once a second, `poller`) **only when
  there are new rule hits**, and only while connected: while no bridge is
  connected, events wait in the batch (up to `MaxEvents`) and arrive with
  the first ping after the next connection. A ping whose RPC fails loses
  its batch (emptied before the RPC), but `RuleHits` still counted it.
- `Uptime` = `time.Since(Started)` in whole seconds (Go's monotonic clock:
  it does not advance during suspend).
- **No run identity:** `ClientConfig.id` is `ts.UnixNano()` of a zero
  `time.Time` (`ui/notifications.go`), the same for every run. There is no
  start time or boot id in `Statistics`. A restart shows only as `uptime`
  or `rule_hits` going backwards, or as a start time (`now − uptime`) that
  moved.
- **E3 (bazzite-tower fork):** a default-applied connection is an event
  with the marked synthetic rule and grows `rule_misses`, never `rule_hits`
  (E3 plan, "Each such event is exactly one `rule_misses` increment").
  `received` already excludes marked events; nothing changes for them here.

Within one daemon run, `Δrule_hits − received` between two pings is exactly
the rule events that never arrived (the "Part 1 as built" invariant). The
fix extends that invariant **across a bridge restart** by saving the
baseline the next run measures from.

## Design

### What is saved (the file, version 2)

`<state>/rule_hits.json` keeps everything it has today, plus:

| Field | Meaning |
|---|---|
| `daemon` (optional) | `{ pingUnixMs, uptime, ruleHits }`: the daemon's counters at the last ping counted **and saved with these counts** (taken under the same lock as the counts, so they always agree). |
| `stoppedUnixMs` (optional) | Written only by the save at shutdown: the counts include every event this run received. A periodic save writes none. |

- `version` is written as `2`; `1` and `2` are read. A version-1 file has
  no `daemon`, so its first restart is "cannot tell" (a gap), once.
- **Unknown fields are ignored** (`deny_unknown_fields` is dropped), so a
  later additive field doesn't make an older bridge distrust the whole file.
  A higher version is still refused (the file is left alone and the counts
  stay in memory, as for any unreadable file).
- `pingUnixMs` and `stoppedUnixMs` get the same plausible-time check as
  every other time. Everything else (O_NOFOLLOW, owner, link count, mode,
  8 MiB cap, temp-file rename) is unchanged.
- The shutdown save **always writes** (today `save_now` skips a write when
  nothing changed since the last periodic save, which would leave no
  `stoppedUnixMs`).
- While a restart is still being judged (below), saves keep the restored
  `daemon` baseline, so a run that sees no ping hands the same baseline to
  the next run.

### Who is trusted: the transport

| Transport | Judged from the counters? | Why |
|---|---|---|
| Unix (`DaemonTransport::Unix`, the system bridge) | **yes** | The system bridge's socket admits only root peers (`RootUnixIncoming`). |
| TCP (`DaemonTransport::Tcp`, legacy per-user, `127.0.0.1:50051`) | **no: every bridge start is a gap, as today**; no baseline is saved | Any local process can dial in and send a ping (#35's residual risk in `daemon_commands.rs`). A forged first ping (`uptime 1, rule_hits 0, events []`) would pass the "daemon restarted" test below and suppress a gap, so a used rule could read "Unused". |

The TCP row is a conservative default, not a technical limit: switching it
is one line, and is the owner's call (listed under "Owner questions").

### The judgement (first ping after a restore, Unix only)

At restore the counts come back as today, but instead of a gap the state
holds a **pending judgement** (the saved baseline and `stoppedUnixMs`) and a
**provisional gap** at the restore time. The provisional gap is shown on the
wire (`lastGapUnixMs` is the later of it and the real one, `lossy` follows)
so nothing reads "Unused" before the judgement; it is never saved.

At the first ping that carries statistics, with that ping's `uptime` U,
`rule_hits` H, `received` R (events minus E3-marked ones) and the bridge's
time `now`, and the saved baseline (`pingUnixMs` P, `uptime` U₀,
`ruleHits` H₀):

- start then: `S₀ = P − U₀·1000`; start now: `S = now − U·1000`. Each is
  within [true start, true start + 2 s) (whole seconds, plus up to 1 s of
  ping delivery: the daemon's RPC timeout). `SLACK = 5 s`.

| # | Case | Test | Result |
|---|---|---|---|
| 1 | No baseline saved (version-1 file; or the saving run was TCP) | – | **gap** at restore, as today (nothing to judge) |
| 2 | Counter reset: `H < H₀` or `U < U₀` | daemon restarted → row 5/6 | |
| 3 | Daemon started after the last ping we counted: `S ≥ P − SLACK` | daemon restarted → row 5/6 | |
| 4 | Same start: `|S − S₀| ≤ SLACK` (and not 2/3) | **daemon stayed up**: baseline = (U₀, H₀), then the usual in-run check: gap iff `H − H₀ ≠ R` | no gap when every hit since the last saved ping arrived (the downtime's hits wait in the daemon's batch and come with this ping) |
| 5 | Restarted, previous run stopped cleanly (`stoppedUnixMs` set) | baseline = (0, 0): gap iff `H ≠ R` | no gap when the first ping holds every hit of the new daemon run |
| 6 | Restarted, no clean stop (crash, kill, a failed final save) | – | **gap**: events the old bridge received after its last periodic save are gone, and the new daemon's counter can't show them |
| 7 | Neither the same start nor a restart (suspend while the daemon kept running: its uptime lags the wall clock; a clock step between runs) | – | **gap** ("cannot tell") |
| 8 | `H − H₀ < R` (row 4) or `H < R` (row 5): impossible within one daemon run | the usual in-run check | **gap** |
| 9 | No ping during this whole run (daemon idle, down, or never connected) | – | stays pending: the wire keeps the provisional gap; the file keeps the old baseline and this run's `stoppedUnixMs`, and the next run judges |
| 10 | First ping is only E3 default-applied events | R = 0 | row 4: no gap iff `H = H₀`; row 5: no gap iff `H = 0` |
| 11 | TCP | – | **gap** at restore, as today |

Why rows 2 and 3 are safe even when wrong: a daemon that in fact stayed up
but is classed as restarted has `H = H₀ + Δ` with `R ≤ Δ`, so `H = R` only if
`H₀ = 0` and every hit since arrived, which is no loss. The dangerous
mistake is the other way (a restarted daemon classed as "stayed up", which
would subtract an `H₀` the new run never counted), and row 4 needs both an
unchanged start time and a start before the last ping: a restarted daemon
started after the old one's last ping (row 3 catches it first). Only a
backward wall-clock step larger than the time between the old daemon's last
ping and the new daemon's start, landing within 5 s of the old start, *and*
counters that happen to agree could fool it.

After the judgement the ping becomes the in-run baseline exactly as today;
daemon restarts **while the bridge runs** (uptime drop, counter drop) stay
gaps, unchanged.

### Limitations (documented, accepted by N3 (3))

- **The loss window.** Hits the *old* daemon run decided after the last
  ping the previous bridge run counted, until that daemon stopped, are not
  recorded and, in row 5, not noticed. With pings only on hits, "after the
  last ping" means the daemon's last batch (under ~1 s of hits) plus
  whatever it decided after the bridge stopped. On a reboot both stop
  within seconds. Pings the bridge receives between its shutdown save and
  the gRPC server's stop fall in the same window.
- **A bridge stopped long before the daemon restarts.** If the bridge is
  stopped while the daemon keeps running, and the daemon then restarts
  before the next bridge starts, every hit in between is lost unnoticed. For
  the system bridge this needs an admin to stop both the service and its
  socket (the socket otherwise re-activates the bridge as soon as the daemon
  redials) or a crash, which is row 6. Not detectable from the daemon's
  counters; a boot id or journal timestamps could bound it later.
- **Suspend.** Go's monotonic uptime stops during suspend, so a bridge
  restart after a suspend with the daemon still up is row 7 (a gap).
- **`MaxEvents`.** More than `MaxEvents` (250 shipped) rule hits while no
  bridge is connected are a gap (correctly: they are lost).
- **TCP** gets nothing from this change (above).

## Tests (written first)

Pure state (`cache/rule_hits/tests.rs`), every table row:
- row 1: a restore without a baseline is a gap at once;
- row 4: stayed up, `H − H₀ = R` → no gap, provisional gap gone, real
  `lastGap` back to the saved one; `H − H₀ > R` → gap at the first ping;
- rows 2 and 3: counter reset / uptime reset / start after the last ping
  with `stoppedUnixMs` and `H = R` → no gap; `H > R` → gap;
- row 6: restarted without `stoppedUnixMs` → gap even with `H = R`;
- row 7: start moved by more than `SLACK` but before the last ping → gap;
- row 8: `H − H₀ < R` → gap;
- row 9: pending survives `to_saved` (old baseline kept) and the wire shows
  the provisional gap;
- row 10: only marked events, both branches;
- row 11 / untrusted: restore is a gap at once, and `to_saved` has no
  baseline;
- the baseline saved is the last ping's (`pingUnixMs`, `uptime`,
  `ruleHits`), and a stayed-up daemon misclassified as restarted with
  `H₀ > 0` is a gap (the safety argument above).

File (`cache/rule_hits_file/tests.rs`): v2 round trip with `daemon` and
`stoppedUnixMs`; a v1 file loads with neither; an unknown field is ignored;
version 3 refused; bad `pingUnixMs` / `stoppedUnixMs` refused; the largest
file still fits.

Handle (`cache/rule_hits_handle/tests.rs`): `save_at_stop` writes even when
nothing changed and sets `stoppedUnixMs`; a periodic save clears it; a
restart through the file with the daemon kept (Unix) is no gap; TCP is.

End to end (bridge-cli, a whole bridge with persistent storage in a tempdir,
`mock_opensnitchd` over the Unix socket, like `leftover_unix_tests.rs`):
start, subscribe, ping (counters H₀); `shutdown()`; keep the mock's counters
and add hits while the bridge is down; start a second bridge on the same
state directory; the first ping's `RuleHits` keeps the old `lastGapUnixMs`.
Then the same with a daemon restart (counters from 0), and over TCP (a gap).

Mutation checks (no `cargo-mutants` here, done by hand): each guard in the
table flipped or removed must fail at least one test; results reported in
the PR.

## Verification

`nice -n 19`, `-j 4`, target inside the worktree, bridge tests with
`XDG_RUNTIME_DIR=/tmp/claude-1000/xdg-n3`:
`cargo fmt --all --check`; `cargo clippy --all-targets -- -D warnings`;
`cargo test -j 4 --no-fail-fast`; `just package-check`.

## Tower r13 gate (system bridge, Unix)

1. **Bridge-only restart.** Note `lastGapUnixMs` in
   `/var/lib/snitchwatch/rule_hits.json` (mode 0600, owner `snitchwatch`,
   `"version":2`). `systemctl restart snitchwatch-system-bridge`; make a few
   connections that match a rule (fewer than 250). After the first one,
   `lastGapUnixMs` in the next `RuleHits` (and in the file after a save) is
   **unchanged**, the journal says the daemon stayed up and nothing was
   missed, and the hits made while the bridge was down are counted.
2. **Over the cap.** Stop the bridge service *and* socket, make more than
   250 matching connections, start both: a gap at the first ping.
3. **Reboot.** `systemctl reboot`. The file has `stoppedUnixMs` from the
   shutdown. After boot and the first matching connection: no new gap
   (journal: the daemon restarted, nothing missed), provided fewer than 250
   rule hits happened before the bridge's first ping.
4. **No clean stop.** Stop the bridge's service and both sockets, remove
   `stoppedUnixMs` from the file (as a crash would leave it; keep owner and
   mode 0600), `systemctl restart opensnitchd`, start the bridge: a gap at
   the first ping (row 6). `kill -9` of the bridge with the daemon up
   (systemd restarts it) is row 4: no gap unless hits were lost.
5. **Daemon restart while the bridge runs** (`systemctl restart
   opensnitchd`): a gap, as before.
6. **"Unused" survives:** a rule that read "Unused" before a bridge-only
   restart reads "Unused" again after the first ping (it reads "No hits
   since <restart>" in between, at most until that ping).

## Owner questions

- **TCP.** Judge per-user (TCP) bridges from the counters too? Today's
  shipped `default-config.json` dials `127.0.0.1:50051`, so until #35 every
  per-user setup keeps the old behaviour (a gap per bridge start).
  **Recommendation: no**, until #35: a forged first ping could hide a gap.

## As built

- As designed above. The judgement is `restart_missed_hits` in
  `cache/rule_hits.rs` (logged at `info!`: `daemon = stayed up | restarted |
  cannot tell`, `clean_stop`, `missed`); trust is set by
  `UiService::with_daemon_transport` through
  `RuleHitsHandle::set_daemon_transport`; `RunningBridge::shutdown` calls
  `RuleHitsHandle::save_at_stop`, after which any later save writes again,
  unmarked.
- Tests: the table's rows in `cache/rule_hits/restart_tests.rs`; file format
  in `cache/rule_hits_file/tests.rs`; handle in
  `cache/rule_hits_handle/tests.rs`; whole bridges (Unix and TCP, two runs on
  one state directory, the mock keeping its counters) in bridge-cli
  `rule_hits_restart_tests.rs`.
- Mutation checks (by hand, 33 mutants: every guard of the judgement, the
  slack both ways, the provisional gap, trust at restore and save, the
  version range, both new time checks, unknown fields, the stop save's
  write/mark/revision, the transport wiring and the shutdown call): all
  killed.
- Not changed: Kirigami. Its `hit_badge.rs` module doc still says a bridge
  restart is a gap; on the Unix socket that is now only when hits may have
  been lost (a doc follow-up, no behaviour change).
