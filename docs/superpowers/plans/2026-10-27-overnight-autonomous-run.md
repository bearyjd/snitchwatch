# Overnight autonomous run, 2026-10-27

Status: **approved by the owner as written, 2026-10-27** ("approve plan as
written"). Main was at `2c27960` when it was written.

## How every item ships

Each item below is its own branch, and goes through every step:

1. **Plan:** a short plan doc for that item if it isn't trivial.
2. **TDD:** tests first, see them fail, then the code.
3. **Local gate:**
   - `cargo fmt --all` and clippy `-D warnings`;
   - headless kirigami tests;
   - `cargo test` (default members);
   - `just package-check` when packaging is touched.
4. **Commit, push, PR.**
5. **Adversarial review with `/codex`:** fix every CRITICAL, HIGH and MEDIUM finding, then review again. LOW findings are fixed when cheap, otherwise recorded in the PR.
6. **Merge when CI is green,** then send the tower the merged SHA (one batched note per batch).

### Stop rules (when an item is parked, not forced)

- **Too many review rounds:** after 4 `/codex` rounds still finding HIGH or CRITICAL, the PR is parked as a draft with a summary. No merge.
- **Out-of-scope change:** if an item turns out to need a bridge, wire, daemon or image change it wasn't planned for, stop and park it.
- **Repeated CI failures:** if CI fails twice on the same unrelated test, record it under item 6 rather than retrying forever.
- **Never without the owner:**
  - staging, rebooting or editing host files;
  - changing tower refs;
  - posting upstream;
  - force-pushing, except a lease-guarded rebase;
  - deleting worktrees or `dist/`.
- **Resource limits:** at most 3 agents at once, scratch output kept small, and `/tmp` checked before each build. If the quota fills again, pause and report. Nothing is deleted without the owner.

## Order of work

### Batch 1: finish what's open (already reviewed or nearly)

1. **#233, Vigie endpoint-events packaging:**
   - merge when CI on `2ca71e4` is green;
   - then send the Vigie session the PR link, `sample-event.json` and the TLS settings;
   - send the tower the rsyslog package list plus the opt-in units.
2. **#235, Q04 honest tray and health:** finish the `/codex` review, fix the findings, merge.
3. **#234, 0c exception-first view:** finish the 6 review fixes, re-review, then rebase onto main once #235 lands, because both touch the model files. Then merge.
4. **One GUI rebuild** from the merged main after 2 and 3: the reviewed wrapper with a new output name, install `--user` `r15.1-candidate`, a 20 s launch check, and the SHA-256 recorded in `.omc/STATE.md`.

### Batch 2: new GUI work that needs no host or owner action

5. **Deny-status display (option B).**
   - **Approval:** the owner chose "B now, A later" in the tower session; approving this plan confirms it here.
   - **Contract:** read `/run/snitchwatch/deny-status.json`, as merged in bazzite-tower #138.
   - **Display:**
     - state;
     - trial countdown from `expiresAt`;
     - "revert overdue";
     - "unknown" for a missing file, a stale `bootId` or an unparseable file;
     - "as of `updatedAt`";
     - the exact `ujust snitchwatch-deny on|confirm|off|status` commands to copy.
   - **Display only:** no action buttons, no privilege, no new Flatpak permission (`/run/snitchwatch:ro` is already granted).
   - **Tests:** fixture files only; the real helper is never run.
   - **Not checked by this run:** reading it from inside the Flatpak on a real boot, which comes after the next image ships #138.

6. **Flaky tests, hardened:**
   - **`connections_row_click_qml`:** this keeps flaking under load. Replace the fixed waits with readiness waits.
   - **bridge-cli `curated_groups`:** two cases failed once under load. Find the timing dependency and fix it.
   - **`analysis_export_controller::tests::stale_delivery_and_off_qt_save_keep_independent_lifetimes`:** it segfaulted once, before tonight's changes. Reproduce it by running it in a loop. If the cause is the test harness, fix the test. If it's product code, fix that with a regression test. If it can't be reproduced in about 200 runs, record that and leave it.

   Each test gets its own PR, and each change is proven by repeated runs under load.

7. **Q01, keyboard and screen-reader coverage (a first slice):**
   - Every action in the Connections context menu and the bulk actions can be reached and fired by keyboard alone.
   - Every interactive control on the Connections page has an `Accessible.name`.
   - Tests:
     - QML probes that drive focus and keys, using real key events, not source-string checks;
     - a guard that new interactive controls carry `Accessible.name`.
   - Not covered: other pages, and the actual screen-reader output with Orca (needs a desktop). Both are recorded as not covered.

### Batch 3: design only (docs PRs, reviewed and merged, no implementation)

8. **r17 history design (H01/H02, with H06 event-time rule identity):**
   - opt-in storage;
   - retention by age and disk size;
   - bounded growth;
   - gap and loss labelling;
   - clear and retention preview;
   - a threat model for storing connection history;
   - recording which rule matched at event time, for R05 "why was this allowed or blocked?".

   This is a docs PR with a `/codex` adversarial review, and a security pass focused on what gets stored and who can read it. The implementation waits for owner approval of the merged design.

9. **Handoff:** update `HANDOFF.md` and `.omc/STATE.md` with each merged SHA, the artifact SHA-256s, the parked items and their reasons. Docs PR, merged on green.

## Out of scope tonight, and why

- **Reboot into the staged image `5ff3751f`, the acceptance checks, closing #35:** need the owner at the machine.
- **Deep Scan polkit allow and deny on the real host:** needs the owner.
- **The Q04 gap "bridge sends a diagnostics report on daemon subscribe":** a bridge change; it needs its own plan and tower notice.
- **Option A, the polkit or D-Bus deny toggle:** the owner said later.
- **Replies to upstream #1699, #1700, #1701:** posting goes out under the owner's name.
- **The Vigie receiver:** owned by the Vigie session.
- **Q05/Q06 eBPF:** image and tower work.

## What the owner gets in the morning

- A list of merged PRs with their SHAs.
- The GUI bundle path and SHA-256, installed on `r15.1-candidate`.
- Parked items, each with its reason.
- Anything that needs a decision.
