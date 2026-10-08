# Prompt-slot UX: living with opensnitchd's single prompt

**Date:** 2026-10-08
**Issue:** #17 (upstream evilsocket/opensnitch#1644). Overnight board item
9b. Roadmap P2.4 (curated defaults) is pulled forward in part.
**Baseline:** `main` @ `4b3ba52` (#48 merged). Part D also needs the
security PR's `rule_policy.rs` (branch `fix/rule-operator-validation`).
**Size:** M overall, as four bridge/UI PRs (A–D) plus a daemon-side note
(E).
- A and B need no owner decision beyond wording.
- C and D were blocked on the owner questions at the end. All of them
  (S1–S5, plus #78) are **DECIDED (owner, 2026-10-08)**, as recommended.
- E is a description for bazzite-tower, not work for this repo.

## Citation convention

- `main:` means `4b3ba52`.
- #48 is merged (PR #60), so its names are cited as `main:`.
- `vendor:` means opensnitch v1.8.0.
- `tower:` means bazzite-tower PR #81. Its system-variant daemon accepts
  only `CHANGE_RULE`/`DELETE_RULE` from the UI and refuses `lists.*`
  operands until #45 PR B.

Functions are cited by name. Line numbers are approximate.

## Problem

**The daemon asks one question at a time.**
- `vendor:daemon/main.go` `acceptOrDeny` applies `DefaultAction` to every
  unmatched connection while `uiClient.GetIsAsking()` is true.
- It holds `SetIsAsking(true)` (with a deferred `false`) across the
  requeue and `uiClient.Ask(con)`.
- `Ask` has a 120 s context (`vendor:daemon/ui/client.go`).

**The bridge holds that slot for exactly as long as the user takes.**
`grpc_server.rs` `ask_rule` waits in a `tokio::select!` on the verdict
receiver and `admission.lost()`. `PendingCleanup` covers the daemon's
deadline. There is no bridge-side timeout.

**After login,** background programs (Steam, the welcome app, `kioworker`)
ask first. Their prompts sit unanswered because nobody is looking. Each
holds the slot for up to 120 s, and the next one from the same program
takes it again. Meanwhile every other new connection gets `DefaultAction`:
- **Under `allow`** (vendor default; the tower r4/r5 images): unprompted
  traffic is allowed. That is a silent security gap.
- **Under `deny`** (Snitchwatch's packaging config,
  `packaging/bluebuild/files/system/etc/opensnitchd/default-config.json`,
  settled decision 5): traffic silently fails. The breakage teaches users
  to click blindly or turn filtering off.
- **A connection's own SYN retransmit during its own prompt** also gets
  `DefaultAction` (`2026-10-08-inline-deny-until-restart.md`, Risks).
  This is derived from the code, not yet seen on a VM.

## Findings: what Snitchwatch can and can't see

- **There is no queue to show.** The daemon sends at most one `AskRule`.
  A rare second one is possible because `GetIsAsking`/`SetIsAsking` is an
  unlocked check-then-set across 16 workers. Everything else that arrives
  meanwhile never reaches the bridge.
- **Default-applied connections leave no per-connection record:**
  - `vendor:daemon/statistics/stats.go` `onConnection` returns *before*
    appending an `Event` when `wasMissed` is true.
  - `Serialize` returns nil unless a *matched* event set `newEvents`.
- **Only aggregates are available.**
  - `rule_misses` is a cumulative counter. `ws_messages.rs`
    `DaemonStatistics` relays it already.
  - The `by_host`/`by_executable`/`by_port`/`by_uid` maps are cumulative,
    capped at `MaxStats` with least-hit eviction, and include matched
    traffic. The bridge drops them today (P3.2).
  - Both arrive late: only on the next ping that also carries a matched
    event (`ping` is skipped when `Serialize` is nil, `client.go` `ping`).
- **When a miss means "skipped because busy".** A miss is an unmatched
  connection that wasn't answered, for one of these reasons:
  - no UI is connected;
  - `isAsking` is set;
  - the requeue timed out (1 s);
  - `Ask` returned nil (bridge error, `Unavailable`, or an invalid rule).

  With a GUI connected and one Ask pending, the misses counted during that
  Ask approximate the "skipped because busy" set. It is a count, not a
  list.
- **Existing mitigations:**
  - `main.qml` `pendingExposureBanner`: a 10 s threshold, a 120 s ceiling,
    and generic text ("may be silently allowed or denied").
    Plan: `2026-08-05-pending-decision-exposure-warning.md`.
  - `notification_controller.rs`: a desktop notification for
    `Notice::Pending` after a 5 s grace, only while the window is hidden,
    with a single "Review" action (`REVIEW_ACTION_ID`).
- **Stock UI precedent.** It auto-answers after `DEFAULT_TIMEOUT = 30` s
  (`vendor:ui/opensnitch/config.py`) with its configured default action,
  which is deny out of the box (`ACTION_DENY_IDX`). It never holds the
  slot for 120 s.
- **The bridge doesn't parse the daemon's configuration.** `subscribe`
  records `is_firewall_running` and, since #48, stages `cfg.rules` for
  commit on HELLO (`RulesSync::stage`). It still ignores the `config`
  field: the daemon's config JSON as a string (`vendor:proto/ui.proto`
  `ClientConfig.config`), which includes `DefaultAction` and
  `Stats.MaxEvents`.
- **tower.** Nothing in A–D needs a notification beyond `CHANGE_RULE`/
  `DELETE_RULE`. `AskRule` replies are not notifications.

## Goal

1. The user can see that a prompt is holding the slot, which program holds
   it, for how long, and roughly how many connections were defaulted
   meanwhile. Wording never claims per-connection detail.
2. A prompt can be answered without opening the main window.
3. *(Owner-gated)* A prompt can't hold the slot indefinitely. An
   unanswered or deferred prompt ends in a defined, visible outcome, and
   the user can turn it into a rule later.
4. *(Owner-gated)* Known background and system services stop asking at
   all, through curated rules installed with `CHANGE_RULE`.
5. The daemon-side options are written down for bazzite-tower to choose
   from.

## Out of scope

- Any new UI→daemon notification action. That includes `CHANGE_CONFIG`,
  so the bridge never changes `DefaultAction`.
- Listing individual default-applied connections. That needs daemon
  option E3.
- The full P2.4 first-run learning mode (P2.3/P2.4). Part D is only the
  curated-rules half.
- Persistent history (P3.1).

## Design

### A. Show who holds the slot and what it costs (BR + UI, S; no owner decision)

1. **`PromptSlot`** (new file `crates/snitchwatch-bridge/src/prompt_slot.rs`):
   - **State, keyed by `row_id`:**
     `holders: BTreeMap<RowId, Holder { process, host, since_ms, misses_baseline: Option<u64>, last_misses: Option<u64>, last_uptime: Option<u64> }>`.
     - Normally there is at most one holder. Two concurrent Asks are rare
       but possible: the daemon's `GetIsAsking`/`SetIsAsking` is an
       unlocked check-then-set across workers.
     - A single `Option` would let the second Ask overwrite the first, and
       the first release would then clear the second.
     - `hold(row_id, …)` inserts. `release(row_id)` removes **only that
       entry**.
     - The broadcast shows the oldest holder plus `holders: n` when
       `n > 1`.
   - **Feeds:**
     - `ask_rule` marks the slot held when it inserts the pending row.
       That is the same point that sends `Notice::Pending`.
     - Every exit releases it: verdict, GUI loss, `PendingCleanup` on
       cancellation. **Release from `PendingCleanup::drop`** so that an
       early return can't leak a holder.
     - The `ping` handler passes `stats.rule_misses` and `stats.uptime`
       in.
   - **Take the baseline from the first reading *after* the hold starts,
     never from an older one.** The daemon pings only when there are
     matched events, so the last reading before the hold can be minutes
     old.
     - Right after login, it would include every Ask answered
       `Unavailable` before a GUI authenticated: exactly the case this
       part is for.
     - Misses between the hold starting and that first reading go
       uncounted, so **the figure is a lower bound: "at least N"**.
   - **Drop the baseline** (back to `None`, re-taken from the next
     reading) when:
     - `uptime` decreases (the daemon restarted, and `rule_misses` is
       cumulative per daemon process);
     - #48 commits a new HELLO snapshot during the hold.
   - **`defaulted_at_least()`** is
     `last_misses.saturating_sub(misses_baseline)`, per holder. It is
     `None` while either value is unknown, and it never underflows.
   - **Display text.** `process`/`host` go through
     `grpc_server::display_summary`, the existing sanitizer.
2. **Protocol** (additive; older clients ignore unknown actions):
   - `ServerMessage::PromptSlot { holder: Option<PromptSlotHolder>, holders: u32, defaulted_at_least: Option<u64> }`,
     where `holder` is the oldest one and `holders` the total;
     sent on hold, release and count change;
   - included in the `RequestSnapshot` answer.
3. **Release summary.** When a holder is released with
   `defaulted_at_least > 0`, send `Notice::PromptSlotSummary { count }`:
   "While that prompt was open, at least N other connections got the
   firewall's default action."
   - **Fixed text.** It carries no connection data.
   - **Build-break edits.** `Notice` is matched exhaustively in the tauri
     `notifier.rs`, the kirigami `notifier.rs` and
     `notification_controller.rs`; #44 Part A lists the same three sites.
     Land after Part A, or coordinate the new variants.
4. **Kirigami `pendingExposureBanner`** (`main.qml`) is driven by
   `PromptSlot` when present, with the age-based logic kept as a fallback
   for older bridges:
   - **Text.** "<process> → <host> is waiting for your answer (Ns). Until
     you answer, other new connections get the firewall's default action
     (at least N so far)." Plain text.
   - **Actions:** Allow (once), Deny (the inline semantics of
     `2026-10-08-inline-deny-until-restart.md`), Review.
   - **When the count is `None`,** drop the "at least N" clause rather
     than show 0.

#### A as implemented (2026-10-08): departures

Branch `feat/prompt-slot-visibility`, which also folds in the honesty half of
issue #78.
- **`what` is not `display_summary`.** That function HTML-escapes, so a
  PlainText label would show `&lt;` literally. The WS `holder.what` is
  `prompt_slot::plain_summary`: control and bidi characters stripped, each
  part truncated, nothing escaped. Clients show it in PlainText labels
  only. Notifications keep their escaping and carry no connection data.
- **Baseline rule.** The first reading after a hold, and any reading after
  a reset, becomes the baseline. The count stays unknown until a later
  reading.
  - A reset is `uptime` dropping or the `rules.synced()` generation
    changing.
  - A count of 0 is shown like an unknown one: the count sentence is
    dropped.
- **The count is of times, not connections** (review of PR #86). The
  daemon's `rule_misses` counts unanswered packets: a retry, or the waiting
  connection's own SYN retransmit, counts again.
  - The text says "the firewall applied its default action N times
    meanwhile (retries count again)", never "at least N other connections".
  - It is still a lower bound: counting starts at the first ping after the
    hold.
  - Overlapping holders count the same misses. Each release summary is true
    of its own prompt; the summaries don't add up.
- **"Oldest" is by hold order,** not row-id order (`ask-10` sorts before
  `ask-9`).
- **Gating on the bridge.** The bridge advertises a `promptSlot`
  capability (the #74 handshake). The age-based `pendingExposureBanner`
  stays, gated on its absence, so it never flashes before the snapshot on
  a new bridge.
- **`PromptSlotSummary { row_id, count }`.** It carries the ask id, so the
  notifiers' cooldown is per prompt.
- **Wording ("usually").** Tower's r8 saw requeued packets dropped under
  nftables chain churn. The text says other new connections "usually get
  the firewall's default action", never that every connection prompts.
- **Issue #78 (honesty only).** While paused with a prompt holding the
  slot, the banner and the tray tooltip add fixed text: "Filtering is
  paused, but a connection is still waiting for your answer. Until you
  answer it, the pause can't reach other new connections; they usually get
  the firewall's default action instead."
  - With several holders it reads "N connections are still waiting".
  - This holds under either `DefaultAction`, which the bridge doesn't know.
  - Nothing is auto-answered in Part A. The owner then chose #78 option 1
    (2026-10-08): pausing answers waiting prompts with Allow once — a
    separate follow-up PR.
- **UI shape.**
  - The banner is its own component, `PromptSlotBanner.qml`: a fixed-text
    InlineMessage and a PlainText label, per #51.
  - Allow once and Deny use `InlineVerdicts`. They are enabled only while
    the model holds the row as pending, and answer each holder once.
  - They also wait until the holder has been shown for 750 ms. The timer
    restarts on each holder change, so a double-click meant for one prompt
    can't answer the next one.
  - Review opens the Connections page, whose auto-select picks the row; it
    doesn't open the inspector.

#### Issue #78 as implemented (2026-10-08): a pause answers what is waiting

Branch `feat/78-pause-answers-waiting`, after #86; owner decision: option 1.
- **Bridge-side, so it works tray-only.** After every `SetFilteringPaused`
  the pump calls `pause_answers::answer_waiting`, then
  `announce_pause_state`.
  - It answers each pending Ask the pause applies to
    (`FilterPause::applies_to`: admitted under the pausing GUI session's
    generation, which is still current).
  - It answers through `ConnectionCache::resolve(Allow, Once, ThisHost)`,
    the call a GUI's Allow once goes through. Nothing is saved and no
    `UpdateRules` is sent.
  - The answered rows go out as `UpdateConnectionRows`. Each Ask's reply
    releases its slot hold, so the banner and tray follow.
- **Label.** `ConnectionRow.autoAnswer: "filterPaused"`, additive. A reason
  this build doesn't know parses as `Unknown`, so a newer bridge can't
  break the row.
  - Kirigami's `answeredWhilePaused` role makes the row's verdict label read
    "Allowed once (filtering was paused)" (PlainText).
  - Asks that arrive during the pause get the same label. The text is true
    for them too.
- **No gap.** `ask_rule` now chooses between prompting and the pause's
  allow under the cache lock (the check used to run before taking it). The
  scan holds that lock too, and runs after the pause is set. Either the
  scan sees the pending row, or the row's insertion sees the pause.
  - #86's warning stays as the fallback.
  - The warning can flash for a moment between `FilterPauseState` and the
    Asks' slot releases. That is a flicker, not a waiting prompt.
- **Menu.** Each pause item reads, e.g., "Pause for 5 minutes (also lets
  waiting connections through once)", but only when the connected bridge
  advertises the `pauseAnswersWaiting` capability
  (`bridge_capabilities::PAUSE_ANSWERS_WAITING`, like `promptSlot`).
  - An older bridge leaves the prompts waiting, so against it the item is
    the plain "Pause for 5 minutes".
  - `TrayController.pauseAnswersWaiting` carries the live session's
    capability to `TrayMenu.qml`; it is false until a bridge says so.

### B. Answer from the notification (UI, S; owner question S5 decided: yes)

5. `notification_controller.rs` `dispatch` adds "Allow once" and "Deny"
   actions next to "Review" for `Notice::Pending`.
   - Deny uses the same per-row token as the inline button
     (`inline_duration_for`).
   - The action handler builds the verdict through `BridgeFeed`'s existing
     path (`pending_decision::build_verdict_message`).
   - **Verify** how `Notice::Pending { row_id: u64 }` maps to Kirigami's
     session-prefixed row ids (#49). A stale id must be dropped, exactly as
     the inspector does since #56.
6. **No remembered Allow from a notification.** Only Allow-once.

#### B as implemented (2026-10-08): departures

Branch `feat/prompt-slot-notification-actions`, after Part C.
- **No QML in the path.** The actions run in Rust on the notification's
  thread (`notification_actions::act`), through `bridge_feed::dispatch_to`
  rather than `BridgeFeed.submitVerdict`. So #77's gate and the
  session-routed send apply, with the window closed too.
  - Deny's duration comes from `InlineDeny::decide`, the inline button's
    rule: until restart only for a bindable program on a session with
    app-bound rules, otherwise once.
  - A once-only Deny follows up with the same plain sentence the page shows.
- **Row ids.** `Notice::Pending { row_id }` is the ask id, so the row is
  `ask_row_id(row_id)` in the notice's session.
- **Still waiting?** The runtime keeps each session's waiting rows
  (`bridge_runtime/pending_rows.rs`), fed by the same row messages as the
  model. It uses the same pending test and starts empty per session.
  - After the 5 s grace a notice is shown only if its row still waits in
    its session. This closes #78's gap: a prompt answered by a tray-only
    pause within 5 s is no longer announced.
  - Every action checks again. A stale one sends nothing and says so in a
    fixed-text notification.
- **Body.** It is built from the waiting row, not the notice:
  "<program> wants to connect to <host>". Both are escaped for the
  notification markup subset by the bridge's `sanitize_for_display`, which
  also strips control and bidi characters.
  - When Deny would last until the firewall restarts, the body says so, as
    the inline Deny's tooltip does.
- **No "Decide later" on the notification.** Item 8 lists it, but the owner's
  S5 is "Allow once and Deny only". It can be added if S5 is widened.
- **PR #100 security review fixes.**
  - **Only the notification server can click.** notify-rust's
    `wait_for_action` accepted `ActionInvoked` from any sender on the bus.
    `notification_signals` sends `Notify` itself, on the connection that
    listens. Its match rule names the server's unique name, path and
    interface. `classify` checks the sender, the id and that the key is one
    of ours. A change of owner voids the notice. Every other notice now
    carries no actions.
  - **Body.** It shows the full program path. A long path or host keeps its
    end behind a leading "…".
  - **Format characters.** The display sanitizer strips every format
    character (Unicode category Cf).
  - **Lost race.** The bridge drops a verdict for a row that stopped
    waiting without saying so. The answer then watches for the row's
    update, and says "This prompt was already answered" when the row was
    settled another way.
  - **Withdrawn.** The notification is closed once its row stops waiting.
- **PR #100 re-review fixes.**
  - **On-demand servers.** A server started on demand (dunst, say) takes
    the name after the notice starts hearing owner changes, so that change
    is already queued when the wait starts. A change now voids the notice
    only when the server that showed it lost the name; any other change is
    ignored. Owner changes are polled before clicks.
  - **A voided notice is closed,** and its row still waits for the window.
    `CloseNotification` goes to the unique name that showed it, never to
    a new owner, whose notification with that id is somebody else's.
  - **CI runs the private-bus tests.** The kirigami job installs
    `dbus-daemon` and sets `CI`; with `CI` set, a missing `dbus-daemon`
    fails these tests instead of skipping them.
  - **Path.** A long program path keeps its start and its end
    (`sanitize_ends_for_display`), so a padded `/tmp/x/…/firefox` still
    shows `/tmp`. A host keeps its end only.
  - **Invisible characters.** The display sanitizer also strips the
    invisible characters outside Cf: the combining grapheme joiner, the
    Hangul fillers and the variation selectors.

### C. Bridge auto-answer and "Decide later" (BR + UI, M; S1/S2 decided: P-a after 30 s, "Decide later" = P-c)

7. **Auto-answer.** `ask_rule`'s `tokio::select!` gains a third arm,
   `sleep(ANSWER_TIMEOUT)`. It cancels the pending row through the same
   `cache.cancel_pending(&row_id)` path as GUI loss, then replies per
   policy S1:
   - **P-a: `Err(Status::unavailable("no answer"))`.** The daemon applies
     its `DefaultAction` to this one connection and stores no rule. This
     is today's outcome at 120 s, just sooner.
   - **P-b: a once-only deny.** It fails closed, but the retransmit
     re-asks within about 1 s, so the same program takes the slot back.
   - **P-c: an app-bound temporary deny,** existing
     `VerdictDuration::FiveMinutes` with `AnyHost` scope (process-only).
     That program's retries are dropped without asking for 5 min. It
     needs an absolute path (#44 Part A); otherwise fall back to P-a.
8. **"Decide later"** is a button on the pending row, the banner and the
   notification. It answers immediately with policy S2, freeing the slot
   now instead of at the timeout.
9. **Visible outcome.**
   - **Countdown.** The pending `ConnectionRow` gains an additive
     `#[serde(default, skip_serializing_if = "Option::is_none")] answer_deadline_ms: Option<i64>`.
     The countdown P0.6 removed can return, because it is now true.
   - **Decided-later rows** get an additive `deferred: bool`, and
     `action` is set to the reply's action when the bridge knows it:
     - for P-a, it knows only if `DefaultAction` was parsed (step 10);
     - otherwise the row says "the firewall's default action".
   - **Make it a rule.** A "Make a rule…" action on such rows opens the
     P2.1 editor prefilled. Until the editor lands, use the sheet's
     scope/duration choices sent as `AddRule`, which is `CHANGE_RULE`.
10. **`daemon_config.rs`** (new, shared with P2.6). `subscribe` parses
    `ClientConfig.config` defensively into
    `DaemonConfigView { default_action: Option<String>, max_events: Option<u32>, checksums_enabled: Option<bool> }`:
    - a field that is missing or the wrong type is `None`;
    - the raw string is never logged.
11. **Unchanged interactions.**
    - The paused branch (#47) answers before insertion. Pausing also
      answers every prompt that is already waiting (#78, decided; see the
      owner questions).
    - GUI loss still returns `Unavailable`.
    - The 120 s daemon deadline can no longer be reached.

#### C as implemented (2026-10-08): departures

Branch `feat/prompt-slot-autoanswer`.
- **Daemon facts checked** (`vendor:daemon/main.go`, `ui/client.go`,
  `ui/notifications.go`).
  - A failed `AskRule` gets `applyDefaultAction`, with no rule stored and no
    disconnect.
  - While a GUI is connected, that action comes from the `DefaultAction` of
    the config the GUI echoes back in `Subscribe`. The bridge echoes the
    daemon's own config.
  - So `daemon_config` names the row's action only for `allow`, `deny` and
    `reject` (shown as deny). Anything else names none.
- **Rows are kept, not removed.** `ConnectionCache::defer_pending` takes a
  prompt out of pending like `cancel_pending`, so a verdict that already won
  is kept. The row stays listed with `deferred: true` and its
  `answerDeadlineMs` cleared.
  - Timed-out rows also carry `autoAnswer: "noAnswer"`, the extension point
    #78 added.
- **"Decide later" is a bridge message**, `ClientMessage::DecideLater`,
  gated on a new `decideLater` capability.
  - The bridge picks the policy: P-c when the row's program path is
    bindable, P-a at once otherwise.
  - A plain `SetVerdict` could neither mark the row deferred nor give the
    daemon no answer at once.
  - Kirigami's `dispatch_to` checks the capability again and routes the
    message to the row's session. Without the capability, the sheet says
    plainly that nothing can put the prompt off.
- **Make a rule…** builds the rule in Kirigami with the bridge crate's own
  `verdict_to_rule` and sends it as `AddRule`.
  - It is offered only for a deferred row with a bindable program, and only
    with remembered durations.
- **Wording.** Labels say "usually allowed" for an allow, because requeued
  packets can drop under chain churn (tower's r8). They never name an action
  the bridge didn't report.
- **Version skew.** A GUI older than this shows a deferred row with no known
  action as pending. With a parseable daemon config, that row always has an
  action.
- **PR #98 security review fixes.**
  - Make a rule on a row with no hostname matches `dest.ip`. The bridge
    shows the IP as the row's host, and `dest.host == <ip>` never matches.
  - A made rule's name ends in `-made-<unix ms>`, so its `CHANGE_RULE`
    can't replace the prompt's own rule, e.g. the 5-minute block.
  - Labels say the block applies "on every host, even ones you allowed": a
    matching deny wins over an allow rule. Make a rule leaves that block in
    place and says so.
  - `daemon_config` parses the daemon's whole `Config` the way Go does:
    keys matched case-insensitively, `null` and unknown keys ignored. A
    type error anywhere, or two keys for one setting, means nothing is
    known, because the daemon then keeps its previous default action.
  - The decision sheet says when an answer or "Decide later" couldn't be
    sent.

### D. Curated defaults for background services (data + BR, M; S3 decided; still blocked on the security PR's `rule_policy.rs` and the reserved-prefix refusal)

12. **Data file.** `crates/snitchwatch-bridge/data/curated-defaults-v1.json`
    lists app-bound **allow** rules. Each has:
    - an absolute `process.path` (sensitive);
    - optionally a host constraint (`dest.host` simple or regexp);
    - duration `always`;
    - **`precedence: false`, always.** A precedence allow stops
      `FindFirstMatch` before any later deny, so it would override
      blocklist denies. That contradicts the settled "blocklist wins"
      decision. A non-precedence allow loses to any matching deny,
      including #45's list rules;
    - a name `snitchwatch-default-<id>`;
    - the description `snitchwatch curated default v1`.

    Every entry passes `rule_policy::validate_operator` (the security PR)
    and the profile layer P2.7 adds, checked by a unit test over the data
    file.
    - **Reserved prefix.** `snitchwatch-default-` joins `z00-blocklist:`
      and `900-blocklist:` as a bridge-owned prefix. P2.7's export leaves
      it out, because those rules come back from the data file.
      - **D must not land without a refusal of that prefix for GUI-authored
        `AddRule`/`UpdateRule`.** Otherwise a user could create a
        `snitchwatch-default-…` rule that D's reconcile would treat as its
        own.
      - The P2.7/P2.1 profile layers provide the refusal. If D lands before
        either, D adds the prefix check itself, in the pump's rule-effect
        arm, before `notification_for_effect`.
13. **Installed via `CHANGE_RULE`** through #48's `DaemonCommands`,
    reconciled like #45 PR B.
    - **When reconciliation runs:**
      - after every committed rules snapshot, i.e. each `RulesSync::synced()`
        generation bump, which #48 makes only after the snapshot's stream
        sent HELLO. This is the same trigger as #45 PR B's reconcile;
      - immediately when the user turns the opt-in on or off;
      - never while the cache is `Unknown`.
    - **What it does:**
      - an entry missing from the daemon is installed;
      - an **unedited** copy under the reserved prefix that is no longer in
        the file, or that exists while the opt-in is off, is deleted with
        `DELETE_RULE`;
      - **a user-edited copy is not deleted, not even on opt-out** (as of
        the firewall service's last rule list). That means same name, but
        some field other than `enabled` differs from the data file after
        list-operand normalisation. It is left alone and flagged (see "D as
        implemented" for the wording).
        Deleting it could silently undo a change the user chose, such as
        narrowing a curated allow's scope;
      - nothing outside the prefix is ever deleted.
    - **Required before building D (critic, 2026-10-08, HIGH):** "an entry
      missing from the daemon is installed" must not reinstall a curated
      rule the **user deleted** — otherwise deleting e.g. the `kioworker`
      allow is silently undone at the next daemon restart. Record
      user-deleted curated names (persisted with the other bridge state)
      and never reinstall them; and the reserved-prefix refusal must still
      allow **pure toggles** (`enabled` only) of curated rules. Tests: a
      deleted curated rule stays deleted across reconcile/restart; toggling
      a curated rule off and on works under the prefix check.
14. **Content.** It comes from the first-boot capture spike (roadmap §6
    item 5): on a fresh tower VM image, record every program that asks in
    the first 10 minutes after login. Known hard cases go to the owner
    (S3):
    - Steam's binaries live under the user's home (no stable absolute
      path);
    - `kioworker` (`/usr/libexec/kf6/kioworker`) does network I/O for
      every KDE app, so an any-host allow for it allows all of them;
    - Flatpak apps report `/app/...` paths that collide across apps
      (P4.2).

#### D as implemented (2026-10-08): departures and the capture

Branch `feat/prompt-slot-curated-defaults`.
- **Data** (`crates/snitchwatch-bridge/data/curated-defaults-v1.json`,
  built into the bridge). Each entry has an id, an exact `/usr` program
  path, exactly one of one host or this computer, one port, `tcp` or `udp`,
  a plain-text `why`, and the capture `evidence`. Its rule is a list:
  - `process.path`, simple, case sensitive;
  - `dest.host` simple, or `dest.ip` regexp `^(127\.0\.0\.1|::1)$`;
  - `dest.port` simple;
  - `protocol` regexp `^tcp6?$` or `^udp6?$` (IPv4 and IPv6).

  No wildcards, no host regexps, no any-address entry.
- **Allowlist.** `curated::check_curated_rule` takes only that shape, in
  that order, with the description, `allow`, `always` and no precedence:
  a program under `/usr` but not `/usr/local`, a port of ASCII digits only.
  It then runs the `Editor` profile. It is applied to the data file (a bad
  file offers nothing; reconcile then treats recorded copies as retired),
  and at the send point.
- **One "unedited" check** (`curated::canonical`, PR #105 review M4).
  `is_unedited` compares a canonical form that applies opensnitchd
  v1.8.0's normalisations: a list's operand `list` and its `data` cleared,
  a case-insensitive regexp lowercased, `created` and `enabled` ignored.
  Reconcile, the Rules page's toggle, the toggle request and the send path
  all use it. Every regexp the data file builds is already lowercase
  (pinned by a test), since `Compile` lowercases in place.
- **Send path.** `DaemonCommands::send_curated` with a `CuratedCommand`
  (crate-private constructors, like `BlocklistCommand`). Its `CHANGE_RULE`
  carries exactly a list entry's rule, apart from `enabled` and `created`
  (security review L2). `send` still refuses the prefix, and the profile
  path (`850-profile:`, #104) and this one each refuse the other's names.
- **Opt-in, per entry.** The choices live in
  `<state>/curated-defaults.json` (version 2; version 1 is still read),
  through `state_file` (owner-only, no links, atomic replace). A recorded
  copy is the canonical form, and must be exactly its entry's rule; one
  that isn't (a crafted file, or an older list) is dropped with a warning,
  so that entry's daemon copy reads as edited. Nothing is on by default.
  Kirigami has a "Recommended background-service rules" page: a switch per
  entry, "Turn all on" and "Turn all off" (asking only for entries not
  already that way), and each entry's program, what it allows and why, as
  plain text.
- **Reconcile**, as item 13, plus:
  - Everything is as of the firewall service's last rule list: the cache
    refreshes only on a reconnect (HELLO) or a confirmed command, so a rule
    edited on disk is seen at the daemon's next reconnect.
  - **Inert** (PR #105 review H1). The bridge changes nothing (no command,
    no choice taken), says why, and reports only what the daemon has
    ("In the firewall (added earlier)", "differs from this description",
    "Not added by this Snitchwatch service") when:
    - it is the per-user bridge (its daemon link is TCP, where an
      impostor's `OK` would read "Installed"), or has no saved settings;
    - its choices file can't be read (like #104's unreadable profiles,
      with `storage.unreadable`): no file is the first run, but a
      damaged, unknown-version, foreign-mode or linked file is never read
      as "empty", which would have deleted every enabled rule;
    - a save fails mid-run.

    The Rules page's toggle of a recommended rule is refused then too.
  - Choices are taken in memory on the inbound pump and saved by the
    worker on a blocking thread, one save at a time and never an older
    version over a newer one, before any command that depends on them.
  - **No retry loop** (PR #105 re-review HIGH). The worker runs a pass
    only when something it reads changed: the rule list's revision, its
    known/withdrawn state, the daemon stream, the choices, inertness, or a
    removal asked for. A held rule list is published on release only if a
    confirmed command changed it (all `PublishHold` users, the rule import
    too). A command that failed (refused, not sent, no answer) is not sent
    again for that entry until its choice changes or the daemon reconnects;
    until then the entry keeps its failure status and text.
  - **A first run leaves the firewall alone** (re-review M1). An entry the
    user never chose (no choices file, e.g. one moved away) whose unedited
    rule is already in the firewall reads "In the firewall (added
    earlier)", with its switch on, and nothing is sent until the user turns
    it off (deleted) or on (adopted). Only an explicit "off" deletes.
  - The ids Snitchwatch installed are kept apart from the recorded copies,
    so a copy dropped as invalid still marks its rule as one Snitchwatch
    installed: gone from the daemon, it is a removal, not a reinstall
    (re-review M2).
  - Just before an install, the live rule list is checked again: a copy
    that arrived since the pass began is adopted next pass, not
    overwritten.
  - "Rule installed" only after the daemon's `OK`; the installed copy is
    recorded then. Each command is re-checked against the choices just
    before it is sent, so a choice changed mid-pass wins. A pass holds the
    rule-list broadcast (one `SetRules` per pass). Every `SetRules` and
    `UpdateRules` wakes the worker, so a withdrawn list shows "Waiting" and
    a late `OK` updates the status. An `info!` line names each entry
    installed, deleted or removed (never the rule body).
  - A rule installed earlier and missing from a committed snapshot was
    removed outside the page. It is recorded and not reinstalled until the
    user turns the entry off and on again; only an off-to-on change
    forgets the removal, so "Turn all on" doesn't (review M1). The status
    says "This rule was removed" neutrally.
  - A copy whose rule differs from the entry reads "The firewall's rule
    under this name differs from this description and still applies; see
    the Rules page." It is never deleted by reconcile, but has a
    **Remove** button (review M2): after a confirmation ("Remove the
    firewall's rule under this name? It differs from this description and
    may allow or block something else; see the Rules page."), the bridge
    sends `DELETE_RULE` for exactly that entry's reserved name, and records
    it as removed. A name too large for the bridge's list (`left_out`)
    counts as edited (security review L4). A removal asked for under a
    list that is then withdrawn is dropped.
  - A refused delete reads "The firewall service refused to remove the
    rule." and is tried again after the daemon reconnects or the user
    changes that entry.
  - Follow-up, not in this PR: a rule under the prefix for an entry no
    longer in the list, with no recorded copy, is left alone and can't be
    removed from the Recommended page (only from the daemon's own UI or
    rules folder). A "No longer recommended" list with Remove would cover
    it.
  - Not handled in v1: a later data file changing an installed entry. Its
    old copy then reads as edited and is left alone. A v2 must decide.
  - Not done: telling a removal from a race. A snapshot staged before an
    install's `OK` and committed after it can read as a removal. That is
    rare (a daemon reconnect racing an install) and fails safe: the entry
    reads "removed" and isn't reinstalled.
- **Pure toggles.** The Rules page can turn a shipped entry's rule on or
  off (wire field `toggleable`). The bridge sends the data file's rule,
  never the GUI's, and only while the daemon's copy is unedited. Adds,
  edits, renames and deletes under the prefix stay refused. An edited copy
  keeps the reserved-name reason.
- **Wire.** `ClientMessage::SetCuratedDefaults { ids, on }` and
  `RemoveCuratedDefault { id }`; `ServerMessage::SetCuratedDefaults
  { entries, storage, unavailable }`; capability `curatedDefaults`.
  Kirigami forgets a session's list when a new session starts, so a bridge
  without the capability shows "not offered".
- **VM check (r11), for the PR:** does opensnitchd v1.8.0 refuse
  `DELETE_RULE` for a rule whose file is already gone (`loader.go` `Delete`
  returns the `os.Remove` error)? Either way the entry shows the result
  and the command isn't repeated until the daemon reconnects or the user
  changes the entry. Also: the status stays "Rule installed" across a
  daemon restart (the canonical comparison against a real daemon's copy).
- **Not added: a `user.id` condition.** NetworkManager's check most likely
  runs as root, but the r10 capture records no uid and this sandbox has no
  NetworkManager unit to read. r11 should record the uid; then entries
  whose program always runs as a fixed system user can add it.
- **The capture** (bazzite-tower r10, idle and after update):

  | Capture entry | v1 | Why |
  |---|---|---|
  | `/usr/bin/NetworkManager` → `fedoraproject.org`:80, tcp/tcp6 | included | the connectivity check |
  | `/usr/bin/chronyc` → `127.0.0.1`/`::1`:323, udp/udp6 | included, this computer only | talks to chronyd |
  | `/usr/bin/flatpak` → `dl.flathub.org`:443, tcp6 | included | Flathub updates (held in r10, so the update timed out) |
  | `/usr/lib/systemd/systemd-resolved` → the network's DNS server:53, udp | **excluded, owner question S6** | the server differs per network, so only an any-address allow fits; S3 says host-constrained |
  | chronyd → NTP servers:123 | pending | chronyd was stopped in the fixture |
  | rpm-ostree, skopeo | pending r11 | r10 has only skopeo to a local image reference (`127.0.0.1`/`::1`:443) |
  | fwupd | pending r11 | not in r10 |
  | tailscaled → `log.tailscale.com` | excluded | optional service |
  | curl → `10.0.2.2`:49190; `<unknown>` → `10.0.2.2`:51820 udp | excluded | the test harness and its WireGuard check |
  | kioworker, Steam | excluded | not in the capture; Steam lives under home (no `/usr` path) |

### E. Daemon-side options (describe only; for bazzite-tower's patch)

None of these adds a UI→daemon notification action. They change daemon
internals only, so they fit tower's `{CHANGE_RULE, DELETE_RULE}`
allowlist.

- **E1. A per-connection pending set instead of the global `isAsking`
  bool** (the upstream fix for #1644).
  - Allow up to N concurrent `AskRule` calls.
  - Dedupe by (process path, destination, port, protocol), so a
    retransmit waits on the in-flight Ask instead of defaulting.
  - The bridge already handles concurrent asks: each RPC is independent
    and pending rows are keyed by `ask_id`. Kirigami would need checking
    with N > 1 pending rows (banner, tray count, notifications).
- **E2. Drop while busy instead of applying `DefaultAction`.**
  `NF_DROP` without a rule. TCP and DNS retry, and the retry is asked once
  the slot frees. That turns silent allows into a short delay. It is the
  smallest patch. Costs:
  - extra latency right after login;
  - more `dropped` in stats.
- **E3. Record default-applied connections as events.**
  - The daemon change: an `Event` with an empty or absent rule (or a
    synthetic `"<default>"` name).
  - The bridge change: `translator/connection.rs` `event_to_row` returns
    `None` without a rule today. It would map such an event to a decided
    row marked "default action".
  - This turns A's count into a real list. The daemon change is small and
    contained in `stats.go`.
- **E4. A configurable Ask timeout.** Not needed if C lands.

## Tests to write first

**A** (`prompt_slot.rs`, `grpc_server/tests.rs`):
- hold on insert and release on verdict;
- release on GUI loss, and on cancellation via `PendingCleanup` (drop the
  future);
- **The baseline is taken after the hold starts:**
  - a `rule_misses` reading from *before* the hold is not used, so misses
    from before the GUI authenticated are not counted;
  - the count is `None` until a reading after the hold exists, then
    `last − baseline`.
- **A daemon restart mid-hold** (`uptime` drops, `rule_misses` resets to
  a smaller value) gives `None` and a fresh baseline, never a huge number
  from underflow.
- **A new #48 HELLO commit mid-hold** also resets the baseline.
- one `PromptSlotSummary` when > 0, none when 0 or unknown;
- **two concurrent Asks:** releasing the first leaves the second holding,
  and each release reports its own count;
- `display_summary` sanitizes a hostile host, using the pattern of the
  `DenyScopeNarrowed` tests;
- `RequestSnapshot` includes `PromptSlot`;
- each notifier gets a test for the new `Notice` variant.

**B** (Kirigami, Qt-free where possible):
- each action id maps to the right `SetVerdict` (Deny →
  `inline_duration_for`);
- an action for a row that is gone sends nothing.

**C** (`tokio::test(start_paused = true)`):
- **Timeout.** No verdict for `ANSWER_TIMEOUT` → the reply per the chosen
  policy (P-a: `Status::unavailable`), the row is removed from pending and
  marked deferred, and the slot is released.
- **Answer just before the deadline** wins. No double settlement; use the
  `PendingCleanup`/`cancel_pending` race tests as the template.
- **"Decide later"** replies at once with the S2 policy.
- **Without an absolute path,** a P-c policy falls back to P-a (with #44
  Part A).
- **`daemon_config`:**
  - garbage, missing keys and wrong types → `None` fields;
  - a real `vendor:daemon/data/default-config.json` parses with
    `default_action = "allow"`;
  - the packaging config parses with `"deny"`.

**D:**
- every curated entry passes `rule_policy` and has an absolute path;
- every curated name starts with `snitchwatch-default-`, and the
  P2.7/P2.1 profile layers refuse that prefix for authored rules;
- reconcile runs on a `synced()` bump and on an opt-in toggle, and not
  while the cache is `Unknown`;
- **opt-out:** removes unedited copies only. An edited copy survives,
  flagged;
- with D's own prefix check (when it lands before P2.7/P2.1), an `AddRule`
  named `snitchwatch-default-x` is refused and the mock receives nothing;
- reconcile with the mock daemon:
  - install sends only `CHANGE_RULE`;
  - removal sends only `DELETE_RULE`;
  - a user-edited entry is left alone.

## Verification

Run at low priority (`nice -n 19`), one crate at a time:
- `cargo test -p snitchwatch-bridge`
- `cargo test -p snitchwatch-integration-tests --test bridge_protocol`
- `cargo test -p snitchwatch-tauri notifier`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test -p snitchwatch-kirigami` (offscreen)

Tower VM checks:
1. **Log in to a fresh image with the GUI autostarting.**
   - The banner names the first background program holding the slot.
   - After a later ping, it shows "at least N" defaulted connections.
2. **A:** answer the prompt; the summary notice appears once.
3. **B:** with the window hidden, Deny from the notification. The
   inline-deny acceptance check holds.
4. **C (once decided):** leave a prompt unanswered. At `ANSWER_TIMEOUT`
   the slot frees, the row shows the outcome, and "Make a rule…" works.
5. Record the image's `DefaultAction` with every result.

## Risks

- **The count is a late, rough lower bound.**
  - It misses whatever was defaulted before the first post-hold reading.
  - It can include a few non-busy misses (a requeue timeout, a failed
    Ask).
  - It arrives only with a ping that carries a matched event.

  The wording says "at least"; never present it as exact.
- **A timeout can preempt a slow user.** Mitigation: a visible countdown,
  and the "Make a rule…" follow-up on the timed-out row.
- **Notification actions answer prompts outside the main window.** This
  adds no trust boundary:
  - any same-uid process can already read the WS token
    (`$XDG_RUNTIME_DIR/snitchwatch/`, 0600) and send `SetVerdict`;
  - `notify-rust`'s `ActionInvoked` matching should still get a
    `security-reviewer` look.
- **Curated allows widen trust** for whole programs, `kioworker`
  especially. That is why D is opt-in in the recommendation.
- **File-conflict hot spots:**
  - `grpc_server.rs` `ask_rule` (A, C), with #44 Part A and #48;
  - `ping`, with P2.6 hit counts;
  - `subscribe` (`daemon_config`), with #48;
  - `notice.rs` and the three notifier sites, with #44 Part A;
  - `main.qml` banner;
  - `ConnectionsPage.qml`, with the inline-Deny plan.

## OWNER QUESTIONS

- **S1. Silent auto-answer** for a prompt nobody answers. Choose on/off,
  the timeout, and the reply:
  - P-a: daemon default, no rule;
  - P-b: once-deny;
  - P-c: app-bound 5 min deny.

  **Recommendation:** on, 30 s (stock-UI parity), **P-a**. It makes no
  decision for the user.
  - Under `DefaultAction: allow`, it is today's outcome, 4× sooner.
  - Under `deny` (the shipped packaging config), the timed-out SYN is
    dropped. Its retransmit re-asks within about 1 s, the same ping-pong
    as P-b, so the same program can take the slot straight back.
  - Choose with that in view. P-c avoids the ping-pong at the price of a
    5 min block nobody chose.

  **DECIDED (owner, 2026-10-08): as recommended (on, 30 s, P-a).** An
  unanswered prompt is answered ONCE after 30 s with the daemon's
  `DefaultAction` for that connection, and listed in Connections.
- **S2. "Decide later" button semantics.** Options:
  - (a) P-a;
  - (b) block this program for 5 min (P-c, any host) and list it under
    "Decide later";
  - (c) allow this program for 5 min.

  **Recommendation: (b).** It is the only option that stops a background
  program from taking the slot right back, and it fails closed.

  **DECIDED (owner, 2026-10-08): (b).** "Decide later" blocks that program
  for 5 minutes, then asks again.
- **S3. Curated defaults.** Decide:
  - opt-in (an onboarding checkbox) or on by default;
  - which programs (from the capture spike);
  - whether `kioworker`/Steam get any-host or host-constrained allows;
  - whether per-user install paths may use a `process.path` regexp,
    which bends #44's absolute-path decision.

  **Recommendation:** opt-in; system paths under `/usr` only in v1;
  host-constrained `kioworker`/Steam; no per-user regexps in v1.

  **DECIDED (owner, 2026-10-08): as recommended.** Curated defaults for
  background services are opt-in, `/usr` paths only. (The rest of the
  recommendation stands with it: host-constrained `kioworker`/Steam, no
  per-user regexps in v1.)
- **S4. May we ask bazzite-tower for E2** ("drop while busy")?
  - E2 changes daemon behaviour under both `DefaultAction` values, which
    is why it needs your OK.
  - The rest needs no decision: we ask tower for E3 (visibility only, no
    behaviour change), and offer E1 upstream as the #1644 fix.
  - **Recommendation:** yes, after VM timing at login.

  **DECIDED (owner, 2026-10-08): yes.** Ask bazzite-tower for E2 ("drop
  while busy") as a daemon option, **OFF by default**. E3 (default-applied
  events) was requested too. Both were sent to tower on 2026-10-08.
- **S5 (borderline). Answering from desktop notifications.** May the
  notification carry Allow-once/Deny, or only "Review"?
  **Recommendation:** Allow-once and Deny; never a remembered Allow from
  a notification.

  **DECIDED (owner, 2026-10-08): yes.** Answer from desktop notifications
  with Allow once and Deny only.
- **#78. Pausing filtering while prompts are waiting.** Not one of the
  questions above. It is recorded here because it shares the
  prompt-answering path.

  **DECIDED (owner, 2026-10-08).** Pausing auto-answers every waiting
  prompt with Allow once, through the same path as the Allow-once button,
  and never persisted as a rule.
  - Rows answered this way are labelled "Allowed once (filtering was
    paused)".
  - The pause menu items say so.
  - PR #86's warning stays as a fallback.
- **S6 (new, from D). A DNS allow for `systemd-resolved`.** On a deny
  default nothing resolves until the resolver's upstream queries are
  allowed (`packaging/README.md` says so for the blocklist fetch too). The
  network's DNS server changes from network to network, so the only rule
  that fits is `/usr/lib/systemd/systemd-resolved` to any address on UDP
  port 53. S3 asks for host-constrained rules, so v1 leaves it out.
  Options:
  - (a) add it as an opt-in entry ("any address, DNS port only");
  - (b) ship it in packaging instead, like the fetch rule;
  - (c) leave DNS to the user.

  **OPEN.**
