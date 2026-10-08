# Prompt-slot UX: living with opensnitchd's single prompt

**Date:** 2026-10-08
**Issue:** #17 (upstream evilsocket/opensnitch#1644). Overnight board item
9b. Roadmap P2.4 (curated defaults) is pulled forward in part.
**Baseline:** `main` @ `4b3ba52` (#48 merged). Part D also needs the
security PR's `rule_policy.rs` (branch `fix/rule-operator-validation`).
**Size:** M overall, as four bridge/UI PRs (A–D) plus a daemon-side note
(E).
- A and B need no owner decision beyond wording.
- C and D are blocked on the owner questions at the end.
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

### B. Answer from the notification (UI, S; owner question S5 confirms)

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

### C. Bridge auto-answer and "Decide later" (BR + UI, M; blocked on S1/S2)

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
    - The paused branch (#47) answers before insertion.
    - GUI loss still returns `Unavailable`.
    - The 120 s daemon deadline can no longer be reached.

### D. Curated defaults for background services (data + BR, M; blocked on S3, the security PR's `rule_policy.rs`, and the reserved-prefix refusal)

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
      - **a user-edited copy is never deleted, not even on opt-out.** That
        means same name, but some field other than `enabled` differs from
        the data file after list-operand normalisation. It is left alone
        and flagged ("Edited by you; Snitchwatch won't change it").
        Deleting it could silently undo a change the user chose, such as
        narrowing a curated allow's scope;
      - nothing outside the prefix is ever deleted.
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
- **S2. "Decide later" button semantics.** Options:
  - (a) P-a;
  - (b) block this program for 5 min (P-c, any host) and list it under
    "Decide later";
  - (c) allow this program for 5 min.

  **Recommendation: (b).** It is the only option that stops a background
  program from taking the slot right back, and it fails closed.
- **S3. Curated defaults.** Decide:
  - opt-in (an onboarding checkbox) or on by default;
  - which programs (from the capture spike);
  - whether `kioworker`/Steam get any-host or host-constrained allows;
  - whether per-user install paths may use a `process.path` regexp,
    which bends #44's absolute-path decision.

  **Recommendation:** opt-in; system paths under `/usr` only in v1;
  host-constrained `kioworker`/Steam; no per-user regexps in v1.
- **S4. May we ask bazzite-tower for E2** ("drop while busy")?
  - E2 changes daemon behaviour under both `DefaultAction` values, which
    is why it needs your OK.
  - The rest needs no decision: we ask tower for E3 (visibility only, no
    behaviour change), and offer E1 upstream as the #1644 fix.
  - **Recommendation:** yes, after VM timing at login.
- **S5 (borderline). Answering from desktop notifications.** May the
  notification carry Allow-once/Deny, or only "Review"?
  **Recommendation:** Allow-once and Deny; never a remembered Allow from
  a notification.
