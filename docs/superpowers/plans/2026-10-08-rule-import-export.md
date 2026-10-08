# Rule import / export with a dry-run diff (roadmap P2.7)

**Date:** 2026-10-08
**Roadmap:** P2.7 in `docs/superpowers/specs/2026-10-07-competitive-feature-roadmap.md`
**Baseline:** `main` @ `d9d1bfe` plus #48.
**Blocked on:** #48 merging. Import needs its `RulesCache` to diff against
and `DaemonCommands` to learn each rule's outcome.
**Size:** S–M. One bridge PR (policy, document, protocol, apply) and one
Kirigami PR (file dialogs, preview sheet).

## Citation convention

- `main:` means `d9d1bfe`.
- `#48:` means branch `fix/48-show-all-rules` @ `ebdd21d`, a squashed
  rebase onto `main` that is about to merge. **Re-check every `#48:` name
  after it merges.**
  - On #48, `rule_to_wire`/`rule_from_wire` live in `rule_wire.rs`.
  - On `main`, they are in `grpc_server.rs`.
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

## Out of scope

- **"Replace all" import,** which would delete rules missing from the
  file. That is owner question X3.
- **Blocklist subscriptions and profiles.** Their stores only persist
  after #45 PR A and #46 Part 1. The v1 envelope rejects unknown keys, so
  v2 can add them cleanly.
- **Automatic or scheduled backups,** and the CLI (P5.1). `rule_io.rs` is
  written so the CLI can reuse it.
- **Importing opensnitchd's on-disk rule files,** which is what the stock
  UI exports (owner question X4).
- **Hash and `lists.*` operands** (see Design step 1).

## Findings

**#48: the rules cache**
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
  counter for "anything changed". The stale-preview check (Design step 3)
  needs one.

**#48: rule shape and validation**
- `rule_wire.rs` `rule_from_wire` checks:
  - the name with `rule_name::validate_rule_name`;
  - that `action`/`duration` are non-empty;
  - that `operator` is present and non-null, and each leaf operand is
    non-empty.

  It does **not** check the action or duration vocabulary, the operand
  vocabulary, `lists.*`, regexps or CIDRs.
- `rule_to_wire` adds the display-only fields `displayName` and (via the
  cache) `readOnlyReason`. Those must not go into an export.

**#48: daemon commands**
- `daemon_commands.rs` `DaemonCommands::send` allows only
  `{ChangeRule, DeleteRule}` (`ALLOWED_ACTIONS`).
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
- `ws_server.rs` sets no explicit message-size limit (tokio-tungstenite
  defaults), so import adds its own cap.

## Design

