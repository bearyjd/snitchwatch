# Overnight run 6 (2026-11-08 night)

Autonomous, unattended. Orchestrator = a FRESH session started for this run (the 2026-11-08 session is far past
the ~250k-token limit in CLAUDE.md: it must write state and stop). Read `.omc/STATE.md` first (<=60 lines); never
read `.omc/HANDOFF.md` whole. One task per sub-agent; briefs = goal, file paths, turn cap (~150), output cap (~1.5k tokens).

## Model routing (owner)
Claude subscription for everything, model set explicitly: opus for privileged/security-relevant code and all reviews of it,
sonnet for explore/test-engineer/docs, haiku for lookups. No grunt tools. Executors CANNOT set HOME for cargo (sandbox):
they edit only; the orchestrator runs fmt/clippy/tests and commits.

## Start (10 minutes, no changes)
1. `.omc/STATE.md`, `gh pr list`, `gh issue list`, `git worktree list`, `df -h /tmp` (a 32G tmpfs: it filled on 2026-11-08).
2. `git fetch`; the primary checkout's local `main` may be behind and an untracked plan file can block `git merge --ff-only`
   (compare with `git show origin/main:FILE | cmp`, then move the untracked copy to the scratchpad).
3. Message `bazzite-tower-94` only with facts (SHAs). Never ask it to cut, promote, publish or repin.

## Tasks, in order (constraint: A before B and before the pin)
1. **PR A: version-gated `source.network`** (`feat/source-network-gate`, worktree `/var/home/user/w3`, plan
   `docs/superpowers/plans/2026-11-08-source-network-gate.md`). STATE AT HAND-OFF: written by an opus executor, verified by the
   orchestrator (clippy -D warnings clean; proto/bridge/bridge-cli/mock suites pass, bridge lib 1652), committed LOCALLY as `00094160`,
   NOT pushed, no PR, no review, gate not yet mutation-checked. Do, in order: (a) re-check `git -C /var/home/user/w3 log -1` and `status`;
   (b) mutation-check the gate (make `PolicyContext::for_daemon` never refuse: the 1.9 tests must fail); (c) apply the DEFAULT for the open
   design point below; (d) push, PR (body: the Decisions + Risks from the plan, trail-check explanation), code review + a security
   review (bridge/daemon trust boundary), merge on green. OPEN DESIGN POINT (owner has not decided; take this default and report it):
   turning OFF an existing `source.network` rule on a >=1.9 daemon is refused with the generic `RefusedOperator` because the gate ignores
   the enabled flag; the daemon does not compile disabled rules, so make `send()` apply the version check only to ENABLED rules.
2. **PR B: GUI half** (same plan): capability/message `daemonRefusesSourceNetwork`, Kirigami editor hides the operand,
   simulator flags `network`+`source.network` ("daemon 1.9 refuses to load") and regexp+network operands ("daemon panics"). Kirigami
   bus tests need a SHORT worktree path (socket path < 108 bytes): `/var/home/user/w1`; the `kirigami` CI job takes ~20 min.
3. **Pin PR** (`chore/pin-opensnitch-master`, worktree `/var/home/user/w2`, uncommitted, plan
   `docs/superpowers/plans/2026-11-08-opensnitch-master-pin.md`). Starts only after PR A merged (rebase w2 on main). Submodule url =
   `https://github.com/bearyjd/opensnitch.git`, commit `a1353848ba1b660320e90cefea782c3fba272c00` (branch `snitchwatch/stable` on the
   fork), `pins.env` `SW_OPENSNITCH_COMMIT` + `SW_OPENSNITCH_TAG=master-a1353848`, the v1.8.0 text in
   `docs/packaging/bridge-release-artifact.md:100,145` and `docs/ENV.md:95-96`, `CLAUDE.md:18` + settled decision text, the
   `recipe.yml:48` comment (the RPM stays v1.8.0), re-check the changed-file `vendor/...:LINE` citations (notifications.go x6,
   the eBPF runbook, plan `2026-10-29-ebpf-q05-q06.md`), add a build-from-pin procedure to the eBPF runbook, and put the two daemon reviews'
   risks in the PR body (daemon needs Go >= 1.25; build daemon + 3 .o as a matched pair with explicit KERNEL_VER/KERNEL_DIR; scheduler
   nil-deref on a bad `"time"` entry and `Stop()` map race, neither reachable with `tasks: []`; stray `println` at rfc3164.go:50).
   Verify: `just package-check`, `cargo test -p snitchwatch-proto -p snitchwatch-bridge`, `just release-bridge-repro` +
   `just release-verify` (podman, heavy; time-box 60 min; expect a NEW tarball hash, no code change). Merge on green.
