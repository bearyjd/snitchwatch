# Rule editor with an operand builder (roadmap P2.1)

**Date:** 2026-10-08
**Roadmap:** P2.1 in `docs/superpowers/specs/2026-10-07-competitive-feature-roadmap.md`
**Baseline:** `main` @ `d9d1bfe` plus #48.
**Blocked on:**
- #48 merging (P0.2). The editor edits the full daemon rule list, needs
  `DaemonCommands` reply outcomes, and builds on #48's `Rule.precedence`
  / `nolog` round-trip and read-only rows.
- `rule_policy.rs`, which this plan creates if P2.7 hasn't landed it yet.

**Unblocks:** #46 Part 2 (profile enforcement reuses the draft model and
policy).
**Size:** M–L, as two PRs:
1. Bridge (policy on authored rules, command results, rename).
2. Kirigami (draft model, editor sheet, entry points).

## Citation convention

- `main:` means `d9d1bfe`.
- `#48:` means branch `fix/48-show-all-rules` @ `ebdd21d` (about to merge;
  re-check names after it does).
  - `rule_to_wire`/`rule_from_wire` are in `rule_wire.rs` there.
  - On `main` they are in `grpc_server.rs`.
- `vendor:` means opensnitch v1.8.0.
- `tower:` means bazzite-tower PR #81. Its daemon accepts only
  `CHANGE_RULE`/`DELETE_RULE` and refuses `lists.*` until #45 PR B.

## Goal

1. Create a rule, and edit an existing one, from the Rules page. The
   builder covers:
   - **process:** path, parent path, command line, user;
   - **destination:** host (exact or pattern), IP, network, port;
   - **source:** IP, port, network;
   - protocol and interface;
   - action, any valid duration, `precedence`, `nolog`, enabled,
     description.
2. Every authored rule passes the bridge's rule policy before anything is
   sent. The daemon's own answer (`OK` or its `ERROR` text) reaches the
   editor.
