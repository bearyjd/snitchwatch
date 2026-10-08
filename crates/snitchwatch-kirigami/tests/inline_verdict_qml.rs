//! Integration smoke: issue #18's inline Allow/Deny buttons + per-process
//! batch actions on `ConnectionsPage.qml`.
//!
//! Scope of this test — same convention as `connections_page_diagnostics_qml.rs`:
//! it instantiates the *real* `ConnectionsPage.qml`, feeds it a pending row via
//! the real `ConnectionsModel`, and drives the exact functions the inline
//! Allow/Deny buttons and the process-header "Allow all"/"Deny all" buttons
//! call (`submitInlineVerdict` / `submitBatchVerdict`) — the same click-path
//! entry points wired into the delegate's `onClicked` handlers. This is a
//! click-path exercise (handler -> `bridgeFeed.submitVerdict` -> recorded
//! tokens), not a synthesized mouse event.
//!
//! **The probe must supply a non-null `bridgeFeed`.** `submitInlineVerdict`
//! is wrapped in a `page.bridgeFeed !== null` guard, and `submitBatchVerdict`
//! funnels through it, so a null feed skips the entire verdict path and
//! leaves this test asserting nothing beyond "the page parsed." The stub
//! below records every call (reset before each step), and the `Timer`
//! asserts what each inline and batch path sent (plan
//! `2026-10-08-inline-deny-until-restart.md`):
//!   * scope is always "this_host";
//!   * Allow is always "this_time";
//!   * Deny is "until_quit" (daemon "until restart") only for a row whose
//!     program file is an absolute path AND whose bridge session advertised
//!     app-bound rules (the stub's `appBoundRulesFor`); otherwise
//!     "this_time", with a passive notification saying why (the program, or
//!     the old bridge), once per click — once for a whole "Deny all";
//!   * the row Deny and "Deny all" tooltips and `Accessible.description`
//!     say which of these a click does;
//!   * the decision sheet labels `until_quit` "Until firewall restarts" and
//!     shows the one-time-Deny hint only while "This time" is selected, for
//!     a bindable program on a capable bridge.
//!
//! Honoring the "QML-side JS asserts are not load-bearing" constraint, this
//! test's Rust-side assertion works in two layers:
//!
//!   1. The probe root is a real, visible `Controls.ApplicationWindow` (not
//!      a bare, unparented `ConnectionsPage`; it records
//!      `showPassiveNotification` instead of drawing), and the whole scene is driven
//!      through a real Qt event loop via `QGuiApplication::exec()` (a QML
//!      `Timer` quits it once the click paths have run) — the offscreen QPA
//!      platform supports this without a real display. That matters because
//!      `QQuickListView` only instantiates its delegates (and evaluates
//!      their bindings, e.g. the new "Allow all (" + row.groupPending + ")"
//!      label expression) during layout/polish passes that happen on the
//!      event loop, not synchronously during `load_data`. Without pumping
//!      the loop, a broken delegate-local binding would never actually run
//!      and this test would falsely pass.
//!   2. Around the `load_data`/`exec()` window, fd 2 (stderr) is redirected
//!      via `libc::dup`/`dup2` into a tempfile (Qt's default message handler
//!      writes `qWarning`/`qCritical` — including QML JS exceptions — to
//!      stderr via the C `FILE*` stream, which respects fd redirection).
//!      After restoring stderr, the captured text is asserted to contain no
//!      line naming this probe's own QML URL or any of the shell's own QML
//!      files (`ConnectionsPage.qml`, its delegates, the decision sheet): a
//!      QML JS error or binding warning (e.g.
//!      `TypeError: Property 'submitInlineVerdict' ... is not a function`,
//!      or a broken binding in the new buttons) is reported by Qt with that
//!      URL as a prefix, so this positively fails on a broken click path
//!      rather than only checking "did the process not crash." A genuinely
//!      null root object (a QML *parse* error, e.g. a syntax error or an
//!      unregistered/misspelled type — as opposed to a JS runtime error
//!      inside a handler, which does NOT null the root) is asserted
//!      separately below, mirroring `connections_page_diagnostics_qml.rs`.
//!
//! The produced `SetVerdict` JSON's *content* for each token is asserted
//! Qt-free by `inline_deny.rs`'s `inline_tokens_reach_the_wire_*` test and
//! by
//! `connections::grouping::tests::pending_row_ids_compose_into_valid_batch_deny_messages`
//! (the batch-action tokens). Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/inline_verdict_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";

