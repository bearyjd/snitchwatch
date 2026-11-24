# History PR 7: gap causes and standing labels

Parent plan: `2026-10-28-connection-history-implementation.md` (PR 7 section).
Spec: `docs/superpowers/specs/2026-10-27-connection-history-design.md` D6 (lines 310-380).
Status: APPROVED 2026-11-24, owner took the recommended option on all three decisions below. Bridge only, dormant (history is off by default).

## What exists (explore pass, 2026-11-24)

- `history/ledger.rs`: `GapCause`, `Gap`, `record_gap` (merge), `enforce_limits`, `read_gaps`, `LossCounter` (`record`, `record_pinned`, `pending_gap`, `clear`).
- `history/durable.rs`: `flush_counter` (gap row + pin release in one transaction), `heartbeat`, `commit_gap`. **No caller outside `durable`.**
- `history/session.rs`: already writes `bridge-down` and start-time `disabled` gaps.
- `history/capture.rs`: `Losses { queue, admission }` counters with pins; `CaptureStats` has `lost_write`, `skipped_nolog`. Comments at :41/:55 and `writer.rs:10` defer counter-to-gap flushing to this PR.
- `cache/rule_hits.rs`: `record` (:233) computes `received`; `missed_events` (:319) only yields a boolean + `last_gap_unix_ms`. The missing count is not exported.
- Daemon down/up edge exists only in `daemon_watchdog.rs` (`cache.set_daemon_down`); history is not told.
- `history/gaps.rs` does not exist. No stock-vs-fork capability flag; `daemon_contract::is_default_action_rule` is per event.

## Design

New `history/gaps.rs` is the single driver; `ledger.rs` stays the storage layer.

| Cause | Source | Mechanism |
|---|---|---|
| `queue-overflow`, `admission-drop` | existing `Losses` counters | flush on heartbeat, disable, shutdown via `flush_counter` (W1: pin held until the row commits) |
| `write-failed` | writer `lost` | new `LossCounter(WriteFailed)` fed from the writer failure path, same flush |
| `daemon-disconnected` | `daemon_watchdog` down/up edge | open gap at down edge (exact start), close at recovery (exact end); crash while open leaves the watermark-based uncertain gap |
| `upstream-loss` | `RuleHits::record` | return `{missing, restarted}` (`missing = max(0, Δrule_hits - received)`); feed an `UpstreamLoss` counter. Lower bound; never WS `Lagged` |
| `disabled` | history toggle | explicit gap writer for mid-session disable (start-time case already in `session.rs`) |
| `evicted` | writer eviction path | marker only; PR 8 owns eviction itself, so PR 7 provides the `record_evicted(earliest_remaining, removed)` API and PR 8 calls it |
| `clock-jump` | heartbeat | compare wall delta with monotonic delta between heartbeats |

Standing labels (not gaps): `nolog` (from `skipped_nolog`), stock-daemon default-applied traffic, no-GUI prompts. Exposed as a small `StandingLabels` struct on the history status; wire exposure is PR 11.

## Decisions for the owner

1. **Stock-vs-fork capability source.** Recommended: in-memory "fork default-action marker seen this session", never persisted. The label reads "stock daemon: default-applied traffic not observable" until a marker is seen. Alternative: persist in `meta` (adds schema change).
2. **`clock-jump` threshold.** Spec leaves it undefined. Recommended: any backwards step, or forward divergence between wall and monotonic delta greater than 60 s per heartbeat interval.
3. **`upstream-loss` count.** Recommended: gap `count` = the `missing` lower bound summed over the merged interval, flagged as a lower bound; counter resets/restarts add count 1 with the uncertain flag.

## Tests first (each seen to fail, then pass)

1. Queue overflow and admission drop flush to gap rows on heartbeat; row and pin release commit together.
2. kill -9 equivalent (fault seam) with an unflushed counter, then restart: the gap begins no later than the pin's arrival (W1).
3. Daemon disconnect via `MockOpensnitchd`: down edge opens, recovery closes, a crash while open yields an uncertain gap.
4. Counter reset and `Δrule_hits > received`: `upstream-loss` count equals the lower bound; a WS `Lagged` event produces no gap.
5. Full-disk fault seam produces `write-failed`; the pin is retained.
6. Backwards clock and a forward jump past the threshold produce `clock-jump`.
7. Mid-session disable produces `disabled`.
8. Standing labels present for `nolog`, stock default-applied traffic, and no-GUI prompts; absent when not applicable.
9. No-hole invariant: after every scenario, each interval without events is covered by a gap or the explicit "no events recorded" statement (ledger continuity).

Mutation checks: break each flush, the pin release, the down-edge hook and the `missing` computation locally and show the named test fail.

## Files

New `history/gaps.rs`. Edited `history/{capture,durable,writer,session,mod}.rs`, `cache/rule_hits.rs` and `cache/rule_hits_handle.rs`, `daemon_watchdog.rs`, `grpc_server.rs` (ping tap). Keep each file under 800 lines.

## Review focus

No hole is ever shown as an empty interval; W1 pin and transaction semantics preserved; `upstream-loss` is a lower bound and independent of WS `Lagged`.