3. Only `CHANGE_RULE` (create, edit, rename's first step) and
   `DELETE_RULE` (rename's second step) reach the daemon.

## Out of scope

- Rule groups (P2.5) and bulk edits.
- **`lists.*` operands.** They are refused until #45 PR B, and afterwards
  only #45's materializer authors them.
- **`process.hash.*` operands.** They match every binary while checksums
  are off (Findings); that is P4.1.
- OR conditions. The daemon has none inside one rule; they need two rules.
- Editing read-only rows (#48 `readOnlyReason`) or bridge-owned
  `z00-blocklist:` rules.
- Port ranges. The daemon's `dest.port` is a string compare, so a range
  needs a regexp; the builder offers one exact port.

## Findings

**Bridge (`main` / #48):**
- **The messages exist.** `ws_messages.rs` has `ClientMessage::AddRule
  { rule }`, `UpdateRule { rule_id, rule }` and `DeleteRule { rule_id }`.
  `translator/upstream.rs` `apply` maps them to `UpstreamEffect`.
- **Add and update are both `CHANGE_RULE`.**
  `translator/rule_notification.rs` `notification_for_effect` builds one
  for each, from `rule_from_wire(rule)`, because the daemon's `Replace`
  creates or overwrites by name.
  - `UpdateRule.rule_id` is ignored for the command. An update whose
    `rule.name` differs from `rule_id` **creates a second rule and leaves
    the old one**: a silent rename bug if any client sent it.
  - `DeleteRule` validates the name (`rule_name::validate_rule_name`).
- **`rule_from_wire` checks only structure plus the name.** See
  `2026-10-08-rule-import-export.md` Findings for what it doesn't check.
- **The #48 pump** sends through `DaemonCommands` and spawns a 5 s
  waiter.
  - On `OK`, the cache is updated (`apply_confirmed`) and `SetRules` is
    published.
  - On any error, the unchanged list is re-published to undo the GUI's
    optimistic change.
  - **No per-request result reaches the GUI.** An editor can't show
    "rejected: bad regexp".
- **`DaemonCommands::send`** allows only `{ChangeRule, DeleteRule}`.
- **Toggles reuse `UpdateRule`.** On #48, `rules_model.rs` `set_enabled`
  sends `UpdateRule` with the whole rule
  (`RulesStore::rule_json_with_enabled`), and `delete_rule` honours
  `is_deletable`.
  - **A stricter policy on every `UpdateRule` would break toggling
    existing rules** that contain hash or `lists.*` operands (stock UI,
    or #45's own rules).
- **Durations.** #48 `cache/rules.rs` `parse_duration_secs` accepts
  `\d+[smh]` sequences (`30s`, `5m`, `1h30m`). Anything else never expires
  in the cache.

**`vendor:` daemon:**
- **Persistence.** `Replace(r, r.Duration == Always)` writes only
  `always` rules to disk.
- **An invalid temporary duration installs a rule that never expires.**
  `loader.go` `replaceUserRule` stores the rule *before*
  `scheduleTemporaryRule` parses the duration with
  `time.ParseDuration`. A bad duration returns `ERROR`, but the rule stays
  installed. The editor must refuse anything outside #48's grammar.
- **Compile errors come back as `ERROR`.** `replaceUserRule` compiles:
  a bad regexp, a bad CIDR, an unknown `user.name` (resolved by
  `user.Lookup` on the daemon host).
- **Operand semantics:** see the table in
  `2026-10-08-rule-insights.md`. The ones that matter for the builder:
  - `process.parent.path` matches **any** ancestor;
  - `process.command` is the args joined with spaces;
  - `regexp` lowercases unless `sensitive`;
  - `process.hash.*` is always true with checksums off.

**Kirigami (#48):**
- `RulesPage.qml` has an inspector, toggle, delete and read-only rows,
  but no create or edit.
- `rules/row_store.rs` `Rule` round-trips `precedence`/`nolog` and
  carries `display_name` / `read_only_reason`.
- #44 Part B plans `Rule::applies_to_all_apps()` there.

## Design

### Bridge

1. **`rule_policy.rs`** is shared with P2.7; whichever lands first creates
   it. See `2026-10-08-rule-import-export.md` Design step 1.
   - **What the `Editor` profile adds over `Import`:**
     - durations `always`, `until restart` and the
       `parse_duration_secs` grammar (make it `pub(crate)` and reuse it),
       with at least 10 s and at most 365 days;
     - a `simple` `process.path` must be absolute (owner question E2);
     - `protocol` must be a short lowercase token
       (`[a-z0-9]{1,16}`).
   - **When it applies:**
     - **every `AddRule`;**
     - **an `UpdateRule` whose operator differs from the cached rule of
       the same name** (#48 cache), or that has no cached rule.
     - A toggle keeps the operator byte-identical, so it gets today's
       structural checks only. You can keep what exists, but you can't
       author new hash or `lists.*` conditions.
     - **Where:** in the pump, before `notification_for_effect`, because
       that is where the cache is reachable.
2. **Command results.**
   - `AddRule`/`UpdateRule`/`DeleteRule` gain an additive
     `#[serde(default, skip_serializing_if = "Option::is_none")] request_id: Option<String>`
     (at most 64 chars of `[A-Za-z0-9-]`, otherwise ignored).
   - When it is present, the pump's waiter task broadcasts
     `ServerMessage::RuleCommandResult { request_id, outcome }`. The
     outcome is one of:
     - `ok`;
     - `rejected { reason }`: the daemon text through
       `translator::verdict::sanitize_for_display(…, 200)`;
     - `refused { problems }`: `rule_policy`, with fixed reasons;
     - `timeout`;
     - `noDaemon`.
   - Toggles from older GUIs omit `request_id` and behave exactly as on
     #48.
3. **Rename** (`UpdateRule` with `rule_id != rule.name`; owner question
   E1):
   - **Today** this silently duplicates. Make it an explicit two-step in
     the waiter task:
     1. `CHANGE_RULE` the new name;
     2. **only on `OK`**, `DELETE_RULE` the old name.
   - **Results:**
     - if step 1 fails, nothing changes;
     - if step 2 fails, report `rejected` with "The new rule was saved,
       but the old one couldn't be removed; both exist."
   - **Not atomic.** Between the steps both rules exist. That is harmless
     for identical content, and the old rule is the one still deciding
     until it's gone. Document it in the sheet.
   - **The new name must not already exist** in the cache; otherwise
     refuse "a rule with that name exists". That keeps a rename from
     overwriting an unrelated rule.

### Kirigami

4. **`rules/editor.rs`** (new, Qt-free): `RuleDraft`.
   - **Fields:**
     - `{ name, description, enabled, action: Allow|Deny|Reject, duration: DurationChoice, precedence, nolog, conditions: Vec<Condition> }`;
     - `Condition { operand, kind: Exact|Pattern|Network, value, case_sensitive }`.
   - **Conditions are ANDed.** One condition is a leaf; two or more
     become a `list` with `operand: "list"`. That is the shape #50 builds.
   - **`to_wire()`** produces exactly #48's wire shape.
   - **`from_wire(&Rule) -> Result<RuleDraft, NotEditable(reason)>`**
     refuses nested lists, `lists.*`, hash operands, `complex`, and
     unknown operands. Those rows keep toggle/delete and show "Edit isn't
     available for this rule: <reason>".
   - **`suggest_name(&draft)`** follows the `rule_name_for` style
     (`snitchwatch-<action>-<target>-<8 hex>`), and always passes
     `validate_rule_name`.
   - **`validate()`** calls `snitchwatch_bridge::rule_policy` directly.
     Kirigami already depends on `snitchwatch-bridge` with
     `default-features = false`; keep the module free of feature-gated
     dependencies. That gives instant feedback, while the bridge stays
     authoritative.
5. **`RuleEditorSheet.qml`** (new, `SizedOverlaySheet` pattern):
   - **Entry points:**
     - "New rule…" on `RulesPage.qml`'s header;
     - "Edit…" on a row, disabled for read-only, `NotEditable` and
       bridge-owned rows;
     - "Create rule from this connection" in `ConnectionsPage.qml`'s
       inspector. It prefills `process.path` (only if absolute),
       `dest.host` or `dest.ip`, and `dest.port`.
   - **Condition builder:**
     - an operand picker (grouped Process / Destination / Source /
       Network);
     - the match kind;
     - the value;
     - a case-sensitivity switch, defaulting to **on** for
       `process.path`/`process.parent.path`, which mirrors #50's
       sensitive path.
     - Field help states the daemon semantics: "matches if any parent
       process has this path", "command line joined with spaces".
   - **Warnings** (plain text, `Kirigami.InlineMessage`):
     - **no `process.*` condition:** "Applies to every program", using
       #44 Part B's predicate, generalised;
     - **`precedence` on:** "Decides before other rules, including ones
       that block";
     - **`nolog` on:** "Hides this rule's connections from Snitchwatch,
       including hit counts";
     - **not `always`:** "Lost when the firewall restarts";
     - **a `Pattern` on `process.path`:** "Matches any program whose path
       matches"; see E2.
   - **Duration** presets (once isn't offered): 5 min, 1 h, 12 h, 1 day,
     until restart, forever, plus a custom field limited to the grammar.
   - **Submit.** `RulesModel.submitRule(json, originalName)` sends
     `AddRule` or `UpdateRule` with a fresh `request_id`. The sheet stays
     open showing "Saving…" until the matching `RuleCommandResult`, then
     closes on `ok` or shows the reason. It times out client-side at 10 s.
   - **"Test this rule"** opens the simulator (P2.6) with the draft
     applied to a copy of the store.
   - All labels are `textFormat: Text.PlainText`, and names show via
     `displayName`.

## Tests to write first

**Bridge:**
- **`rule_policy`, `Editor` profile:**
  - duration grammar: `5m` and `1h30m` accepted; `1.5h`, `300ms`, `5`,
    `forever` and `""` refused; `9s` refused (under the 10 s minimum);
  - a relative `simple` `process.path` refused;
  - a regexp `process.path` accepted;
  - every refusal reason is fixed text.
- **Pump** (bridge-cli `lib.rs` tests, `MockOpensnitchd` via #48
  readiness):
  - `AddRule` with `lists.domains` → `RuleCommandResult{refused}`; the
    mock receives **nothing**;
  - `UpdateRule` toggling a cached rule that has `lists.domains`, operator
    unchanged → sent as today;
  - `UpdateRule` changing that operator → refused;
  - `ok` / `rejected` (daemon text sanitized and capped) / `timeout` /
    `noDaemon` each produce one result carrying the right `request_id`;
  - without `request_id`, no `RuleCommandResult`, which is #48's
    behaviour;
  - a `request_id` of 65 chars or with `/` is ignored.
- **Rename:**
  - `CHANGE_RULE` new → `OK` → `DELETE_RULE` old, in that order;
  - `CHANGE_RULE` `ERROR` → no `DELETE_RULE`;
  - `DELETE_RULE` `ERROR` → the "both exist" result;
  - a target name that exists → refused before sending.
- **Allowlist.** Across all tests, the mock only ever sees `CHANGE_RULE`
  and `DELETE_RULE`.

**Kirigami** (`rules/editor.rs`, Qt-free):
- **Round trip.** `to_wire` → `from_wire` for each supported operand and
  match kind; a single condition is a leaf, two make a `list`.
- **`from_wire` refusals:** a nested list, `lists.nets`,
  `process.hash.sha1`, `complex`.
- **`suggest_name`** passes `validate_rule_name` for hostile hosts (reuse
  `rule_name_for`'s hostile-input cases).
- **Warnings.** The no-process warning appears exactly when no
  `process.*` operand is present.

**QML (offscreen):**
- the sheet opens from "New rule…";
- Edit is disabled on a read-only row;
- the precedence warning is visible when the switch is on;
- a result `rejected` keeps the sheet open with the reason as plain text.

## Verification

Run at low priority (`nice -n 19`):
- `cargo test -p snitchwatch-bridge rule_policy`
- `cargo test -p snitchwatch-bridge-cli`
- `cargo test -p snitchwatch-integration-tests --test bridge_protocol`
- `cargo test -p snitchwatch-kirigami` with `QT_QPA_PLATFORM=offscreen`
  and `QT_QUICK_CONTROLS_STYLE=Basic`
- `cargo clippy --all-targets -- -D warnings`

Tower VM checks:
1. **Create and edit.** New rule: deny `/usr/bin/curl` → `example.com`.
   `curl https://example.com` fails without a prompt. Edit it to allow:
   it succeeds.
2. **Temporary expiry.** A 1 min rule disappears from the daemon at
   expiry, and from the Rules page within about 30 s of it (#48 prune
   tick).
3. **Rename.** Exactly one JSON file remains in `/etc/opensnitchd/rules/`.
4. **Daemon errors reach the editor.** A `user.name` the VM doesn't have
   shows the daemon's error in the sheet, and the rule list is unchanged.
5. **tower.** The daemon log shows only `CHANGE_RULE`/`DELETE_RULE`.

## Risks

- **Rename isn't atomic** (see step 3). A crash between the steps leaves
  both rules. Each is visible and deletable.
- **Toggling a rule whose operator the cache doesn't match exactly** is
  treated as an authored change, so it is policy-checked. This can refuse
  a toggle of an odd legacy rule. The refusal reason says so; the rule
  stays deletable.
- **Regex dialects differ** (Go RE2 vs the Rust `regex` crate). The
  daemon's `ERROR` is shown in the editor.
- **Power-user footguns stay possible:** `precedence` allows and
  all-program denies. Warnings, not prevention.
- **File-conflict hot spots:**
  - bridge-cli `run_with_incoming` pump rule-effect arm, with #48
    follow-ups and P2.7's new arms;
  - `ws_messages.rs` (`request_id`, `RuleCommandResult`);
  - `translator/upstream.rs`;
  - `rules_model.rs` and `RulesPage.qml`, with #44 Part B, P2.6 and P2.7;
  - `ConnectionsPage.qml` inspector, with the inline-Deny and prompt-slot
    plans.

## OWNER QUESTIONS

- **E1. Renaming.** Options:
  - (a) allow it, as the two-step `CHANGE_RULE` new → `DELETE_RULE` old,
    with the partial-failure message;
  - (b) the name is fixed, and "Duplicate…" makes a copy under a new
    name.

  **Recommendation: (a).** (b) is the same two steps done by hand, with
  worse error handling.
- **E2. `process.path` in hand-written rules.** #44 settled "absolute
  paths only" for *prompt* rules. Options for the editor:
  - (a) exact match must be absolute, and a pattern is allowed with a
    warning;
  - (b) absolute only, with no patterns;
  - (c) anything.

  **Recommendation: (a).** Patterns are how power users cover per-user
  installs (Steam under `~`), and they're explicit authoring, not a
  daemon fallback value.
- **E3. Expose `precedence` and `nolog` in v1?**
  **Recommendation:** yes, under an "Advanced" expander with the warnings
  above. They round-trip already (#48), so hiding them only hides what
  existing rules do.
