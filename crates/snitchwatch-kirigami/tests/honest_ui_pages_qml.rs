//! Integration smoke: the "not enforced" banners on `BlocklistsPage.qml` and
//! `ProfilesPage.qml` (issues #45/#46) are live, visible and non-dismissable
//! once the real pages are instantiated — on both pages, the warning that the
//! data is lost on restart until the bridge reports persistent storage (the
//! Profiles page then says profiles are saved, and always that they are not
//! applied); on the Blocklists page, the "not blocking" warning exactly while
//! some list isn't "rule installed"; every page's inspector sheet draws its
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

    // The Profiles header (issue #46): "not applied" always, "lost on
    // restart" while storage isn't persistent, "saved" while it is. Returns
    // the always-shown warning.
    function checkProfilesBanners(page, expectPersistent) {
        const found = {};
        for (let i = 0; i < page.header.children.length; i++) {
            const item = page.header.children[i];
            found[item.objectName] = item;
        }
        const notApplied = found["notAppliedBanner"];
        const memoryOnly = found["memoryOnlyStorageBanner"];
        const saved = found["savedNote"];
        if (!notApplied || !memoryOnly || !saved) {
            throw new Error("ProfilesPage: banners not found - probe lookup drifted");
        }
        const name = "ProfilesPage (persistent=" + expectPersistent + ")";
        if (notApplied.visible !== true || memoryOnly.visible !== !expectPersistent
                || saved.visible !== expectPersistent) {
            throw new Error(name + ": wrong banners visible");
        }
        for (const banner of [notApplied, memoryOnly]) {
            if (banner.type !== Kirigami.MessageType.Warning) {
                throw new Error(name + ": banner is not a Warning");
            }
            if (banner.showCloseButton) {
                throw new Error(name + ": banner is dismissable");
            }
        }
        if (notApplied.text.indexOf("not applied") < 0) {
            throw new Error(name + ": banner text does not say it is not applied: "
                            + notApplied.text);
        }
        if (memoryOnly.text.indexOf("restart") < 0) {
            throw new Error(name + ": memory-only warning lacks the restart sentence");
        }
        return notApplied;
    }

    // The Blocklists header holds two warnings (issue #45): "not blocking"
    // while some list isn't "rule installed", and "lost on restart" while
    // storage isn't persistent. Returns the visible ones.
    function checkBlocklistsBanners(page, expectPersistent, expectNotEnforced) {
        let notEnforced = null;
        let memoryOnly = null;
        for (let i = 0; i < page.header.children.length; i++) {
            const item = page.header.children[i];
            if (item.objectName === "notEnforcedBanner") {
                notEnforced = item;
            } else if (item.objectName === "memoryOnlyStorageBanner") {
                memoryOnly = item;
            }
        }
        if (!notEnforced || !memoryOnly) {
            throw new Error("BlocklistsPage: banners not found - probe lookup drifted");
        }
        const name = "BlocklistsPage (persistent=" + expectPersistent
            + ", notEnforced=" + expectNotEnforced + ")";
        if (memoryOnly.visible !== !expectPersistent || notEnforced.visible !== expectNotEnforced) {
            throw new Error(name + ": wrong banners visible");
        }
        for (const banner of [notEnforced, memoryOnly]) {
            if (banner.type !== Kirigami.MessageType.Warning) {
                throw new Error(name + ": banner is not a Warning");
            }
            if (banner.showCloseButton) {
                throw new Error(name + ": banner is dismissable");
            }
        }
        if (notEnforced.text.indexOf("aren't confirmed") < 0) {
            throw new Error(name + ": warning does not say lists aren't confirmed as blocking");
        }
        if (memoryOnly.text.indexOf("restart") < 0) {
            throw new Error(name + ": memory-only warning lacks the restart sentence");
        }
        return [notEnforced, memoryOnly].filter(b => b.visible);
    }

    function laidOut(page, banner) {
        if (banner.width < page.width - 1 || banner.height <= 0) {
            throw new Error("BlocklistsPage: banner not laid out: " + banner.width
                            + "x" + banner.height);
        }
    }

    function list(enforcement) {
        return { id: "ads", displayName: "ads", url: "https://x.example/ads", entryCount: 2,
                 status: "ok", enforcement: enforcement };
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
                // No SetBlocklists yet: not persistent, so "lost on restart";
                // no lists, so nothing to call unenforced.
                laidOut(blocklistsPage, checkBlocklistsBanners(blocklistsPage, false, false)[0]);
                const persistent = { persistent: true };
                blocklistsPage.model.applyServerMessageJson(JSON.stringify({
                    action: "setBlocklists", blocklists: [], storage: persistent
                }));
                checkBlocklistsBanners(blocklistsPage, true, false);
                blocklistsPage.model.applyServerMessageJson(JSON.stringify({
                    action: "setBlocklists", blocklists: [list("not_enforced")], storage: persistent
                }));
                // Shown at once; laid out only after the next polish, so not
                // measured here (the memory-only one above is).
                checkBlocklistsBanners(blocklistsPage, true, true);
                blocklistsPage.model.applyServerMessageJson(JSON.stringify({
                    action: "setBlocklists", blocklists: [list("rule_installed")], storage: persistent
                }));
                checkBlocklistsBanners(blocklistsPage, true, false);
                // An unreadable store: its own warning, not "memory only".
                blocklistsPage.model.applyServerMessageJson(JSON.stringify({
                    action: "setBlocklists", blocklists: [],
                    storage: { persistent: true, unreadable: true, reason: "x" }
                }));
                checkBlocklistsBanners(blocklistsPage, true, false);
                let unreadable = null;
                for (let i = 0; i < blocklistsPage.header.children.length; i++) {
                    const item = blocklistsPage.header.children[i];
                    if (item.objectName === "unreadableStoreBanner") {
                        unreadable = item;
                    }
                }
                if (!unreadable || unreadable.visible !== true) {
                    throw new Error("BlocklistsPage: no warning for an unreadable store");
                }
                blocklistsPage.model.applyServerMessageJson(JSON.stringify({
                    action: "setBlocklists", blocklists: [],
                    storage: { persistent: false, reason: "blocklist store: <b>locked</b>" }
                }));
                checkBlocklistsBanners(blocklistsPage, false, false);
                if (blocklistsPage.storageReason !== "blocklist store: <b>locked</b>") {
                    throw new Error("BlocklistsPage: storage reason not exposed");
                }
                // No SetProfiles yet: not persistent. Laid out for real: full
                // page width and a non-zero height, not a zero-sized item
                // that merely reports `visible`.
                const notApplied = checkProfilesBanners(profilesPage, false);
                if (notApplied.width < profilesPage.width - 1 || notApplied.height <= 0) {
                    throw new Error("ProfilesPage: banner not laid out: " + notApplied.width
                                    + "x" + notApplied.height);
                }
                profilesPage.model.applyServerMessageJson(JSON.stringify({
                    action: "setProfiles", profiles: [], storage: { persistent: true }
                }));
                checkProfilesBanners(profilesPage, true);
                profilesPage.model.applyServerMessageJson(JSON.stringify({
                    action: "setProfiles", profiles: [],
                    storage: { persistent: false, reason: "profile store: <b>locked</b>" }
                }));
                checkProfilesBanners(profilesPage, false);
                if (profilesPage.storageReason !== "profile store: <b>locked</b>") {
                    throw new Error("ProfilesPage: storage reason not exposed");
                }
                // Issue #46 Part 2: a bridge that applies profile rules swaps
                // the warning for a note saying they are applied.
                profilesPage.model.applyServerMessageJson(JSON.stringify({
                    action: "setProfiles", profiles: [], storage: { persistent: true },
                    appliesRules: true
                }));
                let applied = null;
                let stillWarned = null;
                for (let i = 0; i < profilesPage.header.children.length; i++) {
                    const item = profilesPage.header.children[i];
                    if (item.objectName === "appliedNote") applied = item;
                    if (item.objectName === "notAppliedBanner") stillWarned = item;
                }
                if (!applied || !applied.visible || stillWarned.visible
                        || applied.type !== Kirigami.MessageType.Information) {
                    throw new Error("ProfilesPage: applied profiles aren't said to be applied");
                }
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
         assertions (visible / Warning / no close button / laid out / wording). \
         Captured stderr:\n{}",
        bad_lines.join("\n")
    );
}
