# History PR 8: retention and eviction

Parent plan: `2026-10-28-connection-history-implementation.md` (PR 8 section).
Spec: `docs/superpowers/specs/2026-10-27-connection-history-design.md` D5 (251-308), D6/L1 (326-345), D7 snapshot bounds and cleanup (484-498), S2/S4.
Branch: `feat/history-pr8-retention-eviction`, stacked on `feat/history-pr7-gap-causes`. Bridge only, dormant (history is off by default).
Status: APPROVED 2026-11-24. The owner accepted all six recommendations in "Decisions" below (1 b, 2 c, 3 a, 4 b, 5 a, 6 events-only pause). Implementation starts after PR 7 (#417) merges; this branch is then rebased onto main.
Update 2026-11-24: finding F1 (forward clock erosion) is implemented with schema v4, owner decision; see "Forward clock erosion (F1): done" below.

## Goal

Age retention (default 30 days; 7/30/90), the disk budget in operation (S2/S3/S4), the D5 eviction order (per-process byte share first, then oldest `ts_ms` regardless of verdict), chunked eviction obeying S2, snapshot cleanup on every deletion path, `evicted` gap markers, and the pause with `write-failed (cap)` plus exponential backoff when eviction cannot get under the high-water mark. Nothing grows past the budget; nothing outside `history.db` is ever deleted.

## What exists (explore pass, 2026-11-24)

Parts of D5 are already in `writer.rs`; PR 8 must replace or extend them, not duplicate them.

- `writer.rs` (700 lines, near the cap): `Engine::{evict_chunk, vacuum_chunk, vacuum_all, make_room, ensure_room}`. `make_room` runs `EVICTION_ROUNDS = 3` rounds of "evict one chunk of `rows_per_txn = 32`, then `vacuum_all`" and returns `false` if still over `Budget::high_water_pages()`. `evict_chunk` is **oldest-first only** (`snapshots::delete_oldest_events`). `Engine::transact` is the one S2 gate (journal tripwire, observer, test fault seam).
- `Writer::spawn` loop: on any `write_batch` error counts `failed`/`lost` and sleeps a fixed 500 ms. No exponential backoff, no `write-failed (cap)` distinction, no gap row (the capture-side `lost_batch` does that for the pinned path).
- `snapshots.rs`: `delete_oldest_events(conn, n)` deletes the n oldest events and, in the same transaction, each `rules_seen` row whose hash no remaining event references. This is the only deletion path today. `admit_snapshot` already evicts the oldest snapshot over its own 2,000 / 4 MiB bound (independent of events).
- `gaps.rs`: `Engine::record_evicted(earliest_remaining_ms, removed, registry, limits)` exists (PR 7), but it commits its **own** transaction via `commit_gap`, not the deleting one. Nothing calls it.
- `ledger.rs`: `record_gap` merges same-cause neighbours; `enforce_limits` compacts to an `uncertain` span (L1). The `evicted` cause is in the frozen schema CHECK (`schema.rs` `GAP_CAUSES`). `gaps` has no free-text/subtype column.
- `admission.rs`: the per-process byte share is a **refusal** of what is admitted this run (`Refusal::ByteShare`); `release_bytes` has no caller except capture's `refund` on a failed batch. A `KNOWN GAP (D5), BLOCKING PR 8` comment lists: (1) self-eviction replaces refusal, (2) the ledger loads from the DB at start, (3) retained bytes accounted with snapshot bytes and `release_bytes` called by retention.
- `capture.rs` (794 lines, near the cap): `estimate_bytes(record)` is the byte measure admission charges. Capture and `Engine` run on different threads; `Admission` sits behind a `Mutex` in capture, the engine is owned by the writer thread.
- `schema.rs`: v1 `events` has indexes on `ts_ms` and `rule_hash` only; **no index on `process_path`**. Schema is frozen/byte-compared on open.
- `budget.rs`: `high_water_pages`, `rows_per_txn`, `vacuum_steps_per_txn`, `ledger_pages` (2% reserve) all exist. `Engine::new` refuses a file already over the cap (`OverBudget`) and deletes nothing.
- `settings.rs`: `retention_days` and `budget_mib` exist and are validated; changing them is PR 10 (control gate), not here.

## Design

New `history/retention.rs` (target under 400 lines) owns the policy; `writer.rs` keeps `transact` and delegates. New `history/retention_tests.rs`. No schema change unless Open decision 3 says so.

1. **Victim selection, three tiers per eviction pass**
   1. Age: events with `ts_ms < now - retention_days` (oldest-first, chunked). Expiry is a normal eviction (marker, snapshot cleanup) and does not need the budget to be tripped.
   2. Process share: if any process holds more than its share (`share_percent` of `main_bytes`), delete **that process's** oldest rows until it is under share.
   3. Global oldest-first by `(ts_ms, id)`, regardless of verdict (owner decision 4).
   Tier 3 runs only when tiers 1 and 2 leave usage over the high-water mark. A process under its share is never a victim of tier 2.
2. **One chunk = one transaction** through `Engine::transact(TxnKind::Evict)`: delete up to `rows_per_txn` rows, delete every unreferenced snapshot (`NOT EXISTS`, same transaction; shared helper used by all three tiers, so no path can skip it), and write the `evicted` gap in the **same** transaction (`record_gap` inside the work closure, not `commit_gap`). `earliest_remaining_ms` is read inside the transaction after the delete.
3. **Space return (S4):** `vacuum_all` after each round exactly as now (`incremental_vacuum(N)` in short transactions); no full `VACUUM`.
4. **Pause rule:** after `EVICTION_ROUNDS` rounds still over high-water, `make_room` reports failure; the writer then pauses recording, counts the queued batch into `write_failed` (the existing `lost_batch` path, so W1 pins hold), and retries with exponential backoff (start 500 ms, x2, cap 60 s, reset on the first successful batch). During a pause the queue keeps its byte/count bound and overflows into `queue-overflow`; it never grows.
5. **Accounting feedback:** every deleted row's size (same `estimate_bytes` formula, see Open decision 2) is returned in the eviction report per process so `Admission::release_bytes` is called; the admission ledger is also seeded from the DB at start (KNOWN GAP 2). Snapshot bytes count toward a process only when attributable to it alone.
6. **Age clock:** cutoff is computed from wall-clock `now_ms`. A backwards step expires nothing extra. See Open decision 4 for forward jumps.

## Decisions (owner accepted every recommended option on 2026-11-24)

Each item below keeps its option text; the option marked "Recommended" is the decision. Item 6 is decided as events-only pause. Consequences to honour: item 1 changes PR 5 behaviour (`Refusal::ByteShare` is replaced by self-eviction, update its tests first and show them fail); item 3 adds no index and no schema bump, but measure the chunk time; item 4 adds the clock-jump expiry guard and a per-pass cap; item 5 adds no schema change, so "cap" lives in live state and logs only; item 6 needs a test that the ledger reserve covers gap and heartbeat writes while event inserts are paused.

1. **Refusal vs self-eviction at the byte share.** Today an over-share process is refused (`Refusal::ByteShare`). The spec (D5 lines 293-296) says an over-share process's own oldest rows are evicted first, then others. Options: (a) keep refusal as the admission gate and add self-eviction only as the budget-pressure tier (a process can sit at its share but never push others out); (b) replace refusal with self-eviction so a noisy process keeps recording newest-for-oldest at its share. Recommended: (b), since the blocking comment says self-eviction replaces refusal and refusal creates a permanent `admission-drop` gap for a process the user may care about. Needs your call because it changes PR 5 behaviour and its tests.
2. **Byte measure for share and release.** Options: (a) reuse `estimate_bytes` (cheap, matches admission, but an estimate that drifts from real page usage); (b) measure real page use per process via `dbstat` (exact, but needs the `SQLITE_ENABLE_DBSTAT_VTAB` build and is slow); (c) sum column lengths plus a fixed per-row and index overhead computed in SQL. Recommended: (c) with the constant shared with `estimate_bytes`, so admission and eviction cannot disagree. Open because the choice sets what "25% of retained bytes" means.
3. **Per-process victim lookup without an index.** Finding a process's oldest rows scans `events` (no `process_path` index). Options: (a) accept a bounded scan per chunk (file is at most B/2, 32 MiB at the default; run only when a process is over share); (b) add `CREATE INDEX events_process_ts ON events (process_path, ts_ms)` as a schema bump to v3 (more write amplification and index pages inside the S2/S3 bound, frozen-schema test and migration needed). Recommended: (a) now, measure, bump only if the chunk time breaks the "short transaction" goal. Open because (b) is a durable format change.
4. **Forward clock jump and age expiry.** A wall clock stepped forward by months would expire all history on the next pass. Options: (a) no guard (matches spec text, which is silent); (b) skip age expiry while the PR 7 `clock-jump` detector flagged the current heartbeat interval, and cap how much one pass may expire (for example at most one retention window's worth of the oldest 10%); (c) require the jump to persist over N heartbeats. Recommended: (b). Open because it adds an unspec'd rule that can delay a legitimate expiry.
5. **How `write-failed (cap)` is stored.** `gaps.cause` is a frozen CHECK enum with no subtype column, so "(cap)" cannot be stored without a schema change. Options: (a) store plain `write-failed` and let the status/log say "cap" from live state only (the label is lost after restart); (b) schema bump to add a `detail` column or a new cause. Recommended: (a). Open because the spec text names the label.
6. **Pause scope.** Whether the pause stops only event inserts (gaps and heartbeat keep writing from the 2% ledger reserve) or all writes. Recommended: events only, ledger writes continue; but that relies on the reserve being enough while paused, which is only proven for the L1 row ceiling.

## Files

- New: `history/retention.rs`, `history/retention_tests.rs`.
- Edited: `history/writer.rs` (delegate `evict_chunk`/`make_room`, backoff; must stay under 800 lines, extract if it would not), `history/snapshots.rs` (shared unreferenced-snapshot helper), `history/gaps.rs` (`record_evicted` becomes the in-transaction form), `history/admission.rs` (remove the KNOWN GAP comment, add seeding), `history/capture.rs` (wire `release_bytes` from the report; only a few lines, file is at 794), `history/mod.rs` (module and test entries).
- Never touched: anything under `vendor/`, any path outside the history directory.

## Tests first (each seen to fail before the code exists, then pass)

1. Age limit exact: rows at `cutoff - 1` are deleted, at `cutoff` and `cutoff + 1` kept; 7/30/90 day settings.
2. Size limit exact: fill to the high-water mark; one more batch evicts exactly the oldest chunks needed; used pages end at or under high-water; main file never above `max_page_count`.
3. Eviction stays inside the page budget: with the journal observer, every `TxnKind::Evict` report has `journal_bytes <= journal_limit_bytes()`; fill-to-limit run keeps main + journal + settings <= B (S2/S3 pattern from `budget_tests.rs`).
4. Chunking: no single eviction transaction deletes more than `rows_per_txn` rows.
5. Order: a mixed-verdict set (allow/deny/prompt answers) is evicted strictly by `(ts_ms, id)`; verdict never changes the order.
6. Process share: a hostile process over its byte share loses its own oldest rows first; another process's small records survive the same flood (mixed-size flood from one process, D5 test); a process under its share is never a tier-2 victim; once the hostile process is back under share, tier 3 may delete others oldest-first.
7. Snapshot cleanup on every path: below the cap, deleting a single event by age, by process share and by global oldest-first each removes its now-unreferenced snapshot in the same transaction; a snapshot still referenced by a remaining event stays; a fault injected before commit leaves both event and snapshot.
8. `evicted` marker: same transaction as the delete (fault seam before commit leaves neither); earliest remaining time correct; consecutive markers merge (L1); a marker never claims events remain that were removed.
9. Pause: eviction that cannot reach high-water after 3 rounds (ledger full / nothing evictable) returns "not made"; the batch is counted into `write_failed` with its pins held until the gap row commits; retry intervals double up to the cap and reset after a success; the file never exceeds `max_page_count` meanwhile.
10. Never deletes unrelated files: a canary file beside `history.db` (and the settings file) is untouched after a fill-to-limit run, a pause and an age sweep.
11. Admission feedback: after eviction the evicted process's `retained_for` drops by the deleted bytes and it is admitted again; the ledger is seeded from the DB at start (restart test).
12. Backwards clock expires nothing extra; Open decision 4 behaviour for a forward jump.
13. S4: delete 90% of rows; file shrinks; no journal over the S2 limit (existing pattern, now through retention).

Isolation: every test that starts a bridge uses a tempdir `XDG_RUNTIME_DIR` and `XDG_STATE_HOME`. No real `opensnitchd`.

## Mutation checks (apply locally, name the test that must fail, revert)

- Order by `id` instead of `(ts_ms, id)`, or filter by verdict: test 5.
- Drop the unreferenced-snapshot delete from each of the three tiers in turn: test 7, one failure per tier.
- Move `record_gap` out of the delete transaction: test 8.
- Make tier 2 evict by global order, or skip the share check: test 6.
- Delete `rows_per_txn + 1` per chunk, or loosen `max_page_count` / the journal limit: tests 4 and 3.
- Remove the backoff doubling, or the pause (fall through to insert): test 9.
- Off-by-one in the age cutoff (`<=` for `<`): test 1.
- Remove `release_bytes` after eviction: test 11.
- Widen any path-based delete (for example delete the settings file or `-journal` on eviction): test 10.

## Review focus

Eviction never deletes unrelated files and never lets the file or journal grow past the budget; the three tiers cannot leave an unreferenced snapshot behind; the `evicted` marker and the delete are one transaction; a hostile process cannot evict others beyond its byte share; the pause path still holds W1 pins; no function over 50 lines, no file over 800 (`writer.rs`, `capture.rs` are close).

## Verification

`CARGO_TARGET_DIR=/home/user/.sw-pr8-target TMPDIR=/home/user/.sw-pr8-tmp just check` and `just test-bridge`, then `just test` before calling it done. Review by `code-reviewer` plus `security-reviewer` (filesystem deletion), separate from the author pass. No push, no PR from this branch without instruction.

## Repair pass (three independent reviews, 2026-11-24)

What changed after review, and what is deliberately not fixed. This supersedes the text above where they differ.

- **Pause rule (design item 4, test 9).** The "three rounds" rule is gone. `make_room` evicts in chunks of `rows_per_txn` while each chunk removes rows, up to `EVICTION_ROUNDS` (now 128 chunks, about 4,096 rows, about a second), and the pause happens only when the store is still over high water after that or a chunk removed nothing. Reason: going down to the low-water mark below cannot be done in three chunks, and a pause while progress is being made throws events away for nothing.
- **Hysteresis.** Eviction goes down to the low-water mark (`Budget::low_water_pages`: high water minus a twentieth of the file), so one eviction serves many batches; before, the headroom was about 32 rows, so every pressured batch evicted again. Cost: an eviction takes up to about 5% of the file's history (the oldest, or an over-share process's own) earlier than the high-water rule alone would.
- **Tier 2 on cheap state.** The two full-table aggregates of tier 2 run only when `largest per-process event tally + all snapshot bytes > share`. The tally (`retention_room.rs`) is learnt by one scan at the first pressure, kept current by both insert paths and by eviction, and dropped (so the next pressure scans) past 1,024 processes.
- **Victims::Process cursor.** A later chunk of the same process starts after the `(ts_ms, id)` the previous one ended at, seeking through the `events_ts` index, instead of reading all older rows of other processes again. Tier 2 also stops at the chunk limit.
- **Age expiry** gives way between chunks when the queue is closed (shutdown) or after one second; a pass that gave way does not vacuum, and the next pass carries on.

### Forward clock erosion (F1): done, schema v4

The owner chose a schema change (v4). Tier 1 now spends from a persisted bucket (`history/expiry_budget.rs`), independent of the jump detector.

- **Storage.** `ALTER TABLE meta ADD COLUMN expiry_debt_milli INTEGER NOT NULL DEFAULT 0 CHECK (expiry_debt_milli >= 0)`: one column, not the two of the earlier proposal (`expiry_window_start_ms` and `expiry_window_removed`). The start column was a wall-clock time, which is the quantity a stepped clock forges, so it is gone; the debt (thousandths of a row spent and not yet refilled, as of the last spend) is all that has to persist. Migration v1/v2/v3 to v4 is one `BEGIN IMMEDIATE` transaction under the page cap (the existing path, `migration_steps`), idempotent, rolls back whole when killed at any step (tested with a real `kill -9` after each step from v1, v2 and v3). A v3 build refuses a v4 file by its version ("newer schema"), as v3 refused v4 before. No `sqlite_master` object is added, so the object count is unchanged.
- **Bucket.** Capacity `(events + debt rows) / 8` (12.5%), never under one chunk (`rows_per_txn`), so a small store still expires, and never under `ceil(1.25 * (events + debt rows) / retention_days)` (second repair pass). The floor matters only at 7 days: keeping up with a window of `R` days takes about `events / R` rows a day, 14.3% at 7 days, over the eighth, so without it the 7-day option has no headroom at 100% uptime (the simulation sat exactly at the break-even) and falls behind on any pause. With it 7, 30 and 90 days all keep up with their inflow at full uptime (`every_retention_option_keeps_up_with_its_inflow_at_full_uptime`); 30 and 90 days stay at the eighth (4.2% and 1.4% are under it). A store of 32 events or fewer expires wholly in one pass (the chunk floor; `a_small_store_still_expires_whole_in_one_pass`). Adding the debt back approximates the count when the window began, so spending a full bucket leaves it exactly empty. Refill is linear over 24 hours. The per-pass tenth, the jump skip and the time limit are unchanged and apply on top.
- **Clock.** Refill uses the process's monotonic uptime (`Instant`), never a wall delta: a wall clock stepped a year forward refills nothing. Time while the bridge is down is not counted (a monotonic reading is meaningless across a restart, and a wall delta across one is what a step forges), and a restart reads the debt back, so it does not reset the bucket. A persisted debt above the bucket is cut to it, so a corrupt value costs at most one day of uptime.
- **Atomicity.** The debit is written in the transaction that deletes the rows of a tier-1 chunk; the in-memory debt changes only after `COMMIT`. A `meta` row exists once a session has begun, which always precedes the first pass; without one the debt is kept in memory only.
- **Trade-off, stated.** A legitimately idle store expires over days, not in one pass: with 1,000 expired events and one current one, passes every ten minutes leave 768, 671, 586 and 511 events at the ends of days 1 to 4 (`an_idle_store_expires_over_days_on_this_exact_schedule`). Changing the retention from 90 days to 7 does not clear the older rows at once; a store of a year of history takes weeks to reach its limit. The first day removes an eighth plus what refills, about 23%. Evictions under disk pressure (tiers 2 and 3) are unaffected: the budget limits only age expiry. Fifty passes on a clock a year ahead and no uptime between them leave 87.5% of the events; fifty passes ten minutes apart (about eight hours) leave 83.8%.
- **Uptime, stated.** Refill counts the monotonic clock, which excludes system suspend (`CLOCK_MONOTONIC` stops while the machine sleeps), so a laptop that sleeps refills only while awake. A mostly-down bridge therefore **retains data longer than the window**: the store settles where the refill it gets matches the inflow. Measured at 50% uptime and 7 days: about 8.8 days (13.5 without the floor); the 7-day year-forward case leaves 822 of 1,001 events (82%) instead of 876. This is an honest privacy note: the retention setting is a ceiling on how fast history is removed, not a guarantee of when it is gone, when the bridge is rarely running. The default (30 days) is unchanged by the floor. A bridge that restarts more often than every 10 minutes never runs an expiry pass (the schedule starts at boot); that over-protects the data, it is not a bypass.
- **Not covered.** A bridge that is mostly down refills only while up (a bridge that runs one hour a day refills one 24th of a bucket a day). A step that moves the clock backwards is not a budget matter: it expires nothing extra, as before.

### Known limits (documented, not fixed)

- A flooder that spreads over four or more distinct paths, each under 25% of the main file, bypasses the share tier; it is then limited only by admission's rates and by tier 3 (oldest first), which takes other processes' history too.
- Owner decision 1b removed `Refusal::ByteShare`: admission no longer caps how much of the store one process's old history holds, and so other processes' old history is no longer protected by admission either; only tier 2 (under pressure) trims an over-share process.
- Persisted L1 gap-ledger compaction is not folded into the evicted marker; `note_released` forgets processes past 1,024; rows dated in the future by a wrong clock are not expired until the clock catches up.
