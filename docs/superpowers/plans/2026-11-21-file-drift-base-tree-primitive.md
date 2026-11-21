# `file_drift` base-tree primitive (rule 2 via per-path OSTree object comparison)

**Date:** 2026-11-21
**Specs:** `docs/superpowers/specs/2026-07-04-scanner-baseline-design.md` (§1 rule 2: "OSTree
object lookup, not a hand-computed hash"; rules 3-5); `docs/superpowers/specs/2026-07-04-scanner-privileged-tier-design.md`
(§4 `detail` table)
**Prior work:** `docs/superpowers/plans/2026-11-19-privileged-file-drift.md` (PR 1 Design
`base_tree_drift`, interim resolution rule, Open Question 1, which this plan answers); PR #405
(classify-then-cap); PR #408 (`scripts/file-drift-evidence.sh`)
**Base:** `main` @ `1b1938fb` **plus PR #409** (`b18732a1`, "treat /var and /boot as mutable in
rpm drift checks"). #409 is a prerequisite: merge it first or rebase onto it. On `1b1938fb`
`provenance.rs:26` `MUTABLE_PREFIXES` still lacks `/var` and `/boot`, and `file_drift.rs` line
numbers below shift by about 23 lines once #409 lands. Re-check every cited line before editing.

## Why now

On the owner's stock Bazzite tower (2026-11-21), `file_drift` cannot produce a clean result.
`file_drift.rs:172` sets `base_tree_match = Some(false)` for every failing base-package file, so
every image-baked change (files that the image build changed after rpm installed them, which live
in the OSTree commit and are expected) is anomalous. After #409 about 500 candidates remain, which
hits `MAX_DRIFT_PATHS` and leaves the run truncated and incomplete on an untouched image. Nothing can
resolve, and real drift is buried. Rule 2 needs a live primitive.

## Measured facts this plan builds on (2026-11-21)

Host: owner's Bazzite tower, composefs read-only root, `ostree` 2026.4, rpm 6.1.0. Evidence from
`scripts/file-drift-evidence.sh` plus manual read-only commands. "Host-verified" means that
command was run there. Anything else is marked **to-verify** and is a Task 1 item.

1. **Stock-image `rpm -Va --nomtime --noscripts --nodeps`:**
   - output: 2,361 lines, 163 KB, exit 1, empty stderr, 39 s as root;
   - 1,749 lines under `/var/lib` and about 68 under `/boot` (removed by #409);
   - the about 500 left: `/usr/lib64` 334 (python `__pycache__` `.pyc` `S.5`, plus `.M.`
     directories), `/usr/lib` 73 (`os-release`, `fedora-release`, presets, firmware `.xz`),
     `/usr/share` 48 (pixmaps), `/usr/local` 31 (`missing`), `/usr/bin` 6, others.
2. **`rpm-ostree status --json` on the stock image:** `packages` and `requested-packages` are `[]`.
   The layered-entry format run (11-19 plan Task 6) is still outstanding.
3. **Rejected: `ostree fsck`.**
   - It aborts at the first corrupt object. In a throwaway repo with 2 corrupt objects it reported
     only the first and stopped at 1/13.
   - On the host it stopped at about 4% of 346,936 objects after 56 s. A full run would take about
     20 min.
   - It reports object checksums, not paths.
4. **Rejected: `ostree diff`.**
   - `ostree diff --fs-diff <rev> /usr` compares the commit root against `/usr` (the wrong
     subtree) and printed 218k lines.
   - `ostree diff <rev> <deployment dir>` took 66 s and printed 5,173 lines of unknown composition.
5. **Rejected: a cached-hash `baseline.db` (11-19 OQ1 candidate d).** It needs an in-process walk in
   the privileged tier and reimplements OSTree, against settled decision #6.
6. **Chosen primitive, host-verified as an unprivileged user.** For each candidate path `p`, compare
   two checksums:
   - expected: the 5th whitespace field of `ostree ls -C <commit> <p>`. The output shape is
     `-00644 uid gid size CHECKSUM path`, with a leading `d` for directories and `l` for symlinks;
   - actual: `ostree checksum <p>`.

   Results:
   - MATCH for `/usr/bin/bash`, `/usr/lib/os-release` and a python `.pyc`. The last two are
     rpm-failing and image-baked.
   - DIFFER for `/usr/etc/shells`: expected `eb3dd455...`, actual `653ecab4...`, the same value
     `fsck` reported. The content is the legitimate image-baked shells list. The mismatch is very
     likely an extra `user.overlay.origin` xattr. OSTree checksums cover xattrs, and
     `--ignore-xattrs` does not reproduce the expected value.
   - In a throwaway repo, an unchanged file matches and an appended-to file differs.
   - Cost: about 22 ms per `ls` + `checksum` pair, so about 11 s for 500 paths.
7. **Code on `1b1938fb`:**
   - `SystemFacts::base_tree_drift() -> Result<Vec<String>>` (`facts.rs:72`). Its live
     implementation (`facts.rs:240-244`) returns `PrimitiveUnavailable("... see plan OQ1")`.
     Implementors: `LiveSystem`, `testing::SyntheticFacts` (`with_base_tree_drift`, `facts.rs:383`)
     and `FakeFacts` (`tests/end_to_end.rs:84`).
   - `scan.rs:168-173` pushes `file_drift_base_tree` into `skipped`. `scan.rs:135-185` implements
     the interim rule: file_drift completes when `rpm -Va` parsed and the run is not incomplete.
   - `assess()` (`file_drift.rs:146-186`): any `Err` from a lookup marks the run `incomplete` and
     stops the loop (`L87-93`). An absent-from-commit lookup must therefore not be an `Err`.
   - `main.rs:121-138` `deployment_checksum()` runs `rpm-ostree status --json` itself, and
     `file_drift::read_deployment` (`file_drift.rs:115`) runs it again: the "two reads" deviation.
   - `DeploymentState` has both `checksum` (booted deployment) and `base_checksum`
     (`scanner-core/src/deployment.rs:17-20`).
   - `command.rs:645-647`: children run with `env_clear()`, `PATH=/usr/bin:/usr/sbin` and
     `LC_ALL=C`, so an inherited `OSTREE_REPO` cannot leak in.
   - `command.rs:572-583` `allowance()` holds the `NotOwned`/`VerifyDrift` exceptions, and
     `exit_allowed()` is at `L805`. `TRUSTED_DIRS` is `/usr/bin`, `/usr/sbin`.
   - Deadlines: `PRIMITIVE_DEADLINE` 300 s, `SCAN_WATCHDOG` 540 s, `FILE_OWNER_DEADLINE` 15 s,
     `FILE_DRIFT_BUDGET` 180 s. The static assert at `file_drift.rs:25-30` budgets one owner-lookup
     overshoot.
8. The trail learning of 2026-11-20 (layered packages are matched by parsed name) still applies.
   The `rpm-ostree status --json` layered-entry format is unverified.

## Scope / PR layout

| PR | Branch | Touches |
| --- | --- | --- |
| A | `chore/file-drift-base-tree-evidence` | `scripts/file-drift-evidence.sh` only (read-only additions) |
| B | `feat/file-drift-base-tree` | scanner-privileged (`facts.rs`, `command.rs`, `checks/file_drift.rs`, new `checks/base_tree.rs`, `scan.rs`, `main.rs`, `tests/end_to_end.rs`), scanner-broker test, `docs/RUNBOOK.md`, 11-19 plan OQ1 status line |

PR A merges first, and the owner runs it on the host. PR B Tasks 2-4 can be written against
`SyntheticFacts`/`FakeFacts` in parallel. The live argv/allowance details (Task 3 GREEN) and the
directory/symlink policy wait for the Task 1 evidence.

---

## Design

### Decision table (after rule 1 `MUTABLE_PREFIXES` and rule 4 `DYNAMIC_ALLOWLIST_PREFIXES`, which stay first and cost zero spawns)

Rule 2 runs **before** the owner lookup, in the same order as `classify()`. A base-tree match is
`Expected` whoever owns the file, so the about 500 image-baked candidates cost no `rpm -qf` spawn.

| On disk (from rpm) | Commit entry (`ostree ls`) | Result | `detail` |
| --- | --- | --- | --- |
| present | present, checksum equal | `Expected` (`base_tree_match = Some(true)`), no owner lookup | none |
| present | present, checksum differs | owner lookup. Layered: rule 3 `Modified`, anomalous. Base or unowned: anomalous | `base-tree mismatch [<attrs>] expected=<hex> actual=<hex> vs base_pkg:<n>` (or `layered_pkg:`/`unowned`) |
| present | absent from commit | owner lookup, then anomalous (rule 5 class: nothing trusted vouches for it) | `not in base commit [<attrs>] vs base_pkg:<n>` / `layered_pkg:` / `unowned` |
| missing | present | anomalous (deleted from the deployed tree) | `missing (present in base commit)` |
| missing | absent | `Expected`: the image itself lacks it, so it is image-baked. Covers the 31 `/usr/local` lines **if** `ostree ls` reports a path under the commit's `/usr/local` symlink as absent (**to-verify**) | none |
| directory or symlink | any | policy from Task 1 evidence (see below) | per policy |

`<hex>` is 64 lowercase hex. Both values go into `detail` verbatim, after validation. Layered
files: the comparison runs against the deployed commit (see "Which commit"). A clean layered file
can therefore match and be `Expected`. That is correct for the deployed tree, and on the stock
image it changes nothing (fact 2).

### Which commit

Use `DeploymentState.checksum`, the booted deployment commit that is actually checked out under
composefs, not `base_checksum`.

- Rationale: drift that matters is drift from what was deployed. A client-side layered or override
  commit is composed by `rpm-ostreed` as root from packages, which is the same trust as rule 3.
- The two are equal when nothing is layered. That is the only state measured (fact 2).
- Open Question 2 asks the owner to confirm this choice for layered and overridden hosts.
- The checksum must match `^[0-9a-f]{64}$` before it is used as `<commit>`. `main.rs`'s
  `"unknown"` fallback, or any other value, is never passed to `ostree`.

### Interface (replaces `base_tree_drift`)

- **Remove** `SystemFacts::base_tree_drift` from all three implementors, `with_base_tree_drift`
  and the `candidates()` merge of base paths. A per-path comparison does not fit a list-producing
  method, and a dead hook would invite a second implementation.
- **Keep** `FileDriftOutcome::base_tree_unavailable`, which may be renamed
  `base_tree_degraded`. It now carries the degraded-mode reason (see below) and is `None` on a
  healthy run.
- **Add** two per-path methods:
  - `commit_entry(&self, commit: &str, path: &str) -> Result<Option<CommitEntry>>`, where
    `CommitEntry { kind: EntryKind /* File | Dir | Symlink */, checksum: String }` and `None`
    means absent from the commit;
  - `path_checksum(&self, path: &str) -> Result<String>`. A path that vanished since `rpm -Va` is
    an `Err` (see TOCTOU).
- **Argv constants** in `command.rs`. All flag spellings are **to-verify** on the host (Task 1):
  - `OSTREE_LS_ARGS = ["ls", "--repo=/ostree/repo", "-C", "-d"]`, then `<commit>`, `"--"`,
    `<path>`. `-d`/`--dironly` stops a directory argument from listing its children;
  - `OSTREE_CHECKSUM_ARGS = ["checksum", "--"]`, then `<path>`;
  - pin `--repo=` even though the environment is cleared, so the reference repo is explicit;
  - use `--` only if Task 1 shows GOption accepts it for both subcommands. The absolute-path
    assertion is the primary guard either way.
- **Parser** (new pure `checks/base_tree.rs`, no I/O):
  - `parse_ls_entry(stdout, requested) -> Result<CommitEntry>` splits the first line into at most
    six fields;
  - it maps field 1's leading `-`/`d`/`l` to `EntryKind`;
  - it validates field 5 as 64-hex;
  - it requires the remaining text to equal `requested` (for symlinks, `requested -> target`, to
    be confirmed in Task 1) and rejects anything else, including extra lines. This rejects a
    child listing, a newline-split path and a spoofed entry;
  - directory `-C` lines may carry two checksums (contents and meta; **to-verify**), and the parser
    handles them per the Task 1 policy;
  - `parse_checksum(stdout) -> Result<String>` takes exactly one 64-hex token and fails closed
    otherwise.
- **`command.rs` allowance `Allow::OstreeAbsent`**, modelled on `NotOwned`:
  - applies only when `program == "ostree"`, the argv is exactly the `OSTREE_LS_ARGS` shape, the
    process exited normally with code 1 (**to-verify**), stdout is empty, and stderr equals one of
    the pinned "absent" texts for that path. Task 1 pins at least two cases, a plain absent path
    and a path through the commit's `/usr/local` symlink, which may differ (**to-verify**);
  - then `LiveSystem::commit_entry` returns `Ok(None)`;
  - any other non-zero exit, signal or argv stays `Err`. No general exit-code switch.
- **Deadlines:** `OSTREE_LOOKUP_DEADLINE` (proposed 10 s) applies to each `ostree` spawn.
  - One candidate can now spawn `ls` + `checksum` + `rpm -qf` after its budget check, so extend the
    `file_drift.rs:25-30` static assert to
    `FILE_DRIFT_BUDGET + FILE_OWNER_DEADLINE + 2 * OSTREE_LOOKUP_DEADLINE + PRIMITIVE_DEADLINE <
    SCAN_WATCHDOG`. That is 180 + 15 + 20 + 300 = 515 < 540.
  - A budget overrun keeps today's semantics: findings so far are kept, the run is `incomplete`
    (`file_drift_incomplete`), and nothing resolves.
- **Single deployment read.** `main.rs` calls `facts.deployment_status_json()` once and parses it
  with `parse_status_json`.
  - One `DeploymentState` (or the parse error) feeds both the scan-run tag (`checksum`, falling
    back to `"unknown"` as today) and `run_file_drift`.
  - `file_drift::read_deployment`'s own spawn and `main.rs::deployment_checksum()`'s direct
    `command::run` are removed.
  - The exact signature change to `run_privileged_scan` is the executor's choice, but there must be
    exactly one `rpm-ostree status --json` per scan (a counting test double asserts it). This
    closes the 11-19 "two reads" deviation.

### Degraded mode and the interim rule (revisited)

- **Healthy:** the deployment parsed, the checksum is valid hex, and `ostree` is resolvable in
  `TRUSTED_DIRS`.
  - The base-tree pass is complete for rpm-visible files.
  - No `file_drift_base_tree` entry is emitted.
  - `file_drift` counts as completed under the existing conditions: no `incomplete`, no
    `unparsed`.
- **Degraded:** `ostree` is missing, there is no booted deployment, or the checksum is not hex.
  - **How it is reached:** a pre-check **before** the candidate loop calls
    `crate::command::trusted_program("ostree")` and `is_commit_hex(deployment.checksum)`. If
    either fails, the whole run is degraded. Inside the loop, a `PrimitiveUnavailable` or any
    other `Err` from `commit_entry`/`path_checksum` keeps today's `assess()` semantics: the run is
    `incomplete` and the loop stops. It never silently degrades midway.
  - Candidates are classified as today (`base_tree_match = Some(false)`), so the run over-reports
    and never under-reports.
  - One `SkippedCheck { check: "file_drift_base_tree", reason }` is emitted, with the concrete
    reason taken from `base_tree_unavailable`.
  - It still counts as completed. The invariant that makes this safe: the degraded anomaly set is
    a superset of the healthy anomaly set for the same host state, so a degraded run cannot
    resolve a finding a healthy run would keep. A test pins this.
  - Why the differing `detail` text does not matter: `reconcile` keys prior open findings on
    `path` (with `check_type`), never on `detail` (`store.rs:161-181`, `WHERE path = ?3`).
- **Rule-5 unowned files under `/usr`** (`rpm -Va` cannot list them) stay out of scope. Proposed
  default: one constant `SkippedCheck { check: "file_drift_unowned", reason: "unowned files under
  /usr are not enumerated (rpm -Va cannot see them); see plan 2026-11-21 Non-goals" }`.
  - It does not block completion: no finding with an unowned-enumeration source can exist to be
    wrongly resolved.
  - When an unowned pass lands, split resolution by source, as the 11-19 interim rule already
    requires.
  - Whether to show this entry at all is Open Question 5.

---

## Tasks (TDD: each RED test is written and shown failing at runtime before its fix; the executor records the RED output)

- [x] **1. Host evidence for the to-verify items. DONE 2026-11-21 (owner-measured; not implemented as a script change).**
  - Measured on a stock Bazzite host, 501 candidates after the /var,/boot filtering:
    446 regular files MATCH; 15 directories MATCH (second checksum field); 3 symlinks MATCH;
    36 BOTH_ABSENT (31 under /usr/local); 1 `/usr/bin/sudoreplay` unreadable by an unprivileged
    user (mode `--x--x--x`; the scanner runs as root, so expected to match, unverified).
  - About 10.6 s for all 501 pairs including host-spawn overhead.
  - `/usr/etc/shells` DIFFERs (xattr-only, legitimate content) but is not an `rpm -Va` candidate.
  - Argv fixed by the owner: `ostree ls -d -C <commit> <abs path>` and `ostree checksum <abs path>`
    (no `--repo=`, no `--`).
  - Absent-path stderr/exit were pinned from a throwaway repo with libostree 2026.2 (exit 1, empty
    stdout, `error: Inspecting path '<p>': No such file or directory: <p>`; through a symlink
    ancestor `error: Inspecting path '<p>': Not a directory`). CONFIRMED on the host's ostree 2026.4 (read-only, 2026-11-21):
    `ostree ls -d -C <commit> /usr/local/no-such-file-xyz` => exit 1, empty stdout, stderr `error: Inspecting path '/usr/local/no-such-file-xyz': No such file or directory: /usr/local/no-such-file-xyz`; `.../usr/bin/bash/x` => exit 1, `error: Inspecting path '/usr/bin/bash/x': Not a directory`. Identical to the pins (the trailing newline was not byte-verified on the host). A different text would fail closed.
  - Original (superseded) Task text follows for the record.
  - Extend `scripts/file-drift-evidence.sh` with a `Q-BT` section. It must run only read-only
    commands, each under a timeout, as the script does today.
  - For every post-#409 candidate (rpm-failing, outside `MUTABLE_PREFIXES` and
    `DYNAMIC_ALLOWLIST_PREFIXES`), classify it as match, differ, absent, missing+present,
    missing+absent, dir or symlink. Write the counts to `summary.txt`, plus up to 20 sample paths
    per class.
  - Pin:
    - exit code, stdout and exact stderr of `ostree ls` for an absent path and for a path under the
      commit's `/usr/local` symlink;
    - whether `-d` and `--repo=/ostree/repo` are accepted;
    - whether `--` is accepted by `ls` and by `checksum`;
    - the directory `-C` line shape (one or two checksums);
    - the symlink line shape (`path -> target`);
    - whether `ostree checksum` works on a directory;
    - whether `ostree checksum` follows symlinks (it must not; see Security);
    - `ostree checksum` behaviour on a FIFO (it must be bounded by the deadline);
    - for each `differ` sample, whether `getfattr -d -m - <p>` shows `user.overlay.*` xattrs.
  - Record wall time for the whole classification.
  - Accept: `bash -n` and `shellcheck` clean, if `shellcheck` is present. The script writes only
    under its outdir. The owner brings back `summary.txt`.
  - **Gate:** if `differ` (xattr-only) or dir/symlink candidates exceed `MAX_DRIFT_PATHS` on a
    clean image, Open Question 1 becomes **blocking** for PR B's merge. A clean image would still
    hit the cap.
- [x] **2. Pure parser and validation (`checks/base_tree.rs`).**
  - RED:
    - a regular-file line yields `File` + checksum;
    - `d` and `l` lines yield `Dir`/`Symlink` (fixtures from Task 1 once available, synthetic
      until then);
    - a path containing spaces round-trips;
    - a first line whose path differs from the requested one (a child listing) is `Err`;
    - an extra second line is `Err`;
    - a 63-char, uppercase or non-hex checksum is `Err`;
    - a `checksum` output with two tokens is `Err`;
    - `is_commit_hex("unknown") == false`.
  - Accept: `cargo test -p scanner-privileged base_tree`.
  - Mutation:
    - drop the path-equality check, and the child-listing test fails;
    - drop the hex check, and the non-hex test fails;
    - split on all whitespace instead of a bounded split, and the spaces test fails.
  - RED 2026-11-21: 6 of 13 parser tests failed against the stub (the Err-expecting ones passed vacuously); mutations (drop path equality, drop hex check, whitespace split) each failed 1-3 tests.
- [x] **3. `SystemFacts` methods, argv constants, `Allow::OstreeAbsent`, deadlines.**
  - RED:
    - fixture-script tests in `command/tests` (pattern of `verify_exit_tests`):
      - exit 1 + the pinned absent stderr under the exact `OSTREE_LS_ARGS` shape gives the
        allowance;
      - the same exit under any other `ostree` argv is `Err`;
      - exit 1 with a different stderr is `Err`;
      - a signal death is `Err`;
    - `LiveSystem::commit_entry`/`path_checksum` return `Err` for a non-absolute path and for a
      path starting with `-`, without spawning (assert via a non-resolvable program, or check
      before `run`);
    - `commit_entry` rejects a non-hex commit without spawning;
    - a unit test asserts `OSTREE_LS_ARGS` contains `-d` and the pinned `--repo=`.
  - GREEN:
    - `LiveSystem` implementations;
    - remove `base_tree_drift`, `with_base_tree_drift` and `FakeFacts::base_tree_drift`;
    - add `with_commit_entry(commit, path, entry|absent|error)` and
      `with_path_checksum(path, sum|error)` to `SyntheticFacts`, mirrored in `FakeFacts`;
    - add `OSTREE_LOOKUP_DEADLINE` and extend the static assert.
  - Accept: `cargo test -p scanner-privileged command` green; watchdog/containment tests
    unchanged.
  - Mutation:
    - widen `OstreeAbsent` to any `ostree` argv, and the second test fails;
    - drop the stderr match, and the third test fails;
    - remove the absolute-path guard, and the leading-`-` test fails;
    - drop the commit-hex check, and the reject-without-spawn test fails;
    - drop `-d` from `OSTREE_LS_ARGS`, and the argv-constant test fails;
    - raise `OSTREE_LOOKUP_DEADLINE` to 30 s, and the static assert fails to compile (record the
      compile error).
  - RED 2026-11-21: `the_exact_ls_shape_with_the_pinned_stderr_is_data` and 3 facts guard tests failed first; mutations (widen allowance, drop stderr match, drop `-d`, drop abs-path guard, drop commit-hex guard) each failed; `OSTREE_LOOKUP_DEADLINE`=30 s fails to compile (E0080 on the static assert).
- [x] **4. `run_file_drift`: rule 2 first, decision table, single deployment read.**
  - RED in `file_drift.rs` tests via `SyntheticFacts`, one per decision-table row:
    - present + equal is `Expected`, with **zero** `file_owner` calls (counted);
    - present + differ is anomalous, and the detail contains both `expected=` and `actual=` hex;
    - present + absent is anomalous `not in base commit ... base_pkg:`;
    - missing + present is anomalous `missing (present in base commit)`;
    - missing + absent is `Expected`;
    - layered-owned differ is anomalous `layered_pkg:`.
  - Further RED tests:
    - **zero `ostree` spawns** for mutable, `/etc`-allowlisted and other rule-4 paths (counted,
      mirroring `expected_paths_never_cost_an_owner_lookup`);
    - a `path_checksum` `Err` (vanished file, TOCTOU) keeps earlier findings and marks the run
      `incomplete`. Proposed: stop the loop, consistent with today's failed-lookup rule. Open
      Question 6;
    - a budget overrun in the middle of the lookups marks the run `incomplete`;
    - degraded mode, reached by the pre-check (no `ostree` / `"unknown"` checksum), gives today's
      anomalies plus a `base_tree_unavailable` reason, with zero `ostree` spawns. A table test
      asserts degraded ⊇ healthy over the same fixture set;
    - a `PrimitiveUnavailable` from `commit_entry` on the third candidate (after a passing
      pre-check) gives `incomplete` with the first two candidates' results kept, not degraded;
    - about 500 image-baked candidates that all match give zero findings, no aggregate `/` finding
      and no `incomplete` (the measured failure, fact 1);
    - a candidate path containing a control character or newline is never passed to `ostree` and
      stays anomalous with an escaped detail.
  - GREEN: implement the table. Delete `read_deployment`'s spawn and take the shared
    `DeploymentState`.
  - Accept: `cargo test -p scanner-privileged`.
  - Mutation:
    - map equal to `Some(false)`, and the 500-match test fails;
    - treat absent as `Err`, and the absent-row test fails (incomplete instead);
    - move the owner lookup before rule 2, and the zero-`file_owner` test fails;
    - map missing+absent to anomalous, and its row fails;
    - drop the control-char guard, and the newline test fails;
    - move the ostree lookup before the rule-1/rule-4 prefix filters, and the zero-`ostree`-spawn
      count fails;
    - treat a `path_checksum` `Err` as a match, and the TOCTOU-vanish test fails;
    - skip the degraded pre-check (always classify as healthy), and the degraded and superset tests
      fail.
  - RED 2026-11-21: 14 of 33 file_drift tests failed before the fix; all 8 listed mutations failed at least one test.
- [x] **5. `scan.rs`/`main.rs` wiring, end-to-end, broker, docs.**
  - `scan.rs`: emit `file_drift_base_tree` only in degraded mode, and add the
    `file_drift_unowned` entry (pending Open Question 5).
  - Update the unit test at `scan.rs:271-273` and the 11-19 live-shaped e2e expectations.
  - RED e2e in `tests/end_to_end.rs` (real `scans.db` under a tempdir, `FakeFacts`):
    - **migration:** a seeded prior run that was over the cap (aggregate `/` + 500 open
      `base_pkg:` findings), then a healthy run where all of them match, resolves all 501;
    - a tampered base file (differ) is `new`; the same on rerun is `still`; restored (match) is
      `resolved`;
    - healthy run: `skipped` contains no `file_drift_base_tree`;
    - degraded run: `skipped` contains `file_drift_base_tree` with the reason, and nothing wrongly
      resolves;
    - exactly one `deployment_status_json` call per scan (counting double).
  - Broker: a deserialization fixture containing a `file_drift` finding with a
    `base-tree mismatch expected=... actual=...` detail and a `file_drift_unowned` skip. The
    broker types `check` as `String`, so no schema change is expected; the test pins that.
  - Docs in the same PR:
    - append the Task 6 items to `docs/RUNBOOK.md`'s `file_drift` checklist, and replace its "still
      lists `file_drift_base_tree`" sentence and checkbox (`RUNBOOK.md:91-92,112-113`);
    - add a `**Answered 2026-11-21:**` status line under OQ1 in the 11-19 plan that points to this
      plan. Annotate, don't rewrite.
  - Accept:
    - `just check` and `just test` (both `--workspace`), `cargo test -p scanner-broker` and
      `cargo test -p snitchwatch-bridge` (the dormant-identifier scan) green;
    - `security-reviewer` reports no CRITICAL or HIGH findings.
  - Mutation:
    - restore the unconditional `file_drift_base_tree` push, and the healthy-run assertion fails;
    - restore `main.rs`'s direct `command::run("rpm-ostree", ...)` read, and the
      one-status-read-per-scan count fails.
  - RED 2026-11-21: `a_healthy_run_reports_no_base_tree_skip_but_states_the_unowned_gap` failed (no `file_drift_unowned`); mutations (unconditional base-tree skip, second status read) each failed.
- [ ] **6. Real-host checklist additions (`docs/RUNBOOK.md`, manual, non-blocking for merge).**
  - On the stock image via the broker (`RunDeepScan`), confirm all of the following:
    - no aggregate `/` finding;
    - no `file_drift_incomplete`;
    - no `file_drift_base_tree`;
    - the anomaly count is the Task 1 `differ` + `absent` + `missing+present` total;
    - `/usr/etc/shells`-style xattr cases appear with both checksums.
  - Record total `file_drift` wall time (`rpm -Va` about 39 s + lookups) against the 180 s budget.
  - `ostree admin unlock` (or the equivalent dev overlay) on a test host, append to a base file,
    and confirm one anomaly `base-tree mismatch`. Revert and reboot, and confirm it resolves.
  - Still outstanding from 11-19 Task 6: the layered-entry format of `rpm-ostree status --json`
    `packages`/`requested-packages` (it was `[]` on the stock image). Layer one package, record the
    entries, and edit one of its files: expect one `layered_pkg:` anomaly with both checksums.
  - Delete one base file in an unlocked overlay and expect `missing (present in base commit)`.

---

## Security considerations (privileged tier)

- **Argv injection.**
  - Every path comes from `rpm -Va` output and is attacker-influenced.
  - Paths must start with `/`. Both new methods assert this before spawning, so a leading `-` is
    impossible.
  - Pass `--` where Task 1 confirms it. Each path is one argv element; there is no shell
    (`command::run`).
  - The commit is validated as 64-hex.
  - Paths with control characters or newlines are never looked up (Task 4): `rpm -Va` output is
    line-based, so such a path was already mangled and cannot be trusted to name the real file.
- **Output trust.** `ostree ls` output is parsed with a bounded split, and the path must equal the
  requested one. Checksums are validated as 64-hex. Anything else fails closed: `Err`, then
  `incomplete`, never a match.
- **Hash strength.** An OSTree content checksum is SHA-256 over content, uid, gid, mode and xattrs.
  An attacker who can write a file cannot also make it match the commit's checksum without a
  SHA-256 second preimage, which is infeasible. A match therefore means the deployed object is
  identical to the committed one.
- **Symlink following.** If `ostree checksum` followed symlinks, an attacker could replace a file
  with a symlink to an identical copy elsewhere and produce a false match. Task 1 must confirm it
  does not follow. If it does, symlink candidates are never compared (they stay anomalous), and
  this must get a security-reviewer pass.
- **TOCTOU between `rpm -Va` and `ostree checksum`.**
  - The scan is point-in-time. A file restored before `checksum` runs is clean at that moment.
  - A file changed after `checksum` runs is caught by the next scan.
  - A file that vanishes between the two calls is an `Err`, so the run is `incomplete` and nothing
    resolves.
  - Kind mismatches (the commit has a regular file, disk has a FIFO or device) are bounded by
    `OSTREE_LOOKUP_DEADLINE`.
  - Writing anything under `/usr` already requires root plus an unlocked or remounted composefs.
- **composefs/xattr false positives (`/usr/etc/shells`).** Recommended: keep these flagged, with
  both checksums in `detail`, so an operator can compare. Ignoring xattr-only mismatches would
  hide a real xattr change (for example a `security.capability` added to a binary). Special-casing
  is Open Question 1, not decided here.
- **No new persistent process or in-process file walk.** The privileged process still never opens
  or hashes target files. `ostree` does, as a child under `command::run`, with the env cleared,
  `TRUSTED_DIRS` resolution, byte caps, per-spawn deadlines and the watchdog-swept process group.
  The 11-19 `no_persistent_service` guard is unaffected.
- **Bounded cost.** `MAX_DRIFT_PATHS`, the 1 MiB stdout cap, `FILE_DRIFT_BUDGET` and the extended
  static assert bound spawns and time. Hitting any of them is surfaced, never treated as clean.

## Non-goals

- Enumerating **unowned** files under `/usr` (rule 5 for paths `rpm -Va` cannot see). This needs
  its own primitive and plan, for example an `ostree ls -R` vs on-disk listing, and it must split
  resolution by source.
- `baseline.db` / cached hashes, `ostree fsck`, `ostree diff`, composefs/fs-verity digest
  verification.
- Batching several paths into one `ostree ls` call. Revisit only if Task 6 timing exceeds the
  budget (Open Question 7).
- Changing `MUTABLE_PREFIXES`/`DYNAMIC_ALLOWLIST_PREFIXES` beyond #409, the polkit policy, the
  broker D-Bus API, the `scans.db` schema or the Kirigami report page.
- Any test against a live host, daemon, `sudo` or real `ostree`/`rpm`. Tests use `SyntheticFacts`,
  `FakeFacts`, fixture scripts and tempdirs only.

## Acceptance criteria (whole plan)

- [ ] PR A merged; the owner's `summary.txt` answers every Task 1 to-verify item. If the gate
  fires, Open Question 1 is answered before PR B merges.
- [ ] PR B:
  - `base_tree_drift` is gone;
  - a healthy run emits no `file_drift_base_tree`;
  - a simulated stock image of about 500 matching candidates gives zero findings and a completed
    `file_drift`;
  - each Task 2-5 guard has a recorded RED run and a failing mutation;
  - `just check` and `just test` green;
  - `security-reviewer` reports no CRITICAL or HIGH findings.
- [ ] Manual (non-blocking): the Task 6 checklist is recorded by the owner.

## Open questions (stop-and-ask; not decided by this plan)

1. **xattr-only mismatches** (`/usr/etc/shells`: likely `user.overlay.origin`). The recommended
   default keeps them anomalous with both checksums. The alternative is to special-case them by
   recomputing without specific `user.overlay.*` xattrs (`--ignore-xattrs` does not reproduce the
   expected value, so this would need a narrower primitive). **Blocking** if Task 1 shows they
   alone exceed the cap on a clean image.
   **Decided 2026-11-21 (owner: "defaults"): xattr-only mismatches stay flagged, anomalous, with both checksums in the detail.**
2. **Which commit:** deployed `checksum` (proposed) or `base_checksum`. They differ on layered or
   overridden hosts, and only the stock image was measured. With `base_checksum`, every file of an
   overridden base package becomes anomalous.
   **Decided 2026-11-21 (owner: "defaults"): compare against the booted deployment `checksum`, not `base_checksum`.**
3. **Directories (`.M.`) and symlinks (`L`).** The policy depends on Task 1: compare via
   `ostree checksum` if it supports them without following, otherwise keep them anomalous with
   `base-tree comparison unsupported: <kind>`. A count of `.M.` directories on the stock image is
   needed first.
   **Decided 2026-11-21 (owner: "defaults"): directories: expected = the SECOND checksum field of `ostree ls -d -C` (dirmeta), actual = `ostree checksum <dir>`; regular files and symlinks: expected = the FIRST checksum field (5th column), actual = `ostree checksum <path>`; `ostree ls -d` so a directory is not expanded.**
4. **Missing on disk and absent from the commit is `Expected`** (proposed): it is image-baked, for
   example the `/usr/local` symlink into `/var/usrlocal`. Confirm. The alternative keeps them
   anomalous, at 31 findings on every stock scan.
   **Decided 2026-11-21 (owner: "defaults"): a path rpm reports missing that is absent from the commit AND absent on disk is Expected.**
5. **`file_drift_unowned` skip entry.** Keep the constant entry so the report says rule-5 coverage
   is absent (proposed), or drop it to avoid a permanent line in the GUI. A follow-up plan for the
   unowned pass is needed either way.
   **Decided 2026-11-21 (owner: "defaults"): keep a constant `file_drift_unowned` skipped entry (rule-5 unowned files under /usr are still not checked).**
6. **TOCTOU vanish:** stop the loop as `incomplete` (proposed, consistent with today's
   failed-lookup rule), or record that one path as anomalous `vanished during scan` and continue.
   **Decided 2026-11-21 (owner: "defaults"): a file that vanishes mid-scan (`ostree checksum` fails on a path that existed) marks the run incomplete and stops; nothing resolves.**
   Refined after review 2026-11-21: this applies to the VANISHED case only (and to an unavailable primitive). Any other lookup failure (deadline, I/O error on a path that still exists) is reported as an anomalous `base-tree lookup failed: <reason>` finding, the run is marked incomplete (nothing resolves) and continues, so one decoy (FIFO, device, huge file) cannot hide later candidates. `ostree ls` exit 0 with empty stdout is an error; absence is proven only by the pinned exit-1 stderr.
7. **Spawn cost:** per-path `ls` + `checksum` (about 22 ms per pair measured) vs batching `ostree
   ls` paths. Revisit only with Task 6 timing; `rpm -qf` cost for the non-matching remainder is
   unmeasured.
   **Decided 2026-11-21 (owner: "defaults"): per-path lookups, no batching.**
8. **Layered-entry format** of `rpm-ostree status --json` (carried from 11-19 Task 6; `[]` on the
   stock image). Still needed before layered-host behaviour can be trusted.
   **Still OPEN (owner, 2026-11-21): left as a RUNBOOK / real-host item (Task 6).**
