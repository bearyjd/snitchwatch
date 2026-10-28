# Connection history: implementation plan (r17)

Status: **PROPOSED, awaiting owner approval.** No code is authorized by this
plan. Parent: the approved design
[`specs/2026-10-27-connection-history-design.md`](../specs/2026-10-27-connection-history-design.md)
(merged as `6587523`; owner decisions 1-14 are in its "Owner decisions"
section). This plan does not reopen those decisions. Main was at `6587523`
when it was written. Roadmap items: H01, H02, H06, serving R05.

Rules this plan inherits from the design, stated once:

- **Off by default.** Nothing creates a file, a thread or a socket message
  handler that records until the owner-visible enable action (PR 11-12). Every
  PR before that lands dormant code, and each carries a test that a bridge
  started without enabling history creates no `history/` directory.
- **Both bridge modes are first-class.** Per-user bridge and system bridge
  (`/var/lib/snitchwatch/history/`, `User=snitchwatch`, `snitchwatch-ui`
  socket group). Every PR that touches the writer, storage or control path
  tests `BridgeMode::System` as well as `BridgeMode::User`.
- **"Implementation must prove", never a guessed number.** S1 field caps beyond
  the three the owner set, the S2 per-row page constant, and src_ip/src_port
  correlation uniqueness are proof obligations assigned to named PRs below.

## How every PR ships

Same pipeline as the overnight run: plan note only if non-trivial, TDD (tests
first and seen to fail), `cargo fmt --all`, `cargo clippy -D warnings`,
`cargo test` (default members), headless kirigami tests when the GUI is
touched, `just package-check` when packaging is touched, PR, `/codex`
adversarial review (fix CRITICAL/HIGH/MEDIUM), merge on green CI, then notify
the tower. **Mutation check:** every invariant test is proven able to fail by
breaking the guard locally and showing the named test fail.

### Reproduction paths (from `CLAUDE.md`; no live daemon, no sudo)

- Bridge/protocol behaviour: `tests/mock_opensnitchd` (`MockOpensnitchd`) with
  the `tests/bridge_protocol_test.rs` pattern: boot a real bridge in-process,
  connect a real WebSocket client over its Unix socket with the handshake
  token, drive it with scripted daemon events.
- Storage tests: a tempdir state directory, built the way
  `crates/snitchwatch-bridge-cli/tests/curated_support` builds a
  `BridgeMode::System` setup (tempdir sockets and state, `Storage::Persistent`).
- **Isolation:** any test that starts a bridge sets `XDG_RUNTIME_DIR` to a
  tempdir and the state directory to a tempdir (the rule from `CLAUDE.md`;
  `snitchwatch-bridge-cli` is never run bare). System-mode ownership checks use
  the pure `DirFacts`-style fact structs (`storage.rs`), not real `root`/chown.
- Crash tests (kill -9 equivalents) use a child test process or a
  fault-injecting writer seam, never a real daemon.
- GUI: headless `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`
  probes, like the existing `tests/*_qml.rs`.

## Order and dependencies

```
PR1 storage+schema+caps -> PR2 writer+page budget -> PR3 pins/watermark/ledger
   -> PR4 rule identity -> PR5 daemon-event capture+admission
   -> PR6 prompt guard+correlation -> PR7 gap causes
   -> PR8 retention+eviction -> PR9 reader+limits
   -> PR10 control gate -> PR11 wire+capability
   -> PR12 GUI opt-in -> PR13 GUI view -> PR14 R05 "why" panel
   -> PR15 packaging, runbook, tower notice
```

