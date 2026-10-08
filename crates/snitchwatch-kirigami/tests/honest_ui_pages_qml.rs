//! Integration smoke: the "not enforced" banners on `BlocklistsPage.qml` and
//! `ProfilesPage.qml` (issues #45/#46) are live, visible and non-dismissable
//! once the real pages are instantiated — on the Blocklists page, the variant
//! that says subscriptions are lost on restart until the bridge reports
//! persistent storage, and the one that doesn't after; every page's inspector sheet draws its
//! title through `SizedOverlaySheet`'s PlainText header, whose hover tooltip is
//! an explicit ToolTip with a PlainText content item (issue #51) — including for
//! a long, markup-named title that actually elides; and the
//! edited `RulesPage.qml` / `ConnectionsPage.qml` (-> `PendingDecisionSheet.qml`)
//! load and open their inspectors on markup-looking data without warnings.
//!
//! What this does NOT prove: that each individual data `Label` is PlainText
//! (cxx-qt-lib can't reach into delegates, and the labels sit in unopened
//! sheets / ListView delegates). That coverage is source-guard only — see
//! `honest_ui_qml_guards.rs`.
//!
//! Same two-layer shape as `inline_verdict_qml.rs`: a real `Window` driven by
//! a real event loop (so delegates and header bindings actually evaluate), with
//! stderr captured so a QML JS error — including the probe `Timer`'s own
//! `throw`s, which Qt reports prefixed with the probe's URL — fails the test,
//! plus a separate null-root check for QML parse errors. Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/honest_ui_pages_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";