1. **`rule_policy.rs`** (new, bridge crate, pure).
   - **Ownership.** It is shared with P2.1 (`2026-10-08-rule-editor.md`)
     and doc 2's curated defaults. **The first of P2.7/P2.1 to land
     creates it; the other extends it.**
   - **API:**
     `validate_user_rule(rule: &Rule, profile: PolicyProfile) -> Result<(), Vec<RuleProblem>>`.
     `RuleProblem { path: String /* e.g. "operator.list[1].operand", built from field names and indices only */, reason: &'static str }`.
     **Fixed reasons only:** never echo file text (the `rule_name.rs`
     pattern).
   - **Checks:**
     - **Name:** `validate_rule_name`. Refuse the prefixes the bridge owns:
       `z00-blocklist:` and the legacy `900-blocklist:` (#45).
     - **Action:** `allow`, `deny` or `reject`.
     - **Duration** (`PolicyProfile::Import`): `always` or `until restart`.
       `once` never persists, and temporaries would expire (owner
       question X1). The editor profile also allows the grammar #48's
       `prune_expired` parses.
     - **Operator tree:**
       - `type` must be `simple`, `regexp`, `network` or `list`. `lists`
         and `complex` are refused.
       - `operand` must be one of: `true`, `list`, `process.path`,
         `process.parent.path`, `process.command`, `process.id`,
         `process.env.<NAME>`, `user.id`, `user.name`, `source.ip`,
         `source.port`, `source.network`, `dest.ip`, `dest.host`,
         `dest.port`, `dest.network`, `protocol`, `iface.in`,
         `iface.out`.
       - **Refused:** `process.hash.md5`/`process.hash.sha1` (match-all,
         see Findings; checksum flows are P4.1) and every `lists.*` at
         any depth. The tower daemon refuses `lists.*`, and only #45's
         materializer may author list rules.
     - **Regexps** compile with the `regex` crate. That only approximates
       Go RE2; the daemon remains authoritative and its `ERROR` is
       reported per rule.
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
       JSON. The GUI checks size before parsing; the bridge checks both
       again on receipt.
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
       (the `z00-blocklist:`/`900-blocklist:` prefixes), and rules whose
       names fail `validate_rule_name` (#48's read-only rows: they could
       never be imported back).
     - Left-out counts are returned beside the document, so the GUI can
       say "N rules not exported, and why".
   - **`preview(doc, cache) -> ImportPreview`,** one item per rule:
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
     - **New `RulesSync::revision()`:** a counter bumped on every cache
       mutation (`replace_all`, `upsert`, `apply_confirmed`,
       `prune_expired`, `withdraw`).
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
   - **Outcomes:**
     - `Ok`: applied. The cache is updated by #48's `apply_confirmed`;
       there is no separate write here.
     - `Rejected(text)`: rejected. The daemon text is shown through
       `translator::verdict::sanitize_for_display(…, 200)`.
     - `Timeout`: shown as "no answer".
     - `NoDaemon`/`StreamClosed`: stop. The remaining rules are reported
       as `not_sent`.
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
     - **Ticks:** Add and Replace are ticked by default, except a
       `weakens` replace, which starts unticked with a warning.
     - **Badges:** "Applies to all apps", "Overrides other rules"
       (precedence), "Lost when the firewall restarts" (not `always`).
     - **Apply.** "Apply N changes" (count of ticked rows), then the
       per-rule results.
     - All text is plain (`textFormat: Text.PlainText`), and names use
       `displayName`.

## Tests to write first

**`rule_policy`** (unit):
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
  - a name with `/`, and a `z00-blocklist:` name;
  - a 16 KiB + 1 field, a 65-member list, depth 5.
- **Accepts:** each allowed operand once, and the `LAN`/`MULTICAST`
  aliases.
- **No error string contains the offending input** (the
  pattern of `rule_name.rs` `errors_do_not_echo_the_name`).
- **Alias pin:** the alias list equals the keys of
  `vendor:daemon/data/network_aliases.json`.

**`rule_io`** (unit):
- **Round trip.** `export` → serialize → parse → equal, with no
  `displayName`/`readOnlyReason` in the output.
- **Export filtering.** Export from `Unknown` is unavailable. Export
  leaves out `once`, `"5m"`, `z00-blocklist:` and invalid-name rules, and
  counts each.
- **Envelope refusals:** version 2, an unknown envelope key, duplicate
  names, 10 001 rules, a document over 8 MiB.
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

**Kirigami:**
- Qt-free: the 8 MiB read cap, preview grouping, and default ticks
  (`weakens` unticked).
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
  flags and `weakens` default make that visible, not prevented.
- **File-conflict hot spots:**
  - bridge-cli `run_with_incoming` (pump arms, task spawn), with #48 and
    #45;
  - `translator/upstream.rs` `apply`/`UpstreamEffect`, with P2.1;
  - `ws_messages.rs`, with every plan;
  - `cache/rules.rs` (`within_limits` visibility, `revision`), with #48
    follow-ups;
  - `RulesPage.qml` header, with #44 Part B, P2.6 and P2.1.

## OWNER QUESTIONS

- **X1. What an export contains.** Options:
  - (a) only `always` + `until restart` user rules, leaving out bridge-owned
    blocklist rules and temporary rules;
  - (b) everything the daemon holds, with temporaries re-armed on import.

  **Recommendation: (a)**, with counts of what was left out. Blocklist
  rules come back from their subscriptions; temporaries would expire.
- **X2. A same-name rule in the file.** Options:
  - (a) replace, ticked by default except a `weakens` replace;
  - (b) skip by default;
  - (c) import under a new name.

  **Recommendation: (a).** It mirrors daemon `Replace` semantics and keeps
  loosening changes opt-in.
- **X3. A "replace all" mode** that deletes rules not in the file.
  **Recommendation: not in v1.** Deleting a deny can unblock traffic for
  every app (#44 Part B's reason for no bulk delete).
- **X4. Accepting opensnitchd's on-disk rule files** (one rule per file,
  which is what the stock UI writes). **Recommendation:** v1.1. It uses
  the same `rule_policy`, with one rule per file wrapped into a v1
  document.
