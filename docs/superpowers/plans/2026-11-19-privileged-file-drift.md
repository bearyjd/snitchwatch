# Privileged-tier `file_drift` check + Component B guard/test/doc bundle

**Date:** 2026-11-19
**Specs:** `docs/superpowers/specs/2026-07-04-scanner-baseline-design.md` (§1 5-step
classification tree, §2 cache split, §3 per-file `rpm -V` trust, §4 diff schema);
`docs/superpowers/specs/2026-07-04-scanner-privileged-tier-design.md` (§3 ordering: file drift
first; §4 `check_type` column)
**Prior work:** `docs/superpowers/plans/2026-07-05-phase6-scanner-privileged.md` (deferred
`file_drift` "until Phase 5 merges"); `docs/superpowers/plans/2026-11-15-scanner-329-items-3-4.md`
(latest scanner-privileged plan, same PR-per-item shape used here)
**Base:** `main` @ `1fd292e0`

## Why now

`crates/scanner-privileged/src/scan.rs:44-52` still pushes a hardcoded
`SkippedCheck { check: "file_drift", reason: "...lands when Phase 5 merges" }`. Phase 5
(`scanner-core`'s userspace tier, `rpmverify.rs`, `classify.rs`, `provenance.rs`,
`stores/baseline.rs`) has merged, so the privileged tier's first-ordered check is the last missing
piece of Phase 6's scan logic. HANDOFF.md:80 records the owner's real tower scan skipping it. The
bundle (PRs 2-4) closes four smaller audit gaps that share no code with PR 1.

## Verified facts this plan builds on (re-checked 2026-11-19 on `1fd292e0`)

Each was confirmed by grep/Read. Re-check any you rely on before editing, because main moves.

1. **No dependency edge.** `crates/scanner-privileged/Cargo.toml` does not depend on
   `scanner-core`. The two crates have separate `Classification`/`Finding`/`ScanStore` types
   (`scanner-privileged/src/model.rs`, `src/store.rs`).
2. **Reusable from scanner-core** (pure, no `SystemInspector`): `classify::classify` +
   `PathFacts`, `rpmverify::{path_verifies_clean, layered_match_for}`,
   `deployment::parse_status_json` / `DeploymentState::has_layered_package`,
   `provenance::{MUTABLE_PREFIXES, DYNAMIC_ALLOWLIST_PREFIXES}`. **Not reusable:**
   `provenance::classify_path` (bound to `SystemInspector`, not `SystemFacts`).
3. **`classify_path`/`classify` have no production caller** in `scanner-core/src/checks/` or
   `scanner.rs`; `BaselineStore` is referenced only from `stores/mod.rs` docs. Expected-drift
   classification is unit-tested in `provenance.rs` (tests ~L155-260) but is not reachable through
   `Scanner::run`.
4. **Exit status turns drift into an error.** `scanner-privileged/src/command.rs:727-736`: any
   non-zero exit is `Err("system primitive failed ...")`, with one exception (`rpm -qf` "not owned",
   L554-557). `rpm -V` exits non-zero *exactly when* files fail verification, so the signal we want
   would abort the scan via `scan.rs`'s `Err(e) => return Err(e)` arms.
5. **Output/time bounds.** `command.rs:10` `STDOUT_LIMIT = 1 MiB` (overflow is an error, L582);
   each primitive has a 300 s deadline (L561); `main.rs:62` whole-scan watchdog = 540 s.
6. **`rpmverify::parse_verify_line` takes the last whitespace token** (`rpmverify.rs:48-60`). A
   failing path containing a space parses as `None`, so `path_verifies_clean` reports it
   **clean (fails open)**.
7. **`reconcile` resolves across all check types.** `store.rs:138-220` loads *every* open finding
   and marks any absent from the current anomaly set resolved. A skipped check (rpm missing,
   chkrootkit absent, timeout) therefore silently "resolves" its previous findings. This is true
   today for `rootkit_signature`/`module_anomaly` too.
