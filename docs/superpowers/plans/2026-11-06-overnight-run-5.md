# Overnight run 5 (2026-11-06 night)

Autonomous, unattended. Orchestrator = the session that starts this run. Read `.omc/STATE.md` first (<=60 lines); never read `.omc/HANDOFF.md` whole. One task per sub-agent; the orchestrator stops at ~250k context or ~300 turns after writing STATE.md.

## Model routing (owner)
Claude subscription for everything: OMC agents with the model set explicitly (sonnet for explore/executor/test-engineer, haiku for lookups, opus for architecture/security judgement). **No** `ask-grunt`, `ask-grunt-think`, `grunt-write`. Adversarial review = codex via `.omc/tools/gate.sh`, and for security-sensitive or repeatedly-failing PRs ALSO an independent read-only reviewer on a different model (opus at high effort, or fable for privileged-tier code), briefed with the code and the claims, never with the orchestrator's conclusion. Agent cap 5; ~150 turns and ~1.5k-token reports per agent.

## Start (10 minutes, no changes)
1. `.omc/STATE.md`, `gh pr list`, `gh issue list`, `ListAgents`, `git worktree list`. Confirm `origin/main`; the primary checkout's local `main` was fast-forwarded on 2026-11-06 (pull with `--ff-only` only).
2. Check `bazzite-tower-94` (SendMessage, facts only) for the state of its PR #176 and whether it needs anything from snitchwatch. Never ask it to cut, promote or publish.

## Tasks, in order
1. **Land #321** (the #259 fix: `cfg!(test)` seam, `TEST_LOCK` in the analysis test, probe-harness home isolation). Merge only when codex AND the second-model review are clean on the exact head and CI is 6/6. Findings above LOW: fix with an executor (failing test first, shown failing at runtime), push, re-gate; max 3 rounds, then stop and record in STATE.md. `Closes #259`. Then `git worktree remove --force ../wt-flake259`.
2. **Codemaps PR** (`docs/codemaps` in `../wt-codemaps`, one local commit already): spot-check at least 15 `path:line` pointers against the code, fix wrong ones, push, PR with a body file, gate on CI plus one read-only accuracy review; docs-only. Merge on green. Remove the worktree.
3. **This plan**: merge `docs/overnight-run-5-plan` on green CI (docs only), remove `../wt-plan`.
4. **Issue #322** (watchdog test fixtures): write the plan doc first (`docs/superpowers/plans/2026-11-MM-fixture-hardening.md`), then executors, tests first: (a) a pidfd-verified kill helper (pidfd_open, verify identity, pidfd_send_signal) at every recorded-pid kill site in `command.rs`, `containment.rs`, and `main.rs:395`; (b) a `/proc`-independent cleanup fallback for `MarkedSleeps::drop`; (c) one `ScannerFixture` with a `Drop` that cleans up, replacing `Reaped`, `ForkedTree`, `MarkedSleeps` and the hand-rolled re-exec tests; (d) `PR_SET_PDEATHSIG` in `fork_tree` children. Production sweep code stays unchanged. Max 3 review rounds, then stop and record.
5. **Prep notes only for r16.1**: an explore agent writes `docs/superpowers/research/2026-11-MM-r16-1-prep-checklist.md` from the #308/#309 diffs, `packaging/release/`, the candidate guard, and the tower's repin steps: what a new candidate name needs, in order. No workflow edits, no tags, no variables, no releases.
6. **Handoff**: refresh `.omc/STATE.md` (<=60 lines), save a learning with `trail note` for anything non-obvious, commit learnings through a docs PR, and write the morning report (below).

## Merge rule (unchanged)
Codex read-only review clean on the exact head, second-model review clean where this plan asks for one, 6/6 CI all COMPLETED (a wait loop must require all six checks to EXIST and be complete; an empty check list right after a push is not "done"), `mergeStateStatus` CLEAN, `gh pr merge N --merge --match-head-commit <full sha>`, never force-push (merge main in), PR bodies via `--body-file`. A MEDIUM or worse never merges overnight; only the owner can accept an exception, and the owner is asleep: record it and stop.