**Integration tests are assigned to the first PR that contains both real
sides.** Shared lifecycle primitives (the generation counter, the
acceptance-sequence counter, the pin registry, and a reader-registry/admission
gate skeleton) land in PR 3, so earlier PRs test against real primitives
without the later control gate. The reader (PR 9) precedes the control gate
(PR 10) so the control gate can be tested against real active readers. The
end-to-end "clear / disable-and-re-enable while prompts are pending" test is
in PR 10, where the guard (PR 6) and the real control gate both exist.
PR 4 sits before capture because captured events need `rule_hash`. PR 7 follows
both capture paths because it reuses their loss accounting. PRs 12-14 need
PR 11. PR 15 can start after PR 11 and must land before the image that ships
the feature.

Notice to the tower: PR 6 (row shape) and PR 11 change the **WS wire protocol**, PR 15 changes
**packaging**, and any PR that changes the bridge binary needs a new bridge
build in the next image. PR 1-10 change the bridge (dormant) and need no
tower action until the image is rebuilt; the tower is told once per merged
batch.

---

## PR 1: storage, schema, field caps, permissions

- **Goal:** the dormant history store: path resolution per bridge mode, strict
  directory and file validation, schema, and the field-cap table.
- **Touches:** new `crates/snitchwatch-bridge/src/history/` (`mod.rs`,
  `schema.rs`, `caps.rs`, `open.rs`); `bridge-cli/src/storage.rs` only to
  expose the state directory. **Bridge only. No wire, packaging or image
  change.**
- **Tests first:**
  - the history directory must be exactly 0700 and owned by the bridge uid
    (0755, wrong owner, symlink, non-directory all make history
    `unavailable`);
  - the database opens only through `sqlite_file::open_owner_only`, file mode
    0600, unsafe `-journal/-wal/-shm` sidecars refused;
  - **schema version policy:** `require_known_schema` checks schema objects,
    not version numbers, so history adds a real version check (`PRAGMA
    user_version`): a newer version is refused (`unavailable: newer schema`),
    an older one is migrated explicitly; both tested;
  - **bridge-authoritative consent and settings:** the enabled flag, the
    consent record and the retention settings live in a small bridge-owned
    0600 settings file in the history directory (outside `history.db`, so they
    survive clear-all recreation), created only when the user enables history.
    Tests: fresh install and upgrade default to off; restart reloads enabled
    state, retention and consent; a stale GUI preference never turns recording
    on; clear-all recreation keeps the settings file;
  - system mode resolves to `/var/lib/snitchwatch/history/`, user mode to the
    user state dir, using fact structs;
  - nothing is created while history is disabled;
  - **S1:** property test that for every column a value at cap+1 is truncated
    or refused and never exceeds the cap on disk.
- **Lands:** S1 (field caps `rule_name` 256 B, `process_path` 4096 B,
  `dst_host` 253 B), D3, the pinned `page_size`.
- **Proof obligation discharged:** **S1.** The implementation must derive a cap
  for every other stored string (including `src_ip`, `rule_class`, snapshot
  parts) from the schema and show none can exceed it. Any cap the owner has
  not set is proposed in the PR for confirmation, not guessed.
- **Review focus:** permission checks (no inherited 0755 tolerance), symlink
  and sidecar handling, that `unavailable` never falls back to another path.

## PR 2: the writer and the page budget

- **Goal:** one writer thread, bounded queue (bytes and count), batched
  transactions with a hard page budget, incremental vacuum.
- **Touches:** `history/writer.rs`, `history/budget.rs`. Bridge only.
- **Tests first:** queue full drops and counts (never blocks a verdict, proven
  with a blocked writer and a timed `AskRule`); worst-case rows (all fields at
  cap) through insert, eviction and incremental vacuum never modify more pages
  per transaction than the budget; main file + peak journal + slack <= B under
  the same workload; freed space shrinks the file without a full `VACUUM`;
  `secure_delete` canary scan after deletes.
- **Lands:** S2, S3, S4, S5 (except clear-all recreation, PR 10), D5 insert path.
- **Proof obligation discharged:** **S2.** The implementation must prove the
  worst-case pages-per-row constant from the schema and caps (indexes,
  snapshot rows, orphan cleanup) and encode it as the transaction budget.
  **Decision point 1:** if it cannot be proved, switch to the design's
  fallback (reserve a full-database rollback journal; main-file share at most
  half of the budget) and ask the owner before PR 3.