#[test]
fn inline_and_batch_verdict_click_paths_run_without_erroring() {
    init_headless_qt_env();
    // Deliberately NOT setting QT_FATAL_WARNINGS: a pre-existing upstream
    // Kirigami OverlaySheet binding-loop warning would abort the whole test
    // binary under it, unrelated to anything this test is checking.

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Controls as Controls
import com.snitchwatch.shell

Controls.ApplicationWindow {
    id: probeWindow
    visible: true
    width: 800
    height: 600

    readonly property string sentence: "Snitchwatch couldn't identify this program's file, so this answer applies only to this connection."
    readonly property string oldBridge: "This firewall bridge is too old to block just this program, so Deny applies to this connection only."
    readonly property string untilRestart: "Blocks this program from this host until the firewall restarts"
    readonly property string allUntilRestart: "Blocks this program from each of these hosts until the firewall restarts"
    readonly property string notSent: "The connection to the background service was lost, so this Deny wasn't sent."
    readonly property string disconnected: "The connection to the background service was lost, so Deny can't be sent."
    // The row delegate re-checked after the feed's state flips.
    property var absDelegate: null
    property var shown: []
    // `shown.length` after the last `expectShown`.
    property int shownMark: 0
    property var failures: []
    property int phase: 0

    // Kirigami.ApplicationWindow's API, recorded instead of drawn.
    function showPassiveNotification(message, timeout) {
        probeWindow.shown.push(message);
    }

    // Recording stand-in for BridgeFeed. `ConnectionsPage.bridgeFeed` is a
    // plain `var`, so any QObject exposing `submitVerdict` satisfies the
    // page's null guard — and satisfying it is the whole point: with a null
    // feed, submitInlineVerdict returns early and nothing downstream runs.
    // `appBound` stands for what the row's bridge session advertised, `ok`
    // for the polled connection state, `queue` for whether a verdict could be
    // queued (BridgeFeed.submitVerdict's result).
    QtObject {
        id: feedStub
        property int allowCount: 0
        property int denyCount: 0
        property var submitted: []
        property bool appBound: true
        property bool ok: true
        property bool queue: true

        function submitVerdict(rowId, choice, scope, duration) {
            if (choice === "allow") {
                feedStub.allowCount++;
            } else if (choice === "deny") {
                feedStub.denyCount++;
            }
            feedStub.submitted.push({ rowId: rowId, choice: choice, scope: scope,
                                      duration: duration });
            return feedStub.queue;
        }
        function appBoundRulesFor(rowId) {
            return feedStub.appBound;
        }
    }

    ConnectionsPage {
        id: page
        anchors.fill: parent
        bridgeFeed: feedStub
        model: ConnectionsModel {
            id: connModel
            Component.onCompleted: {
                setGroupedMode(true);
                applyServerMessageJson(JSON.stringify({
                    action: "insertConnectionRows",
                    rows: [
                        { id: "r1", process: "curl", processPath: null, dstHost: "github.com",
                          dstIp: "1.1.1.1", dstPort: 443, protocol: "tcp", direction: "outgoing",
                          action: null, bytesSent: 0, bytesReceived: 0, startedAtMs: 0,
                          matchedRule: null }
                    ]
                }));
            }
        }
        Component.onCompleted: {
            // Exercise the inline-button click path directly (the delegate's
            // Allow/Deny buttons call exactly this function with the row id).
            page.submitInlineVerdict("r1", "allow");

            // Exercise the process-header batch-action click path: re-seed a
            // fresh pending row, then batch-decide the whole "curl" process
            // group the same way "Allow all" would.
            connModel.applyServerMessageJson(JSON.stringify({
                action: "insertConnectionRows",
                rows: [
                    { id: "r2", process: "curl", processPath: null, dstHost: "example.com",
                      dstIp: "2.2.2.2", dstPort: 443, protocol: "tcp", direction: "outgoing",
                      action: null, bytesSent: 0, bytesReceived: 0, startedAtMs: 0,
                      matchedRule: null }
                ]
            }));
            page.submitBatchVerdict("curl", "deny");
            // A displayed old header can survive until a replacement snapshot
            // flushes. Its captured session must not select new IDs with the
            // same wire value/process; a fresh header remains actionable.
            connModel.applyServerMessageJson(JSON.stringify({action: "clearConnectionRows"}));
            connModel.applyServerMessageJson(JSON.stringify({
                action: "insertConnectionRows",
                rows: [
                    { id: "2:1", process: "curl", processPath: null, dstHost: "example.com",
                      dstIp: "2.2.2.2", dstPort: 443, protocol: "tcp", direction: "outgoing",
                      action: null, bytesSent: 0, bytesReceived: 0, startedAtMs: 0,
                      matchedRule: null }
                ]
            }));
            const beforeStaleBatch = feedStub.denyCount;
            page.submitBatchVerdict("curl", "deny", "1:");
            if (feedStub.denyCount !== beforeStaleBatch) {
                throw new Error("old batch header targeted a replacement session row");
            }
            page.submitBatchVerdict("curl", "deny", "2:");
            if (feedStub.denyCount !== beforeStaleBatch + 1) {
                throw new Error("fresh batch header could not target its session row");
            }
        }
    }

    function check(ok, what) {
        if (!ok) {
            probeWindow.failures.push(what);
        }
    }
    function row(id, process, processPath, host) {
        return { id: id, process: process, processPath: processPath, dstHost: host,
                 dstIp: "93.184.216.34", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: null, bytesSent: 0, bytesReceived: 0, startedAtMs: 0,
                 matchedRule: null };
    }
    // Runs `action` and checks it submitted exactly `expected`
    // ("id choice scope/duration", any order).
    function expectSent(action, expected, what) {
        feedStub.submitted = [];
        action();
        const got = feedStub.submitted.map(s => s.rowId + " " + s.choice + " " + s.scope
                                                + "/" + s.duration).sort();
        probeWindow.check(JSON.stringify(got) === JSON.stringify(expected.slice().sort()),
                          what + ": sent " + JSON.stringify(got));
    }
    // The notifications shown since the last call are exactly `expected`.
    function expectShown(expected, what) {
        const got = probeWindow.shown.slice(probeWindow.shownMark);
        probeWindow.shownMark = probeWindow.shown.length;
        probeWindow.check(JSON.stringify(got) === JSON.stringify(expected),
                          what + ": notifications " + JSON.stringify(got));
    }
    function expectText(got, expected, what) {
        probeWindow.check(got === expected, what + ": " + got);
    }
    function openOn(id, process, host) {
        page.openInspector({
            rowId: id, process: process, host: host, port: 443,
            protocol: "tcp", verdict: "pending", pending: true,
            matchedRule: "", matchedRuleDisplay: ""
        });
    }

    // Plan 2026-10-08-inline-deny-until-restart.md, on a bridge that
    // advertised app-bound rules.
    function checkCapableBridge() {
        connModel.applyServerMessageJson(JSON.stringify({
            action: "insertConnectionRows",
            rows: [
                probeWindow.row("abs", "curl", "/usr/bin/curl", "github.com"),
                probeWindow.row("kernel", "kernel", "Kernel connection", "example.com"),
                probeWindow.row("k2", "kernel", "Kernel connection", "github.com"),
                probeWindow.row("w1", "wget", "/usr/bin/wget", "github.com"),
                probeWindow.row("w2", "wget", "/usr/bin/wget", "example.org")
            ]
        }));
        probeWindow.expectShown([], "before any Deny");

        probeWindow.expectSent(() => page.submitInlineVerdict("abs", "deny"),
                               ["abs deny this_host/until_quit"], "abs inline Deny");
        probeWindow.expectShown([], "a remembered inline Deny");
        probeWindow.expectSent(() => page.submitInlineVerdict("kernel", "deny"),
                               ["kernel deny this_host/this_time"], "kernel inline Deny");
        probeWindow.expectShown([probeWindow.sentence], "a once-only inline Deny");
        probeWindow.expectSent(() => {
            page.submitInlineVerdict("abs", "allow");
            page.submitInlineVerdict("kernel", "allow");
        }, ["abs allow this_host/this_time", "kernel allow this_host/this_time"], "inline Allow");
        probeWindow.expectShown([], "inline Allow");

        // D1: "Deny all (N)" sends each row's own inline Deny.
        probeWindow.expectSent(() => page.submitBatchVerdict("/usr/bin/wget", "deny"),
                               ["w1 deny this_host/until_quit", "w2 deny this_host/until_quit"],
                               "wget Deny all");
        probeWindow.expectSent(() => page.submitBatchVerdict("/usr/bin/wget", "allow"),
                               ["w1 allow this_host/this_time", "w2 allow this_host/this_time"],
                               "wget Allow all");
        probeWindow.expectShown([], "a remembered Deny all / Allow all");
        probeWindow.expectSent(() => page.submitBatchVerdict("Kernel connection", "deny"),
                               ["kernel deny this_host/this_time", "k2 deny this_host/this_time"],
                               "kernel Deny all");
        probeWindow.expectShown([probeWindow.sentence], "a once-only Deny all (explained once)");

        probeWindow.expectText(page.inlineVerdicts.denyText("abs"), probeWindow.untilRestart, "abs tooltip");
        probeWindow.expectText(page.inlineVerdicts.denyText("kernel"), probeWindow.sentence, "kernel tooltip");
        probeWindow.expectText(page.inlineVerdicts.denyAllText("/usr/bin/wget"), probeWindow.allUntilRestart,
                               "wget Deny all tooltip");
        probeWindow.expectText(page.inlineVerdicts.denyAllText("Kernel connection"), probeWindow.sentence,
                               "kernel Deny all tooltip");

        // D3 and D2 on the decision sheet.
        probeWindow.openOn("abs", "curl", "github.com");
        const sheet = page.decisionSheet;
        const durations = sheet.durationSelector;
        probeWindow.check(durations.count === 4 && durations.valueAt(2) === "until_quit"
                          && durations.textAt(2) === "Until firewall restarts",
                          "D3: until_quit is labelled " + durations.textAt(2));
        durations.currentIndex = 0;
        probeWindow.check(sheet.showDenyOnceHint === true, "D2: no hint for This time");
        probeWindow.check(sheet.denyOnceHint.text
                          === "A one-time Deny blocks only this attempt; most apps retry within seconds."
                          && sheet.denyOnceHint.textFormat === Text.PlainText,
                          "D2: hint text " + sheet.denyOnceHint.text);
        for (const i of [1, 2, 3]) {
            durations.currentIndex = i;
            probeWindow.check(sheet.showDenyOnceHint === false,
                              "D2: hint shown for " + durations.currentValue);
        }
        probeWindow.openOn("kernel", "kernel", "example.com");
        probeWindow.check(sheet.showDenyOnceHint === false,
                          "D2: hint shown for a program that can't be remembered");
    }

    // The same clicks against a bridge that predates app-bound rules: a
    // remembered "This host" deny there would block every app.
    function checkOldBridge() {
        feedStub.appBound = false;
        probeWindow.expectSent(() => page.submitInlineVerdict("abs", "deny"),
                               ["abs deny this_host/this_time"], "old bridge: abs inline Deny");
        probeWindow.expectShown([probeWindow.oldBridge], "old bridge: abs inline Deny");
        probeWindow.expectSent(() => page.submitBatchVerdict("/usr/bin/wget", "deny"),
                               ["w1 deny this_host/this_time", "w2 deny this_host/this_time"],
                               "old bridge: wget Deny all");
        probeWindow.expectShown([probeWindow.oldBridge], "old bridge: wget Deny all");
        probeWindow.expectSent(() => page.submitInlineVerdict("kernel", "deny"),
                               ["kernel deny this_host/this_time"], "old bridge: kernel inline Deny");
        probeWindow.expectShown([probeWindow.sentence], "old bridge: the program comes first");
        probeWindow.expectText(page.inlineVerdicts.denyText("abs"), probeWindow.oldBridge,
                               "old bridge: abs tooltip");
        probeWindow.expectText(page.inlineVerdicts.denyAllText("/usr/bin/wget"), probeWindow.oldBridge,
                               "old bridge: wget Deny all tooltip");
        probeWindow.openOn("abs", "curl", "github.com");
        page.decisionSheet.durationSelector.currentIndex = 0;
        probeWindow.check(page.decisionSheet.showDenyOnceHint === false,
                          "old bridge: D2 hint shown");
        feedStub.appBound = true;
    }

    // A Deny that couldn't be queued (the connection dropped) says so, not
    // that the bridge is too old or the program unknown. A disconnected
    // session never advertises app-bound rules.
    function checkLostConnection() {
        feedStub.queue = false;
        feedStub.appBound = false;
        probeWindow.expectSent(() => page.submitInlineVerdict("abs", "deny"),
                               ["abs deny this_host/this_time"], "lost: abs inline Deny");
        probeWindow.expectShown([probeWindow.notSent], "lost: abs inline Deny");
        probeWindow.expectSent(() => page.submitBatchVerdict("/usr/bin/wget", "deny"),
                               ["w1 deny this_host/this_time", "w2 deny this_host/this_time"],
                               "lost: wget Deny all");
        probeWindow.expectShown([probeWindow.notSent], "lost: wget Deny all (explained once)");
        probeWindow.expectSent(() => page.submitInlineVerdict("kernel", "deny"),
                               ["kernel deny this_host/this_time"], "lost: kernel inline Deny");
        probeWindow.expectShown([probeWindow.notSent], "lost: kernel inline Deny");
        probeWindow.expectSent(() => page.submitInlineVerdict("abs", "allow"),
                               ["abs allow this_host/this_time"], "lost: abs inline Allow");
        probeWindow.expectShown([], "lost: inline Allow");
        feedStub.queue = true;
        feedStub.appBound = true;
    }

    // The delegate whose `prop` is `value`, or null.
    function delegateWhere(prop, value) {
        const list = page.connectionList;
        for (let i = 0; i < list.count; i++) {
            const item = list.itemAtIndex(i);
            if (item && item[prop] === value) {
                return item;
            }
        }
        return null;
    }
    // The real buttons carry the same text for screen readers.
    function checkHeaderDelegates() {
        const wget = probeWindow.delegateWhere("groupKey", "/usr/bin/wget");
        probeWindow.check(wget !== null && wget.isGroupHeader, "no /usr/bin/wget header delegate");
        if (wget !== null) {
            probeWindow.expectText(wget.denyAllButton.Accessible.description,
                                   probeWindow.allUntilRestart, "Deny all description");
        }
    }
    function checkRowDelegates() {
        const abs = probeWindow.delegateWhere("rowId", "abs");
        const kernel = probeWindow.delegateWhere("rowId", "kernel");
        probeWindow.check(abs !== null && kernel !== null, "no abs/kernel row delegates");
        if (abs !== null && kernel !== null) {
            probeWindow.expectText(abs.denyButton.Accessible.description,
                                   probeWindow.untilRestart, "abs Deny description");
            probeWindow.expectText(kernel.denyButton.Accessible.description,
                                   probeWindow.sentence, "kernel Deny description");
        }
        probeWindow.absDelegate = abs;
    }
    // The same delegate's text follows the feed: no stale "Blocks this
    // program" once the connection or the capability is gone.
    function checkAbsDelegate(expected, what) {
        probeWindow.check(probeWindow.absDelegate !== null, what + ": no abs delegate");
        if (probeWindow.absDelegate !== null) {
            probeWindow.expectText(probeWindow.absDelegate.denyText, expected, what + " tooltip");
            probeWindow.expectText(probeWindow.absDelegate.denyButton.Accessible.description,
                                   expected, what + " description");
        }
    }

    // Quits the event loop once the delegate layout/polish pass (and any JS
    // errors it would surface) has had a chance to run — see the module doc
    // comment above for why pumping the loop matters here. The delegate
    // checks run on later ticks, after the views have laid out.
    //
    // Also where the verdict path is actually asserted. A `throw` here is
    // reported by Qt as a JS error prefixed with this probe's URL, which the
    // Rust-side stderr assertion below treats as a failure — so the check is
    // load-bearing in Rust, not a QML-side assert. `finally` guarantees
    // Qt.quit() still runs on the failing path; without it a throw would
    // leave the event loop spinning and hang the test binary.
    Timer {
        interval: 150
        running: true
        repeat: true
        onTriggered: {
            probeWindow.phase++;
            let done = true;
            try {
                if (probeWindow.phase === 1) {
                    if (feedStub.allowCount < 1) {
                        throw new Error("inline Allow never reached bridgeFeed.submitVerdict");
                    }
                    if (feedStub.denyCount < 1) {
                        throw new Error("batch Deny never reached bridgeFeed.submitVerdict");
                    }
                    // The rows above have no program path, so every one is
                    // once, and each "Deny all" that sent one explained it
                    // once (the stale header's sent nothing).
                    for (const s of feedStub.submitted) {
                        probeWindow.check(s.scope === "this_host" && s.duration === "this_time",
                                          "path-less " + s.choice + " " + s.rowId + " sent "
                                          + s.scope + "/" + s.duration);
                    }
                    probeWindow.expectShown([probeWindow.sentence, probeWindow.sentence],
                                            "path-less Deny all clicks");
                    probeWindow.checkCapableBridge();
                    probeWindow.checkOldBridge();
                    probeWindow.checkLostConnection();
                    done = false;
                } else if (probeWindow.phase === 2) {
                    probeWindow.checkHeaderDelegates();
                    connModel.setGroupedMode(false);
                    done = false;
                } else if (probeWindow.phase === 3) {
                    probeWindow.checkRowDelegates();
                    // The connection drops; its session is no longer capable.
                    feedStub.appBound = false;
                    feedStub.ok = false;
                    done = false;
                } else if (probeWindow.phase === 4) {
                    probeWindow.checkAbsDelegate(probeWindow.disconnected, "disconnected");
                    // Reconnected, to a bridge without app-bound rules.
                    feedStub.ok = true;
                    done = false;
                } else {
                    probeWindow.checkAbsDelegate(probeWindow.oldBridge, "old bridge after reconnect");
                }
            } catch (e) {
                probeWindow.failures.push("phase " + probeWindow.phase + " threw: " + e);
            }
            if (done || probeWindow.failures.length > 0) {
                stop();
                try {
                    if (probeWindow.failures.length > 0) {
                        throw new Error("inline verdict probe: " + probeWindow.failures.join("; "));
                    }
                } finally {
                    Qt.quit();
                }
            }
        }
    }
}
"#;

    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(&QByteArray::from(qml), &QUrl::from(PROBE_URL));
        }
        if let Some(app) = app.as_mut() {
            app.exec();
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "ConnectionsPage QML probe failed: root object was null — a QML *parse* error (syntax \
         error, unregistered/misspelled type) in the probe or ConnectionsPage.qml. (A JS runtime \
         error inside a handler/binding does NOT null the root — that's what the stderr capture \
         below catches instead.)"
    );

    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains(SHELL_QML_PREFIX))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "QML runtime error(s) reported against the probe URL while exercising the inline/batch \
         verdict click paths — this covers both a broken binding/handler AND the probe's own \
         Timer assertions on what each inline/batch Allow and Deny sent to \
         bridgeFeed.submitVerdict. Captured stderr:\n{}",
        bad_lines.join("\n")
    );
}
