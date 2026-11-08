# Overnight run 6 (2026-11-08 night)

Autonomous, unattended. Orchestrator = a FRESH session (the 2026-11-08 session is far past the ~250k-token limit in CLAUDE.md).
Read `.omc/STATE.md` first (<=60 lines); never read `.omc/HANDOFF.md` whole. One task per sub-agent; briefs = goal, file paths, turn cap (~150),
output cap (~1.5k tokens). At ~250k context or ~300 turns: write a STATE checkpoint and stop. Revised 2026-11-08 after an independent critic
(verdict REVISE: 3 BLOCKERs, 9 MAJORs, all folded in below).

## Model routing and tools
Claude subscription for everything, model set explicitly: opus for privileged/security-relevant code and every review of it, sonnet for
explore/test-engineer/docs, haiku for lookups; no grunt tools. Executors CANNOT set HOME for cargo (sandbox): they edit only; the orchestrator
runs fmt/clippy/tests and commits. Test runner: copy the pattern of `/var/home/user/.cache/prA.sh` (a gated script writing a log whose last
line is `COMMITTED LOCALLY <sha>` or `NOT COMMITTED`); read only that line. Reviews: an independent Opus review (brief it with code and claims,
NEVER your conclusion) AND the codex gate `.omc/tools/gate.sh PR WORKTREE FULLSHA PROMPTFILE` as an extra reviewer (`codex exec` needs `</dev/null`).
**Merge tool:** `bash /var/home/user/.cache/overnight/merge_when_green.sh PR FULLSHA` (the 40-char SHA). `/tmp` is a tmpfs lost on reboot: first copy
that script to `.omc/tools/merge_when_green.sh` too. If neither exists, STOP: do not hand-roll `gh pr merge`.

## Start (15 minutes, no changes)
1. `.omc/STATE.md`, `gh pr list`, `gh issue list`, `git worktree list`, `df -h /tmp` (32G tmpfs; it filled on 2026-11-08), `git fetch`.
2. Untracked plan files in the primary checkout block `git merge --ff-only`. Move a file aside ONLY if `git show origin/main:FILE | cmp - FILE` says
   identical. `docs/superpowers/plans/2026-11-08-opensnitch-master-pin.md` is committed on the PR #340 branch (w4) but until #340 merges it is only
   untracked in the primary checkout: do not move it before then; after #340 merges its copy is cmp-identical and can be moved aside.
3. Message `bazzite-tower-94` only with facts (SHAs). Never ask it to cut, promote, publish or repin.

## Step 0: critique and checkpoint (about 25 minutes)
Run `oh-my-claudecode:critic` (opus, read-only) on this file ONLY if it was edited after 2026-11-08 night; otherwise skip (the 2026-11-08 critique is
applied). Then task 0.

## Tasks, in order (A before B and before the pin; cut order at the bottom)
0. **PR #340** (HANDOFF 2026-11-08 + this plan; docs branch worktree `/var/home/user/w4`, branch `docs/handoff-2026-11-08`). Independent Opus review of the
   HANDOFF facts against `git`/`gh` on the exact head (this session had to correct its own text twice), then merge on green. Do NOT open a second docs PR
   with this plan: task 6 appends the run's outcome to HANDOFF in a follow-up PR from a fresh branch off main.
1. **PR A: version-gated `source.network`** (`feat/source-network-gate`, worktree `/var/home/user/w3`, plan `2026-11-08-source-network-gate.md`, committed
   in the w3 commit). STATE AT HAND-OFF: written by an opus executor, verified (clippy -D warnings clean; proto/bridge/bridge-cli/mock suites pass),
   committed LOCALLY as `00094160`, NOT pushed, no PR, no review. Do, in order: (a) `git -C /var/home/user/w3 log -1` / `status`; (b) apply the default for the
   open design point: in `version_gate.rs` `refuses_conditions`, only the `validate_operator_in(op, context)` call becomes `rule.enabled && ...`; the
   missing-operator refusal stays for every rule; add TWO failing-first tests (turning OFF a `source.network` rule is accepted on a 1.9 daemon; turning
   it ON is refused with the exact message). Rationale (verified in a1353848): `loader.go:261-268,431` skips `Compile()` for disabled rules; the CLI already
   allows turning a rule off (`gates.rs:146-151`); (c) THEN mutation-check the gate (make `PolicyContext::for_daemon` never refuse: the unit tests must fail;
   the protocol test may still pass because the 1.9 mock refuses on its own) and require a clean `git diff` after reverting; (d) push, PR (body: plan
   Decisions + Risks, the default taken, trail-check explanation), Opus code review + Opus security review (bridge/daemon trust boundary) + codex gate; merge.
