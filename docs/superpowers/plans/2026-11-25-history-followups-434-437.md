# History follow-ups #434-#437

Goal: close four filed follow-ups of history PR 8/9 in one small PR. The
reader is dormant until PR 11; PR 10 (control gate) depends on #434.

## Approach, tests first

- **#434** `NotIdle::wait_again(self, within)` is a method that waits on the
  registry the `NotIdle` was made from (its `Closer` holds the `Inner`), not on
  one the caller passes. Red: with the old API, an idle other registry
  completed a stuck one's quiesce. Green test:
  `waiting_again_is_bound_to_the_registry_that_timed_out`. Changed existing
  test: `a_timed_out_quiesce_can_wait_again_or_give_up` (call shape only).
- **#435** `production()` in `reader_structure_tests.rs` removes only
  `#[cfg(test)] mod x { .. }` / `mod x;` items by brace matching; any other
  `#[cfg(test)]` stays and is caught. New tests use synthetic sources: a seam
  after a test module, a seam before one, and nested/out-of-line modules.
- **#436** `Engine.eviction_rounds` (default `EVICTION_ROUNDS`, a test setter
  only under `cfg(test)`); the scenario runs on a 3 MiB store with 3 rounds, 6
  pages over high water: the first attempt pauses, the retry makes room. The
  `wait_for` deadline stays at 60 s.
- **#437** (1) A time cut with no event at limit 1 is `TooExpensive`,
  `retry: false`, reason "still too slow at the smallest page: try again
  later". Chosen over a counter because requests are stateless and over a new
  kind because the client action (stop, back off) is the same as TooExpensive.
  (2) The duplicate-flag test checks every row and the exact view at limits
  1, 50, 200. (3) The 200-ping/257-prompt test drops the wall-clock assertion
  and keeps the candidate bound and the VM-step ceiling. E1 is documented in
  `docs/CODEMAPS/bridge.md`.

## Mutation checks

- #434: `wait_again` succeeding regardless of idleness fails the new test.
- #435: the old split-at-first-`cfg(test)` fails the three synthetic tests; a
  seam inserted into `reader_sql.rs` fails the real check; a brace counter
  that ignores depth fails the nested-module test.
- #436: a pause that never clears fails the `cap_paused` assertion; an
  eviction that never runs in a retry times out.
- #437: the old `try_again` at limit 1 fails both limit-1 tests; marking
  one later row a duplicate fails the strengthened flag test.
