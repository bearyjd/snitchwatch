# Unanswered-prompt diagnosability (post-r14 tower finding)

**Status 2026-10-22: item 1 shipped in #186 (r15). Items 2-3 are open.** This
note was written on 2026-10-21, when the tower had asked that nothing ship until
its r15 cut; r15 is out now, so no hold applies.

## Finding
On the r14 laptop, the bridge logged 63 `nobody answered the prompt` lines in
~38 min (ask-1106..ask-1198, ~2/min, still going), while the system was
otherwise quiet. The line is `tracing::info!` at
`crates/snitchwatch-bridge/src/deferred_answers.rs:61` and carries only
`row_id`. opensnitchd's journal logs no connections, so the host journal cannot
say what is asking. The line fires once per prompt that times out
(`ANSWER_TIMEOUT` = 30 s), so ~2/min means ~2 unmatched asks/min.

## 1. Log what was asked (small, bridge-only)
`answer_unanswered` already holds the `ConnectionRow` returned by
`cache.defer_pending`. Add `process`, `process_path`, `dst_host`, `dst_ip`,
`dst_port`, `protocol` to the `info!`. `ConnectionRow`
(`ws_messages/verdict_wire.rs:93`) has **no uid field**, so uid is not
available there; getting it means checking what the translator keeps from the
daemon's `Connection` (`user_id`) before promising it. Decide: log uid only if
the row or cache already has it, otherwise leave it out and note why.
- Test: drive `answer_unanswered` with a known row and assert the fields appear
  in the captured log (pattern: existing `deferred_answers` tests).
- Check the fields are not sensitive beyond what the GUI already shows; the
  journal is root/user-readable only.

## 2. Triage the other two lines from that boot
- `re-broadcast state snapshots after feed lag` x10
  (`snitchwatch-bridge-cli/src/lib.rs:614`): read the surrounding code and
  decide if 10 in ~38 min is a lagging subscriber under normal load or a
  broadcast-capacity problem. No change unless the cause is real.
- `profiles: NetworkManager poll failed; keeping last known network identity`
  (`profiles/network_watcher.rs:107`), one WARN: need the full `error` text from
  the tower's journal (it was truncated). Likely NM absent or D-Bus briefly
  unavailable; confirm before touching.

## 3. What the unmatched asks are (needs the owner, not code)
The tower is having the owner read the pending/dismissed rows in the GUI. Once
named, either a recommended rule covers them or it is a gap for #178. Known
nearby facts from the tower: systemd-resolved runs as uid 990, chronyd as uid
994 (not root), and DNS resolved via tailscale0 MagicDNS. (Correction
2026-10-22: the DNS opt-in rule is pinned by account NAME and the uid is looked
up at runtime, not fixed at 193; see the platform plan, WS2.) Don't write rules
from guesses; wait for captures.

## Order
Item 1 is done (#186) -> item 2 findings -> item 3 once the owner reports. `just check` and `just test-bridge` before the PR; no bridge
spawned outside an isolated `XDG_RUNTIME_DIR`.