- **Review focus:** that the budget is enforced before commit, not measured
  after; arithmetic reviewed against the schema, not against a test fixture.

## PR 3: pins, watermark, sessions, ledger

- **Goal:** the shared lifecycle primitives (generation counter,
  acceptance-sequence counter, pin registry, reader-registry/admission gate
  skeleton), the durable coverage watermark, the session ledger, and the
  bounded gap/session ledger.
- **Touches:** `history/pins.rs`, `history/ledger.rs`, `history/session.rs`.
  Bridge only.
- **Tests first:** kill -9 with (a) a queued event, (b) a pending prompt,
  (c) an unflushed drop counter, each followed by a heartbeat commit: the
  reconstructed gap begins no later than the pinned item's arrival (W1); a pin
  is released in the same transaction as its durable event or gap row; clean
  stop gives an exact `bridge-down` gap, crash gives an uncertain one; thousands
  of non-mergeable restarts stay within the ledger byte and row limits and
  collapse into one explicit `uncertain` span (L1).
- **Lands:** **W1, L1**, D6 session ledger.
- **Review focus:** the pin/commit atomicity and the `min(commit, oldest pin)`
  computation; that the ledger can never lose a gap silently.

## PR 4: event-time rule identity

- **Goal:** the recursive-allowlist redaction, canonical encoding, rule hash
  (including `Operator.sensitive`), `rule_class`, `rules_seen`, snapshot bounds
  and snapshot cleanup.
- **Touches:** `history/rule_identity.rs`, `history/snapshots.rs`; reads
  `rule_policy`, `rule_wire`. Bridge only.
- **Tests first:**
  - canaries in `process.command`, `process.env.*`, an unknown operand kind and
    a description never reach the DB bytes (nested lists included);
  - **redaction-equivalence:** vary only withheld values; redacted bytes and
    hashes are identical;
  - changing only `Operator.sensitive` changes the hash;
  - list-backed rules carry the "contents not recorded" qualifier flag;
  - snapshot caps (count, bytes each and total) with snapshot-only eviction;
    deleting one event below the cap removes its now-unreferenced snapshot in
    the same transaction.
- **Lands:** D7 identity and snapshot parts. **Review focus (security pass):**
  the allowlist, that no digest of a withheld value is stored, and the stated
  limit on raw paths/hosts.

## PR 5: daemon-event capture, admission, coalescing

- **Goal:** record matched-rule ping events and default-action events through
  the insert API; global and per-process admission; per-process **byte** share;
  duplicate compression into `repeat_count`.
- **Touches:** `history/capture.rs`, `history/admission.rs`; hook point in the
  bridge's ingestion path (`translator`, `cache`) that must not hold the
  verdict lock. Bridge only.
- **Tests first:** field allowlist test (a canary in `process_args` and
  `process_env` never appears; `uid` never appears; `src_ip`/`src_port` only
  where PR 6 sets them); varied-destination flood from one process cannot evict
  another application's history; a **mixed-size flood** (large records) cannot
  evict others' small records while over its byte share; bucket map capped at
  1,024 entries with idle expiry; drops flushed as one aggregated gap row per
  cause per minute; coalescing counts are exact; `nolog` and stock
  default-applied traffic produce the standing "not observable" labels.
- **Lands:** D4 allowlist, D5 admission, the per-process byte share, D1
  coverage table behaviour.
- **Verdict-latency test:** with the writer blocked and then with the queue
  full, `mock_opensnitchd` drives an `AskRule` through a real bridge and the
  prompt/verdict completes within its normal bound (repeated in PR 6 and PR 11).
- **Review focus:** that the hook adds no latency to verdicts; quota fairness.

## PR 6: prompt outcome guard and correlation

