# Inline Deny that a retransmit can't slip past

**Date:** 2026-10-08
**Issue:** none filed yet. Overnight board item 9a, an owner request
relayed through both sessions.
**Baseline:** `main` @ `4b3ba52` (#48 merged).
**Hard dependency:** #44 Part A
(`2026-10-07-app-bound-prompt-scopes-part2.md`) must merge first. See
"Why #44 Part A first".
**Size:** S. Kirigami only, plus bridge tests that pin the behaviour. No
protocol change and no new daemon notification.

## Owner decision (settled)

1. **Duration.** The GUI's inline **Deny** (row button) sends "until
   restart" (`VerdictDuration::UntilRestart`, daemon `"until restart"`).
   The daemon keeps that rule in memory and drops it when it restarts.
2. **App-bound.** The rule is a `list`: `process.path` (sensitive) AND
   `dest.host` / `dest.ip`. That is the shape #50 already builds for the
   "This host" scope.
3. **Only when `process_path` is absolute.** Otherwise the answer is
   once-only and the GUI explains why, as the #44 part 2 plan settles.

## Decided here (was owner questions D1–D3)

- **D1 = yes.** "Deny all (N)" gives each row the same app-bound
  until-restart deny. It follows directly from the owner's inline-Deny
  decision: the batch button goes through `submitInlineVerdict`, has the
  same retransmit flaw, and a once-only batch Deny would look like it
  blocks when it doesn't.
- **D2 = yes.** When Deny + "This time" is selected, the sheet shows a
  hint under the Duration box: "A one-time Deny blocks only this attempt;
  most apps retry within seconds." It keeps the explicit choice and tells
  the truth.
- **D3 = yes.** The sheet's "Until quit" label becomes "Until firewall
  restarts". This is the honest-UI rule: the daemon has no notion of
  "quit". Wording only; the token stays `until_quit`.

## Citation convention

- `main:` means `4b3ba52`.
- `vendor:` means opensnitch v1.8.0.
- `tower:` means bazzite-tower PR #81. Its system-variant daemon accepts
  only `CHANGE_RULE`/`DELETE_RULE` notifications from the UI.

Functions and tests are cited by name. Line numbers are approximate.

## Root cause (board, tower VM r4/r5)

- **What inline Deny sends today.** Scope `this_host`, duration
  `this_time`, which is `Once`.
- **Nothing is stored.** `vendor:daemon/rule/loader.go` `addUserRule`
  returns early for `Once`. `main.go`'s "Added new rule" log is misleading:
  it prints "Added" for every reply whose duration isn't `always`.
- **The reply drops only the packet that asked.** The kernel resends the
  SYN about 1 s later.
  - nftables queues `ct state new` (`vendor:daemon/firewall/nftables/rules.go`),
    so the retransmit is a fresh NEW packet.
  - It goes back through `acceptOrDeny` (`vendor:daemon/main.go`), and
    `FindFirstMatch` finds nothing.
  - If another Ask holds the daemon's single prompt slot (`isAsking`), the
    retransmit gets `DefaultAction`. That was `allow` on the r4 image, so
    the connection went through.
- **r5 confirmed the fix path.** The sheet's "Until quit" app-bound deny
  refuses retries without prompting, and is gone after a daemon restart.

## Goal

1. Inline Deny on a row whose `process_path` starts with `/` sends
   `SetVerdict { verdict: deny, scope: this_host, duration: until_restart }`.
   The daemon stores an app-bound deny until it restarts.
2. Inline Deny on a row without a bindable path sends a once-only deny,
   and the GUI shows the #44 sentence.
3. Inline **Allow** is unchanged: `this_host` + `this_time`.
4. The tower VM acceptance check passes (quoted under Verification).

## Out of scope

- Retransmits that arrive *while the prompt is still open*. They still hit
  `isAsking` and get `DefaultAction` (see Risks). That is
  `2026-10-08-prompt-slot-ux.md`.
- Per-retransmit deny rows in Connections (issue #55). After this change
  each retransmit matches a stored rule and produces a daemon event, so
  expect *more* such rows until #55 lands.
- Changing the sheet's default duration. D2/D3 change only a hint and a
  label.
- The Tauri shell and the vendored web UI. They keep sending what they
  send.

## Findings (`main`)

**Kirigami:**
- `ConnectionsPage.qml` `submitInlineVerdict(rowId, choice)` hardcodes
  `"this_host", "this_time"` for both choices. Its doc comment says inline
  and sheet decisions "produce the same rule". That stops being true for
  Deny.
- `submitBatchVerdict` loops `submitInlineVerdict`. So the process-header
  **"Deny all (N)"** button changes too, which is intended (D1).
- `pending_decision.rs` `parse_duration` maps `until_quit` to
  `UntilRestart`. **Any other string**, including a plausible
  `until_restart`, silently becomes `Once`. A test must check the parsed
  `VerdictDuration`, not just the QML token.
- `PendingDecisionSheet.qml` labels that token "Until quit".
  `VerdictDuration`'s doc comment in `ws_messages.rs` calls this the one
  lossy mapping.
- `crates/snitchwatch-kirigami/tests/inline_verdict_qml.rs` asserts
  `feedStub.lastScope === "this_host"` and
  `feedStub.lastDuration === "this_time"` for every inline and batch
  click. Flip it.
- `ConnectionRow.process_path: Option<String>` reaches Kirigami.
  - #44 Part A step 4 adds a `bindableProcessPath` role (leading `/`).
  - `ConnectionsModel` has no such role today (roles 0–20 in
    `connections_model.rs`).

**Bridge:**
- `translator/verdict.rs` `build_operator_checked` binds `ThisHost` through
  `bind_to_process(this_host_operator(conn), conn)` for **every** duration.
  The operator is already app-bound today; only the duration is wrong.
- `this_host_operator` uses `dest.host`, or `dest.ip` when `dst_host` is
  empty. There is no port operand, so the rule covers every port of that
  host.
- `bind_to_process` checks only `process_path.is_empty()`.
- `grpc_server.rs` `ask_rule` broadcasts `UpdateRules` when
  `resolution.duration.remembers()`. #48 also upserts the rule into the
  `RulesCache`. The deny shows on the Rules page at once, and deleting it
  there is the undo. That sends `DELETE_RULE`, which tower allows.
- **The deny is the `AskRule` reply, not a notification.** This plan sends
  nothing outside tower's `{CHANGE_RULE, DELETE_RULE}` allowlist.

### Why #44 Part A first

On `main`, an `until restart` deny for a row with:
- **an empty path** becomes host-only. It blocks **every app** from that
  host until the daemon restarts.
- **`Kernel connection` or a bare comm name** binds to a path that any
  program can collide with.

Part A's `bindable_process_path` / `RuleRefusal::ProcessFileUnknown` is
the enforcement:
- a remembered verdict without an absolute path becomes a once-only reply
  plus `VerdictNotRemembered`;
- the GUI gate below is only the UX.

Shipping the GUI gate alone would protect current Kirigami builds, but not
a stale or third-party client that sends `until_restart`. **Do not merge
this before Part A.**

## Design

1. **One Qt-free decision**, `pending_decision.rs`:
   - `pub(crate) fn inline_duration_token(choice: VerdictChoice, process_path: Option<&str>) -> &'static str`
     - Deny with a bindable path → `"until_quit"` (the token
       `parse_duration` maps to `UntilRestart`);
     - Deny without one → `"this_time"`;
     - Allow → `"this_time"`.
   - **Bindable** means the predicate #44 Part A defines: it starts with
     `/`.
     - Part A's `bindable_process_path` takes a proto **`&Connection`**, so
       Kirigami can't call it on a row's `Option<String>`. Use Part A's
       **`&str` form**, `is_bindable_process_path(path: &str)` (in the
       in-progress `translator/process_binding.rs`).
     - Require it to be `pub` when Part A merges. Kirigami already depends
       on `snitchwatch-bridge` (`default-features = false`).
     - If it isn't exported, keep a local copy plus a test that runs the
       #44 path table through both.
2. **Per-row lookup, so batch works too.**
   - **Store.** `connections/row_store.rs` gets
     `inline_duration_for(&self, row_id, choice) -> &'static str`. It
     looks up the row's `process_path`.
     - A missing row returns `"this_time"`. The bridge rejects a verdict
       for a gone row anyway (#49).
   - **Model.** `ConnectionsModel.inlineDurationFor(rowId, choice)` is a
     `qinvokable` over the store function.
3. **QML** (`ConnectionsPage.qml`):
   - **Submit.** `submitInlineVerdict` calls
     `bridgeFeed.submitVerdict(rowId, choice, "this_host", page.model ? page.model.inlineDurationFor(rowId, choice) : "this_time")`.
     Rewrite its doc comment: Allow matches the sheet default; Deny
     deliberately doesn't, and say why.
   - **Tooltip on the row Deny button,** plain text
     (`Controls.ToolTip` content with `textFormat: Text.PlainText`, per
     the honest-UI rule):
     - with a bindable path: "Blocks this program from this host until
       the firewall restarts";
     - without one: the #44 sentence.
   - **Non-bindable submit.** After it, show the same passive notification
     #44 Part A uses for `VerdictNotRemembered`, with the sentence from
     `RuleRefusal::describe()` (single source).
   - **Labels.** "Deny" and "Deny all (N)" keep their text. The tooltip
     carries the duration.
4. **Sheet (D2/D3)**, in `PendingDecisionSheet.qml`:
   - rename the `until_quit` option's label to "Until firewall restarts";
   - add the D2 hint, a plain-text `Controls.Label` that is visible only
     while action = Deny and duration = `this_time`.
5. **No bridge change.** The bridge test below pins the reply shape so a
   later `verdict.rs` refactor can't quietly undo this.

### Why inline Allow stays "once"

The owner asked only about Deny, and there is no strong reason to change
Allow:

- **The failure is asymmetric.**
  - An accepted SYN establishes the flow. Later packets are conntrack
    ESTABLISHED and never re-queued, so Allow-once already does what it
    says.
  - A dropped SYN is retransmitted as a NEW packet, so Deny-once doesn't.
- **Remembering an Allow is a trust grant.** An "until restart" allow is
  live for the daemon's lifetime, often days. It's a decision the user
  never made with an inline click.
- **The cost of keeping it** is a repeat prompt for that app's next
  connection. That is prompt fatigue (doc 2), not a security gap.

## Tests to write first

**Kirigami unit tests** (Qt-free):

- **`inline_duration_token` table.**

  | Choice | `process_path` | Token |
  |---|---|---|
  | Deny | `/usr/bin/curl` | `until_quit` |
  | Deny | `""`, `None`, `Kernel connection`, `curl`, `bin/curl` | `this_time` |
  | Allow | each of the above | `this_time` |

- **Through to the wire.** Feed each token to `build_verdict_message(id,
  "deny", "this_host", token)` and assert the message carries
  `duration: Some(UntilRestart)` or `Some(Once)`. This guards against
  `parse_duration`'s silent `Once` fallback.
- **`inline_duration_for`:**
  - a known row with an absolute path;
  - a known row with a placeholder path;
  - an unknown id.

**`crates/snitchwatch-kirigami/tests/inline_verdict_qml.rs`** (offscreen):
- the row Deny on an absolute-path row records `this_host`/`until_quit`;
- the row Deny on a `Kernel connection` row records `this_time`;
- the row Allow records `this_time`;
- the batch Deny (D1) records a per-row token;
- the Deny tooltip text is plain text;
- the sheet shows "Until firewall restarts" for the `until_quit` option
  (D3), and the D2 hint appears only for Deny + "This time".

**Bridge** (`grpc_server/tests.rs`, pinning existing behaviour):
- `inline_deny_until_restart_reply_is_app_bound_and_remembered`. The
  admitted Ask is for `/usr/bin/curl` → `github.com`, and the verdict is
  `{deny, ThisHost, UntilRestart}`. The reply has:
  - action `deny` and duration `until restart`;
  - a `list` operator of `process.path` (sensitive) and `dest.host`;
  - shape accepted by `mock_opensnitchd::validate_rule_shape`;
  - exactly one `UpdateRules`.
- The same test with an empty `dst_host` uses `dest.ip`.
- **Protocol** (`tests/bridge_protocol_test.rs`, pattern
  `ask_rule_round_trip_*`): a WS client sends the exact JSON Kirigami
  builds for inline Deny, and the mock daemon receives the app-bound
  `until restart` rule.
- Part A's own test (`ask_rule`, remembered verdict, `Kernel connection` →
  once + `VerdictNotRemembered`) already covers a stale client. Reference
  it; don't duplicate it.

## Verification

Run at low priority (`nice -n 19`):

- `cargo test -p snitchwatch-kirigami` with `QT_QPA_PLATFORM=offscreen`
  and `QT_QUICK_CONTROLS_STYLE=Basic`
- `cargo test -p snitchwatch-bridge grpc_server`
- `cargo test -p snitchwatch-integration-tests --test bridge_protocol`
- `cargo clippy --all-targets -- -D warnings`

**Tower VM acceptance (verbatim from the board, item 9a):**

> Tower's VM acceptance: inline Deny → `curl --retry` must fail; rule app-bound with duration "until restart"; survives the next Ask; gone after `systemctl restart opensnitch`.

**This check depends on `DefaultAction`.** Run it on a
**`DefaultAction: deny`** image (Snitchwatch's packaging config), or click
Deny within about 1 s of the prompt appearing.

Under `allow`, curl's SYN retransmit (~1 s) arrives while the prompt
still holds the daemon's slot (`isAsking`). It gets `DefaultAction` =
allow, so the flow is established before a slower click. The stored rule
then blocks only *later* connections, and `curl --retry` can succeed for
reasons unrelated to this change. See Risks.

Additional VM checks:
1. Record the image's `DefaultAction` and the click latency in the
   evidence.
2. A row whose `process_path` isn't absolute: the inline Deny replies once
   and shows the sentence. No rule appears on the Rules page.
3. The new rule is listed on the Rules page. Deleting it there lets the
   next `curl` prompt again.
4. "Deny all (N)" on a process header (D1): one rule per host, each
   app-bound.

## Risks and open questions

- **A retransmit during the open prompt still slips through.** This is
  derived from the code and not yet seen on a VM; add it to tower's
  check.
  - The daemon holds the asking SYN while the user decides. Its own
    retransmit (~1 s) re-enters `acceptOrDeny`, finds `isAsking` set, and
    gets `DefaultAction`.
  - Under `allow`, the flow can be established before the user clicks
    Deny. The stored rule then blocks only *later* connections; conntrack
    never re-queues an established flow.
  - Under `deny` (Snitchwatch's packaging config,
    `packaging/bluebuild/files/system/etc/opensnitchd/default-config.json`),
    that retransmit is dropped instead.
  - Doc 2 and the Phase 1 deny-by-default decision own this. It is also
    why the acceptance check runs on a deny image (Verification).
- **The daemon may rename the rule.** It stores prompt replies through
  `setUniqueName`, so a second deny for the same app/host/port before a
  restart is stored as `<name>-2`. #48's cache documents this divergence;
  the next `Subscribe` corrects it.
- **Flatpak paths collide.** Flatpak apps report in-namespace paths
  (`/app/bin/...`), which can be the same in different apps. An inline
  deny for one blocks the same-path binary of another app from that host.
  That fails closed; P4.2/P4.3 own real app identity.
- **Event-row noise grows** (#55). See Out of scope.
- **File-conflict hot spots:**
  - `ConnectionsPage.qml` (`submitInlineVerdict`, the row buttons), with
    #44 Part A and doc 2;
  - `connections_model.rs` roles, with #44 Part A (`bindableProcessPath`)
    and doc 2 (deadline role);
  - `pending_decision.rs`, with D2/D3 if accepted.

## OWNER QUESTIONS

None. D1–D3 are decided above.