## Safety rules (learned the hard way on 2026-11-05/06)
- **Never signal with a live placeholder.** A failing-first test must not use a value that can reach `kill(-1)`, `kill(0)`, `killpg`, or any real process. One executor really executed `kill(-1, SIGKILL)`. Fixtures signal only pids they created and only pids > 1; check every `fork()` result.
- **Tests run on a throwaway HOME**, never the real one: `H=$(mktemp -d); mkdir -p $H/.config $H/.state $H/.data $H/run; chmod 700 $H/run; CARGO_HOME=$REAL_HOME/.cargo RUSTUP_HOME=$REAL_HOME/.rustup HOME=$H XDG_CONFIG_HOME=$H/.config XDG_STATE_HOME=$H/.state XDG_DATA_HOME=$H/.data XDG_RUNTIME_DIR=$H/run ...`. Before #321 lands, the whole kirigami test binary rewrites the real `~/.config/snitchwatch/learned.json`. Compare its mtime before and after any run that could touch it (was `1794017684` on 2026-11-06).
- **Every test run is wrapped in `timeout`** (an old hang cost 11 h). Kill load processes by pid you created, never by pattern (`pkill -f` kills the shell that runs it). Check for stopped/zombie leftovers: `ps -eo stat,comm | awk '$2=="sleep" && ($1 ~ /^[TZ]/)' | wc -l` must be 0.
- **The word `lifecycle`** must not appear in any workspace source: `snitchwatch-bridge` `history::dormant_tests` matches it across crates and fails only in the CI `test` job.
- **Worktrees**: the release container build cannot run from a worktree (its `.git` is a gitfile); use a real clone with a self-contained submodule. `git worktree remove` needs `--force` when a submodule is initialised. `git -c protocol.file.allow=always submodule update --init` in a new worktree.
- **Interim agent notifications are not results.** "Waiting on background work" means the agent has produced nothing yet; read the final report. Verify every executor claim yourself (run the tests, read the diff) before pushing; shell filters like `head -3` have hidden real results.
- Executors do not follow test-first unless told to show the failing run; say so in every brief.

## Hard stops (record in STATE.md and stop; do NOT proceed)
Release actions (r16 or r16.1 cut or promote, tags, `r*-candidate`, `R*_CANDIDATE_REVIEWED_SHA`, publishing anything); repairing or deleting the owner's `~/.config/snitchwatch/learned.json` or its stray `.learned.json.tmp-*` files; deleting remote branches; the stale `feat/q01-rules-keyboard` branch; click-ology cleanup (owner has not given details); anything under `vendor/`; any live daemon, `sudo`, or `rm -rf`; pushing to `main` directly.

## Owner-only on waking
- Install a build with #315, #316, #319 and capture the title flicker per `docs/superpowers/research/2026-11-01-title-flicker-3.md` (`SNITCHWATCH_TEXT_TRACE=1`; the trace goes to `console.warn`). `flatpak install` needs the owner's `!`.
- r16: the published `r16-candidate` (a75214b3) is stale against main (Fedora 45 builder pins change the reproducible hash): decide `r16.1`, then give the promote go in the tower session.
- The real `learned.json` was inflated by earlier test runs (about +80 on the `/usr/bin/curl` entry, no backup): decide whether to inspect or clean it.
- Review the accepted exceptions: #316 (closed by #319), #318 (codex/Fable MEDIUMs, tracked in #322).
- Decide on `feat/q01-rules-keyboard` and the merged remote branches (`chore/pins-env-fedora45`, `fix/scanner-watchdog-spawn-stall`, `feat/text-trace`, `fix/text-trace-title-identity`, `fix/watchdog-sweep-sigstop`).

## Morning report (numbered, short)
1. Merged (PR, SHA). 2. In flight (PR, state, what is blocking). 3. Blocked or stopped (why). 4. Decisions needed from the owner (numbered). 5. Anything unexpected (leaked processes, touched real config, executor incidents).
