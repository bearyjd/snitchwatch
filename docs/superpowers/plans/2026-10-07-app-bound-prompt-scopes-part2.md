# App-bound prompt scopes, second half (issue #44)

**Date:** 2026-10-07 (revised after #39 merged as `670f42c`)
**Issue:** #44, second half. The first half shipped in #50; its plan is
`2026-10-07-app-bound-prompt-scopes.md`.
**Baseline:** `main` @ `670f42c`.
**Blocked on:**
- nothing, for Part A, now that #39 has merged. It edits `ask_rule`, so
  land it after #47 and #48 or rebase onto them;
- #48, for Part B, because pre-#50 rules come from earlier sessions.

**Size:** S + S, as two independent PRs.

## Owner decisions (settled)

1. **Refuse instead of degrading.** Only a `process_path` that starts with
   `/` gets an app-bound **remembered** rule.
   - An empty or non-absolute path never produces a remembered rule. That
     covers the daemon's `"Kernel connection"` placeholder and its
     comm/argv[0] fallbacks.
   - The user's answer applies to this connection only, as a once-only
     reply.
   - The GUI says why: "Snitchwatch couldn't identify this program's file,
     so this answer applies only to this connection."
   - The same rule applies to the `AnyHost` scope's existing empty-path
     fallback.
2. **Flag old rules on the Rules page.** Mark pre-#50 Snitchwatch
   host-only rules as "applies to all apps", with one-click delete per row.
   No automatic migration.

## Citation convention

- `main:` means `670f42c`. Functions and tests are cited by name. Line
  numbers are approximate.
- `vendor:` means opensnitch v1.8.0.

## Goal

- **Part A:** the bridge never creates a remembered rule that isn't bound
  to an absolute executable path. Otherwise:
  - the user's verdict still applies to this connection;
  - the GUI is told why nothing was remembered.
- **Part B:** users can see which saved Snitchwatch rules match every
  program, understand what deleting one does, and delete it with one
  click.

## Out of scope

- Automatically rewriting old rules.
- An explicit "any app" host scope (P2.2).
- Flagging hand-written or stock-UI rules.
- A bulk "delete all" action. It goes beyond the owner decision, and
  deleting a deny can *unblock* traffic (see Part B).

## Findings (`main`)

**Rule construction** (`translator/verdict.rs`):
- `verdict_to_rule` is infallible.
- `bind_to_process` checks only `process_path.is_empty()`. It logs `warn!`
  and returns the bare host operator.
- `any_host_operator_checked` degrades `AnyHost` with an empty path to the
  host-only `this_host_operator` and reports
  `ScopeDegradation::ProcessPathUnavailable`.
- `rule_name_for` appends `-p<basename>-<16 hex>` whenever `process_path`
  is non-empty.

**Why `is_empty()` rarely fires.** The daemon fills `Process.Path` when
`/proc/<pid>/exe` is unreadable (`vendor:daemon/procmon/details.go`
`ReadPath`, deferred fallback):
- `"Kernel connection"` (`procmon/process.go` `KernelConnection`) when
  `/proc/<pid>/maps` is empty;
- otherwise `p.Comm`.

So today these produce rules bound to `process.path == "Kernel connection"`
or to a bare comm name. Such a rule matches any process the daemon
describes the same way, and anyone can name a binary to collide with a
comm name. That is why the owner chose absolute paths only.

**Callers of `verdict_to_rule`:**
- `grpc_server.rs` `ask_rule`: the paused branch (`Allow`/`Once`/`ThisHost`)
  and the main path;
- `grpc_server/tests.rs` `process_bound_verdict_rule_survives_the_wire_round_trip`;
- about 30 call sites in `verdict.rs` tests.

The persistent-rule `UpdateRules` broadcast in `ask_rule` is gated on
`resolution.duration.remembers()`.

**`Notice` is matched exhaustively** in
`crates/snitchwatch-tauri/src/notifier.rs` (`From<&Notice> for NoticeKey`,
and the summary/body `match`), `crates/snitchwatch-kirigami/src/notifier.rs`
(`From<&Notice> for NoticeKey`), and
`crates/snitchwatch-kirigami/src/notification_controller.rs`
(summary/body `match`). A new `Notice` variant breaks all three.

**Tests that pin today's fallback** (`verdict.rs`):
- `host_scopes_keep_the_host_only_fallback_when_process_path_is_empty`
- `any_host_scope_degrades_to_this_host_when_process_path_is_empty`
- `deny_verdict_any_host_degradation_is_surfaced_when_process_path_empty`

**Old rules:**
- Every Snitchwatch prompt rule since M1.5 (`a81d571`) carries
  `description: "snitchwatch interactive verdict"` (`verdict_to_rule`).
  `rule_from_wire` keeps it across GUI toggles.
- Pre-#50 host scopes emitted a bare `dest.host`/`dest.ip` simple operator,
  or a `regexp` `dest.host` for the domain scope.
- Pre-#50 `AnyHost` emitted `process.path` only.

**Evaluation** (`vendor:daemon/rule/loader.go` `FindFirstMatch`): a matching
deny returns immediately. A non-precedence allow is remembered while the
scan continues. So a host-only **deny** currently overrides every app's
process-only (`AnyHost`) allow for that host. Deleting the deny lets those
apps through to the host; apps without an allow are prompted.

## Design

### Part A: refuse in the bridge (S)

1. **One predicate** in `verdict.rs`:
   `fn bindable_process_path(conn) -> Option<&str>` returns `Some` only when
   `conn.process_path.starts_with('/')`. Use it everywhere the code
   currently checks `is_empty()`:
   - `verdict_to_rule(...) -> Result<Rule, RuleRefusal>` returns
     `Err(RuleRefusal::ProcessFileUnknown)` when `duration.remembers()` and
     the path is not bindable. This holds for **every** scope, `AnyHost`
     included: its fallback no longer produces a remembered host-only rule.
   - A new infallible `once_rule(verdict, scope, conn, now)` covers the
     paused branch and the refusal fallback. The daemon never stores a
     `once` rule (`loader.go` `addUserRule`).
   - `bind_to_process` and `any_host_operator_checked` treat a non-bindable
     path like an empty one. Both are now reachable only for once replies.
     `any_host_operator_checked` still reports `ProcessPathUnavailable`, so
     a once-only `AnyHost` **Deny** with a non-bindable path still sends the
     `DenyScopeNarrowed` notice. Downgrade `bind_to_process`'s `warn!` to
     `debug!`.
   - `rule_name_for` appends the `-p…` component only when the path is
     bindable, so a name always matches its operator. Already-saved
     `…-pKernel_connection-…` rules keep their stored names; the GUI
     addresses rules by stored name.
   - `RuleRefusal::describe()` returns the owner's fixed sentence as a
     `&'static str` (the same pattern as `ScopeDegradation::describe`).
2. **`grpc_server.rs` `ask_rule`.**
   - The paused branch calls `once_rule`.
   - On the main path, `Err(ProcessFileUnknown)`:
     - replies with `once_rule(resolution.verdict, resolution.scope, …)`;
     - skips the `UpdateRules` broadcast and #48's cache upsert;
     - broadcasts `ServerMessage::VerdictNotRemembered { row_id, reason }`;
     - sends `Notice::VerdictNotRemembered { row_id }`, for a user who
       answered with the window hidden.
3. **Build-break edits planned with step 2.**
   - **New `Notice` variant:** add `NoticeKey::VerdictNotRememberedForRow(u64)`
     and summary/body text in the tauri `notifier.rs`, the kirigami
     `notifier.rs`, and the kirigami `notification_controller.rs`.
     `cargo check --workspace` plus a kirigami build catch any missed site.
   - **`Result` return:** `.expect("absolute process path")` in
     `process_bound_verdict_rule_survives_the_wire_round_trip` and in the
     `verdict.rs` tests that build remembered rules for `/usr/bin/curl`;
     `once_rule` where they test `Once`.
   - **New `ServerMessage` variant:** grep for exhaustive `match`es on
     `ServerMessage` in the Kirigami and Tauri crates. Today they use
     `matches!` or `_` arms (**verify**).
4. **Kirigami.**
   - `VerdictNotRemembered` shows the sentence as a passive notification on
     `ConnectionsPage`.
   - Prevention: when the row has no bindable path, `PendingDecisionSheet.qml`
     offers only "This time", with the same sentence as a hint. Add a
     `bindableProcessPath` role that uses the same leading-`/` rule.
   - Land this after the honest-ui PR, which edits that sheet.

### Part B: flag old rules (S, after #48)

5. **Kirigami `rules/row_store.rs`** `Rule::applies_to_all_apps()` is true
   when **both** hold:
   - `description == "snitchwatch interactive verdict"` (provenance);
   - the operator contains **no** `process.path` operand at any depth, and
     is host-only: a `simple` `dest.host`/`dest.ip` or a `regexp`
     `dest.host`.

   No name check. A name regex can't tell a pre-#50 host that starts with
   `p` or contains `-p` (`snitchwatch-allow-pypi.org-<16hex>-443`) from
   #50's `-p<program>-<hex>` component. The operator shape is the
   authoritative signal.
6. **`RulesModel` and `RulesPage.qml`.**
   - **Model:** an `appliesToAllApps` role and a `legacyHostOnlyCount`
     property.
   - **Flagged row:** a Warning chip "Applies to all apps", and a row-level
     **Delete** button that calls `deleteRule(name)` in one click.
   - **Flagged allow rows**, hint: "Deleting this makes every app ask again
     before reaching <host>."
   - **Flagged deny rows**, which must say so explicitly: "Deleting this
     unblocks <host> for every app that is allowed to reach any host; other
     apps will be asked."
   - **Page header:** an informational `InlineMessage` with the count. No
     bulk action.
   - All new labels use `textFormat: Text.PlainText`, per the honest-ui PR.
7. **Release note.** Link the existing note in
   `2026-10-07-app-bound-prompt-scopes.md` ("Behavior changes and limits").

## Tests to write first

**Part A** (`verdict.rs`, `grpc_server/tests.rs`):

- **Process path table**, for `ThisHost`, `AnyHostOnDomain` and `AnyHost`,
  each with `FiveMinutes`, `UntilRestart` and `Always`:

  | `process_path` | Expected |
  |---|---|
  | `""` | `Err(ProcessFileUnknown)` |
  | `"Kernel connection"` | `Err(ProcessFileUnknown)` |
  | bare comm `"curl"` | `Err(ProcessFileUnknown)` |
  | relative `"bin/curl"` | `Err(ProcessFileUnknown)` |
  | `"/usr/bin/curl"` | `Ok`, bound with a `list`, `process.path` sensitive |

- **`Once`** with each non-bindable path returns `Ok`, host-only, and has
  no `-p` name component.
- **`AnyHost` + `Deny` + `Once`** with `"Kernel connection"` still reports
  `ProcessPathUnavailable`.
- **Flip the three pinned tests** listed in Findings.
- **`ask_rule`, remembered Allow, `process_path = "Kernel connection"`:**
  - the reply has duration `once` and action `allow`;
  - no `UpdateRules`;
  - exactly one `VerdictNotRemembered`, with the fixed sentence, plus one
    `Notice::VerdictNotRemembered`.
- **`ask_rule`, paused, empty path:** still auto-allows once.
- **Mock acceptance:** the reply passes
  `mock_opensnitchd::validate_rule_shape`.
- **Notifiers:** a new test in each of the three notifier/controller files
  covers the new variant.

**Part B** (Kirigami unit and QML):

- **`applies_to_all_apps` truth table:**
  - true for a pre-#50 `snitchwatch-allow-github.com-<16hex>-443` with a
    `dest.host` operator;
  - true for **`snitchwatch-allow-pypi.org-<16hex>-443` with a `dest.host`
    operator**;
  - true for a pre-digest `snitchwatch-deny-github.com-443`;
  - true for a `-2` suffix;
  - true for a `regexp` `dest.host`;
  - false for a #50 `list` rule;
  - false for a pre-#50 `AnyHost` rule (a `process.path` operator);
  - false for a `dest.host` rule with another description;
  - false for a `z00-blocklist:` rule.
- **QML:**
  - a flagged deny row shows the unblock warning, and an allow row shows
    the ask-again hint;
  - Delete emits `DeleteRule` with that name;
  - no "delete all" control exists.

## Verification

Run at low priority:

- `cargo test -p snitchwatch-bridge`
- `cargo test -p snitchwatch-tauri notifier` (it shares the `Notice`
  enum)
- `cargo check --workspace`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test -p snitchwatch-kirigami` with `QT_QPA_PLATFORM=offscreen`
  and `QT_QUICK_CONTROLS_STYLE=Basic`

Manual VM check:

- **Part A:** a remembered answer for a kernel-thread connection replies
  once and shows the sentence.
- **Part B:**
  1. Drop a pre-#50-shaped deny JSON into `/etc/opensnitchd/rules/`.
  2. Restart the bridge: the rule is flagged with the unblock warning.
  3. One click deletes it, from the page and from disk.

## Risks and open questions

- **Reply on refusal (resolved).** The bridge replies once-only with the
  user's verdict, rather than `Unavailable` (default action).
- **Refusing non-absolute paths is stricter than today.** Kernel and
  comm-fallback connections will never be remembered, which may cause
  repeat prompts for kernel threads. That follows the owner's decision; a
  dedicated kernel-connection rule type would be a later P2.2 item.
- **False negatives in flagging.** A pre-#50 rule whose description was
  edited elsewhere isn't flagged. Acceptable.
- **File-conflict hot spots:**
  - `grpc_server.rs` `ask_rule`, with #47 and #48;
  - `notice.rs` and both `notifier.rs` files, plus
    `notification_controller.rs`;
  - `ws_messages.rs`, with #47, #48 and #45;
  - `PendingDecisionSheet.qml`/`RulesPage.qml`, with the honest-ui PR.