8. **`findings.check_type`** exists (`store.rs:46`, `DEFAULT 'file_drift'`) and is written from
   `finding.check_type.as_db()` (L187). `CheckType::FileDrift` ↔ `"file_drift"` already exists
   (`model.rs:15-40`). Non-file checks key `path` with a prefix (`kargs:`, `module:`, ...).
9. **`SystemFacts` implementors:** `LiveSystem` (`facts.rs:64`), `testing::SyntheticFacts`
   (`facts.rs:219+`), `FakeFacts` (`tests/end_to_end.rs:17`). The unit test at `scan.rs:189-191`
   asserts `file_drift` is skipped. The Kirigami fixtures (`scanner_report_renders_qml.rs:24`,
   `scanner_click_path_qml.rs:20`, `scanner_sticky_activation_qml.rs:19`) are JSON data and need no
   change.
10. **`ScannerPage.qml:206-207` renders `path`/`detail` with `Text.PlainText`**, so an
    attacker-named file is not interpreted as rich text in the GUI.
11. `scanner-privileged` and `scanner-broker` are in `default-members` (`Cargo.toml:23-34`), so
    their tests run in CI's `test` job.
12. Trail learning (2026-11-07): never name an identifier `lifecycle` outside the bridge's
    history module, because `history::dormant_tests` scans all workspace sources. Executors must show
    each RED test failing at runtime before writing the fix.

## Scope / PR layout

Four independent PRs, each reviewable and revertible alone. Order: PR 1 first (largest); PRs 2-4
can run in parallel with it.

| PR | Branch | Gap | Touches |
| --- | --- | --- | --- |
| 1 | `feat/privileged-file-drift` | 1 | scanner-privileged, scanner-core (`rpmverify.rs`) |
| 2 | `test/scanner-no-persistent-service` | 2 | new test in scanner-privileged |
| 3 | `test/userspace-scan-prior-state` | 4 | `scanner-core/tests/userspace_scan.rs` (+ possibly `deployment.rs`) |
| 4 | `docs/plan-status-3a-phase2` | 5, 6 | two docs only |

---

## PR 1: privileged `file_drift` (Gap 1)

### Design