- **Goal:** the per-prompt outcome guard (`replied`, `timeout`, `error`,
  `gui-lost`, `cancelled`) with eligibility and generation captured at
  acceptance, and exact prompt-to-event correlation.
- **Touches:** `history/guard.rs`; `grpc_server.rs`, `grpc_server/replies.rs`,
  `grpc_server/answer_wait.rs` (hook sites); the **row/wire shape carries
  `src_ip` and `src_port`** (additive, omitted when absent). **This PR is a
  WS wire/row-shape change: the tower is notified when it merges.** Every
  consumer is updated in the same PR: Kirigami builds exhaustive
  `ConnectionRow` literals (for example `demo.rs`), and default-member CI
  excludes Kirigami, so the PR must also run `cargo build -p
  snitchwatch-kirigami` and the headless kirigami tests, plus `just check`
  (which passes `--workspace`).
- **Tests first (with `mock_opensnitchd`):** interactive allow, deny,
  `FilterPaused` and `NoAnswer` each record exactly one event; terminal
  `Status` errors and cancellation record one event; **the last authenticated
  GUI disconnecting while a prompt waits** (`answer_wait.rs`), including a race
  with an already-settled verdict: exactly one outcome, with the correct pin
  release and no orphan snapshot; **G1 at unit level:** the guard captures
  consent and generation at acceptance and discards a stale outcome when the
  generation primitive (PR 3) is bumped directly by the test (immutable
  acceptance generation, no re-stamp); the end-to-end clear and
  disable-then-re-enable test waits for PR 10; **old/new payload fixtures:**
  a row without the new fields still parses, a row with them round-trips;
  the verdict-latency test of PR 5 repeated with the guard hooks in place;
  correlation: equal `src_ip`, `src_port`
  and the rest of the key merge; missing fields or an ambiguous match keep
  both events flagged `possible-duplicate`; **collision test:** same
  executable, same source port, protocol and destination, different source IPs
  are never merged.
- **Lands:** **G1** (unit level; end-to-end in PR 10), the D7 capture path, `answered_by` table (`gui` is labelled
  "answered in GUI (may be silent mode)"; no protocol provenance change).
- **Proof obligation discharged:** **correlation uniqueness.** The
  implementation must prove that `src_ip`, `src_port`, protocol, destination
  and process path identify one live socket inside the settlement window, or
  keep events flagged. (Storing `src_ip` was confirmed by the owner under
  design decision 11; it is not a gate here.)
- **Review focus:** the guard drop paths (every exit records an outcome), and
  that a pending guard pins the watermark.

## PR 7: gap causes and standing labels

- **Goal:** all gap causes of D6 (daemon-disconnected, upstream-loss from the
  existing `Δrule_hits − received` accounting, queue-overflow, admission-drop,
  write-failed, disabled, evicted markers, clock-jump) and the standing
  "not observable" labels (including the stock-vs-fork daemon capability).
- **Touches:** `history/gaps.rs`; reads `cache/rule_hits.rs` state. Bridge only.
- **Tests first:** each cause produced by a scripted scenario (daemon
  disconnect via the mock, counter reset, full queue, full disk via a fault
  seam, backwards clock); `upstream-loss` is a lower bound reusing the existing
  counters, never WS `Lagged`; standing labels present for `nolog`, stock
  default-applied traffic and no-GUI prompts.
- **Lands:** D6 causes and labels. **Review focus:** that no hole is shown as
  an empty interval.

## PR 8: retention and eviction

- **Goal:** age retention (default 30 days), the budget (64 MiB, ceiling
  256 MiB), eviction order (per-process byte share first, then oldest-first
  regardless of verdict), chunked eviction obeying S2, snapshot cleanup on
  every deletion path, `evicted` gap markers.
- **Touches:** `history/retention.rs`. Bridge only.
- **Tests first:** age and size limits exact; eviction stays inside the page
  budget; pause with `write-failed (cap)` after bounded failed rounds and
  backoff; deletion below the cap removes unreferenced snapshots; a hostile
  process cannot evict others beyond its byte share.
