# r16.1 GUI candidate: prep checklist (notes only, no release steps)

Date: 2026-11-07. Base: `origin/main` 606083d2. Nothing here changes a workflow,
variable, tag or release. Every claim not read from a file or `gh ... view` output is marked
**UNVERIFIED**. Citations are `path:line` at 606083d2, or PR numbers.

## 0. Why a new name

| Fact | Source |
|---|---|
| `r16-candidate` is a published pre-release (not draft) at a75214b3 (merge of #309), candidate run 41315786846 | `gh release view r16-candidate`; HANDOFF.md:12 |
| A published candidate is never replaced under the same name; the workflow refuses an existing release or a tag at another commit | candidate-gui.yml:194-196; docs/packaging/r13-candidate-gui-ci.md:4-5 |
| main has moved 62 commits past a75214b3; only workflow-adjacent change is the Fedora 45 pin bump (#317) | `git log a75214b3..HEAD -- .github/workflows packaging/flatpak docs/packaging` |
| Precedent for a point-release name: r15.1 (prep commit 5c969a2e, 6 files) | `git show --stat 5c969a2e` |

## 1. What a new name needs in the repo (list only; do NOT apply here)

#308 renamed every live r15.1 reference to r16 in 6 files (+/- lines 26/26, 1/1, 11/11, 1/1, 6/6, 29/26).
The r16.1 prep PR would touch the same 6 files. Current hard-codes:

| File | Lines (at 606083d2) | Token(s) to change |
|---|---|---|
| `.github/workflows/candidate-gui.yml` | 1 | workflow name `r16 System GUI Candidate` |
| | 10 | push `branches: [r16-candidate]` |
| | 16 | concurrency group `r16-gui-candidate-publication` |
| | 21, 28, 36, 65, 75 | variable `R16_CANDIDATE_REVIEWED_SHA`; 28 and 65 also `refs/heads/r16-candidate` |
| | 59, 81 | artifact `r16-system-gui-candidate` |
| | 100, 102, 133-134 | workflow ref / ctx ref / packet `target` + `ref` `r16-candidate` |
| | 107, 208 | bundle `snitchwatch-system-r16.flatpak` (size cap + path) |
| | 177, 194, 198, 234, 238, 242, 247, 256 | branch/tag/release lookups, `createRef`, `tag_name` `r16-candidate` |
| | 221 | ref check `refs/heads/r16-candidate` |
| | 248 | release title `r16 system GUI candidate` |
| `.github/workflows/ci.yml` | 5 | push `branches: [main, r16-candidate]` (the candidate branch must stay listed; a test asserts it) |
| `packaging/flatpak/build-candidate.py` | 47 | `GITHUB_REF` check |
| | 88, 119, 142 | `--default-branch=`, bundle export branch, `app/{APP}/x86_64/<branch>` |
| | 117 | bundle file name `snitchwatch-system-r16.flatpak` |
| | 155 | packet `target` |
| `packaging/flatpak/test-candidate-publication.py` | 15, 32, 95, 113-114, 123, 183, 194, 200, 220, 256, 271, 277, 283, 316-334 | fixtures for ref, bundle, packet, tag |
| | 195-210 | refusal lists: add `r16-candidate` to the never-replaced list (line 208); line 196/198 decide the old-bundle refusals |
| | 370-384, 406 | trigger-branch, artifact, ostree-branch and variable assertions |
| `docs/packaging/r13-candidate-gui-ci.md` | 1, 3-4, 6, 8-9, 20-21, 25, 73 | title, destinations, never-replaced list (add `r16-candidate`), bundle, variable, tag |
| `packaging/flatpak/README.md` | 124 | link text `r16 candidate GUI CI` |

Not hard-coded by name (checked by grep `r16|r15.1` over `.github/workflows packaging docs/packaging`):
`release.yml`, `packaging/release/*`, `docs/packaging/bridge-release-artifact.md`. Other r16 hits are in
HANDOFF.md, `.trail/learnings/`, and plan/spec docs (history, not live config).

Decisions for the prep PR author (surface, do not pick silently):

| Question | Evidence |
|---|---|
| Bundle name: keep `snitchwatch-system-r16.flatpak` (r15.1 kept the r15 name, "follows the line's major number") or change it | #308 body; test lines 196-200 refuse the previous line's name, so the test must be reconsidered either way |
| Name shape `r16.1-candidate`: dots in git tag/branch and Flatpak branch | `r15.1-candidate` was used before (test:208, HANDOFF.md:40); whether Flatpak accepts the new string is UNVERIFIED beyond that precedent |
| Doc filename `r13-candidate-gui-ci.md` is stale-named but linked from README:124 and referenced by #308/#309; renaming it is scope creep | docs/packaging/ listing |
| Variable name `R16_1_CANDIDATE_REVIEWED_SHA` vs `R16P1_...`: GitHub variable names allow `[A-Za-z0-9_]` only, so no dot | UNVERIFIED (GitHub rule from memory; not in repo) |

Verification the prep PR must carry (as #308/#309 did): `python3 packaging/flatpak/test-candidate-publication.py`
(22 tests at #309), `just package-check`, YAML parse; actionlint was not installed in earlier runs.

## 2. Who does what

| Step | Owner | Source |
|---|---|---|
| Prep PR (rename in section 1), review (codex read-only on exact head), 6/6 CI, merge with `--match-head-commit` | snitchwatch session | learnings 2026-11-01:12; #308 |
| Send the tower the final merged main SHA, CI green | snitchwatch session, then idle | learnings 2026-10-22-who-sets:12 |
| Owner's explicit go, typed in the TOWER session (a peer relay is not a go) | owner | learnings 2026-11-01:13 |
| Set `R<N>_CANDIDATE_REVIEWED_SHA` (before the push) to the exact reviewed SHA | tower | learnings 2026-10-22-who-sets:11,16; candidate-gui.yml:21 |
| Push the reviewed SHA to the candidate branch | tower | same; HANDOFF.md:113-115 |
| Install and verify on Plasma | owner | HANDOFF.md:18-20 |
| Promotion to the final release | tower, on a separate owner go | HANDOFF.md:12 |

We set nothing and push nothing for candidates (HANDOFF.md:115). The 2026-10-18 learning saying "owner sets the variable"
is superseded (learnings 2026-10-22-who-sets:14).

## 3. Before asking for the go

| Precondition | Status / source |
|---|---|
| main at a known full SHA, prep PR merged, CI 6/6 on that exact SHA (check, test, kirigami, audit, candidate-contract, package-check) | learnings 2026-10-18-r15-r16:21; the publisher itself requires `check, test, package-check, kirigami, candidate-contract` (candidate-gui.yml:189) |
| Fedora 45 builder pins landed | Yes: #317 merged as 32eca9a7; `packaging/release/pins.env` has fedora:45@sha256:ea0998f4..., rust 1.99.1, protoc 33.5 |
| Candidate branch pushed only after main or PR CI on that SHA is green | docs/packaging/r13-candidate-gui-ci.md (end of the CI paragraph); #309 |
| Owner checked the GUI on Plasma before the cut | learnings 2026-11-01:13 |
| Reviewed-SHA variable equals the pushed SHA, else build and publish jobs stay skipped | candidate-gui.yml:21,28,65 |
| Branch not moved after review; a changed head needs fresh review and CI | r13-candidate-gui-ci.md:22-23 |
| Bridge/image inputs: the builder change alters the reproducible bridge binary hash, so any earlier r16 bridge tarball hash/re-pin is stale; the tower re-pins with `ci/repin-snitchwatch.py` from the merged SHA | learnings 2026-11-06-fedora-45:20-21 (tool name taken from the learning, not run) |
| No outcome recorded yet for `just release-bridge-repro` under Fedora 45; `fc44` NEVRA fixtures in `release_artifact/main.rs` not checked | learnings 2026-11-06-fedora-45:16. Whether this was resolved after the learning: UNVERIFIED |
| Does the GUI Flatpak candidate depend on `pins.env`? No reference in candidate-gui.yml or build-candidate.py (grep), and the doc says the candidate produces no bridge tarball (r13-candidate-gui-ci.md:14-16). The learning's "r16 candidate needs a rebuild" is about the bridge/image side. A new GUI candidate is a fresh build from the new SHA anyway | grep; docs. Impact on the image is UNVERIFIED |

### Candidate guard behaviour now (differs from #309's PR text)

| Aspect | Current code |
|---|---|
| Run judged | newest-first `ci.yml` run for the SHA, event push or pull_request, that is completed OR has `run_attempt !== 1`; only an unfinished FIRST attempt is skipped (candidate-gui.yml:180-185) |
| Origin | #309 skipped any in-flight run (`status === 'completed'`); later commit 959f2f20 tightened it so an unfinished rerun holds publication. (#309 body does not mention this; PR number of 959f2f20 UNVERIFIED) |
| Must hold | judged run completed + success, repo `bearyjd/snitchwatch`, 5 named jobs each exactly once, `success`, same `run_attempt` (candidate-gui.yml:186-193) |
| Why it exists | the candidate push starts its own `ci.yml` run (~19 min kirigami) vs the ~10 min build, so the publisher looked while it was in flight (#309; learnings 2026-11-01:20) |

## 4. Failure modes on record

| Failure | Rule | Source |
|---|---|---|
| Publish failed "exact-head native CI missing or failed" in the first r16 cut; no release was created | structural race, fixed by #309; push the branch only after CI on the SHA is green | #309 body |
| `gh run rerun --failed` on a failed publish | can never pass: packet/provenance pins `GITHUB_RUN_ATTEMPT` ("run_attempt mismatch"). Use a FULL rerun, after the branch CI run has completed; it builds a new candidate needing fresh acceptance | learnings 2026-11-01:16; HANDOFF.md:12; r13-candidate-gui-ci.md (publisher-rerun sentence) |
| Guard sees a newer in-flight or failed run | judges the newest COMPLETED run (first-attempt in-flight skipped); a newer completed failure or cancel still holds | #309; candidate-gui.yml:180-185 |
| Replace a published candidate | not possible by design; a fix needs a new name (this document's reason) | candidate-gui.yml:194-196 |
| Failure after the draft is created | leaves a draft and HOLD, never overwrites or deletes | r13-candidate-gui-ci.md (end of publisher paragraph) |
| Variable absent or different | build and publish both skipped, silently from the owner's view | candidate-gui.yml:21 |
| `gh pr checks --watch` as a CI wait | returns at once on QUEUED; poll until COMPLETED | learnings 2026-11-01:15 |
| Open-ended polling loops matching command lines | self-match, run for hours | learnings 2026-11-01:17 |
| Clean text merge can still break QML (two sides each added a `contentItem`) | rerun targeted tests after such merges | learnings 2026-11-01:12 |
| Blank labels under KDE native style invisible to Basic-style headless tests | source guard exists; owner must look on Plasma | learnings 2026-11-01:20 area; HANDOFF.md:15 |

## 5. Owner verification after install

The install and checks need the owner's own shell outside the agent sandbox (HANDOFF.md:13 "owner runs it outside the
agent sandbox"). Agents must not install on the host, publish, or touch candidate refs (HANDOFF.md:74).

| Step | Command / what to check | Source |
|---|---|---|
| Get the bundle | download the pre-release asset from the new release page (asset name per the bundle decision in section 1); verifying CI provenance and comparing published bytes against the packet comes first | r13-candidate-gui-ci.md (provenance paragraph) |
| Install | `flatpak install --user --reinstall <bundle>` | HANDOFF.md:18 |
| Run the candidate branch | `flatpak run --user --branch=<candidate>-candidate org.snitchwatch.Snitchwatch` (HANDOFF.md:67 shows the r15.1 form; the new branch name is UNVERIFIED until the prep PR lands) | HANDOFF.md:67 |
| Plasma look | labels on every restyled button, connections list highlight, focus rings, Orca, drag-select | HANDOFF.md:15 |
| Tray and prompt behaviour (#315, merged as 233f54e7: idle timers stop while hidden/minimized, tray poll event-driven) | hide/minimize then restore: banners and bridge status refresh at once; tray state clears when the session goes stale; pending prompts still answer/expire/defer while hidden. #315's wakeup numbers are reasoned from source, NOT measured | `gh pr view 315` body |
| Flicker capture (Daemon Health / Security Scan title area) | `flatpak run --env=SNITCHWATCH_TEXT_TRACE=1 --env=SNITCHWATCH_LOG=snitchwatch_kirigami=debug org.snitchwatch.Snitchwatch 2>&1 \| tee trace.log`; wheel over the title for ~20 s on each page; send `trace.log`, grep `text-trace`. An empty trace is itself a result | docs/superpowers/research/2026-11-01-title-flicker-3.md:119-157; plan docs/superpowers/plans/2026-11-03-text-trace.md |
| Debug logging, private | README `systemd-run ... SNITCHWATCH_LOG=...`; treat the log as private | packaging/flatpak/README.md:100-110 |
| Bridge capability | a GUI reinstall alone does not supply new bridge capability; the matching SYSTEM overlay/image is separate | HANDOFF.md:63 |

## 6. Gaps and surprises

| Item | Note |
|---|---|
| Remote branch `origin/r16-candidate` still exists | seen in `git branch -a`; its tip not inspected |
| Whether the tower already has a plan or variable for r16.1 | UNVERIFIED; no repo file mentions r16.1 |
| Whether PR #315 and the flicker work are the only owner-visible changes since a75214b3 | UNVERIFIED; 62 commits not individually read |
| Final-release promotion mechanics | HANDOFF.md says tower, owner go; steps not documented in files read; UNVERIFIED |
