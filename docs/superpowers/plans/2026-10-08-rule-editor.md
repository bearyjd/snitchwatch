# Rule editor with an operand builder (roadmap P2.1)

**Date:** 2026-10-08
**Roadmap:** P2.1 in `docs/superpowers/specs/2026-10-07-competitive-feature-roadmap.md`
**Baseline:** `main` @ `4b3ba52`. #48 has merged, bringing:
- the full rule list;
- `DaemonCommands` reply outcomes;
- the `Rule.precedence`/`nolog` round-trip;
- read-only rows.

**Blocked on:** the security PR (branch `fix/rule-operator-validation`),
which creates `rule_policy.rs` with `validate_operator` (pairing, `list`
shape and nesting, `lists.*` refusal) and applies it in `rule_from_wire`.
- This plan **reuses** it.
- It adds `PolicyProfile::Editor`, plus `validate_user_rule` if P2.7
  hasn't landed it yet.

**Unblocks:** #46 Part 2 (profile enforcement reuses the draft model and
policy).
**Size:** M–L, as two PRs:
1. Bridge (policy on authored rules, command results, rename).
2. Kirigami (draft model, editor sheet, entry points).

## Citation convention

- `main:` means `4b3ba52`.
- #48 is merged, so its names are cited as `main:`.
  `rule_to_wire`/`rule_from_wire` are in `rule_wire.rs`, and
  `rule_to_wire` emits `displayName`/`readOnlyReason`.
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

**Bridge (`main`):**
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
- **`rule_wire.rs` `rule_from_wire`** checks structure plus the name.
  - After the security PR, it also runs `validate_operator` on GUI-sourced
    rules.
  - It still doesn't check vocabulary, durations, regexps or CIDRs; see
    `2026-10-08-rule-import-export.md` Findings.
- **The #48 pump** sends through `DaemonCommands` and spawns a 5 s
  waiter.
  - On `OK`, the cache is updated (`apply_confirmed`) and `SetRules` is
    published.
  - On any error, the unchanged list is re-published to undo the GUI's
    optimistic change.
  - **No per-request result reaches the GUI.** An editor can't show
    "rejected: bad regexp".
- **`DaemonCommands::send`** allows only `{ChangeRule, DeleteRule}`.
- **Toggles reuse `UpdateRule`.** `rules_model.rs` `set_enabled` sends
  `UpdateRule` with the whole rule (`RulesStore::rule_json_with_enabled`),
  and `delete_rule` honours `is_deletable`.
  - **An `Editor`-profile check on every `UpdateRule` would break toggling
    existing rules** that the profile refuses: hash operands, relative
    `process.path`, odd durations, from the stock UI.
  - Toggling a `lists.*` rule from the GUI is already refused by the
    security PR's `validate_operator` in `rule_from_wire`. That is its
    decision; this plan doesn't change it.
  - **But a "same operator" `UpdateRule` can still change `duration`,
    `action`, `precedence`, `nolog` or `description`.** So the toggle
    exemption must compare every field except `enabled`, not just the
    operator.
- **List operand spelling differs by source.** Daemon-sourced `list`
  operators carry `operand: "list"`; wire-parsed ones carry `""`. Compare
  only after normalising.
- **Durations.** `cache/rules.rs` `parse_duration_secs` accepts
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

**Kirigami (`main`):**
- `RulesPage.qml` has an inspector, toggle, delete and read-only rows,
  but no create or edit.
- `rules/row_store.rs` `Rule` round-trips `precedence`/`nolog` and
  carries `display_name` / `read_only_reason`.
- #44 Part B plans `Rule::applies_to_all_apps()` there.

## Design

### Bridge