#[test]
fn preview_banners_are_visible_and_not_dismissable() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Window {
    visible: true
    width: 1200
    height: 600

    // Markup-looking AND long enough to elide in the sheet's title heading,
    // which is the case that arms the title's hover tooltip.
    readonly property string longRuleName: "<b>bold</b>-<img src=x>-" + "x".repeat(200)

    BlocklistsPage {
        id: blocklistsPage
        anchors { left: parent.left; top: parent.top; bottom: parent.bottom }
        width: parent.width / 4
        model: BlocklistsModel {}
        entriesModel: BlocklistEntriesModel {}
    }
    ProfilesPage {
        id: profilesPage
        anchors { left: blocklistsPage.right; top: parent.top; bottom: parent.bottom }
        width: parent.width / 4
        model: ProfilesModel {}
    }
    RulesPage {
        id: rulesPage
        anchors { left: profilesPage.right; top: parent.top; bottom: parent.bottom }
        width: parent.width / 4
        model: RulesModel {
            Component.onCompleted: applyServerMessageJson(JSON.stringify({
                action: "setRules",
                rules: [
                    { name: longRuleName, enabled: true, action: "allow",
                      duration: "always", description: "",
                      operator: { operand: "process.path", data: "<i>x</i>" } }
                ]
            }))
        }
    }
    ConnectionsPage {
        id: connectionsPage
        anchors { right: parent.right; top: parent.top; bottom: parent.bottom }
        width: parent.width / 4
        model: ConnectionsModel {
            Component.onCompleted: applyServerMessageJson(JSON.stringify({
                action: "insertConnectionRows",
                rows: [
                    { id: "r1", process: "<b>evil</b>", processPath: null,
                      dstHost: "<i>h</i>.example", dstIp: "203.0.113.9", dstPort: 443,
                      protocol: "tcp", direction: "outgoing", action: null,
                      bytesSent: 0, bytesReceived: 0, startedAtMs: 0 }
                ]
            }))
        }
    }

    // Every inspector sheet is a SizedOverlaySheet declared directly on its
    // page; its title must be drawn by a PlainText Heading, not Kirigami's
    // AutoText default, and the heading's hover tooltip must be an explicit
    // ToolTip with a PlainText content item, not the style's AutoText one
    // (reached via the attached `ToolTip.text`). `expectElided` names a title
    // that must actually be elided, proving the tooltip scenario is live.
    function checkSheetTitles(page, name, expectElided) {
        let sheets = 0;
        let elided = false;
        for (let i = 0; i < page.scrollablePageData.length; i++) {
            const sheet = page.scrollablePageData[i];
            if (sheet.header === undefined || sheet.title === undefined) {
                continue;
            }
            sheets++;
            if (sheet.header.textFormat !== Text.PlainText) {
                throw new Error(name + ": sheet title header is not PlainText");
            }
            if (sheet.header.text !== sheet.title) {
                throw new Error(name + ": sheet header does not show its title");
            }
            let tips = 0;
            for (let j = 0; j < sheet.header.data.length; j++) {
                const tip = sheet.header.data[j];
                if (tip.contentItem === undefined || tip.delay === undefined) {
                    continue;
                }
                tips++;
                if (tip.contentItem.textFormat !== Text.PlainText
                        || tip.contentItem.text !== sheet.title) {
                    throw new Error(name + ": title tooltip is not a PlainText label of the title");
                }
            }
            if (tips !== 1) {
                throw new Error(name + ": expected exactly one explicit title ToolTip, found " + tips);
            }
            if (expectElided && sheet.title === expectElided) {
                elided = sheet.header.truncated;
            }
        }
        if (sheets === 0) {
            throw new Error(name + ": found no OverlaySheet to check - probe lookup drifted");
        }
        if (expectElided && !elided) {
            throw new Error(name + ": the long title was not elided, so the tooltip case is not live");
        }
    }

    function checkBanner(page, name) {
        const banner = page.header;
        if (!banner || banner.visible !== true) {
            throw new Error(name + ": not-enforced banner is not visible");
        }
        if (banner.type !== Kirigami.MessageType.Warning) {
            throw new Error(name + ": banner is not a Warning");
        }
        if (banner.showCloseButton) {
            throw new Error(name + ": banner is dismissable");
        }
        // Laid out for real: full page width and a non-zero height, not a
        // zero-sized item that merely reports `visible`.
        if (banner.width < page.width - 1 || banner.height <= 0) {
            throw new Error(name + ": banner not laid out: " + banner.width + "x" + banner.height
                            + " in a page " + page.width + " wide");
        }
        if (banner.text.indexOf("not applied") < 0) {
            throw new Error(name + ": banner text does not say it is not applied: " + banner.text);
        }
    }

    // The Blocklists header holds two banner variants keyed on storage
    // (issue #45); exactly one is shown.
    function checkBlocklistsBanner(page, expectPersistent) {
        let persistent = null;
        let memoryOnly = null;
        for (let i = 0; i < page.header.children.length; i++) {
            const item = page.header.children[i];
            if (item.objectName === "persistentStorageBanner") {
                persistent = item;
            } else if (item.objectName === "memoryOnlyStorageBanner") {
                memoryOnly = item;
            }
        }
        if (!persistent || !memoryOnly) {
            throw new Error("BlocklistsPage: banner variants not found - probe lookup drifted");
        }
        const shown = expectPersistent ? persistent : memoryOnly;
        const hidden = expectPersistent ? memoryOnly : persistent;
        const name = "BlocklistsPage (persistent=" + expectPersistent + ")";
        if (shown.visible !== true || hidden.visible !== false) {
            throw new Error(name + ": wrong banner variant visible");
        }
        if (shown.type !== Kirigami.MessageType.Warning) {
            throw new Error(name + ": banner is not a Warning");
        }
        if (shown.showCloseButton) {
            throw new Error(name + ": banner is dismissable");
        }
        if (shown.text.indexOf("not applied") < 0) {
            throw new Error(name + ": banner text does not say it is not applied: " + shown.text);
        }
        if ((shown.text.indexOf("restart") >= 0) === expectPersistent) {
            throw new Error(name + ": banner gets the restart sentence wrong: " + shown.text);
        }
        return shown;
    }

    // Opens the Rules and Connections inspectors on markup-looking data. Done
    // from a Timer, not Component.onCompleted: the order in which a page's and
    // its child model's onCompleted handlers run is undefined, and the rule
    // would not exist yet if the page's ran first. The sheets then get until
    // the checker below to lay out, so any warning they raise is captured.
    Timer {
        interval: 50
        running: true
        repeat: false
        onTriggered: {
            rulesPage.openRuleByName(longRuleName);
            connectionsPage.openInspector({
                rowId: "r1", process: "<b>evil</b>", host: "<i>h</i>.example", port: 443,
                protocol: "tcp", verdict: "", pending: true,
                matchedRule: "", matchedRuleDisplay: "awaiting decision"
            });
        }
    }

    // Quits the loop once layout/polish has run; `finally` keeps Qt.quit()
    // reachable when a check throws, which would otherwise hang the binary.
    Timer {
        interval: 300
        running: true
        repeat: false
        onTriggered: {
            try {
                // No SetBlocklists yet: not persistent, so "lost on restart".
                const memoryOnly = checkBlocklistsBanner(blocklistsPage, false);
                if (memoryOnly.width < blocklistsPage.width - 1 || memoryOnly.height <= 0) {
                    throw new Error("BlocklistsPage: banner not laid out: " + memoryOnly.width
                                    + "x" + memoryOnly.height);
                }
                blocklistsPage.model.applyServerMessageJson(JSON.stringify({
                    action: "setBlocklists", blocklists: [], storage: { persistent: true }
                }));
                checkBlocklistsBanner(blocklistsPage, true);
                blocklistsPage.model.applyServerMessageJson(JSON.stringify({
                    action: "setBlocklists", blocklists: [],
                    storage: { persistent: false, reason: "blocklist store: <b>locked</b>" }
                }));
                checkBlocklistsBanner(blocklistsPage, false);
                if (blocklistsPage.storageReason !== "blocklist store: <b>locked</b>") {
                    throw new Error("BlocklistsPage: storage reason not exposed");
                }
                checkBanner(profilesPage, "ProfilesPage");
                if (rulesPage.inspectName !== longRuleName) {
                    throw new Error("RulesPage inspector did not open on the markup-named rule");
                }
                if (connectionsPage.inspectProcess !== "<b>evil</b>") {
                    throw new Error("ConnectionsPage inspector did not open on the pending row");
                }
                checkSheetTitles(blocklistsPage, "BlocklistsPage", "");
                checkSheetTitles(profilesPage, "ProfilesPage", "");
                checkSheetTitles(rulesPage, "RulesPage", longRuleName);
                checkSheetTitles(connectionsPage, "ConnectionsPage", "");
            } finally {
                Qt.quit();
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
        "honest-UI probe failed: root object was null — a QML parse error (syntax error, \
         unregistered/misspelled type) in the probe or one of the edited pages"
    );

    // The probe's own URL (its Timer `throw`s) plus any warning raised inside
    // one of this shell's pages. An unfiltered scan would trip on the upstream
    // Kirigami OverlaySheet binding-loop warning every sheet emits.
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains(SHELL_QML_PREFIX))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "QML runtime error(s) or warning(s) reported against the probe URL or one of the shell's \
         pages — a broken page binding / anchor loop, or the probe Timer's own banner \
         assertions (visible / Warning / no close button / laid out / says \"not applied\"). \
         Captured stderr:\n{}",
        bad_lines.join("\n")
    );
}
