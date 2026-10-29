# Overnight autonomous run 3 (2026-10-29)

Status: **Drafted for the owner, 2026-10-29.** The run starts when the owner says
"go" (or "start the overnight run"). It records the order and the stop rules; the
owner can edit it first. Main at drafting time: `3cdbbfa` (history PR 2, #262,
merged by the owner's release).

## Standing workflow, for every item

1. A failing test first, shown red; every new guard gets a mutant check.
2. Commit, push, open a PR (never a force-push; a lease-guarded rebase only).
3. `/codex` adversarial review (always `</dev/null`, filesystem-boundary preamble;
   security-touching changes also get `security-reviewer`). Fix every CRITICAL,
   HIGH and MEDIUM, then review the delta again; LOW when cheap.
4. Merge when CI is green and the last review is clean. A merge commit, branch kept.
5. At most 3 agents at once, sonnet, explicit turn and output caps. Fresh agents get
   a full brief. `/tmp` is tmpfs: a reboot wipes the scratch reviews.
6. After a merge batch, write the SHA list for the tower (see Hand-offs).

## Lessons from run 2 (build them in)

- **Reviews found real bugs on almost every PR.** Budget 3 to 5 codex rounds per PR
  and plan the night around review latency, not coding.
- **A PR with a stale `mergeable` flag runs no CI.** If `gh pr checks` says "no
  checks", merge `origin/main` into the branch and push (a plain push).
- **Stacked PRs:** merge the base with a merge commit, then `gh pr edit --base main`
  and wait for fresh CI before merging the next.
- **Flakes seen:** `inspector_panel_narrow_qml`, and the learned/silent-answer tests
  (#259). Rerun the failed job once; a second failure on the same test is a real
  failure.
- **Race class:** two feeds that update independently need a level-gated display,
  not an edge-triggered reset (#265 took four rounds).
- **The model can be at capacity** (codex returned "Selected model is at capacity").
  Retry once, then continue with other work and come back.

## Stop rules

- Park as a draft after 4 `/codex` rounds that still have a HIGH or CRITICAL.
  A repeating MEDIUM after round 4 is brought to the owner with the choice: merge
  with a documented limit, or keep going.
- Park anything that needs an unplanned bridge, wire, daemon, image or packaging
  change. The bridge gets notice to the tower before any merge that touches
  `crates/snitchwatch-bridge`, `crates/scanner-*` or `packaging/`.
- An unrelated CI failure twice is recorded as a flake and not chased tonight.
- Never without the owner: staging or rebooting the host, host files, tower refs,
  upstream posts, installing the Flatpak, deleting worktrees or `dist`, running
  anything that needs the owner's admin prompt, turning history on, or releasing
  anything beyond what is listed under "Owner decisions in force".
- Check `/tmp` and the disk before every build. At 80% used, pause new builds, say
  so in the report, and delete nothing. (Note: a `cargo clean -p` removed 47 GiB
  from one worktree last run; per-worktree `target/` dirs are large.)
- The owner's API key is **not needed** for this plan and is not used. Subagents run
  on the subscription; grunt tools (`grunt-write`, `ask-grunt`) use their own
  pinned keys. If a step ever wants the key, stop and ask.

## Owner decisions in force

D1: in the prompt Return only SELECTS a focused scope/duration choice, Escape is
always Deny. D2: Tab works in the prompt. D3: drawer only, no page shortcuts.
D4/D5: defaults (no value-only names; high contrast is Q02). S2: the full-journal
fallback, merged. History stays **off**; PR 2 merged dormant. chkrootkit stays OFF.

## Batch A: close what is open (first, in this order)

| Item | State at drafting | Next |
|---|---|---|
| #263 click-path test | codex: MEDIUM, test seams (`force_transport_for_tests`, `run_deep_scan_via`) are ungated | gate behind an off-by-default feature, RAII cleanup for the private bus, re-review, merge |
| #264 sticky scan failures | codex: MEDIUM (breaks the merged #260 a11y assertion), 2 LOW | fix after #263, re-review, merge |
| #265 About page | round 5: a MEDIUM (counters dropped while down) being fixed | final review, CI, merge |
| #252 scanner watchdog | **parked draft (owner)** | stays a draft |
| Follow-up: same rename race in `seen_programs.rs:102` and `logging.rs:221` | found by codex in #262 | one small PR: retry on `nlink == 0`, with the deterministic replace-hook tests used in #262 |

## Batch B: Q01 keyboard and screen-reader slices (3 agents' worth, one at a time)

Per `2026-10-28-q01-next-slices.md`, one small PR each, review focus as stated there:

1. **Slice 3, Rules page and sheets.** Rebase `origin/feat/q01-rules-keyboard`
   (WIP `dd065b0`, unreviewed) onto main rather than restarting.
2. **Slice 4, learning review.** Apply stays reachable only by its own button.
3. **Slice 5, list pages** (`RecommendedRulesPage`, `ProfilesPage`, `BlocklistsPage`).
4. **Slice 6, status and settings pages.**
5. **Slice 2b, the map on the Connections page.**
6. **Slice 7, drawer only** (D3). Keys open the drawer and switch pages, focus lands
   on the new page's first control. Runs last.

Each PR names its desktop-only checks (Orca, focus rings) in the description; the
tests cannot prove real key or screen-reader delivery.

## Batch C: connection history, after PR 2 (database-free halves only)

History is dormant on main. Per `2026-10-28-history-run-1.md`, the halves that do
not need the owner's further release:

1. **PR 3a:** in-memory primitives (generation counter, acceptance-sequence counter,
   pin registry, reader-gate skeleton). No database.
2. **PR 4a:** pure rule identity (recursive allowlist, canonical encoding, hash
   including `Operator.sensitive`, `rule_class`, qualifier flags, redaction tests).

Every PR keeps the test that a bridge started without history enabled creates no
`history/` directory. These touch `crates/snitchwatch-bridge`: the tower gets notice
before the first merge. **PR 3b / 4b (durable parts) are not started** until the
owner says history may go beyond dormant.

## Batch D: eBPF track (Q04 to Q06), preparation only

No host changes, no root. Agents write documents and scripts the owner runs:

1. A plan doc `docs/superpowers/plans/2026-10-29-ebpf-q05-q06.md`: the three-module
   and kernel checks (roadmap "Dual-mode GUI requirement"), the fallback test, the
   unknown-process-rate report, and the controlled workload for comparable
   full-day CPU/RSS of `/proc` vs eBPF.
2. A measurement script (read-only: samples CPU/RSS, counts unknown-process rows)
   for the owner to run on the host, with a test against synthetic input.
3. Q04's remaining GUI work only if the plan shows a gap (the tray already shows
   actual disconnected, paused and pending state; the monitor mode stays "not
   reported by opensnitchd").

## Batch E: Deep Scan flicker (analysis only)

Open: text flickers when the mouse moves over the Security Scan page before a scan
(frames 5.0 to 5.3 s of the owner's recording). Using frames near 4.5 to 5.3 s and
14 to 21 s via `ffmpeg`, look for hover effects (Button hover on a disabled state,
`BusyIndicator`, tooltip). If the cause is found, a TDD fix in one PR; if not, a
short note for the owner asking which text flickers. Do not guess-fix.

## Batch F: tidy and hand-off (last)

1. **GUI rebuild** (build only, never install): copy
   `dist/keyboard-deny-build/rebuild-local.py` to a NEW `dist/<name>-build`, change
   only the bundle name; needs `origin/main == SHA` and main CI green; use
   `--extra-sources` for the DB-IP cache. Lists #251 to #265 and the About page.
2. Update `.omc/STATE.md` (35 lines or fewer) and the top of `HANDOFF.md` (its
   current section is still dated 2026-10-26) and save a trail learning.
3. Record flakes: file the `inspector_panel_narrow_qml` flake next to #259.

## Hand-offs for the tower

After the merges: the new main SHA (batched), the list of bridge-touching PRs
(#261 storage, #262 writer, both dormant; any Batch C PRs), and a reminder that the
image repin for history/polkit is the owner's decision.

## Needs the owner (not blocking the run)

1. A "go" to start; any edits to this plan.
2. Desktop checks: Orca on the scan outcome announcement, focus rings, tray by
   keyboard, prompt focus.
3. Tower session: hash-pin confirmation, #133/#134 review, repin decision.
4. Which text flickers on the Security Scan page.
5. Whether history may go past dormant (PR 3b/4b), and the host steps for the eBPF
   measurements.

## Context and cost budget

One task per agent session; at about 250k context or 300 turns an agent writes its
state and stops. The orchestrator keeps `.omc/STATE.md` current after every merge
batch so a clear or reboot resumes cleanly. Expected load: 6 to 10 PRs, 3 to 5
review rounds each, mostly sonnet; opus only for security re-reviews.
