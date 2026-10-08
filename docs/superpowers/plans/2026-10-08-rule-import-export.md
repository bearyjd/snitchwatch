# Rule import / export with a dry-run diff (roadmap P2.7)

**Date:** 2026-10-08
**Roadmap:** P2.7 in `docs/superpowers/specs/2026-10-07-competitive-feature-roadmap.md`
**Baseline:** `main` @ `4b3ba52` (#48 merged: `RulesCache`,
`DaemonCommands`, `rule_wire.rs`).
**Blocked on:** the security PR (branch `fix/rule-operator-validation`),
which creates `rule_policy.rs` with `validate_operator`. This plan
**reuses** it and adds only an import profile layer on top.
**Size:** S–M. One bridge PR (policy, document, protocol, apply) and one
Kirigami PR (file dialogs, preview sheet).

## Citation convention

- `main:` means `4b3ba52`.
- #48 is merged, so its names are cited as `main:`.
  `rule_to_wire`/`rule_from_wire` live in `rule_wire.rs`.
- `vendor:` means opensnitch v1.8.0.
- `tower:` means bazzite-tower PR #81. Its daemon accepts only
  `CHANGE_RULE`/`DELETE_RULE` from the UI and refuses `lists.*` operands,
  nested ones included, until #45 PR B.

## Goal

1. **Export** the daemon's current rules to a versioned JSON document. The
   GUI saves it through a file dialog.
2. **Import** a document:
   - validate it against a fixed schema plus the bridge's rule policy;
   - show a **dry-run diff** against the current rules (add / replace /
     unchanged / refused);
   - apply only the rows the user keeps;
   - report each rule's daemon outcome.
3. The daemon only ever sees `CHANGE_RULE`, one rule per notification.
   Import never sends `DELETE_RULE`.

## Decided here (was owner questions X1–X4)

- **X1. Export contents:** `always` + `until restart` user rules only,
  with counts of what was left out. Bridge-owned rules are left out (the
  `z00-blocklist:`, `900-blocklist:` and `snitchwatch-default-` prefixes),
  and so are temporaries.
  - Blocklist and curated rules come back from their own sources.
  - Temporaries would expire on a different schedule after import.
