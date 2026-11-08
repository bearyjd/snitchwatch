# Overnight run 4 (2026-11-03)

Autonomous, unattended. Orchestrator = this session. Read `.omc/STATE.md` first (<=60 lines); never read `.omc/HANDOFF.md` whole.

## Model routing (owner, 2026-11-03)
Claude subscription for everything (OMC agents: sonnet for explore/executor/reviewers/test-engineer, haiku for lookups, opus only for architecture/security judgement). **No** `ask-grunt`, `ask-grunt-think`, `grunt-write`. **Codex is used only as the adversarial review** (`.omc/tools/gate.sh`). Always set agent model explicitly; agent cap 5; ~150 turns and ~1.5k-token reports per agent.

## Tasks, in order
1. **#316 text-trace**: let the running gate finish (result `/tmp/claude-1000/gate316c.result`). If it says NOT merged: read the codex findings (`grep -n -E '^(CRITICAL|HIGH|MEDIUM|LOW)' /tmp/claude-1000/gate316c-codex.out`), fix real ones in `../snitchwatch-texttrace` (tests first, mutant-check), push the PR branch, re-gate. Max 3 rounds, then stop and record. After merge: `git worktree remove --force ../snitchwatch-texttrace`, tell the tower the new main SHA (SendMessage to bazzite-tower-61, FYI only).
2. **#259 flake** (`silent_answer_tests`): one bounded reproduction attempt (leads: `spawn_sweep` in `silent_mode.rs`, the recorder writer thread). Loop the test under `timeout` (e.g. 200 runs, every run wrapped). If reproduced and the cause is proven: fix with a test, PR, gate. If not: write findings to `docs/superpowers/research/2026-11-03-flake-259.md`, PR it docs-only, gate. Do not guess a fix.
3. **Handoff**: refresh `.omc/STATE.md` (<=60 lines), update memory notes, save a learning (`trail note`) for anything non-obvious.

## Merge rule (unchanged)
codex read-only review clean on the exact head, 6/6 CI all COMPLETED, mergeStateStatus CLEAN, `gh pr merge N --merge --match-head-commit <sha>`, never force-push. PR bodies via `--body-file`. No long polling loops; background jobs notify.

## Hard stops (record in STATE.md and stop; do NOT proceed)
Release actions (r16 promote, tags, `r*-candidate`), #252, the stale `feat/q01-rules-keyboard` worktree, click-ology cleanup (owner has not given details), anything touching `vendor/`, any live daemon/sudo, `rm -rf`. Flatpak install needs the owner's `!` (sandbox cannot).

## Owner-only on waking
Install a build with #316 and capture per `docs/superpowers/research/2026-11-01-title-flicker-3.md`; install + check r16 candidate on Plasma then give the tower its separate go; #252 and q01-s3 decisions; click-ology details.
