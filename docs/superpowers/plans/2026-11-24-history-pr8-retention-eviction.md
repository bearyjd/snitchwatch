# History PR 8: retention and eviction

Parent plan: `2026-10-28-connection-history-implementation.md` (PR 8 section).
Spec: `docs/superpowers/specs/2026-10-27-connection-history-design.md` D5 (251-308), D6/L1 (326-345), D7 snapshot bounds and cleanup (484-498), S2/S4.
Branch: `feat/history-pr8-retention-eviction`, stacked on `feat/history-pr7-gap-causes`. Bridge only, dormant (history is off by default).
Status: DRAFT 2026-11-24, waiting on the owner for the "Open decisions" below.

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

## Open decisions (not guessed; owner to decide)

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
