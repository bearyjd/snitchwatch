//! Source-level invariants for the verdict click path in `ConnectionsPage.qml`.
//!
//! These are deliberately crude text assertions, and they exist because the
//! two richer harnesses each miss this:
//!
//!   * The cxx-qt probes (`inline_verdict_qml.rs`) drive `submitInlineVerdict`
//!     directly — cxx-qt-lib exposes no way to synthesise a mouse event, so
//!     they never exercise a Button's handlers at all.
//!   * `tests/qml/tst_delegate_input.qml` *can* synthesise a real click, but
//!     `qmltestrunner` cannot load `com.snitchwatch.shell` (cxx-qt links those
//!     types statically; there is no QML plugin on disk), so it tests a
//!     structural mirror rather than the real page.
//!
//! That leaves a gap exactly where a regression already happened once, so it
//! gets a cheap guard rather than no guard.

/// The Connections page and the components split out of it (monitor-tree step
/// 1): guards that used to scan the single page file scan the union, so moving
/// code between these files never silently drops a check.
const CONNECTIONS_PAGE: &str = concat!(
    include_str!("../qml/InlineVerdicts.qml"),
    "\n",
    include_str!("../qml/ClipboardCopy.qml"),
    "\n",
    include_str!("../qml/ConnectionDetailsCopy.qml"),
    "\n",
    include_str!("../qml/ConnectionExplanation.qml"),
    "\n",
    include_str!("../qml/RuleScopePreview.qml"),
    "\n",
    include_str!("../qml/ConnectionsPage.qml"),
    "\n",
    include_str!("../qml/ConnectionRow.qml"),
    "\n",
    include_str!("../qml/ConnectionActions.qml"),
    "\n",
    include_str!("../qml/ConnectionsHeader.qml"),
    "\n",
    include_str!("../qml/ConnectionsFilterBar.qml"),
    "\n",
    include_str!("../qml/ConnectionsTopBar.qml"),
    "\n",
    include_str!("../qml/InspectorState.qml"),
    "\n",
    include_str!("../qml/HoverTip.qml"),
    "\n",
    include_str!("../qml/ProgramLabel.qml"),
    "\n",
    include_str!("../qml/GroupVerdictCounts.qml"),
    "\n",
    include_str!("../qml/GroupPendingBadge.qml"),
    "\n",
    include_str!("../qml/BackgroundGroupBar.qml"),
    "\n",
    include_str!("../qml/InspectorPanel.qml"),
    "\n",
    include_str!("../qml/ConnectionDetailPage.qml"),
    "\n",
    include_str!("../qml/ConnectionList.qml"),
    "\n",
    include_str!("../qml/RuleButton.qml"),
    "\n",
    include_str!("../qml/LevelRuleActions.qml"),
    "\n",
    include_str!("../qml/MapBand.qml"),
    "\n",
    include_str!("../qml/MapView.qml"),
    "\n",
    include_str!("../qml/MapHierarchy.qml"),
    "\n",
    include_str!("../qml/MapDrilldown.qml"),
    "\n",
    include_str!("../qml/MapDestinationsPopup.qml"),
    "\n",
    include_str!("../qml/MapCamera.qml"),
    "\n",
    include_str!("../qml/MapNavigation.qml"),
    "\n",
    include_str!("../qml/MapMarker.qml"),
    "\n",
    include_str!("../qml/MonitorPane.qml"),
);
const MAIN_QML: &str = include_str!("../qml/main.qml");

