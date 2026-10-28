# History run 1, 2026-10-28

Status: **PROPOSED, awaiting owner approval.** No code starts until the owner
approves this plan. Main was at `a05c1b4` when it was written. Parent plan:
[`2026-10-28-connection-history-implementation.md`](2026-10-28-connection-history-implementation.md)
(approved and merged). Design:
[`specs/2026-10-27-connection-history-design.md`](../specs/2026-10-27-connection-history-design.md).
This run does **PRs 1-4 of the implementation plan** and nothing past them. All
four are dormant bridge code behind the off-by-default opt-in: nothing in this
run records a connection, adds a WS message or changes packaging.

## How every item ships

Each item below is its own branch and goes through every step:

1. **Plan:** the parent implementation plan is the plan; a short note in the PR
   only if the item deviates.
2. **TDD:** tests first, seen to fail, then the code. Every invariant test
   gets a **mutation check**: break the guard locally, show the named test
   fail, restore it.
3. **Local gate:**
   - `cargo fmt --all` and `cargo clippy --workspace --all-targets -D warnings`
     (the repo's `just check`);
   - `cargo test` (default members) and `just test-bridge`;
   - `just package-check` only if packaging is touched (it should not be).
4. **Commit (conventional), push -u, PR.**
5. **Adversarial review with `/codex`:** fix every CRITICAL, HIGH and MEDIUM
   finding, then review again. LOW findings are fixed when cheap, otherwise
   recorded in the PR. PR 4 also gets a **security pass** (what is stored and
   who can read it).
6. **Merge when CI is green,** then send the tower the merged SHAs (one batched
   note). Dormant bridge code needs no tower action until the image is rebuilt.

### Reproduction rules (from `CLAUDE.md`)

- Bridge/protocol behaviour: `tests/mock_opensnitchd` and the
  `tests/bridge_protocol_test.rs` pattern. Never a live `opensnitchd`, a real
  Bazzite host, or `sudo`.
- Every test that starts a bridge sets `XDG_RUNTIME_DIR` to a tempdir **and**
  the state directory to a tempdir. `snitchwatch-bridge-cli` is never run bare.
- System-mode ownership checks use fact structs (the `DirFacts` style in
  `storage.rs`), not real `root` or `chown`.
- Crash tests use a child test process or a fault-injecting writer seam.
- **Every PR keeps the test "a bridge started with history disabled creates no
  `history/` directory (and no settings file)".**
- Test budgets are small (16 MiB) and tempdirs live under the worktree's own
  ignored target dir, not `/tmp`.

### Stop rules (when an item is parked, not forced)

- **Too many review rounds:** after 4 `/codex` rounds still finding HIGH or
  CRITICAL, park the PR as a draft with a summary. No merge.
- **Out-of-scope change:** if an item turns out to need a bridge **wire**,
  daemon, image or packaging change it wasn't planned for, stop and park it.
  (PRs 1-4 are planned as bridge-internal only.)
- **Unrelated CI failure:** if CI fails twice on the same unrelated test,
  record it in the report rather than retrying forever.
- **Never without the owner:**
  - staging, rebooting or editing host files;
  - changing tower refs;
  - posting upstream;
  - force-pushing, except a lease-guarded rebase;
  - deleting worktrees or `dist/`.
- **Resource limits:** at most 3 agents at once; scratch output kept small;
  check `df -h /tmp` before each build and stop and report above 85%. Nothing
  is deleted without the owner.

## Dependencies (checked against the implementation plan)

The implementation plan orders PR 1, 2, 3, 4 and says PR 3's durable parts
commit with the writer's transactions and PR 4's snapshots are bounded by the
page budget. Read literally:

| Item | Depends on | Why |
|------|-----------|-----|
| PR 1 storage, schema, caps | nothing | Creates the store and the S1 cap table |
| PR 2 writer, page budget | PR 1 | Needs the schema and caps; **produces the S2 result** |
| PR 3a in-memory primitives (generation counter, acceptance-sequence counter, pin registry, reader-gate skeleton) | PR 1 (module only) | Pure in-memory; no database |
| PR 3b durable parts (watermark committed with each batch, session ledger, bounded gap/session ledger) | **PR 2** | The watermark is advanced in the writer's transaction and the ledger lives inside the page budget |
| PR 4a pure rule identity (recursive allowlist, canonical encoding, hash incl. `Operator.sensitive`, `rule_class`, qualifier flags, redaction-equivalence tests) | PR 1 (caps) | Pure functions over `Rule`; no database |
| PR 4b persistence (`rules_seen`, snapshot caps, snapshot-only eviction, same-transaction cleanup) | **PR 2** | Writes through the writer and must fit S2's budget |

So: **PR 3 (durable half) and PR 4 (persistence half) depend on PR 2.** The
parent plan lists them as single PRs. **Contingency, for the owner to approve
with this plan:** if PR 2 is parked (see decision point 1), PR 3 and PR 4 are
delivered only as their independent halves (3a, 4a), each as its own PR, and
3b and 4b wait. If PR 2 lands cleanly, PRs 3 and 4 ship whole as in the parent
plan (no split).

## Order of work

### Batch 1: the store

1. **History PR 1: storage, schema, field caps, permissions.**
   - **Goal:** the dormant store: per-mode path resolution (system:
     `/var/lib/snitchwatch/history/`, `snitchwatch`-owned; user: the user state
     dir), strict exact-0700 directory and 0600 file validation, schema with a
     real `PRAGMA user_version` check, the bridge-owned settings/consent file,
     and the S1 field-cap table.
   - **Touches:** new `crates/snitchwatch-bridge/src/history/`
     (`mod.rs`, `schema.rs`, `caps.rs`, `open.rs`, settings);
     `bridge-cli/src/storage.rs` only to expose the state dir. Bridge only.
   - **Tests first:** exactly-0700 directory (0755, wrong owner, symlink,
     non-directory all make history `unavailable`); the database opens only via
     `sqlite_file::open_owner_only`, mode 0600, unsafe sidecars refused; newer
     `user_version` refused, older migrated; both bridge modes resolve to the
     right path via fact structs; the disabled-creates-nothing test; settings
     file: fresh install and upgrade default off, restart reloads, stale GUI
     preference ignored, survives recreation; atomic write at every field cap
     stays inside its reserved footprint and leaves no stray temp file;
     **S1** property test: a value at cap+1 never exceeds the cap on disk.
   - **Lands:** S1, D3, the pinned `page_size`, durable consent (design
     decisions 1, 2, 13).
   - **Proof obligation:** **S1.** The implementation must derive a cap for
     every stored string beyond the three the owner set (`rule_name` 256 B,
     `process_path` 4096 B, `dst_host` 253 B). **Any cap the owner has not set
     is proposed in the PR description, not merged as a guess.**
   - **Review focus:** permission checks (no inherited 0755 tolerance), symlink
     and sidecar handling, that `unavailable` never falls back to another path.
   - **Merge condition:** green CI; `/codex` clean of CRITICAL/HIGH/MEDIUM; the
     proposed caps are listed for the owner (merge may proceed with them marked
     provisional only if the owner pre-approves; otherwise the PR waits).

2. **History PR 2: writer and page budget.** *(depends on PR 1)*
   - **Goal:** one writer thread, bounded queue (bytes and count), batched
     transactions bounded by pages, incremental vacuum, no full `VACUUM`.
   - **Touches:** `history/writer.rs`, `history/budget.rs`. Bridge only.
   - **Tests first:** a full queue drops and counts without ever blocking a
     verdict (blocked writer plus a timed `AskRule` through `mock_opensnitchd`);
     worst-case rows (every field at cap) through insert, eviction and
     incremental vacuum never modify more pages per transaction than the
     budget; main file + peak journal + the settings file at maximum with its
     atomic replacement in flight + unallocated slack <= B under the same
     workload; freed space shrinks the file; `secure_delete` canary scan.
   - **Lands:** S2, S3, S4, S5 (clear-all recreation arrives in PR 10 of the
     parent plan), D5 insert path.
   - **Proof obligation:** **S2.** The implementation must prove the worst-case
     pages-per-row constant from the schema and caps (indexes, snapshot rows,
     orphan cleanup) and encode it as the transaction budget.
   - **Decision point 1 (owner):** see below. The PR records the result either
     way.
   - **Review focus:** the budget is enforced before commit, not measured after;
     the arithmetic is checked against the schema, not a fixture.
   - **Merge condition:** green CI, `/codex` clean, **and S2 proved**. If S2
     cannot be proved, the PR is parked as a draft (below).

### Batch 2: primitives and identity

3. **History PR 3: lifecycle primitives, watermark, ledger.**
   - **Goal:** generation counter, acceptance-sequence counter, pin registry,
     reader-gate skeleton (3a); durable coverage watermark, session ledger and
     bounded gap/session ledger (3b, *depends on PR 2*).
   - **Touches:** `history/pins.rs`, `history/ledger.rs`, `history/session.rs`.
     Bridge only.
   - **Tests first:** kill -9 with a queued event, a pending prompt and an
     unflushed drop counter, each followed by a heartbeat commit: the
     reconstructed gap begins no later than the pinned item's arrival; a pin is
     released in the same transaction as its durable event or gap row; clean
     stop gives an exact `bridge-down` gap, a crash an uncertain one; thousands
     of non-mergeable restarts stay inside the ledger's byte and row limits and
     collapse into one explicit `uncertain` span.
   - **Lands:** **W1, L1**, the D6 session ledger.
   - **Review focus:** pin/commit atomicity and `min(commit, oldest pin)`; the
     ledger can never lose a gap silently.
   - **Merge condition:** green CI, `/codex` clean. 3a alone may merge if PR 2
     is parked; 3b waits for PR 2.

4. **History PR 4: event-time rule identity.**
   - **Goal:** recursive-allowlist redaction, canonical encoding, rule hash
     including `Operator.sensitive`, `rule_class`, and (4b, *depends on PR 2*)
     `rules_seen`, snapshot bounds and snapshot cleanup.
   - **Touches:** `history/rule_identity.rs`, `history/snapshots.rs`; reads
     `rule_policy`, `rule_wire`. Bridge only.
   - **Tests first:** canaries in `process.command`, `process.env.*`, an unknown
     operand kind and a description never appear in the DB bytes (nested lists
     included); **redaction-equivalence:** varying only withheld values leaves
     redacted bytes and hashes identical; changing only `Operator.sensitive`
     changes the hash; list-backed rules carry the "contents not recorded"
     flag; snapshot caps (count, bytes each, total) with snapshot-only eviction;
     deleting one event below the cap removes its unreferenced snapshot in the
     same transaction; a canary in a *path* or *host* **does** appear
     (documenting the stated limit).
   - **Lands:** D7 identity and snapshot parts.
   - **Review focus (with a security pass):** the allowlist, that no digest of a
     withheld value is stored, the stated limit on raw paths and hosts.
   - **Merge condition:** green CI, `/codex` clean, security pass clean. 4a alone
     may merge if PR 2 is parked; 4b waits.

### Batch 3: small queued items (optional; run only after Batch 1-2 are done or parked)

5. **#240's two LOW findings.** Fix the two LOW review findings recorded on
   #240. I have not read them while writing this plan, so the exact text is
   taken from that PR's review notes at the start of the item; if either turns
   out to need more than a small docs or test change, it is recorded and left.
   Merge condition: green CI and a clean `/codex` pass.
6. **Endpoint-events README update.** **Only if the tower has sent the rsyslog
   image digest.** Without the digest the item is skipped and recorded, not
   guessed. Docs only; merge on green CI.

## Decision points

1. **PR 2 produces the S2 result.** If the per-row page constant **can** be
   proved, the run continues as written. If it **cannot**, the design's
   fallback (reserve a full-database rollback journal; main-file share at most
   half of the budget) is an **owner decision**, because it halves the usable
   cache. The run then:
   - records the attempted proof and the measured numbers in the PR;
   - parks PR 2 as a draft;
   - continues **only** with items that do not depend on it: PR 3a and PR 4a
     (as the independent halves above) and Batch 3.
2. **Any field cap the owner has not set** (S1) is proposed in the PR, not
   merged as a guess.
3. **The split contingency** (PR 3a/3b and 4a/4b) is approved with this plan or
   the run parks PR 3 and PR 4 whole if PR 2 parks.

## Out of scope and why

- **History PRs 5-15:** capture, prompt guard, gaps, retention, reader, control
  gate, wire, GUI, packaging. They follow in later runs after the owner sees
  the S2 result. PR 6 and PR 11 change the WS wire protocol and need a tower
  notice, so they get their own run.
- **Any WS message, GUI change, or packaging change:** none are needed for
  PRs 1-4; if one appears necessary, the item is parked.
- **Enabling history on a real host, or any staging, reboot or host-file
  edit:** needs the owner at the machine.
- **Exclusion lists, per-uid scoping, encryption, polkit, daemon changes:**
  deliberately out of v1 (design decisions).
- **Posting upstream, tower refs, the Vigie receiver:** the owner or other
  sessions own them.

## What the owner gets in the morning

- A list of merged PRs with their SHAs.
- The **S2 result**: proved constant, or the measured numbers and the parked
  draft with the full-journal fallback question.
- The proposed S1 field caps beyond the three already set, for confirmation.
- Parked items, each with its reason and `/codex` round count.
- Anything that needs a decision, and the tower note listing the merged SHAs.
