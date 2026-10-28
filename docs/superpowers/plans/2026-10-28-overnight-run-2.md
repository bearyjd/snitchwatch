# Overnight autonomous run 2 (2026-10-28)

Status: **Started by the owner, 2026-10-28** ("plan to work through all of the
listed issues autonomously through the night. commit, push, pr, TDD, then
adversarial review, then CI. when CI is green, merge. work through each").
This plan records the order and the stop rules; it does not wait for approval.

## Standing workflow, for every item

1. A failing test first, shown red.
2. Commit, push, open a PR (never a force-push).
3. `/codex` adversarial review (always `</dev/null`, filesystem-boundary
   preamble). Fix every CRITICAL, HIGH and MEDIUM, then review again; LOW when
   cheap, otherwise recorded in the PR.
4. Merge when CI is green. After each merge or batch, give the tower the SHA.
5. At most 3 agents at once. Fresh agents get a full brief; a reboot wipes `/tmp`.

## Stop rules

- Park as a draft after 4 `/codex` rounds that still have a HIGH or CRITICAL.
- Park anything that needs an unplanned bridge-wire, daemon, image or packaging
  change, and tell the tower before merging anything that touches
  `crates/snitchwatch-bridge`, `crates/scanner-*` or `packaging/`.
- An unrelated CI failure twice is recorded as a flake and not chased tonight.
- Never without the owner: staging or rebooting the host, host files, tower refs,
  upstream posts, a force-push (except a lease-guarded rebase), deleting
  worktrees or `dist`, or running anything that needs the owner's admin prompt.
- Keep `/tmp` below 80%; pause if it fills and delete nothing.

## Batch A: close what is open

| Item | State | Next |
|---|---|---|
| #249 Q01 plan | CI green, review round 4 done | merge |
| #256 scan-page render test | hardened after review | re-review, merge |
| #253 Q01 slice 2 | round 3 of 4: two MEDIUM test gaps | fix, review, merge |
| Q01 slice 1 (the allow/deny prompt) | built, PR opening | review, fix, merge |
| #252 scanner watchdog | **parked by the owner** | stays a draft |
| GUI rebuild + install | needed for #251, #253, slice 1 | once #253 and slice 1 merge |

## Batch B: Deep Scan shows nothing in the GUI

The owner ran Deep Scan; the root scan, the broker reply and a headless render of
the page all work, so the cause is not found yet. Without the owner's input, do:

1. **Make the outcome visible.** After every scan the page must say what happened,
   including a clean result ("Last scan: HH:MM, scan #N, 0 new, 2 informational")
   and an unreachable broker, never a page that looks unchanged. TDD with the
   existing render probe.
2. **Log the scan request and outcome** (request sent, reply code, report size) to
   the GUI's own log, so a launch from a terminal shows it.
3. If the owner reports what the real window shows, follow that first.

## Batch C: Q01 keyboard and screen-reader slices

Per `2026-10-28-q01-next-slices.md`: slice 3 (rebase the pushed work-in-progress
`feat/q01-rules-keyboard` onto main), then 4 learning review and its banner, 5 list
pages, 6 status and settings, 7 navigation, plus the map slice 2b. One small PR
each. Owner decisions in force: Return only selects a focused scope or duration
choice and Escape is always Deny (D1); Tab works in the prompt (D2).

## Batch D: connection history, run 1

Per `2026-10-28-history-run-1.md`, which the owner approved without the S2 waiver.
The owner's message starts it. Order: PR 1 storage, schema and field caps; PR 2
writer and page budget; then **stop for the owner's release**; then only the
database-free halves 3a and 4a. Every PR keeps the test that a bridge started
without history enabled creates no `history/` directory. This batch touches
`crates/snitchwatch-bridge`, so the tower gets notice before the first merge of
bridge code. The S2 page-constant result, or its full-journal fallback, is the
owner's decision and is never merged on a guess.

## Batch E: handoff

Update `.omc/STATE.md` and `HANDOFF.md` with every merged SHA, the artifact
SHA-256s, the parked items and why, and send the tower one batched SHA.

## Needs the owner (not worked tonight)

- Deep Scan through the admin prompt on the real host, `ujust snitchwatch-deny`
  status and the on/confirm/off trial as root, and closing tower #35.
- Releasing the history run after PR 2; any S2 fallback.
- The tower's decisions: hash-pinning the scanner files, the worktree cleanup list,
  #133 and #134, and adding chkrootkit to the image (currently left off, Bazzite
  being immutable).
- Replies on upstream OpenSnitch #1699, #1700 and #1701.