2. **PR B: GUI half** (branch `feat/source-network-gate-gui`, short worktree `/var/home/user/w1`: CREATE it with `git worktree add /var/home/user/w1`
   and `git -c protocol.file.allow=always submodule update --init`; the socket path must stay < 108 bytes). Open design choice, take the default and report it:
   a post-Subscribe `ServerMessage` over a dynamic `bridge_capabilities` hook (the `advertised()` list is static today). Editor hides the operand,
   simulator flags `network`+`source.network` ("daemon 1.9 refuses to load") and regexp/simple/range + network operands ("daemon panics"; `operator.rs:542`
   says "definitely doesn't match" today). The `kirigami` CI job takes ~20 min per push.
3. **Pin PR** (`chore/pin-opensnitch-master`, worktree `/var/home/user/w2`). Only after PR A has merged. ENDS AT "pushed, reviewed (Opus + codex),
   6/6 green, NOT MERGED": the pin plan still says `Merge only after the owner confirms`, it rewrites a CLAUDE.md settled decision (an owner file) and
   switches the submodule URL to a fork branch that is not protected. Steps: (i) commit the pin plan into w2 first; (ii) commit the local `.gitmodules` +
   gitlink change in w2, `git rebase origin/main`, `git submodule sync`; (iii) edits per the pin plan (`pins.env` `SW_OPENSNITCH_COMMIT`/`SW_OPENSNITCH_TAG=master-a1353848`,
   `docs/packaging/bridge-release-artifact.md:100,145`, `docs/ENV.md:95-96`, `CLAUDE.md:18` + settled decision text, `recipe.yml:48` comment, the changed-file
   `vendor/...:LINE` citations, a build-from-pin procedure in the eBPF runbook, the two daemon reviews' risks in the PR body); (iv) **rewrite the two vendor
   tests PR A added in `history/rule_identity_tests.rs`** (operator.go `Type()` set; version.go "1.8.0") against master's real `Compile`, or CI fails for
   certain; (v) verify with `just test-bridge`, `just package-check`, `cargo test -p snitchwatch-proto -p snitchwatch-bridge`, and the release build in a REAL
   CLONE (the recipes mount `$PWD:/src:ro`, and a worktree's `.git` is a gitfile pointing outside the mount, so `check_submodule` dies in a worktree):
   `git clone --recurse-submodules -b chore/pin-opensnitch-master <url> /var/home/user/.cache/pinclone`, run `just release-bridge-repro` there, then
   `just release-verify dist/<tarball> <SHA256 printed by the build>` (podman via host-spawn; time-box 60 min; if podman cannot run, say so in the PR and
   continue without it). Expect a NEW tarball hash.
4. **Cleanup PR** (`chore/review-lows`; only if time remains): `// SAFETY:` comments on the unsafe blocks in `containment.rs`; the hostile-comm start-time
   test (`x) 1 2 (y` via `prctl(PR_SET_NAME)`); the `WATCHDOG_CLEANUP_WAIT` doc fix (exit 3); the four #322 LOW notes; one shared pid-file reader; the six
   eBPF-runbook LOWs. **NOT in this run:** arming the exit alarm on the timed-out `Expiring` wait (privileged watchdog behaviour next to the unapproved exit-3 change).
   Security review (privileged code) + merge on green.
5. **Upstream findings note (local file only)**: `docs/superpowers/research/2026-11-MM-opensnitch-upstream-findings.md` (scheduler nil-deref on a bad `"time"` entry, `Stop()` map race, stray `println` at
   rfc3164.go:50, Makefile `KERNEL_VER` text sort and the `>= 6.19` string compare). Write it during CI waits. Do NOT file anything upstream.
6. **Handoff**: refresh `.omc/STATE.md` (<=60 lines; also fix its stale lines), `trail note`, a follow-up docs PR appending the run's outcome to HANDOFF.md, the morning report. Reserve 60 minutes.

## Time budget and cut order (estimates: A 2-3 h, B 3-5 h, pin 3-4 h incl. the 60-minute release build, cleanup ~3 h; total 11-15 h vs ~8 available)
Cut order if time runs out: cleanup (4) first, then B (2), then the pin's release build (v). Always keep 0, 1, 5 and 6. Do independent work during A's CI waits
(the Task 5 note; preparing the pin clone). Checkpoint `.omc/STATE.md` after each merge.

## Loop bounds
- A review finding above LOW: fix with a failing-first test, re-gate, at most 3 rounds (as in run 5), then STOP that PR and record it. A MEDIUM+ also needs the
  owner's exception to merge. LOWs are fixed or filed as follow-ups in the PR body.
- CI: one `gh run rerun --failed` for the known timing flakes; a second failure of the same job stops that PR.
- A bad merge is undone with a revert PR through the same gate, never a force-push.

## Merge rule
Independent Opus review (code; plus security for kill/watchdog/bridge-trust/daemon-contract changes) AND the codex gate, clean on the EXACT head; 6/6 CI all COMPLETED;
state CLEAN; the merge tool above with the 40-char SHA. `ci.yml` has 6 jobs and no path filters, so 6/6 applies to docs PRs too.

## Safety rules (learned 2026-11-05..08)
- Tests only via cargo on a throwaway HOME **under `/var/home/user/.cache`** (not `/tmp`), `CARGO_TARGET_DIR` = the shared `target/`; long runs in the background with `timeout`.
- Never run binaries from `target/debug/deps` directly. Never touch the real D-Bus session bus (tests use `PrivateBus` + an explicit address).
- New worktrees: `git -c protocol.file.allow=always submodule update --init`. `git worktree remove` can refuse because `/home/user` is a bind of `/var/home/user`:
  compare inodes, then remove only the exact directory.
- Banned tokens in non-history `.rs` (bridge `history::dormant_tests` strips comments and strings): references to the `lifecycle`/`pins` modules, `identify(`, `rule_identity`, `RuleIdentity`, `IdentityError`, `Qualifiers`, `RuleClass::of`.
- A failing-first test must not use a live signal placeholder. Interim "waiting on background work" agent notices are not results.
- trail check conflicts are keyword matches: read them, then say why in the PR body.

## Hard stops (record in STATE.md and stop)
Release actions (r16.2 or any cut/promote/tag, `r*-candidate`, `R*_CANDIDATE_REVIEWED_SHA`, publishing); `gh workflow run`, `gh variable`, `gh secret`, `gh pr merge --admin`,
and any edit under `.github/workflows`; anything on the tower (repin, image changes, enabling ebpf, the eBPF load test, which touches the firewall); `sudo` and any live
daemon; pushing directly to `main`; force-push to ANY remote; any remote-branch deletion; branch protection or any change to the fork `bearyjd/opensnitch`; filing upstream
issues or PRs; editing `vendor/opensnitch` content (the ONE exception: the gitlink bump to `a1353848` in the pin PR); the bluebuild RPM line; the CI speed-up and the broker
exit-3 change (not approved); the owner's real `~/.config/snitchwatch` (incl. `learned.json`). `rm -rf` only on a path NAMED IN THIS PLAN (`/var/home/user/.cache/pinclone`); otherwise
only `git worktree remove`. Any edit to `CLAUDE.md` must be reviewed, never just merged.

## Owner-only on waking
1. Merge the pin PR (or not). 2. Broker on exit 3: show the report with a containment warning instead of discarding it? 3. Ask the tower to confirm the root-only socket on
r16.x (#35, #178). 4. CI speed-up yes/no. 5. Branch protection + README note on the fork `snitchwatch/stable`, before the pin's URL switch matters. 6. Tower manual check of
the single-instance GUI. 7. The eBPF load test in the tower VM (you present). 8. Whether r16.2 carries the master daemon (only after the load test). 9. The 127 remote branches.
10. A primary with no window still answers `Activate`: design. 11. The exit-alarm-on-timed-out-wait change (cut from this run).

## Morning report (numbered, short)
1. Merged (PR, SHA). 2. In flight (PR, state, what blocks). 3. Blocked or stopped (why). 4. **Decided for the owner by default** (enabled-only gate; the gate plan's Decisions 1-3, which
are still DRAFT; the `master-a1353848` tag label; the fork URL switch; the 3 patch branches left out of `snitchwatch/stable`; the dynamic-capability choice for PR B). 5. Decisions needed
(numbered). 6. Anything unexpected (leaks, disk, flakes). 7. Context used by the orchestrator.