Implement baseline-spec §1 rules 1, 3, 4 and 5 for rpm-owned files in the privileged tier by
**delegating** to `rpm -Va` (settled decision #6: no re-implemented manifest engine). Classify each
failing path with scanner-core's pure `classify()`. Rule 2's authoritative OSTree base-tree check
and rule-5 *unowned* files under `/usr` (which `rpm -Va` cannot see) go behind a separate
`SystemFacts` primitive. Its live implementation is **blocked on Open Question 1**. Until that is
answered it returns `PrimitiveUnavailable` and the report says so explicitly, as a partial check
and not a silent pass.

- **Dependency:** add `scanner-core = { path = "../scanner-core" }` to scanner-privileged. Both
  are Component B, so this does not touch settled decision #1. Map
  `scanner_core::Classification` → `scanner_privileged::model::Classification` in one small `From`
  impl (exhaustive match, no wildcard, so a new variant fails to compile).
- **New `SystemFacts` methods** (all three implementors updated):
  - `rpm_verify_all(&self) -> Result<String>`: live implementation runs one fixed argv constant,
    `RPM_VERIFY_ARGS = ["-Va", "--nomtime", "--noscripts", "--nodeps"]`.
    - `--noscripts`: never execute `%verifyscript` from (possibly attacker-layered) packages as
      root.
    - `--nomtime`: OSTree checkouts normalize mtimes, so without it every base file reports `T`
      and blows the 1 MiB cap (to be confirmed on a host; see Open Question 3).
    - `--nodeps`: suppresses dependency lines that aren't file drift.
  - `file_owner(&self, path: &str) -> Result<Option<String>>`: `rpm -qf --queryformat
    %{NAME}\n <path>`. This reuses the existing not-owned special case at `command.rs:554`, which
    currently matches only `args.len()==2` and must be extended to the queryformat argv
    deliberately, with its own test.
  - `deployment_status_json(&self) -> Result<String>`: `rpm-ostree status --json`. `main.rs:59-75`
    already runs this for the checksum; refactor it so one read feeds both.
  - `base_tree_drift(&self) -> Result<Vec<String>>`: paths under `/usr` that differ from, or are
    absent from, the booted base commit. The live implementation is **Open Question 1**; until it
    is answered it returns `PrimitiveUnavailable("base-tree check not implemented: see plan OQ1")`.
- **`command.rs` exit allowance (keyed on stdout, not a specific code):** `man rpm` documents
  only "a nonzero failure code" on failure. rpm's query/verify status is conventionally a failure
  *count* (possibly modulo 256), so "exit 1 only" would break on drift spanning two or more
  packages. Rule:
  - applies only when `program == "rpm"`, `args == RPM_VERIFY_ARGS` exactly, and the process
    exited normally;
  - a non-zero `CLD_EXITED` code is `Ok(stdout)` only if stdout contains at least one parseable
    verify line;
  - a non-zero code with no parseable line (locked or corrupt rpmdb, usage error) is `Err`;
  - stdout is parsed on exit 0 too;
  - signal death and any other argv stay `Err`.

  Modelled on the `not_owned` special case; no general "ignore exit codes" switch. Confirm the
  real exit-code semantics on a host (Task 6).
- **`rpmverify.rs` (scanner-core):** add `failing_entries(output) -> Vec<VerifyFailure { path,
  attrs, missing }>`. Fix `parse_verify_line` to take the path as **everything after** the attribute
  field and optional single-char file-type marker, not the last token. Lines that don't yield an
  absolute path are counted and reported, not dropped silently.
- **New `checks/file_drift.rs`** (`pub fn run_file_drift(facts) -> Result<FileDriftOutcome>`):
  1. Parse deployment JSON → `DeploymentState`.
  2. For each failing entry, cap the count at `MAX_DRIFT_PATHS` (a constant, proposed 500). Over
     the cap, emit one aggregate anomaly with `path = "/"` and detail `"N paths failed rpm -Va;
     first 500 listed"`. Then:
     - rule 1: `MUTABLE_PREFIXES` → skip.
     - owner lookup (`file_owner`); layered if `has_layered_package`.
     - build `PathFacts`: `base_tree_match = Some(false)` for a failing base-package file,
       `layered_pkg = Some(Modified)` for a failing layered one, unowned otherwise.
       `on_dynamic_allowlist` comes from `DYNAMIC_ALLOWLIST_PREFIXES`.
     - `classify()` → `Expected` is dropped, `Anomalous` becomes
       `Finding::anomalous(CheckType::FileDrift, path, detail)`.
  3. `detail` = `"rpm -V mismatch [<attrs>] vs layered_pkg:<name>"` / `base_pkg:<name>` /
     `"missing"` / `"unowned"`, matching the privileged spec §4 example table.
  4. Merge `base_tree_drift()` paths through the same classifier when available. When it returns
     `PrimitiveUnavailable`, the outcome is marked `partial` with the reason.
- **`scan.rs`:** replace the hardcoded skip (L44-52) with the real call, still first in order.
  Error policy:
  - `rpm` missing → skipped (as today's pattern).
  - Any other primitive error (over-limit output, deadline, unparseable) → `skipped` with the
    concrete reason, **without** aborting the scan, so kargs/modules/lockdown/chkrootkit still run
    (spec §3's "cheaper checks still recorded" intent).
  - Partial (base-tree unavailable) → findings kept **and** a `SkippedCheck { check:
    "file_drift_base_tree", reason }` entry.

  Whether a `file_drift` primitive failure should also force a non-zero exit is Open Question 4.
- **`reconcile` scoping (fixes fact 7 for all checks):** `reconcile(scan_id, findings,
  completed: &[CheckType])`. Only prior open findings whose `check_type` is in `completed` may be
  resolved. Findings from skipped or partial checks are inserted/bumped but never resolve anything.
  Callers derive `completed` from the checks that ran. Prior-open lookup selects `check_type`
  alongside `path`.

  **Interim rule for `file_drift`:** it counts as completed when `rpm -Va` succeeded, even while
  `base_tree_drift` is unavailable. Rationale: until OQ1 lands, no base-tree/unowned finding can
  exist to wrongly resolve. Without this rule the live check would always be partial, and nothing
  it found would ever resolve on a real host. When OQ1's primitive lands, split resolution so an
  unavailable base-tree pass cannot resolve base-tree-sourced findings (e.g. detail tag `unowned`
  or a `base_tree` sub-source column). Revisit with OQ1.
- **Time budget:** add a `command::run_with_deadline(program, args, Duration)` (or a parameter on
  the existing internal `capture`). The `file_drift` primitives use a combined
  `FILE_DRIFT_BUDGET` (proposed 180 s) so a slow `rpm -Va` cannot starve the 540 s watchdog for
  the later checks. The value is Open Question 2.
- **Not in this PR:** writing `baseline.db` (`BaselineStore`). With rule 2 delegated to `rpm`/
  OSTree there is nothing to cache yet. Whether the base-tree primitive needs the cached-hash
  baseline of spec §2 is part of Open Question 1.

### Tasks (TDD: each RED test is written and shown failing at runtime before its fix)

- [ ] **1. scanner-core `rpmverify` hardening.**
  - RED: `path_with_space_is_not_reported_clean` (`"S.5....T.  c /usr/share/a b.conf"` →
    `path_verifies_clean(.., "/usr/share/a b.conf") == false`); `failing_entries` returns
    `missing` lines and type-marker lines with full paths; a non-absolute garbage line is counted
    as unparsed, not returned as a path.
  - GREEN: the new parser. Existing `rpmverify` tests stay green.
  - Accept: `cargo test -p scanner-core` green.
  - Mutation: revert to last-token parsing → space test fails.
- [ ] **2. `command.rs` exit allowance.**
  - RED (fixture-script tests, same pattern as `command/tests` `exit_tests` around L776-798):
    - exit 1 **and** exit 3 with verify-shaped stdout under the exact `RPM_VERIFY_ARGS` →
      `Ok(stdout)`;
    - exit 1 with verify-shaped stdout under any other `rpm` argv → `Err`;
    - exit 1 under `RPM_VERIFY_ARGS` with only `error: rpmdb open failed` on stderr and empty
      stdout → `Err`;
    - killed by signal → `Err`;
    - `rpm -qf --queryformat ... <p>` not-owned → `Ok("")`/`None`.
  - Accept: `cargo test -p scanner-privileged command` green; watchdog/containment tests
    unchanged.
  - Mutation:
    - delete the allowance → the first test fails;
    - widen it to any argv → the second fails;
    - drop the "≥1 parseable line" condition → the rpmdb-failure test fails;
    - accept only code 1 → the exit-3 case fails.
- [ ] **3. `reconcile` scoped by completed check types.**
  - RED in `store.rs` tests and `tests/end_to_end.rs`:
    - scan 1 records a `file_drift` and a `rootkit_signature` anomaly; scan 2 has rpm unavailable
      and chkrootkit absent (both skipped) → both remain `still`-open, **zero** resolved;
    - scan 3 with both checks completing and clean → both resolved.
  - Accept: the existing `full_scan_diffs_new_still_and_resolved_across_runs` passes unchanged.
  - Mutation: pass every `CheckType` as completed → the skipped-check test fails.
- [ ] **4. `SystemFacts` + `run_file_drift` + classification mapping.**
  - RED unit tests via `SyntheticFacts`, one per tree outcome:
    - rule 1 `/var/log/x` failing → no finding;
    - rule 3 layered clean → no finding;
    - rule 3 layered `..5......` → anomalous, detail names `layered_pkg:`;
    - base package failing → anomalous `base_pkg:`;
    - rule 4 `/etc/foo` `c` → expected, no finding;
    - rule 5 unowned under `/usr` (via `base_tree_drift`) → anomalous `unowned`;
    - `MAX_DRIFT_PATHS + 1` failures → capped list + one aggregate finding;
    - every `file_drift` finding path starts with `/` (no collision with `kargs:`/`module:`
      keys);
    - control characters in a path are escaped in `detail`/`path` (see Security).
  - GREEN: implement. Update `FakeFacts` and `SyntheticFacts`.
  - Accept: `cargo test -p scanner-privileged`.
  - Mutation, one per guard:
    - drop the rule-1 check → `/var/log` test fails;
    - map `Expected` → `Anomalous` → `/etc` test fails;
    - remove the cap → cap test fails;
    - remove escaping → control-char test fails.
- [ ] **5. Wire into `scan.rs` + end-to-end.**
  - Replace the skip and update the `scan.rs:189-191` assertion. Mind the `"file_drift_base_tree"`
    skip name; it is not `lifecycle`.
  - RED end-to-end in `tests/end_to_end.rs` (real on-disk `scans.db` under a tempdir):
    - tampered layered file → `new`; same on rerun → `still`; restored → `resolved`;
    - `rpm_verify_all` returning `Err(Io)` → scan still completes, kargs/module findings still
      reported, `file_drift` in `skipped` with the reason;
    - **live-shaped case:** `base_tree_drift` returns `PrimitiveUnavailable` (as the live
      implementation will until OQ1) → tamper → `new`; restore → `resolved` still happens (interim
      rule); `file_drift_base_tree` present in `skipped`. Add a broker deserialization test whose
      fixture contains `"file_drift_base_tree"` (the broker types `check` as `String`,
      `scanner-broker/src/lib.rs:53`, and `ScannerPage.qml:170` maps skipped entries generically,
      so no enum/label change is expected; the test pins that).
  - Accept:
    - `just test` and `just check` green (both `--workspace`; Kirigami fixtures untouched);
    - `cargo test -p scanner-broker` green, confirming its typed wire schema accepts real
      `file_drift` findings;
    - `cargo test -p snitchwatch-bridge` green (the dormant-identifier scan, fact 12).
  - Mutation: restore the hardcoded skip → the e2e `new` assertion fails.
- [ ] **6. Real-host checklist (manual, not blocking merge, same boundary as the Phase 6 plan).**
  Append a "file_drift" section to the existing scanner runbook entry (`docs/RUNBOOK.md`). Items:
  - run on a Bazzite host via the broker: `busctl --user call org.snitchwatch.Scanner ...
    RunDeepScan`, as in HANDOFF.md:80;
  - record `rpm -Va --nomtime --noscripts --nodeps` wall time, output size and exit code, with
    drift in 0, 1 and 2+ packages (pins the exit-code semantics Task 2 assumes);
  - record the real format of `rpm-ostree status --json` `packages`/`requested-packages` entries:
    plain names or NEVRAs (PR 3's NEVRA fix depends on it);
  - confirm no flood of `T`/`M`/`missing` lines on an untouched image;
  - layer a package, edit one of its files, confirm one anomaly.

### Security considerations (privileged tier)

- **No persistent privileged process.** Nothing here adds a unit, timer, socket or listener. The
  check runs inside the existing on-demand, polkit/pkexec-gated invocation and its watchdog. PR 2's
  guard enforces this mechanically.
- **The privileged process never opens or hashes target files itself.** All file inspection is
  delegated through `command::run`: `TRUSTED_DIRS` (`/usr/bin`, `/usr/sbin`) program resolution,
  fixed argv with no shell, `PATH=/usr/bin:/usr/sbin`, bounded stdout/stderr, deadline,
  watchdog-swept process group. If Open Question 1 resolves to an in-process walk, that walk must
  use no-follow lookups (`O_NOFOLLOW`/`openat`-relative), byte caps and the same deadline, and must
  get a security-reviewer pass.
- **`--noscripts` is mandatory:** `%verifyscript` from a layered (possibly malicious) package
  would otherwise run as root. A unit test asserts `RPM_VERIFY_ARGS` contains it.
- **Untrusted paths/contents.** Every path in `rpm -V` output is attacker-influenced (a file
  name an attacker chose). Concretely:
  - parse without fail-open (Task 1);
  - pass paths to `rpm -qf` as a single argv element. A leading `-` is impossible (the path must
    start with `/`); assert it anyway;
  - SQL is already parameterized (`store.rs` `params![]`);
  - escape ASCII control characters, including `\n`, `\r` and ESC, when rendering into the text
    report/JSON `detail`, so terminal output can't be spoofed;
  - the GUI already uses `PlainText` (fact 10).
- **Output size:** the 1 MiB cap and `MAX_DRIFT_PATHS` bound memory and spawn count. Hitting
  either is surfaced, never treated as clean.
- **Fail closed on reporting, fail soft on scheduling.** A failed primitive is a visible `skipped`
  entry and never resolves prior findings (Task 3). It does not abort the later checks.
- Run `security-reviewer` on PR 1 (`command.rs` and new privileged parsing are security triggers
  per the repo's code-review rules).

---

## PR 2: guard against persistent scanner services (Gap 2)

`packaging/scanner/test-staging.py` checks staged bytes, modes, the polkit `exec.path` and the D-Bus
`Exec=` line. It does **not** assert the "no persistent process" invariant. The only `.service` in
the tree, `packaging/scanner/org.snitchwatch.Scanner.service`, is a **D-Bus activation file**
(`[D-BUS Service]`, `Name=`, `Exec=`, no `Type=`). A naive "every `.service` has `Type=dbus`" rule
would fail on day one.

- [ ] New `crates/scanner-privileged/tests/no_persistent_service.rs` (default-member, so CI `test`
  runs it). Core is `fn audit(root: &Path) -> Vec<Violation>` over:
  - `packaging/scanner/`, `packaging/polkit/` and any `packaging/bluebuild/**` file whose name
    contains `scanner`:
    - a `*.timer` or `*.socket` → violation;
    - a `*.service` whose first section is `[Service]`/`[Unit]` (systemd unit) without
      `Type=dbus` → violation;
    - a `[D-BUS Service]` file with a `SystemdService=` line → that unit must exist in the tree
      and pass the `Type=dbus` rule, else violation;
  - `crates/scanner-privileged/src/**` and `crates/scanner-broker/src/**` (non-test code):
    `TcpListener`, `UnixListener`, `UdpSocket`, `std::os::unix::net::UnixDatagram::bind`,
    `.bind(` on a socket, `ListenStream`/`ListenDatagram` → violation. The broker's session-bus
    *name ownership* (via its D-Bus library) is the allowed case. Read the broker and pin the
    exact API it uses in the test's allowlist comment.
- [ ] Tests:
  - `repo_tree_has_no_persistent_service` on the real repo root;
  - mutation tests copy the audited files into a `tempfile::tempdir()` and add, in turn:
    - a `.timer`;
    - a `[Service]` unit with `Type=simple`;
    - a `[D-BUS Service]` with a dangling `SystemdService=`;
    - a `.rs` with `TcpListener::bind`.

    Each must yield exactly one violation. Never mutate the real tree.
- [ ] Accept: `cargo test -p scanner-privileged --test no_persistent_service` green;
  `just package-check` unchanged and green.

---

## PR 3: userspace integration tests against stored prior state (Gap 4)

The existing `scan_lifecycle_new_outstanding_resolved` (`userspace_scan.rs:26-90`) already runs
four scans against one open `ScanStore`. Don't duplicate it. Add the seams it misses:

- [ ] `prior_state_survives_store_reopen`: run 1 is clean. Run 2 flags an evil autostart entry.
  **Drop and reopen** `ScanStore::open` on the same path (simulating a separate process). Run 3 →
  `still_outstanding == 1`, `new` empty. Run 4 after another reopen → resolved.
- [ ] `deployment_change_keeps_open_findings`: a finding opened under checksum `A`, then a scan
  under checksum `B` with the finding still present → still outstanding (spec §4 diffs by `path`,
  not deployment). Re-read §4 L163-231 first; if it implies otherwise, stop and record it as an open
  question rather than asserting either way.
- [ ] **Expected-drift classification** is not reachable through `Scanner::run` (fact 3), and
  `provenance.rs` already unit-tests each tree outcome. Add one integration test only for the
  cross-module seam: real `parse_status_json` (NEVRA package strings) →
  `has_layered_package` → `provenance::classify_path` with `MockInspector`. Cases:
  - layered clean → `Expected`;
  - layered tampered → `Anomalous`;
  - `/etc` → `Expected`;
  - **base package `python3` with only `python3-foo-1.0-1.fc40` layered** →
    `has_layered_package("python3")` must be `false`. `deployment.rs:109-113`'s prefix match makes
    this `true` today. If RED, fix NEVRA matching (strip `-version-release`) in a separate commit
    with its own unit test. The fix **must keep exact-name matches** and be tested against both
    forms (`"opensnitch"` and `"opensnitch-1.6.0-1.fc40"`). Only the fixture shows NEVRAs; the real
    `rpm-ostree status --json` format is unverified here (PR 1 Task 6 records it). The verdict here is still `Expected`/`Anomalous`-correct, so this fixes
    misattribution (which PR 1's `detail` text depends on), not a false negative.
- [ ] Accept: `cargo test -p scanner-core` green.
  - Mutation: truncate the db file before the reopen → the `still` assertion fails, proving the
    test reads prior state from disk.
  - Mutation: revert the NEVRA fix → the seam test fails.

---

## PR 4: plan/status doc corrections (Gaps 5, 6), docs only

- [ ] **Gap 5, `docs/superpowers/plans/2026-07-04-kirigami-shell-rewrite.md`.** Add one line
  under the H1:
  `> **Phase 3a GO (2026-07-04):** all four spike tasks passed (crates/kirigami-spike, 7fb46a5a);
  go/no-go per docs/superpowers/specs/2026-07-04-cxx-qt-feasibility-research.md.`

  Tick Part A Task 1-4 checkboxes (~L90-147, not only L90-122) as **annotated** ticks. Cite
  evidence:
  - Task 1/2: `crates/kirigami-spike/src/cxxqt_object.rs` (`qt_thread().queue`,
    `#[base = QAbstractListModel]`);
  - Task 3: commit `7fb46a5a`'s workspace build. Note that the plan's `snitchwatch-tauri`
    co-build is historical; the Tauri crate was retired in #379;
  - Task 4: `crates/kirigami-spike/tests/link_770.rs` passed, so #770 does not bite.

  Note once that the spike crate was named `kirigami-spike`, not `cxx-qt-spike`. Leave any box you
  cannot tie to evidence unticked, with a note.
- [ ] **Gap 6, `IMPLEMENTATION_PROMPT.md` Phase 2 (L120-236).** The DefaultAction "Superseded
  2026-10-25" note (L195-197) already exists, so don't duplicate it. Add one `> **Shipped design
  (2026-11-19):**` note above **Acceptance criteria** (~L210) stating:
  - `DefaultAction: allow` (settled decision #5);
  - the bridge ships as the **system bridge** only (#35); TCP `127.0.0.1:50051` is dev-only
    behind `SNITCHWATCH_DEV_TCP=1`;
  - there is no per-user `systemctl --user snitchwatch-bridge` unit;
  - the GUI is `snitchwatch-kirigami` (Tauri retired #379);
  - cite `docs/packaging/system-bridge-integration.md`.

  Annotate, don't rewrite, the three stale criteria (the TCP bind, the `systemctl --user` unit, the
  "Tauri GUI window") as superseded.
- [ ] Accept: `git diff --stat` touches only those two files; no code changes; links/paths named
  resolve (`ls` each).

---

## Non-goals

- The userspace `outbound` connections check and the Component A connection-log signal. Both
  are deliberately deferred by the owner.
- Building/writing `baseline.db` (pending Open Question 1), and any change to the polkit policy,
  broker D-Bus API, `scans.db` schema columns, or the Kirigami report page.
- Any test against a live `opensnitchd`, real Bazzite host, `sudo` or real `rpm`/`ostree`. All
  tests use `SyntheticFacts`/`FakeFacts`/`MockInspector`, cargo, and tempdirs only.
- Changing the rule-4 `/etc` allowlist breadth (spec rule 4 already defers `/etc` precision to
  `ostree admin config-diff`).

## Acceptance criteria (whole plan)

- [ ] PR 1: `file_drift` no longer appears as a hardcoded skip. Each Task 1-5 guard has a recorded
  RED run and a mutation that makes it fail. `just check` + `just test` green. `security-reviewer`
  shows no CRITICAL/HIGH.
- [ ] PR 2: the guard passes on the repo and catches each of its four mutations.
- [ ] PR 3: the new tests are green, with the disk-truncation mutation failing as expected.
- [ ] PR 4: docs-only diff limited to the two files.
- [ ] Manual (non-blocking): the Task 6 real-host checklist recorded by the owner.

## Open questions (stop-and-ask; not decided by this plan)

1. **Rule-2 base-tree primitive (blocks the live `base_tree_drift`).**
   - Spec §1 rule 2 asks for an OSTree object lookup, "not a hand-computed hash"; spec §2 describes
     a cached full-hash `baseline.db`. These pull in different directions, and composefs on Bazzite
     (F42+ atomic default) changes what is even checkable.
   - Candidates:
     - (a) `ostree diff <booted-commit> /usr`-style comparison (verify argv/semantics on a host);
     - (b) `ostree fsck` (repo object integrity, not deployed-tree content);
     - (c) composefs/fs-verity digest verification;
     - (d) the §2 cached hash baseline in `baseline.db` (an in-process walk, which needs extra
       hardening).
   - Related: baseline-spec stop-and-ask #2 (cache ownership split).
   - **Still open (2026-11-19):** `base_tree_drift` returns `PrimitiveUnavailable` and the check is reported partial (`file_drift_base_tree` under `skipped`); no ostree/composefs/baseline.db primitive is implemented.
   - **Answered 2026-11-21:** per-path `ostree ls -d -C` vs `ostree checksum` comparison; see `docs/superpowers/plans/2026-11-21-file-drift-base-tree-primitive.md`. `base_tree_drift` was replaced by `commit_entry`/`path_checksum`.
2. **Time budget.** Proposed `FILE_DRIFT_BUDGET = 180 s` out of the 540 s watchdog. Is that the
   right split, given chkrootkit runs last and is also slow? Needs the real-host timing from Task 6.
   - **Decided 2026-11-19 (owner: "defaults"):** keep `FILE_DRIFT_BUDGET = 180 s` as a named, provisional constant; revisit with the Task 6 real-host timing.
3. **`--nomtime` and `missing` noise on OSTree.** Is `--nomtime` sufficient, or do
   `--excludedocs`/`%ghost`/`/usr/etc` paths also flood `rpm -Va` on a stock Bazzite image? Only a
   real host answers this; the parser and cap make the failure visible, not silent.
   - **Still open (2026-11-19):** `--nomtime` kept as planned; needs a real host (Task 6 checklist in `docs/RUNBOOK.md`).
4. **Exit code on a skipped `file_drift`.** Should a failed/partial file-drift check make the
   scanner exit non-zero, and the broker/GUI show it as a degraded scan, or stay as a `skipped` entry
   with exit 0/2 as for chkrootkit today?
   - **Decided 2026-11-19 (owner: "defaults"):** a skipped or partial `file_drift` stays a `skipped` entry; no forced non-zero exit.
5. **Aggregate-over-cap representation.** Is a single `path = "/"` aggregate finding acceptable for
   more than `MAX_DRIFT_PATHS` failures, or should the scan instead refuse with "too much drift:
   inspect manually"?
   - **Decided 2026-11-19 (owner: "defaults"):** over `MAX_DRIFT_PATHS` (500) emit the single `path = "/"` aggregate finding and do not refuse the scan; its detail states the total count and that only the first 500 are listed.
6. **Resolution semantics.** Task 3 changes resolution so that only completed checks resolve.
   Whether resolved findings should need explicit acknowledgment remains baseline-spec stop-and-ask
   #3. Unchanged here, cited, not re-litigated.
   - **Still open (2026-11-19):** unchanged.