4. **Cleanup PR** (one, `chore/review-lows`): `// SAFETY:` comments on the unsafe blocks in `containment.rs`; the hostile-comm
   start-time test (`x) 1 2 (y` via `prctl(PR_SET_NAME)`); arm the exit alarm on the timed-out `Expiring` wait and fix the
   `WATCHDOG_CLEANUP_WAIT` doc (exit 3); the four #322 LOW notes (see the #322 closing comment); share one pid-file reader; the six
   eBPF-runbook LOWs (`STATE.md`). Review + merge on green.
5. **Upstream findings note (local file only)**: `docs/superpowers/research/2026-11-MM-opensnitch-upstream-findings.md` with draft
   text for the issues found in a1353848 (scheduler nil-deref, `Stop()` race, `println`, Makefile `KERNEL_VER` text sort and the
   `>= 6.19` string compare). Do NOT file anything upstream.
6. **Handoff**: refresh `.omc/STATE.md` (<=60 lines), `trail note` for anything non-obvious, a docs PR for learnings + this plan
   (`docs/overnight-run-6-plan`), the morning report.

## Merge rule
Independent Opus review (code, plus security for kill/watchdog/bridge-trust changes) clean on the EXACT head, 6/6 CI all
COMPLETED, state CLEAN, `bash /tmp/claude-1000/merge_when_green.sh PR FULLSHA` (**the 40-char SHA**; a short one only ends in "NOT merged").
A MEDIUM or higher finding STOPS that PR (record it; the owner gives exceptions). Push only gated on a green test run.

## Safety rules (learned 2026-11-05..08)
- Tests only via cargo on a throwaway HOME **under `/var/home/user/.cache`**, `CARGO_TARGET_DIR` = the shared `target/`, never under
  `/tmp` (tmpfs, quota). Every long run in the background with `timeout`; poll nothing in a loop over a large context.
- Never run binaries from `target/debug/deps` directly. Never touch the real D-Bus session bus (tests use `PrivateBus` + an explicit address).
- `git worktree add` + `git -c protocol.file.allow=always submodule update --init`. `git worktree remove` can refuse because
  `/home/user` is a bind of `/var/home/user`: compare inodes, then remove the exact directory.
- Banned in non-history `.rs`: `lifecycle`, `identify(`, `rule_identity`, `RuleIdentity`, `IdentityError`, `Qualifiers`, `RuleClass::of`, the word `pins`.
- `rm -rf` only on an exact path the plan or the owner named; never glob. Never run `snitchwatch-bridge-cli` outside an isolated `XDG_RUNTIME_DIR`.
- A failing-first test must not use a live signal placeholder. Interim "waiting on background work" agent notices are not results.
- trail check conflicts are keyword matches: read them, then say why in the PR body.

## Hard stops (record in STATE.md and stop)
Release actions (r16.2 or any cut/promote/tag, `r*-candidate`, `R*_CANDIDATE_REVIEWED_SHA`, publishing); anything on the tower
(repin, image changes, enabling ebpf, running the eBPF load test, which touches the firewall); force-push, branch protection or deletion on
the fork `bearyjd/opensnitch` or any remote branch deletion; filing upstream issues or PRs; editing `vendor/opensnitch` content;
the bluebuild RPM line; the CI speed-up and the broker exit-3 change (owner has not approved); the owner's real `~/.config/snitchwatch`.

## Owner-only on waking
1. Broker on exit 3: show the report with a containment warning instead of discarding it? 2. Ask the tower to confirm the root-only
socket on r16.x (#35, #178). 3. CI speed-up (drop the redundant `cargo check`, faster linker, less debug info): yes/no.
4. Branch protection + README note on the fork `snitchwatch/stable`. 5. Tower manual check of the single-instance GUI (Flatpak owns
its name, second launch raises, `kill -9` relaunch). 6. The eBPF load test in the tower VM (you present). 7. r16.2 scope: does it carry the
master daemon (only if the load test passed). 8. The 127 remote branches (list in `.omc/backup-2026-11-06/`). 9. A primary whose
window never loads still answers `Activate`: decide the design.

## Morning report (numbered, short)
1. Merged (PR, SHA). 2. In flight (PR, state, what blocks). 3. Blocked or stopped (why). 4. Decisions needed from the owner
(numbered). 5. Anything unexpected (leaks, disk, flakes). 6. Context used by the orchestrator.
