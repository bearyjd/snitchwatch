# N3 rule-hit counts and test hygiene: #120 follow-ups

**Date:** 2026-10-13
**Source:** issue #120 items 5, 21, 23-26, 28, 32, 34, 35/39.
**Baseline:** `main` @ `bbc5778`, branch `fix/followups-120-n3-tests`.
**Size:** S. Bridge `cache/rule_hits_handle.rs`, tests, one Kirigami comment.
Item 27 (owner decision) and the curated/cache production code are out of scope.

## Items

- **24** A TCP bridge never trusts a clean stop, so it has none to consume: a
  failed rewrite of a Unix-written file must keep the file. `consume` needs
  `daemon_counters_trusted()`. Test first (Unix file with a stop, TCP
  attach, unwritable rewrite).
- **23** `a_clean_stop_that_cannot_be_rewritten_is_removed_with_the_file`
  gets a 3 ms sleep before `started`. Proven against the pre-fix source in a
  private copy.
- **25** Pings counted after the shutdown save (gRPC stops later) are saved:
  a `record` after `save_at_stop` writes the counts again as a clean stop
  (`SaveKind::AfterStop`, only when something changed). The check follows
  the state update, so a stop save that read the state earlier is always
  seen. Tests: handle unit test, and a whole-bridge test (Unix transport)
  that pings between the shutdown save and the shutdown.
- **26** Already documented in the N3 plan ("Two bridges on one state
  directory ... no lock ... unreachable"): nothing to add.
- **21, 35/39** Curated test files only: reach the record through a refused
  install; bound the unbounded wait so a broken `entries()` fails.
- **32, 34** Check the allowance test against the old code; a Unix e2e for
  the count hint if the harness allows.
- **5** One loader model for `test_daemon` and `mock_opensnitchd`.
- **28** Comment in `hit_badge.rs`.