- **X2. A same-name rule in the file:** replace, ticked by default, except
  loosening ones (see step 5's default ticks). This mirrors the daemon's
  `Replace` semantics and keeps loosening changes opt-in.
- **X3. No "replace all" in v1.** Deleting a deny can unblock traffic for
  every app (#44 Part B's reason for no bulk delete).
- **X4. opensnitchd on-disk rule files: v1.1.** Same policy, with one rule
  per file wrapped into a v1 document.

## Out of scope

- **"Replace all" import,** which would delete rules missing from the
  file (X3).
- **Blocklist subscriptions and profiles.** Their stores only persist
  after #45 PR A and #46 Part 1. The v1 envelope rejects unknown keys, so
  v2 can add them cleanly.
- **Automatic or scheduled backups,** and the CLI (P5.1). `rule_io.rs` is
  written so the CLI can reuse it.
- **Importing opensnitchd's on-disk rule files,** which is what the stock
  UI exports (X4: v1.1).
- **Hash and `lists.*` operands** (see Design step 1).

## Findings

**The rules cache (`main:cache/rules.rs`)**
- `cache/rules.rs` `RulesCache` is `Unknown | Synced(BTreeMap<String, Rule>)`.
  - `snapshot_wire()` returns the rules name-sorted, through
    `rule_to_wire`.
  - `RulesSync::withdraw` sets it back to `Unknown` when the committing
    stream goes away.
- **Caps:** `MAX_SNAPSHOT_RULES = 10_000`, `MAX_RULE_FIELD_BYTES = 16 KiB`,
  and the private `MAX_OPERATOR_LIST_LEN = 64` / `MAX_OPERATOR_DEPTH = 4`
  checked by the private `within_limits`.
  - A daemon rule over these limits is *left out* of a staged snapshot.
  - So an import that exceeds them would install a rule that vanishes from
    the Rules page at the next reconnect. **Import must use the same
    caps.**
- `RulesSync::synced()` bumps only on snapshot commits. There is no
  counter for "anything changed", and the stale-preview check (Design
  step 3) needs one.
  - It must live **in `RulesCache` itself**, not `RulesSync`:
    `prune_expired_rules_every` locks the cache directly and calls
    `RulesCache::prune_expired`, bypassing `RulesSync`.

**Rule shape and validation (`main:rule_wire.rs`)**
- `rule_from_wire` checks:
  - the name with `rule_name::validate_rule_name`;
  - that `action`/`duration` are non-empty;
  - that `operator` is present and non-null, and each leaf operand is
    non-empty.

  It does **not** check the action or duration vocabulary, the operand
  vocabulary, regexps or CIDRs.
  - The security PR adds `rule_policy::validate_operator` to it for
    GUI-sourced rules: pairing, `list` shape and nesting, and `lists.*`
    refusal.
- `rule_to_wire` itself emits the display-only fields `displayName` and
  `readOnlyReason` (from `validate_rule_name`). Those must not go into an
  export.
- **List operand spelling differs by source.**
  - Rules from the daemon carry `operand: "list"` on a `list` operator,
    because `Compile` overwrites it.
  - Rules parsed from the wire by `operator_from_wire` carry `""`.
  - Compare only after normalising, or every app-bound rule previews as
    `Replace`.

**Daemon commands (`main:daemon_commands.rs`)**
- `DaemonCommands::send` allows only `{ChangeRule, DeleteRule}`
  (`ALLOWED_ACTIONS`) and validates rule names at the send point.
  `SendError` has four variants:
  - `NotAllowed`;
  - `InvalidRuleName`;
  - `NoDaemon`;
  - `NotQueued`: no stream could queue the command.
- Each stream queue holds `STREAM_QUEUE_CAPACITY = 64` and is fed with
  `try_send`. **A full queue drops the command for that stream.** A bulk
  apply must pace itself.
- `PendingReply::wait` resolves to `Ok`, `Rejected(text)`, `Timeout` or
  `StreamClosed`.
- An `OK` updates the cache in reply order (`RulesSync::apply_confirmed`)
  and republishes `SetRules`.

**`vendor:` daemon behaviour**
- **Only the last error is reported.** `daemon/ui/notifications.go`
  `handleActionChangeRule` loops over `ntf.Rules` and **overwrites
  `rErr`**. With several rules per notification, the outcome of all but
  the last is lost. Hence one rule per notification.
- **Persistence.** `Replace(r, r.Duration == Always)` writes only `always`
  rules to disk. `until restart` lives in memory; a temporary duration
  starts its timer at `Replace`.
- **Same name means overwrite.** `Replace` replaces by name with no
  `setUniqueName`, so a same-name import is a **replace**.
- **Compile errors come back as `ERROR`.** `replaceUserRule` compiles the
  operator: a bad regexp, a bad CIDR, or a `user.name` the daemon host
  doesn't know.
- **Hash operands match everything when checksums are off.**
  `operator.go` `Match` returns `true` for `process.hash.*` when
  `hasChecksums` is false, which is the default (`EnableChecksums: false`
  in both the vendor and the packaging configs). `hashCmp` also fakes a
  match on an empty hash. An imported "this binary only" condition is
  silently "any binary".
- **Network aliases.** The `network` type accepts a CIDR or an alias from
  `/etc/opensnitchd/network_aliases.json` (vendored copy:
  `daemon/data/network_aliases.json`, keys `LAN` and `MULTICAST`).

**Who can touch the file**
- The system bridge runs as `snitchwatch` and can't read a user's home.
- Kirigami (a Flatpak) can, through the file-chooser portal.
- So **the GUI reads and writes files, and the bridge validates.** GUI-side
  checks are only for instant feedback.

**Protocol**
- `ws_messages.rs` `ClientMessage`/`ServerMessage` are internally tagged
  action enums. New variants are additive, and older clients ignore
  unknown actions.
- **Message size.** `ws_server.rs` `pump_authenticated` parses every text
  frame with `serde_json::from_str::<ClientMessage>` and sets no explicit
  size limit; the tokio-tungstenite defaults apply. An oversized document
  is therefore fully parsed into a `serde_json::Value` before any
  import-level check. The size cap must run **before** that parse.

## Design

1. **Policy: reuse `rule_policy.rs`, add an import profile.**
   - **Ownership.** The security PR creates `rule_policy.rs` with
     `validate_operator`, which checks:
     - **pairing:**
       - type `network` ⇔ operand `dest.network`/`source.network`;
       - operand `list` ⇔ type `list`;
       - `true` only as `simple`;
     - **`list` shape:** 1–64 members, no nested `list`;
     - **refusal** of `lists.*` at any depth.

     **This plan reuses it** and doesn't redefine any of those rules.
   - **Why pairing, not separate allowlists.** A wrong pairing can crash
     the root daemon:
     - `network` + `dest.ip`, or `simple` + `dest.network`, panic on a type
       assertion in the compare callback (`Match` dispatches on the
       operand). The result is a crash loop and fail-open through
       `QueueBypass`;
     - operand `list` with a non-`list` type, or an empty list, matches
       everything;
     - a nested list arrives empty (`Deserialize` copies one level) and so
       also matches everything.
   - **What P2.7 adds** (the profile layer):
     `validate_user_rule(rule: &Rule, profile: PolicyProfile) -> Result<(), Vec<RuleProblem>>`.
     It calls `validate_operator` first, then the profile checks.
     `RuleProblem { path: String /* e.g. "operator.list[1].operand", built from field names and indices only */, reason: &'static str }`.
     **Fixed reasons only:** never echo file text (the `rule_name.rs`
     pattern).
   - **`PolicyProfile::Import` checks:**
     - **Name:** `validate_rule_name`. Refuse the bridge-owned prefixes
       `z00-blocklist:`, the legacy `900-blocklist:`, and
       `snitchwatch-default-` (prompt-slot D).
     - **Action:** `allow`, `deny` or `reject`.
     - **Duration:** `always` or `until restart` (X1).
     - **Operand vocabulary:** `true`, `list`, `process.path`,
       `process.parent.path`, `process.command`, `process.id`,
       `process.env.<NAME>`, `user.id`, `user.name`, `source.ip`,
       `source.port`, `source.network`, `dest.ip`, `dest.host`,
       `dest.port`, `dest.network`, `protocol`, `iface.in`, `iface.out`.
       Refuse `process.hash.md5`/`sha1`: they match every binary while
       checksums are off, and even with checksums on, a process with no
       checksums matches (`operator.go` `Match`, hash branch). Checksum
       flows are P4.1.
     - **Type:** `simple`, `regexp`, `network` or `list`. `lists` and
       `complex` are refused. `regexp` is allowed only on string-subject
       operands, because a network operand passes `net.IP` to the
       `reCmp` string assertion.
     - **Regexps** compile with the `regex` crate. That only approximates
       Go RE2; the daemon stays authoritative and its `ERROR` is reported
       per rule.
     - **`network` data** is a CIDR, or one of the vendored alias keys. A
       test pins the alias list against
       `vendor:daemon/data/network_aliases.json`.
     - **Ports** are `0..=65535`. `process.id`/`user.id` are decimal.
     - **#48's caps.** Make `within_limits` and the two operator constants
       `pub(crate)` and call them, so import and the cache can never
       disagree.
2. **`rule_io.rs`** (new, bridge crate, pure): the document format.
   - **Version 1:**
     ```json
     {
       "format": "snitchwatch.rules",
       "version": 1,
       "exportedAtUnixMs": 1791400000000,
       "source": { "daemonVersion": "1.8.0" },
       "rules": [
         { "name": "…", "enabled": true, "action": "deny", "duration": "always",
           "description": "…", "precedence": false, "nolog": false,
           "operator": { "type": "list", "operand": "list", "operands": [ … ] } }
       ]
     }
     ```
     - Each rule is **exactly #48's wire shape** (`rule_to_wire`) without
       `displayName`/`readOnlyReason`. There is no second rule shape.
     - The envelope uses typed serde structs with `deny_unknown_fields`.
       A `version` other than 1 is refused with "This file was made by a
       newer Snitchwatch."
     - **Caps:** at most `MAX_SNAPSHOT_RULES` rules and at most 8 MiB of
       JSON.
       - The GUI checks the size before reading the file.
       - The bridge checks the byte length of the WS text frame in
         `ws_server.rs` `pump_authenticated`, **before**
         `from_str::<ClientMessage>`. It drops and logs any frame over
         8 MiB + 64 KiB envelope slack. It also sets
         `WebSocketUpgrade::max_message_size` to the same bound in
         `ws_handler`, so tungstenite refuses the frame while reading it.
       - The rule count is checked again after parsing.
     - **Duplicate names** inside one file: both copies are refused.
   - **Schema.** `docs/schemas/snitchwatch-rules-v1.schema.json` is
     published for outside tools. Fixtures under
     `tests/fixtures/rules-io/{valid,invalid}/` keep it and the Rust parser
     in step. This adds no new crate dependency; the Rust validator is
     authoritative.
   - **`export(cache: &RulesCache) -> Result<Document, ExportUnavailable>`:**
     - `Unknown` → `ExportUnavailable` ("Rules haven't loaded from the
       firewall yet").
     - **Left out:** `once` rules, temporary rules, rules the bridge owns
       (the `z00-blocklist:`/`900-blocklist:`/`snitchwatch-default-`
       prefixes), and rules whose names fail `validate_rule_name` (#48's
       read-only rows: they could never be imported back).
     - Left-out counts are returned beside the document, so the GUI can
       say "N rules not exported, and why".
   - **`preview(doc, cache) -> ImportPreview`,** one item per rule.
     **Normalise before comparing:** set every `list` operator's operand to
     `"list"` on both sides (see Findings), then compare all fields.
     Items:
     - `Add`;
     - `Replace { changed_fields }` (same name, different content);
     - `Unchanged`;
     - `Refused { problems }`.

     **Flags on each item:**
     - `weakens`: a replace that turns deny/reject into allow, disables a
       deny/reject, or adds `precedence` to an allow;
     - `appliesToAllApps`: no `process.*` operand at any depth. This
       generalises #44 Part B's `applies_to_all_apps`;
     - `precedence`;
     - `persists`: whether the duration is `always`.
     - A cache in `Unknown` refuses the preview.
3. **Protocol** (additive, `ws_messages.rs`):
   - `ClientMessage::ExportRules` → `ServerMessage::RulesExport { document, omitted: OmittedCounts }`
     or `RulesExportUnavailable { reason }`.
   - `ClientMessage::PreviewRulesImport { document: serde_json::Value }` →
     `ServerMessage::RulesImportPreview { preview_id, items }`.
   - `ClientMessage::ApplyRulesImport { preview_id, include: Vec<String> }`
     (names the user kept ticked) → one
     `ServerMessage::RulesImportProgress { name, outcome }` per rule, then
     `RulesImportResult { applied, rejected, not_sent }`.
   - **Preview state.**
     - **One pending preview,** held in a new
       `crates/snitchwatch-bridge-cli/src/rules_import.rs` task owned by
       `run_with_incoming`. A new preview replaces the old one, and a
       preview expires after 10 min.
     - **What it holds:** the validated rules and the cache revision at
       preview time.
     - **New revision counter inside `RulesCache`:** turn it into a struct
       holding the existing state plus `revision: u64`, bumped by every
       mutating method:
       - `replace_all`, `upsert`, `remove` and `apply_confirmed`;
       - `prune_expired` when it removed something, which covers
         `prune_expired_rules_every`'s direct prune;
       - the `Unknown` reset behind `RulesSync::withdraw`.

       `RulesSync` and the importer read it under the same lock.
     - **Apply is refused** when the `preview_id` is unknown or expired,
       the revision moved ("Rules changed since the preview — preview
       again"), or another import is running.
     - **Who may apply.** `ServerMessage`s are broadcast to every
       authenticated GUI, and any `snitchwatch-ui` member can already
       change rules, so the preview needs no per-session ownership.
4. **Apply** (spawned task, never blocking the inbound pump):
   - **Order and filter.** Walk the included names in name order. Skip
     anything not in the preview or refused there.
   - **Send.** For each rule, run `rule_from_wire` → `rule_policy` again
     (defence in depth), then `DaemonCommands::send(CHANGE_RULE with this
     one rule)`.
   - **Pacing.** **At most 8 in flight,** well under
     `STREAM_QUEUE_CAPACITY = 64`, which leaves room for the user's own
     toggles. Wait on each `PendingReply` (5 s, as in #48's pump).
   - **Outcomes of `send`** (`SendError`):
     - `NotQueued` (every stream queue full): transient. Wait for one
       in-flight reply, then retry, at most 3 times. After that, report the
       rule as `not_sent` ("the firewall was busy") and continue.
     - `InvalidRuleName`: report the rule as refused, with a fixed reason,
       and continue. The policy layer should already have refused it; this
       is defence in depth.
     - `NoDaemon`: stop. The remaining rules are reported as `not_sent`.
     - `NotAllowed`: a programming error (import only builds
       `CHANGE_RULE`). Abort the import with an `error!` log.
   - **Outcomes of `PendingReply::wait`:**
     - `Ok`: applied. The cache is updated by #48's `apply_confirmed`;
       there is no separate write here.
     - `Rejected(text)`: rejected. The daemon text is shown through
       `translator::verdict::sanitize_for_display(…, 200)`.
     - `Timeout`: shown as "no answer".
     - `StreamClosed`: stop. The remaining rules are reported as
       `not_sent`.
   - **Never `DELETE_RULE`,** and never any other action. That is
     enforced by #48's allowlist anyway.
5. **Kirigami.**
   - **Entry points.** `RulesPage.qml` header actions "Export…" and
     "Import…". Both are disabled while the Rules list is unknown.
   - **`rules_io_controller.rs`** (new `QObject`):
     - `requestExport()`;
     - `writeExport(url)`: writes the received document with mode 0600,
       because it names programs and hosts;
     - `readImport(url)`: reads at most 8 MiB, checks JSON syntax, then
       sends `PreviewRulesImport`;
     - `apply(names)`.

     **File dialogs** are `QtQuick.Dialogs.FileDialog`, which goes through
     the portal under Flatpak; no new sandbox permission is needed.
   - **`RulesImportSheet.qml`** (new, `SizedOverlaySheet` pattern):
     - **Sections:** Add, Replace, Unchanged (collapsed), Refused (each
       with its reasons).
     - **Default ticks.** Add and Replace rows start ticked, **except**
       these, which start **unticked** with a warning:
       - a `weakens` replace;
       - any imported **`precedence` allow**, which overrides other rules,
         including denies;
       - any imported **allow that applies to all apps**
         (`appliesToAllApps`).
     - **Badges:** "Applies to all apps", "Overrides other rules"
       (precedence), "Lost when the firewall restarts" (not `always`).
     - **Apply.** "Apply N changes" (count of ticked rows), then the
       per-rule results.
     - All text is plain (`textFormat: Text.PlainText`), and names use
       `displayName`.

## Tests to write first

**`rule_policy::validate_user_rule`, `Import` profile** (unit):
- **Refusals,** each with a fixed reason:
  - `lists.domains` at the top level;
  - `lists.nets` at depth 3 inside lists;
  - `type: "lists"`;
  - `process.hash.md5`;
  - `once`, `"5m"` (import profile), `""`;
  - an unknown operand;
  - a bad regexp and a bad CIDR;
  - the `LAN2` alias;
  - port 70000;
  - a name with `/`, a `z00-blocklist:` name, and a `snitchwatch-default-`
    name;
  - a 16 KiB + 1 field, a 65-member list, depth 5.
- **Accepts:** each allowed operand once, and the `LAN`/`MULTICAST`
  aliases.
- **No error string contains the offending input** (the
  pattern of `rule_name.rs` `errors_do_not_echo_the_name`).
- **Alias pin:** the alias list equals the keys of
  `vendor:daemon/data/network_aliases.json`.
- **Pairing reaches the import path.** One failing case per
  `validate_operator` rule, run through `validate_user_rule(…, Import)`.
  This pins that P2.7 calls it; the security PR owns the rule tests
  themselves.

  | Case | Expected |
  |---|---|
  | `network` + `dest.ip` | refused |
  | `simple` + `dest.network` | refused |
  | `regexp` + `source.network` | refused |
  | operand `list` with type `simple` | refused |
  | an empty `list` | refused |
  | a 65-member `list` | refused |
  | a `list` nested in a `list` | refused |
  | `true` as `regexp` | refused |
  | `lists.domains` | refused |

**`rule_io`** (unit):
- **Round trip.** `export` → serialize → parse → equal, with no
  `displayName`/`readOnlyReason` in the output.
- **Export filtering.** Export from `Unknown` is unavailable. Export
  leaves out `once`, `"5m"`, `z00-blocklist:` and invalid-name rules, and
  counts each.
- **Envelope refusals:** version 2, an unknown envelope key, duplicate
  names, 10 001 rules, a document over 8 MiB.
- **List-operand normalisation.** A cached daemon rule with
  `operand: "list"` and the same rule from a file with `operand: ""`
  classify as `Unchanged`.
- **Preview classification:**
  - add, replace (the `changed_fields` list), unchanged, refused;
  - `weakens` for: deny→allow, disabling a deny, `precedence` added to an
    allow, and *not* for allow→deny;
  - `appliesToAllApps` true for a `dest.host`-only rule and false for a
    `list` that contains `process.path`.
- **Fixtures:** every file in `tests/fixtures/rules-io/valid` parses and
  every file in `invalid/` fails with its named reason.

**Bridge integration** (`tests/bridge_protocol_test.rs` with
`MockOpensnitchd`, waiting on #48's `daemon_stream_ready()` with
`wait_for(|g| *g >= 1)`):
- **Export:**
  - before HELLO, `RulesExportUnavailable`;
  - after `subscribe_with_config` (3 rules) and HELLO, a document with
    those 3 rules, name-sorted.
- **Preview → apply:**
  - the mock receives **exactly one `CHANGE_RULE` per ticked rule, each
    with one rule**, and no other notification type;
  - with the mock answering `OK`, `ERROR "bad regexp"` and `OK`, the
    result lists one rejected rule with the sanitized text;
  - `SetRules` contains only the two `OK` rules.
- **Stale preview.** A toggle between preview and apply makes the apply
  refused.
- **Backpressure.** Apply 200 rules against a mock that delays its
  replies 50 ms: never more than 8 in flight, and no "queue full" warning.
- **Never deletes.** A preview of a document *missing* a cached rule
  sends no `DELETE_RULE`.
- **Busy queue.** With the mock's stream queue held full, `send` returns
  `NotQueued`. Apply retries, then reports `not_sent` "busy" and
  continues with the next rule.
- **Stale preview after a prune.** A temporary rule expiring through
  `prune_expired_rules_every` between preview and apply bumps the revision,
  and the apply is refused.
- **Size before parse.** A WS frame over the cap is dropped by
  `pump_authenticated` without reaching `from_str::<ClientMessage>`.
  Check this with a counter or log assertion in the `ws_server.rs` tests.

**Kirigami:**
- Qt-free: the 8 MiB read cap, preview grouping, and default ticks.
  Unticked: `weakens`, precedence allows, all-app allows. Ticked: other
  adds and replaces.
- QML smoke (offscreen): the sheet renders each section, Apply's count
  follows the ticks, and the labels are plain text.

## Verification

Run at low priority (`nice -n 19`), one crate at a time; never against the
live daemon:
- `cargo test -p snitchwatch-bridge rule_policy rule_io`
- `cargo test -p snitchwatch-integration-tests --test bridge_protocol`
- `cargo test -p snitchwatch-bridge-cli`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test -p snitchwatch-kirigami` (offscreen)

Tower VM checks:
1. Export on a VM with stock-UI and Snitchwatch rules.
2. Import into a fresh VM: the preview shows adds, and Apply installs them.
3. `always` rules are on disk under `/etc/opensnitchd/rules/`; `until
   restart` rules are gone after `systemctl restart opensnitch`.
4. A document containing a `lists.domains` rule is refused **by the
   bridge**: the tower daemon's own refusal is never exercised. The
   daemon log shows only `CHANGE_RULE`.

Flatpak checks (manual, not verified from this sandbox):
5. Export through the portal's save dialog: the file lands where chosen,
   mode 0600, with no `.<name>.*.tmp` file left beside it. The export is
   written to a new file in the same directory and renamed into place; if
   the document portal refuses that file, the export fails with "The file
   couldn't be saved there." (never a readable file).
6. Import through the portal's open dialog reads the chosen file.

## Risks

- **Regex dialects differ.** Go RE2 and the Rust `regex` crate disagree
  on a few constructs. The daemon's `ERROR` is reported per rule, never
  swallowed.
- **`user.name` resolves on the daemon host.** A name unknown there comes
  back as `ERROR`.
- **Non-`always` rules don't persist.** Imported `until restart` rules
  vanish at a daemon restart; the badge says so.
- **An export isn't a full mirror of the daemon,** because of the
  left-out rules. The export summary says how many were left out and why.
- **Imported deny/precedence rules affect all traffic they match.** The
  flags and the unticked defaults (`weakens`, precedence allows, all-app
  allows) make that visible, not prevented.
- **File-conflict hot spots:**
  - bridge-cli `run_with_incoming` (pump arms, task spawn), with #45 and
    P2.1;
  - `translator/upstream.rs` `apply`/`UpstreamEffect`, with P2.1;
  - `ws_messages.rs`, with every plan;
  - `cache/rules.rs` (`within_limits` visibility, the `RulesCache`
    revision), with P2.6's prune hook;
  - `rule_policy.rs`, with the security PR (lands first) and P2.1;
  - `ws_server.rs` `pump_authenticated`/`ws_handler` (size cap);
  - `RulesPage.qml` header, with #44 Part B, P2.6 and P2.1.

## Implementation notes (2026-10-08)

Decided while implementing, against the merged code (base `1c3615c`):

- **X5. Document cap: 960 KiB, not 8 MiB** (orchestrator call, logged for
  the owner). `ws_handler` already caps every client message at
  `MAX_CLIENT_MESSAGE_BYTES` = 1 MiB (#45 PR A), and a security limit isn't
  raised for this. `rule_io::MAX_DOCUMENT_BYTES` = 1 MiB − 64 KiB envelope
  slack. Exports are compact JSON: a rule takes about 500 bytes (program,
  host and port) or 285 (host only), so a file holds about 2,000 to 3,500
  rules; the GUI's refusal and the export summary say so.
  `pump_authenticated` still drops an over-cap frame before parsing it.
- **Network aliases are refused.** The merged `validate_operator` refuses
  `LAN`/`MULTICAST` (the bridge can't see the daemon host's alias file), and
  `DaemonCommands::send` refuses what it refuses. The alias tests became
  "refused through the Import path".
- **Match-all is refused** (review H1), by a heuristic: a rule is refused when
  none of its conditions narrows it. These don't narrow: `true`; a `/0`
  network; a regexp that matches every one of a few representative
  subjects of its operand (paths, commands, hosts, IPs, ports, protocols,
  IDs, interfaces, environment values), searched unanchored and lowercased
  like the daemon (`/` on a path, `.+` on a host). That probe is a
  heuristic, not a proof: `\.` on a host still counts as narrowing. An
  IPv4-mapped network (`::ffff:…`) is refused outright (re-review): Go reads
  `::ffff:0:0/96` as every IPv4 address. An empty `simple` value is refused
  on every operand except `dest.host` (`EqualFold("", "")` matches
  everything without the field; an empty `dest.host` matches every
  connection without a host name).
- **All-apps means not tied to programs:** only a `simple` `process.path`
  naming a real program file, or a non-empty `simple` `process.command`,
  ties a rule to programs. A `process.id` (reused after a reboot), the
  daemon's "Kernel connection" placeholder, a `process.parent.path` (the
  daemon walks every ancestor to PID 1), a `process.env.*` value or a path
  regexp doesn't, so such a rule is flagged "Applies to all apps" (and an
  allow starts unticked).
- **Default ticks and cautions are the bridge's** (review H2). An add or
  replace starts unticked, with plain-word reasons, when it is an allow
  that overrides other rules where the old one didn't, or an allow for
  every app; or a replace that turns a deny/reject into an allow, turns it
  on or off, changes its conditions or how long it lasts, or stops its
  logging; or a replace of an allow that changes its conditions, turns it
  on, makes it permanent, or stops its logging. An allow for an
  interpreter or launcher (python, a shell, env, perl, ruby, node, flatpak,
  steam) with no destination condition starts unticked too (re-review).
  A replace carries the rule it overwrites, and the sheet shows it. Changed
  fields and problem locations are plain words ("logging", not `nolog`;
  "condition 2's value", not `operator.list[1].data`).
- **Hidden rules and the list's size** (review M1, M3). Daemon rules the
  cache leaves out for the size limits keep their name and size; importing
  one of those names is refused. A preview is refused when the firewall's
  rules plus the adds would pass 10,000, or the daemon snapshot would pass
  4 MiB less a 256 KiB margin (protobuf sizes plus 16 bytes per rule). An
  apply sends at most 2,000 rules.
- **Each send is checked again** (review M2): the daemon rule must still be
  what the preview compared (or still absent), and the daemon stream the
  one the apply began on; otherwise the rule isn't sent ("changed since the
  preview") or the import stops. Twenty unanswered rules in a row stop it.
  A drop guard owns the apply: whatever ends it publishes the held rules,
  frees the import, and sends the result.
- **Not on the TCP transport** (review M5): export, preview and apply are
  refused on the legacy per-user transport until #35.
- **Answers go to the asking GUI only** (review #8): each request carries
  a `requestId` the answer echoes; progress and result carry the
  `previewId`, and the GUI acts only on the answer it waits for
  (`io_view::awaits`). `ws_server` stamps import requests with a channel
  back to their connection; an in-process sender gets them on the
  broadcast. A GUI that stops reading costs one 5 s wait; after that its
  answers are dropped unwaited, so it can't hold the import (or the rule
  list held for it) for everyone else.
- **Reserved names:** `snitchwatch-default-` is refused at the send point
  for every GUI command (its rules are listed read-only, "This name is
  reserved for Snitchwatch's own rules", and not deletable);
  `000-snitchwatch-` (the packaged rules' prefix, reserved at the send
  point by #91) is refused on import and left out of exports, through
  `rule_name::is_reserved_name`, which now covers all three prefixes.
- **Version and `enabled`:** `version: 1.0` is version 1, as in JSON Schema;
  an imported rule must say `enabled` (the GUI's rule shape defaults it on,
  `rule_from_wire` off). A test ties the schema's enums, caps and reserved
  prefixes to the code.
- **Export file:** compact JSON, written to a new file
  (`O_CREAT|O_EXCL|O_NOFOLLOW`, 0600), synced and renamed into place. The
  GUI says "Ready to save N rules" before the dialog and "Nothing was saved"
  on cancel, and keeps the left-out counts when a write fails. Daemon
  errors are shown with hidden characters stripped and shortened, never
  HTML-escaped.
- **`SendError` has six variants.** `ReservedName` and `RefusedOperator` are
  reported as refused, like `InvalidRuleName`.
- **Protocol additions:** `RulesImportRefused { requestId, reason }`
  (preview or apply refused as a whole) and `noAnswer` in
  `RulesImportResult`.
- **Export also leaves out rules the import would refuse** (aliases, `true`,
  hash conditions), counted per category. `source.daemonVersion` is left
  empty: the bridge doesn't keep the daemon version.
- **Import never shortens what it shows:** every condition is shown in full;
  hidden characters are stripped from the display and flagged.
- **Buttons aren't gated on an unknown rule list:** the GUI can't tell it
  from an empty list (`withdraw` sends an empty `SetRules`); the bridge's
  answer is shown instead.
- **`SetRules` is coalesced during an apply** (`RulesSync::hold_publishes`):
  one full list per confirmed rule would cost O(n²) serialization in every
  GUI. The user's own toggles show once the apply ends.
- **Tests:** the end-to-end flows run on the Unix transport in bridge-cli
  (`rules_import::e2e_tests`, a daemon socket that accepts any peer);
  `tests/rules_io_test.rs` checks the TCP refusal. The busy queue, the
  expiry-tick staleness and the reconnect stop are unit tests against a
  real `DaemonCommands`.
- **Follow-ups, by design:** any cache change between preview and apply
  makes the apply stale (coarse, on purpose).

## OWNER QUESTIONS

None. X1–X4 are decided at the top of this plan; X5 is logged above.