1. **Policy.** `rule_policy.rs` comes from the security PR
   (`validate_operator`). P2.7 adds `validate_user_rule` and the `Import`
   profile; if P2.7 hasn't landed yet, this plan adds them.
   - **The `Editor` profile:** `validate_operator` first (pairing, `list`
     shape and nesting, `lists.*`), then everything `Import` checks
     (vocabulary, hash refusal, regexp/CIDR/alias, ports, caps, the
     bridge-owned name prefixes `z00-blocklist:`/`900-blocklist:`/
     `snitchwatch-default-`), plus:
     - durations `always`, `until restart` and the `parse_duration_secs`
       grammar (make it `pub(crate)` and reuse it), with at least 10 s and
       at most 365 days;
     - a `simple` `process.path` must be absolute (owner question E2, decided 2026-10-08);
     - `protocol` must be a short lowercase token (`[a-z0-9]{1,16}`).
   - **When it applies:**
     - **every `AddRule`;**
     - **every `UpdateRule` that is not a pure toggle.**
   - **A pure toggle** is an `UpdateRule` for a name in #48's cache where
     every field **except `enabled`** equals the cached rule, after
     normalising every `list` operator's operand to `"list"` on both
     sides. The fields are `name`, `action`, `duration`, `precedence`,
     `nolog`, `description` and the whole operator tree. Pure toggles get
     only `rule_from_wire`'s checks, including `validate_operator`.
     - Anything else, including a same-operator change of `duration`,
       `action` or `precedence`, gets the full `Editor` profile.
     - You can keep what exists, but you can't author or alter around a
       hash condition.
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
3. **Rename** (`UpdateRule` with `rule_id != rule.name`; E1, decided
   below):
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
     refuses:
     - anything `validate_operator` refuses: bad pairings, empty or
       oversized lists, nested lists, `lists.*`;
     - hash operands, `complex`, and unknown operands.

     Those rows keep toggle/delete and show "Edit isn't available for this
     rule: <reason>".
   - **Match kind is restricted per operand,** so the builder can't
     express a pairing `validate_operator` would refuse:
     - `dest.network`/`source.network` offer **only** `Network` (a CIDR or
       alias);
     - every other operand offers `Exact` or `Pattern`, never `Network`;
     - `true` and `list` are never offered: two or more conditions build
       the `list`.
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
     - the match kind, limited to what the chosen operand allows (step 4);
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
- **Pairing reaches the editor path.** One `AddRule` per
  `validate_operator` rule; each gives `RuleCommandResult{refused}`, and
  the mock receives nothing:
  - `network` + `dest.ip`;
  - `simple` + `dest.network`;
  - operand `list` with type `simple`;
  - an empty `list`;
  - a 65-member `list`;
  - a nested `list`;
  - `true` as `regexp`;
  - `lists.domains`.
- **Pump** (bridge-cli `lib.rs` tests, `MockOpensnitchd` via #48's
  `daemon_stream_ready().wait_for(|g| *g >= 1)`):
  - a pure toggle of a cached rule with a `process.hash.md5` condition (a
    stock-UI rule) is sent as today;
  - **the same rule with only `enabled` flipped but `operand: ""` instead
    of `"list"`** is still a pure toggle;
  - **the same operator, a changed `duration`** (e.g. `always` → `"1.5h"`)
    is not a toggle → refused by the `Editor` profile;
  - **the same operator, a changed `action` or `precedence`** is not a
    toggle → policy-checked;
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
  `process.hash.sha1`, `complex`, `network` + `dest.ip`.
- **Match kinds per operand:** `dest.network` offers only `Network`;
  `dest.ip` offers no `Network`; `true`/`list` are never offered.
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
- **Toggling a rule whose cached copy differs in any field but `enabled`**
  is treated as an authored change, so it is policy-checked. For example,
  the daemon renamed it with `-2`, or the cache is stale. This can refuse
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

## Decided here (was E1 and E3)

- **E1. Rename: allowed,** as the two-step `CHANGE_RULE` new →
  `DELETE_RULE` old, with the partial-failure message (step 3). A fixed
  name plus "Duplicate…" is the same two steps done by hand, with worse
  error handling.