/// Drop whole-line `//` comments so a guard can't trip over prose that merely
/// *names* the thing it forbids — the doc comments below deliberately discuss
/// `TapHandler`, and an unfiltered substring scan flags itself.
///
/// Only lines whose trimmed form starts with `//` are removed, so no code line
/// is ever touched (a trailing comment after code keeps its code intact, and a
/// `//` inside a string literal is left alone).
fn code_lines(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A `TapHandler` alongside `onClicked` on a `Controls.Button` dispatches
/// TWICE for a single click — measured with QtTest `mouseClick()` against this
/// delegate's exact shape, identical on the Basic/Fusion/Material/Universal
/// styles (click logic lives in the shared `QQuickAbstractButton` base, not in
/// a style). `tests/qml/tst_delegate_input.qml` pins that measurement.
///
/// The verdict buttons carried both for a while, on the theory that a nested
/// action needs its own pointer grab to beat the delegate. That was true of the
/// original `Controls.ItemDelegate` root, but the delegate is now a plain
/// `Item` with the row's `MouseArea` at `z: 0` beneath the content — plain
/// `onClicked` wins on its own, and the second handler only bought a
/// double-submit of every pending row under a process group.
#[test]
fn verdict_buttons_carry_no_redundant_tap_handler() {
    assert!(
        !code_lines(CONNECTIONS_PAGE).contains("TapHandler"),
        "ConnectionsPage.qml gained a TapHandler. On a Controls.Button that \
         double-dispatches with onClicked (one click -> two handler runs), which \
         previously let \"Allow all\" submit every pending verdict twice. The row's \
         MouseArea sits at z:0 below the content, so onClicked already wins the \
         grab — no extra pointer handler is needed. If you genuinely need one, \
         remove the sibling onClicked and update tests/qml/tst_delegate_input.qml."
    );
}

/// Every verdict button must dispatch through `row.decideOnce(...)`, which owns
/// the `row.submitted` latch. When the guard was written inline at each call
/// site instead, two of the four sites silently lacked it.
#[test]
fn verdict_buttons_all_dispatch_through_the_single_guard() {
    let code = code_lines(CONNECTIONS_PAGE);
    let dispatches = code.matches("onClicked: row.decideOnce(").count();
    assert_eq!(
        dispatches, 5,
        "expected exactly 5 verdict buttons dispatching via row.decideOnce() \
         (inline Allow/Deny/Decide later + batch Allow all/Deny all), found {dispatches}. If a \
         button was added or removed, update this count; if one now dispatches \
         some other way, it has bypassed the re-entry guard."
    );

    let latches = code.matches("row.submitted = true").count();
    assert_eq!(
        latches, 1,
        "`row.submitted = true` must be set in exactly one place (inside \
         decideOnce), found {latches}. A second latch site means the re-entry \
         guard has been duplicated, which is how it previously ended up missing \
         from the batch handlers."
    );
    // Delegate lifetime no longer owns context menus. The real-click probe
    // verifies both entry points share InlineVerdicts' per-pending-id guard
    // through model resets and delegate replacement.
    assert!(code.contains("page.submitInlineVerdict(n.rowId, choice, true)"));
    assert!(code.contains("actions.page.inlineVerdicts.submit(n.rowId, choice, true)"));
}

/// Issue #31: the pending-count badge used to fill with `neutralBackgroundColor`
/// under a `neutralTextColor` label. Under the Fusion/Basic QQC2 styles those two
/// roles resolve to the identical orange, so the "N pending" text vanished into
/// its own fill (Breeze happened to give the background role a pale tint, which
/// is why this went unnoticed on Plasma). Fixed by dropping the fill in favor of
/// a border-only outline — this guard stops the fill from coming back.
#[test]
fn pending_badge_does_not_fill_with_neutral_background_color() {
    assert!(
        !code_lines(CONNECTIONS_PAGE).contains("neutralBackgroundColor"),
        "ConnectionsPage.qml reintroduced neutralBackgroundColor as a fill. Under \
         Fusion/Basic this resolves to the same color as neutralTextColor, making \
         the pending-count badge's \"N pending\" label unreadable (issue #31). Use \
         a border-only outline instead of relying on the two neutral roles to \
         contrast."
    );
}

/// Issue #17 / evilsocket/opensnitch#1644 mitigation: the pending-decision-
/// exposure banner must stay wired to `oldestPendingAgeSecs`, bounded below
/// by the response-latency threshold AND above by opensnitchd's own 120s
/// AskRule timeout (past that, the exposure window is already closed —
/// see the ceiling's doc comment in `main.qml`), and the poll `Timer` that
/// keeps it live (elapsed wall-clock time changes with no new bridge
/// message to trigger a recompute) must keep calling `refreshPendingAge()`
/// on a real repeating cadence, only while something is actually pending.
/// Any of these silently detaching would leave the banner permanently
/// hidden, frozen at a stale value, or (for the missing ceiling) stuck on
/// forever making a claim that's gone false — with no other test-visible
/// symptom. See
/// docs/superpowers/plans/2026-08-05-pending-decision-exposure-warning.md.
///
/// Matches on operands separately (not the whole `visible:` line as one
/// string) so a `qmlformat` rewrap doesn't trip this over cosmetic-only
/// changes.
#[test]
fn pending_exposure_banner_stays_wired_to_oldest_pending_age() {
    let code = code_lines(MAIN_QML);
    let ticker = code_lines(include_str!("../qml/PendingAgeTicker.qml"));
    assert!(
        code.contains("pendingAgeSecs >= pendingAgeThresholdSecs")
            && code.contains("pendingAgeSecs < pendingAgeCeilingSecs"),
        "main.qml's pendingExposureBanner is no longer bounded by both \
         [pendingAgeThresholdSecs, pendingAgeCeilingSecs) — either it stopped \
         reflecting real pending-decision age, or lost the upper bound that stops it \
         warning forever past opensnitchd's own 120s AskRule timeout."
    );
    assert!(
        code.contains("pendingAgeThresholdSecs: 10"),
        "pendingExposureBanner's response-latency threshold changed — if intentional, \
         update this guard to match the new value."
    );
    assert!(
        code.contains("pendingAgeCeilingSecs: 120"),
        "pendingExposureBanner's ceiling no longer matches opensnitchd's 120s AskRule \
         timeout (vendor/opensnitch/daemon/ui/client.go:366) — if that upstream timeout \
         ever changes, update this guard to match it, not just main.qml."
    );
    assert!(
        code.contains("PendingAgeTicker { connections: root.connectionsModelRef }")
            && ticker.contains("running: ticker.connections.oldestPendingAgeSecs >= 0")
            && ticker.contains("oldestPendingAgeSecs >= 0 || ticker.connections.heldBackShown")
            && ticker.contains("repeat: true")
            && ticker.contains("ticker.connections.refreshPendingAge()"),
        "main.qml's pending-age poll Timer is no longer wired the way the banner needs: \
         running only while something is ACTIVELY pending (oldestPendingAgeSecs >= 0, not \
         merely pendingCount > 0 — a Codex review finding: gating on pendingCount alone \
         ticks forever once a permanently-stuck/expired row exists, since such a row never \
         resolves and never leaves pendingCount either), repeating, and calling \
         refreshPendingAge() each tick — oldestPendingAgeSecs would go stale between \
         bridge messages otherwise, since elapsed wall-clock time advances with nothing \
         to trigger a recompute. It also keeps running while a held-back notice count \
         is shown (heldBackShown, 0d part 2), so that count's hour still ends on a row \
         unanswered past 120 s."
    );
}

/// LS redesign phase 1, owner decision: the alert is its own window, so a new
/// prompt never raises the main window (not in "Ask me", where the alert
/// asks, and not in silent mode, which answers without interrupting). Only the
/// tray and a notification's "Review" click bring the main window back.
#[test]
fn a_new_prompt_never_raises_the_main_window() {
    let main = code_lines(MAIN_QML);
    assert!(
        !main.contains("onPendingCountChanged") && !main.contains("noteRaise"),
        "main.qml reacts to a new prompt again; the alert window handles prompts"
    );
    let review = main
        .split("function onReviewRequested()")
        .nth(1)
        .and_then(|rest| rest.split('}').next())
        .expect("main.qml no longer handles a notification's Review click");
    assert!(
        review.contains("root.raiseAndActivate();"),
        "the Review click no longer raises the window: {review}"
    );
    assert!(
        !include_str!("../src/notification_controller.rs").contains("noteRaise"),
        "NotificationController still exposes the removed noteRaise"
    );
}

const DURATIONS_QML: &str = include_str!("../qml/Durations.qml");
const SILENT_MODES_QML: &str = include_str!("../qml/SilentModes.qml");
const ALERT_HOST_QML: &str = include_str!("../qml/AlertHost.qml");
const ALERT_WINDOW_QML: &str = include_str!("../qml/AlertWindow.qml");
const SHEET_QML: &str = include_str!("../qml/PendingDecisionSheet.qml");
const MAKE_RULE_QML: &str = include_str!("../qml/MakeRuleSheet.qml");
const INLINE_QML: &str = include_str!("../qml/InlineVerdicts.qml");

/// The `token: "..."` values of a QML list, in order.
fn tokens(source: &str) -> Vec<String> {
    code_lines(source)
        .split("token: \"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .map(str::to_string)
        .collect()
}

/// LS redesign phase 1, owner decision 1: Once / 5 minutes / Until firewall restarts /
/// Forever, defined once (`Durations.qml`) and used by every surface. The QML
/// list, the Rust vocabulary and the settings default must agree.
#[test]
fn the_duration_list_is_defined_once_and_matches_rust() {
    assert_eq!(
        tokens(DURATIONS_QML),
        snitchwatch_kirigami::settings::DURATION_TOKENS
            .iter()
            .map(|t| t.to_string())
            .collect::<Vec<_>>()
    );
    for label in ["Once", "5 minutes", "Until firewall restarts", "Forever"] {
        assert!(
            DURATIONS_QML.contains(&format!("label: \"{label}\"")),
            "Durations.qml lost the {label:?} label"
        );
    }
    // The label Rust puts on a notification's Allow is the QML list's.
    for token in snitchwatch_kirigami::settings::DURATION_TOKENS {
        let label = snitchwatch_kirigami::settings::duration_label(token);
        assert!(
            DURATIONS_QML.contains(&format!("label: \"{label}\", token: \"{token}\"")),
            "settings::duration_label({token:?}) = {label:?} drifted from Durations.qml"
        );
    }
    assert!(
        DURATIONS_QML.contains(&format!(
            "property string defaultToken: \"{}\"",
            snitchwatch_kirigami::settings::DEFAULT_DURATION
        )),
        "the QML default and settings::DEFAULT_DURATION drifted"
    );
    for (name, source) in [
        ("PendingDecisionSheet.qml", SHEET_QML),
        ("AlertWindow.qml", ALERT_WINDOW_QML),
        ("MakeRuleSheet.qml", MAKE_RULE_QML),
    ] {
        let code = code_lines(source);
        assert!(
            code.contains("Durations."),
            "{name} has its own duration list"
        );
        for stale in [
            "until_quit",
            "Until Restart",
            "For 5 minutes",
            "\"This time\"",
        ] {
            assert!(!code.contains(stale), "{name} still offers {stale:?}");
        }
    }
}

/// The silent modes are defined once and match `settings::SilentMode`.
#[test]
fn the_silent_mode_list_matches_rust() {
    use snitchwatch_kirigami::settings::SilentMode;
    assert_eq!(
        tokens(SILENT_MODES_QML),
        [SilentMode::Off, SilentMode::Allow, SilentMode::Deny].map(|m| m.token().to_string())
    );
    assert!(SILENT_MODES_QML.contains("readonly property string recommended: \"allow\""));
    // The honest limit of silent mode is stated wherever it is chosen.
    assert!(SILENT_MODES_QML.contains("When Snitchwatch is not running at all"));
    assert!(SILENT_MODES_QML.contains("the firewall's own default action applies"));
}

/// Closing the window still only hides it to the tray (`main.qml`), and that
/// changes nothing about prompts: the alert is its own window. `AlertHost`
/// reports only the silent mode to Rust (never the window's visibility) and
/// shows the alert whenever silent mode is off.
#[test]
fn a_hidden_main_window_does_not_change_how_prompts_are_handled() {
    let main = code_lines(MAIN_QML);
    let closing = main
        .split("onClosing")
        .nth(1)
        .and_then(|rest| rest.split("globalDrawer").next())
        .expect("main.qml has no onClosing");
    assert!(
        closing.contains("close.accepted = false") && closing.contains("root.hide()"),
        "closing the window must still only hide it: {closing}"
    );
    let host = code_lines(ALERT_HOST_QML);
    assert!(
        host.contains("setSilentState(host.mode)") && !host.contains("windowShown"),
        "AlertHost must report only the mode to Rust, not the window's visibility"
    );
    assert!(
        host.contains("active: host.mode === \"\" || host.mode === \"off\""),
        "the alert must show whenever silent mode is off, whatever the main window does"
    );
    assert!(main.contains("AlertHost {"), "main.qml lost the AlertHost");
    assert!(
        include_str!("../src/bridge_feed.rs").contains("#[cxx_name = \"setSilentState\"]"),
        "BridgeFeed no longer exposes setSilentState"
    );
}

/// Inline Allow follows the Settings duration instead of hard-coding
/// `this_time` (the bug: the daemon stores only `always` rules).
#[test]
fn inline_allow_sends_the_settings_duration() {
    let code = code_lines(INLINE_QML);
    assert!(
        code.contains("inlineDurationFor(rowId, choice, appBound, Durations.defaultToken)"),
        "InlineVerdicts no longer passes the Settings duration"
    );
    assert!(
        include_str!("../src/notification_actions.rs")
            .contains("allow_token(kind, allow_duration)"),
        "notification Allow no longer follows the Settings duration"
    );
}

/// `ListView.*` attached properties exist only on the delegate root. Inside a
/// child item (the row's `MouseArea`) an unqualified `ListView.view` is null, so
/// the click handler throws and the inspector never opens. Everything but the
/// root's own `width:` binding must go through the root id (`row.ListView.`).
#[test]
fn connection_row_reaches_listview_only_through_the_delegate_root() {
    let code = code_lines(include_str!("../qml/ConnectionRow.qml"));
    assert!(
        code.contains("row.ListView.view.currentIndex = row.index"),
        "the row click no longer selects its row through the delegate root"
    );
    for line in code.lines() {
        let rest = line.replace("row.ListView.", "");
        if rest.contains("ListView.") {
            assert!(
                line.trim_start().starts_with("width: ListView.view ?"),
                "unqualified ListView attached property outside the delegate root: {line}"
            );
        }
    }
}

/// 0a: the Connections list shows the friendly program label through
/// `ProgramLabel.qml`, which flags a label that cannot be trusted with a
/// marker, the path tail and a real-path tooltip, all plain text. The leaf and
/// program-header labels no longer show the raw `process` / `groupLabel`.
#[test]
fn connection_row_shows_the_friendly_label_with_its_real_path() {
    let row = code_lines(include_str!("../qml/ConnectionRow.qml"));
    for needed in [
        "required property string programLabel",
        "required property bool programLabelUnverified",
        "required property string programPathTail",
        "required property string programPathFull",
        "src: row",
    ] {
        assert!(row.contains(needed), "ConnectionRow.qml lost: {needed}");
    }
    assert_eq!(
        row.matches("ProgramLabel {").count(),
        2,
        "leaf and program header"
    );
    assert!(
        !row.contains("text: row.process\n") && !row.contains("text: row.groupLabel\n"),
        "a leaf or program-header label shows the raw name again"
    );
    let part = code_lines(include_str!("../qml/ProgramLabel.qml"));
    for needed in [
        "text: part.src.programLabel + (part.programUnverified ? \" ?\" : \"\")",
        "text: part.src.programPathTail",
        "part.src.programPathFull",
        "text: part.fullPath",
    ] {
        assert!(part.contains(needed), "ProgramLabel.qml lost: {needed}");
    }
    assert_eq!(
        part.matches("onlyWhen: part.programUnverified").count(),
        2,
        "the name and the tail each need the unverified-only real-path HoverTip"
    );
}

/// Learning mode phase L1: the fail-closed end reaches the silent-answer state
/// by two routes that must both stay wired. `AlertHost` pushes the (already
/// overridden) `silentMode` property through `setSilentState`, whose Rust side
/// lets the forced-mode override win, and a timer runs the expiry check.
#[test]
fn the_learning_end_stays_wired_into_the_silent_state_push() {
    let host = code_lines(ALERT_HOST_QML);
    assert!(
        host.contains(
            "readonly property string mode: host.settings ? host.settings.silentMode : \"\""
        ) && host.contains("host.bridgeFeed.setSilentState(host.mode)"),
        "AlertHost must push settingsController.silentMode (the possibly overridden mode)"
    );
    assert!(
        host.contains("interval: 60000")
            && host.contains("running: host.settings !== null")
            && host.contains("host.settings.tickLearning()"),
        "AlertHost lost the learning expiry timer"
    );
    let feed = include_str!("../src/bridge_feed.rs");
    let push = feed
        .split("fn set_silent_state(&self")
        .nth(1)
        .and_then(|rest| rest.split("fn start_bridge_feed").next())
        .expect("bridge_feed.rs has no set_silent_state");
    assert!(
        push.contains("forced_or(mode)")
            && push.find("forced_or(mode)") < push.find("silent_mode::set_state"),
        "set_silent_state must apply the forced-mode override before setting the state"
    );
    let controller = include_str!("../src/settings_controller.rs");
    for needed in [
        "settings_learning::startup(",
        "SETTINGS_WRITE.transact(",
        "settings_learning::tick(",
        "SETTINGS_WRITE.honor(",
        "apply_if_newest(",
        "SETTINGS_WRITE.claim(",
        "settings_write_failed",
    ] {
        assert!(
            controller.contains(needed),
            "settings_controller.rs lost {needed}"
        );
    }
    assert!(
        !controller.contains("write_settings("),
        "every settings write must go through the SETTINGS_WRITE transaction"
    );
}
