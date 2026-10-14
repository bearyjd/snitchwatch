# N3 clean-stop trust window, and the HELLO generation race

**Date:** 2026-10-14
**Source:** issue #120 items 27 and 22 (owner decisions of 2026-10-14), issue #159.
**Baseline:** `main` @ `4ead981`, branch `fix/round2-n3-window`.
**Size:** S. Bridge `cache/rule_hits.rs` (+ tests), docs; item 22 is test-first
and may add nothing but a pin.

## Item 27 + #159: a clean stop is trusted for a time window only

Owner decision: trust a saved clean stop (`stoppedUnixMs`) only if the new
daemon started at or after `stoppedUnixMs` and within about 10 minutes of it.
The daemon's start is `now - Statistics.uptime` (the same estimate
`restart_missed_hits` already makes as `started_now`). Otherwise the restart
is a gap. A false gap after a long power-off is accepted: a missed gap is the
costly error. A boot id was rejected (every reboot would become a gap).

Change, in `restart_missed_hits` only (the "restarted" row; the "stayed up"
row doesn't use the stop):

- `CLEAN_STOP_WINDOW_MS = 10 * 60 * 1000`, named and documented.
- `clean_stop` = stop present AND `uptime > 0` AND
  `stopped <= started_now <= stopped + window`.
- Conservative cases, all a gap: `uptime == 0` (also what a Statistics with no
  uptime decodes to), a start before the stop (also a clock that stepped back),
  a start further than the window after it.
- No slack on the lower edge: the start estimate is never earlier than the
  true start (uptime is floored, delivery only adds), so a daemon that started
  before the stop cannot look as if it started after it by more than ~2 s; a
  false gap in that corner is the safe direction.

This covers #159: a stale Unix stop that outlives a TCP run is old by the time
the next Unix run's daemon restarts (the TCP run plus the restart take longer
than the window in practice, and a daemon that restarts within the window of
the stop did so before the TCP run could have counted unsaved hits, only if
the TCP run is shorter than the window; see the limitation below).

Tests first (`cache/rule_hits/restart_tests.rs`): a long power-off -> gap; a
quick restart -> no gap; daemon started before the stop -> gap; zero uptime ->
gap; window edges; #159's scenario (Unix stop, TCP run in between, restart
outside the window) -> gap. The existing "started after the last ping" test
uses an hour of uptime, which now correctly reads as started before the stop;
it is changed to a young daemon.

Docs: the N3 plan's Limitations (rollback/roll-forward and the new window) and
the `attach_file` doc (the item-24 trade-off text).

### Residual

A TCP run shorter than the window, followed by a daemon restart inside the
window, is still not noticed. The TCP run would have to start and end, and the
daemon restart, all within ~10 minutes of the Unix stop.

## Item 22: does `hello()` bumping the generation drop records?

`MaybeApplied` records (curated manager) carry the stream generation
(`DaemonCommands::stream_ready`, bumped by every HELLO) and a pass keeps only
records of the current generation (#120 item 14: a reconnect's snapshot is the
daemon's memory). Suspected race: a HELLO that does not bring a new snapshot
from the daemon (a second stream re-adopting the staged pre-install snapshot
inside its 30 s, or a second HELLO on the same Unix stream) bumps the
generation and forgets a record, while the list is the stale pre-install one.

Test first (curated `manager_unanswered_tests.rs`): an unanswered install,
then a second stream's HELLO re-adopts the staged snapshot; turning the entry
off must still delete by name. If it fails, the race is real and the fix
stamps records with the staged snapshot's sequence (bumped by `stage`, i.e. by
a new `Subscribe`) instead of the HELLO count. If it passes it stays as a pin
and this section records that.

## As built

(filled in below as work lands)