- **Lands:** D5 eviction, S4 in operation. **Review focus:** that eviction never
  deletes unrelated files and never grows past the budget.

## PR 9: the reader and its limits

- **Goal:** a read-only connection via a **new validated read-only opener**
  (`open_owner_only_readonly`: same checks as `open_owner_only`, no create, no
  chmod, `query_only=ON`), keyset paging, byte-bounded pages, the bridge-wide
  admission gate and response slots, cancellation, generation tagging, and
  the D8 deadline semantics: a **2 s per-query deadline that returns the
  partial result with a cursor** (not the cache export's 10 s deadline); a
  query is **cancelled when a replacement query arrives or the client
  disconnects**.
- **Touches:** `snitchwatch-bridge/src/sqlite_file.rs` (new function),
  `history/reader.rs`. Bridge only.
- **Tests first:** the opener refuses what the writer opener refuses and never
  creates or changes a file; 200-row and 256 KiB caps; at most 2 queries
  bridge-wide with `busy` refusals; a cancelled worker holds its permit until it
  exits (pattern of `bridge-cli/src/analysis_export.rs`); reads never block a
  verdict (timed with a long query running); deadline expiry returns a partial
  page plus a usable cursor; a replacement query and a disconnect each cancel
  the previous one; a **slow-consumer test** shows the response stays bounded
  (byte cap, bounded slot) when the client reads slowly. These are exercised
  again through the real WS path in PR 11.
- **Lands:** **D8**, the read side of C1. **Review focus:** permit lifetimes and
  lock ownership.

## PR 10: the control gate (clear, preview, disable, delete)

- **Goal:** the barrier for disable, clear, delete, range delete and
  retention change: validate-before-destruct, generation increment, reader
  quiesce, scope split, immutable guard generations, acceptance-boundary
  preview tokens, file recreation for clear-all.
- **Touches:** `history/control.rs`. Bridge only (the wire comes in PR 11).
- **Tests first (blocked writer + non-empty queue, each):** a **rejected stale
  preview** leaves queue, guards, pins and watermark untouched; **preview then
  accept another event** is refused on confirm and the event survives;
  retention change keeps what the new retention keeps and loses nothing
  silently; **disable keeps history** and accepted queued events and
  invalidates pending guards with durable gaps before pin release; a reader
  mid-query when clear arrives is closed before unlink and delivers no deleted
  row afterwards; stale pages after clear are dropped; clear refuses if newer
  rows exist (C2); clear recreates the file and leaves no journal growth.
  Added matrix, each asserting "a rejected operation leaves everything
  unchanged; a successful one deletes exactly its scope":
  - **preview expiry** (a token older than 60 s is refused);
  - **retention changing the previewed set** between preview and confirm;
  - **coalescing raising `repeat_count` without a new max id** (checksum
    catches it);
  - **S5 canary scan** of the database and sidecars after clear;
  - **same-transaction orphan-snapshot cleanup** for range delete and limit
    reduction;
  - **real active readers** (PR 9 exists) during clear, disable and retention
    change;
  - **end-to-end G1:** clear, and disable then re-enable, while prompts are
    pending (guards from PR 6): after they settle nothing from before is
    written except the gaps committed at disable;
  - settings and consent survive clear-all recreation (PR 1 file).
- **Lands:** **C1, C2**, H02, owner decisions 10 and 12 (keep-on-disable;
  stale preview refused).
- **Review focus:** the ordering of the barrier steps and the "no pin released
  by discard alone" rule. This is the highest-risk PR; it gets two `/codex`
  passes (design and security) as the history design did.

## PR 11: WS messages and capability

- **Goal:** request/response messages for status, query (paged), preview,
  enable/disable, clear/delete, retention settings; capability flag
  `historyV1`; older-bridge behaviour is "history unavailable", never empty.
- **Touches:** `ws_messages*`, `bridge_capabilities.rs`, `ws_server`,
  `bridge-cli` wiring; protocol tests in `tests/bridge_protocol_test/`.
  **WS wire protocol change: tell the tower.**
- **Tests first:** protocol tests through a real WebSocket client with the
  token; unknown fields rejected; ids validated like the export design;
  authorisation: in **system mode, only a client on the GUI socket**
  (`snitchwatch-ui`, per decision 14) can enable, disable, clear and read;
  per-user mode behaves as today; no history control is reachable
  unauthenticated; stale-generation responses refused; restart reloads the
  bridge-authoritative enabled/retention/consent state;
  **non-blocking routing (verdict latency):** the bridge consumes inbound WS
  messages in a sequential pump (`bridge-cli/src/lib.rs`), so history messages
  must be handed to a separate bounded task and never awaited there. Test with
  `mock_opensnitchd` and a real WS client: block the history work (a long
  query, a held clear barrier, a blocked writer, a full queue), submit a
  verdict, and prove the `AskRule` completes **before** the history work is
  released; the D8 deadline, cancellation and slow-consumer cases of PR 9 are
  re-run through the WS path.
- **Lands:** the externally visible surface of C1/C2/D8 and decisions 1 and 14.
  **Review focus (security pass):** the authorisation boundary and message
  validation.

## PR 12: GUI opt-in and health

- **Goal:** Settings opt-in with the required text (stored fields, raw paths
  and hosts are stored, the bridge-reported path, who can read it, what is not
  observable, **unencrypted, protected by file permissions**, backup-exclude
  recipe for the reported path), enable/disable (keep by default) and a
  separate confirmed "Delete history"; storage health banner.
- **Touches:** `crates/snitchwatch-kirigami` (a controller and QML page; stay
  inside the file budgets and register new QML per the repo conventions).
  GUI only.
- **Tests first:** headless probes: nothing is recorded until the explicit
  confirmation; disable keeps; delete needs a preview; unavailable and
  older-bridge states render honestly; the reported path (not a computed one)
  is shown for both bridge modes.
- **Decision point 3:** the opt-in wording is shown to the owner before
  merge, since it is the consent record. **Review focus:** that the text is
  true and complete.

## PR 13: GUI history view

- **Goal:** paged history list with gaps and standing labels inline, `answered
  in GUI (may be silent mode)`, plain-text rendering of untrusted strings,
  bounded window of pages, generation-aware page drop.
- **Touches:** `crates/snitchwatch-kirigami` only.
- **Tests first:** every gap cause label and each standing "not observable"
  note renders; an interval is never shown empty without a gap or an explicit
  continuous-recording statement; the page window stays bounded under a
  long scroll; a stale generation drops pages; strings are `Text.PlainText`.
- **Review focus:** honesty of labels and memory bounds.

## PR 14: R05 "why" panel

- **Goal:** the event-time "why was this allowed or blocked?": recorded rule
  name, hash comparison with the current rule ("definition unchanged",
  "changed since", "deleted"), the recorded snapshot beside the current rule,
  and the mandatory qualifiers (list contents not recorded; some conditions
  not recorded).
- **Touches:** `crates/snitchwatch-kirigami` (inspector extension). The
  existing live matched-rule display and "Show rule" navigation are preserved
  and are not re-sold as new.
- **Tests first:** edit and delete the rule after the event (with the
  `mock_opensnitchd` scenario): the stored explanation is unchanged and the
  panel says what changed; qualifiers present for list-backed and
  partly-withheld rules.
- **Review focus:** that nothing claims "unchanged" without its qualifier.

## PR 15: packaging, runbook, tower notice

- **Goal:** confirm the system unit already provides the writable state
  (`ProtectSystem=strict` with `StateDirectory=snitchwatch`, mode 0700; no new
  `ReadWritePaths` expected, to be verified, not assumed); document the
  history paths, the backup-exclude recipe, the multi-user behaviour
  (decision 1) and the `snitchwatch-ui` boundary (decision 14); add a manual
  verification runbook entry (opt-in on a real host is a **tower** step, not
  this repo's).
- **Touches:** `docs/packaging/`, `packaging/system/` only if the check shows a
  change is needed (then **packaging and image change: tell the tower**),
  `just package-check`.
- **Tests first (the system-unit contract gate):** `package-check` assertions
  that the unit keeps `ProtectSystem=strict`, `StateDirectory=snitchwatch`,
  `StateDirectoryMode=0700`, `ReadWritePaths=/run/snitchwatch-auth` and no
  other writable path, and that the socket stays `SocketMode=0660` with
  `SocketGroup=snitchwatch-ui`.
- **Tower acceptance step (precise):** on a real system-bridge host, with
  hardening unchanged, enable history, write events, read them back through the
  GUI, restart the bridge, and read again; the files appear under
  `/var/lib/snitchwatch/history/` owned by `snitchwatch` (0700 directory,
  0600 files) via `StateDirectory`, and the unit still has
  `ReadWritePaths=/run/snitchwatch-auth` only.
- **Removal and uninstall (documented):** removing the Flatpak GUI does **not**
  stop recording: the host bridge keeps writing history while enabled, and the
  history persists on disk until deleted. The supported procedure is: disable
  history, then perform the confirmed "Delete history" action, then remove the
  GUI; if the GUI is already gone, stop the bridge and remove only the
  `history/` subdirectory (`/var/lib/snitchwatch/history/` or the per-user
  `…/snitchwatch/history/`). **Never delete the whole shared state directory**:
  it also holds blocklists, profiles and curated state.
- **Review focus:** that no unit hardening is loosened.

---

## Risks

- **Page-budget arithmetic (PR 2).** Reviews found holes here repeatedly in the
  design. Mitigation: the invariant is "implementation must prove", with the
  full-journal fallback; decision point 1.
- **Capture hooks on the verdict path (PR 5, 6).** Any added latency or lock
  ownership is a regression. Mitigation: timed tests with a blocked writer.
- **Control-gate complexity (PR 10).** Most review findings so far were in this
  area. Mitigation: its own two-pass review and mutation checks for each step.
- **Wire compatibility (PR 6, 11).** Older GUIs and bridges. Mitigation:
  additive fields, a capability flag, "unavailable" rather than empty.
- **Multi-user exposure on the system bridge.** Every `snitchwatch-ui` member
  reads, enables, disables and clears all users' history (decisions 1, 14).
  Mitigation: stated in the opt-in text and the packaging docs, not hidden.
- **Raw paths and hosts may embed secrets (decision 9).** Stated limit; tests
  document it so it cannot change silently.
- **Resource use of tests.** Flood and crash tests are heavy; they use small
  budgets (16 MiB) and tempdirs on the worktree's own target dir, not `/tmp`.

## Deliberately out of v1

Exclusion lists and basename-plus-hash paths (v2); per-uid row tagging and
uid-scoped reads; at-rest encryption; polkit for control; any daemon change
(exact loss counts, `nolog` visibility, no-GUI prompt capture); provenance on
`SetVerdict`; blocklist content revisions; export of history (the cache export
stays cache-only); H03 filters, H05 baselines, H08 digests, Q07 tuning.

## Decision points where the owner is asked again

1. After PR 2: the S2 result. Proven constant, or switch to the full-journal
   fallback (changes the usable cache size).
2. Before PR 12 merges: the exact opt-in text, because it is the consent
   record.
3. Any S1 field cap beyond the three the owner set.
4. If any PR turns out to need a daemon, image or unplanned wire change beyond
   those listed: stop and park, per the repo's stop rules.

## Approval requested

Approve the order and the PR breakdown, or name changes. Implementation of PR 1
starts only after approval; nothing here has been built.