- **E3. `precedence`/`nolog` are in v1,** under an "Advanced" expander
  with the warnings above. They already round-trip (#48), so hiding them
  would only hide what existing rules do.

## OWNER QUESTIONS

- **E2. `process.path` in hand-written rules.** #44 settled "absolute
  paths only" for *prompt* rules. Options for the editor:
  - (a) an exact match must be absolute, and a pattern is allowed with a
    warning;
  - (b) absolute only, with no patterns;
  - (c) anything.

  **Recommendation: (a).** Patterns are how power users cover per-user
  installs (Steam under `~`), and they're explicit authoring, not a
  daemon fallback value.

  **DECIDED (owner, 2026-10-08): (a).** Exact absolute program paths by
  default; path patterns are allowed, with a warning.

## Departures (implementation, 2026-10-08)

Recorded where the built code differs from the design above.

**Bridge:**
- **Results go to the asking connection only.** They use P2.7's
  per-connection reply (`ReplyTo`/`Replier`), not a broadcast, so another
  GUI never sees them. Without a `request_id`, a command behaves as on #48.
- **A fifth outcome, `unsure { reason }`,** covers a rename whose result
  isn't known.
- **Rename failure handling is stricter than step 3:**
  - if the old rule's delete is refused, the new rule is deleted again and
    the result says nothing changed;
  - "both exist" is reported only after a second failure (that undo fails,
    or the delete is never sent);
  - an unanswered delete undoes nothing (deleting the new rule then could
    leave neither, losing a deny) and says both may exist.

  "Neither" can't result.
- **An `AddRule` never overwrites.** A name that is cached, hidden (left
  out of the cache for size) or being renamed is refused, as is a rename
  onto one. Both names of a rename stay busy until it ends.
- **Add, edit and rename are refused on the legacy TCP transport.**
  Another program could pose as the daemon there (#35). Toggles and
  deletes still go through.
- **Every `UpdateRule` that isn't a pure toggle needs a cached rule
  Snitchwatch may change** (`read_only_reason` is `None`). That excludes:
  - blocklist rules;
  - the packaged `000-snitchwatch-` rule and curated defaults;
  - numeric `user.name` rows (#91);
  - rules hidden for size.
- **Timed rules are stamped with `created`** when the daemon starts their
  clock (a new rule, or a changed duration), so the cache expires them
  when the daemon does. An edit that keeps the same timed duration keeps
  the cached `created`, because the daemon's old timer still fires
  (`RulesCache::upsert`).
- **"Absolute" is `is_bindable_process_path`.** An exact `process.path`
  must also not be under `/proc`, and must have no `//` and no trailing
  `/`.

**Kirigami:**
- **A separate `RuleEditorController`** sends and waits, the way the
  import does, instead of `RulesModel.submitRule`. `RulesModel` gains only
  `editableRuleJson(name)`.
- **The client waits 30 s, not 10 s.** A rename waits for up to three 5 s
  daemon answers plus the reply. On a timeout the sheet says the change
  may have been saved; it doesn't say it failed.
- **Cautions need a second click.** The import preview's `edit_cautions`,
  checked against the rule being replaced, change Save to "Save anyway".
  Problems block Save outright.
- **The builder doesn't offer `process.id` or `process.env.*`.**
  `process.id` names whatever process gets that number next.
  - `user.name` offers only an exact match.
  - Only `dest.network`/`source.network` offer a network.
- **Prefill uses the simulator's prefill form** (`SimulationForm`). It
  takes `process.path` only when it is a bindable program path, so not
  "Kernel connection".
- **Edit is in the rule inspector,** since a row opens the inspector. It
  is disabled, with the reason shown, for read-only rules and for rules
  the editor can't express. Toggle and delete stay available.
- **One more warning:** a timed rule is removed when its time runs out,
  and an edit that keeps the same time keeps the original timer.
- **"Test this rule" is deferred.** It needs the simulator (P2.6) run on a
  draft.
- **"Create rule from this connection" isn't added to `ConnectionsPage`.**
  Prompt-slot C's "Make a rule…" uses the entry point instead:
  - `main.qml` `openRuleEditor(prefillJson)` goes to the Rules tab and
    calls `RulesPage.openEditor(prefillJson)`;
  - this mirrors "Simulate this connection" → `openSimulator`.
- **`snitchwatch-proto` is a normal Kirigami dependency** (it was
  dev-only), for reading checked rules' conditions.
