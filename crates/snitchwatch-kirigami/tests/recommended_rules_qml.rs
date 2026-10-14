//! Prompt-slot D in the window: the real `RecommendedRulesPage.qml` with a
//! real `CuratedDefaultsModel`.
//!   * Nothing is offered until the bridge sends the list; an older bridge
//!     never does, and the page says so.
//!   * Every entry is off unless the bridge says on; a switch click only
//!     asks, and the switch keeps the bridge's value until it answers.
//!   * Each row shows exactly what its rule allows, as plain text, and its
//!     status in fixed words.
//!   * "Turn all on/off" asks only for the entries not already that way.
//!   * Remove is offered only for a rule edited outside Snitchwatch, and
//!     asks for a confirmation before anything is sent.
//!   * On a bridge that never adds them, nothing can be turned on, and the
//!     reason is shown.
//!
//! Fails on any QML warning from the probe or the shell's QML. Run headless
//! with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::curated_defaults_model as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/recommended_rules_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";

#[test]
fn the_page_offers_nothing_unasked_and_says_what_each_rule_allows() {
    init_headless_qt_env();

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
    width: 900
    height: 700

    readonly property string markup: "<b>/usr/bin/flatpak</b> may connect to <i>dl.flathub.org</i>"
    property var sent: []

    function expect(condition, message) {
        if (!condition) {
            throw new Error(message);
        }
    }

    function find(item, name) {
        if (!item) return null;
        if (item.objectName === name) return item;
        for (let i = 0; i < item.children.length; i++) {
            const hit = probeWindow.find(item.children[i], name);
            if (hit) return hit;
        }
        return null;
    }

    function entry(id, on, status, broad) {
        const e = { id: id, program: "/usr/bin/" + id, allows: probeWindow.markup,
                    why: "Why " + id, on: on, status: status };
        if (broad) e.broad = true;
        return e;
    }

    function send(entries, unavailable) {
        const msg = { action: "setCuratedDefaults", entries: entries,
                      storage: { persistent: true } };
        if (unavailable) msg.unavailable = unavailable;
        curated.applyServerMessageJson(JSON.stringify(msg));
    }

    function row(index) {
        const item = page.entriesList.itemAtIndex(index);
        probeWindow.expect(item !== null, "no delegate for row " + index);
        return item;
    }

    CuratedDefaultsModel { id: curated }

    Connections {
        target: curated
        function onCuratedChangeRequested(json) {
            probeWindow.sent.push(JSON.parse(json));
        }
    }

    RecommendedRulesPage {
        id: page
        anchors.fill: parent
        model: curated
    }

    Timer {
        interval: 150
        running: true
        repeat: false
        onTriggered: {
            try {
                const header = page.header;
                // The fixed explanation is honest about the one any-address rule
                // (S6) and about what "Turn all on" turns on.
                const explanation = find(header, "explanation").text;
                expect(explanation.indexOf("reach one host, or this computer, on one port") >= 0
                       && explanation.indexOf("except the DNS rule, which lets the system "
                           + "resolver reach any address on port 53.") >= 0,
                       "explanation hides the any-address DNS rule: " + explanation);
                expect(explanation.indexOf('The "Turn all on" button skips the DNS rule, which '
                           + 'you turn on by itself; "Turn all off" turns it off too.') >= 0,
                       "explanation doesn't say what Turn all on does: " + explanation);
                expect(explanation.indexOf("turns on every rule") < 0,
                       "explanation still says Turn all on turns on every rule: " + explanation);
                expect(explanation.indexOf("one place") < 0,
                       "explanation calls any address one place: " + explanation);
                // An older bridge: nothing offered, and the page says so.
                expect(find(header, "notOfferedBanner").visible, "no not-offered banner");
                expect(!find(header, "allOnButton").visible, "Turn all on before any list");
                curated.setEntry("flatpak", true);
                curated.setAll(true);
                expect(probeWindow.sent.length === 0, "asked before the bridge offered any");

                send([entry("flatpak", false, "off"), entry("chronyc", true, "installed"),
                      entry("nm", true, "editedByYou")]);
                expect(!find(header, "notOfferedBanner").visible, "not-offered banner stayed");
                expect(page.entriesList.count === 3, "rows: " + page.entriesList.count);
                const sw = find(row(0), "entrySwitch");
                expect(sw.enabled && !sw.checked, "an entry the bridge says is off shows on");
                expect(find(row(1), "entrySwitch").checked, "an entry turned on shows off");
                const allows = find(row(0), "allowsLabel");
                expect(allows.text === probeWindow.markup && allows.textFormat === Text.PlainText,
                       "what the rule allows isn't shown as plain text");
                expect(find(row(1), "statusLabel").text === "Rule installed.",
                       "status: " + find(row(1), "statusLabel").text);

                // A click only asks; the switch keeps the bridge's value.
                sw.toggle();
                sw.toggled();
                expect(probeWindow.sent.length === 1
                       && JSON.stringify(probeWindow.sent[0].ids) === '["flatpak"]'
                       && probeWindow.sent[0].on === true
                       && probeWindow.sent[0].action === "setCuratedDefaults",
                       "click not sent: " + JSON.stringify(probeWindow.sent));
                expect(!sw.checked, "the switch claims on before the bridge says so");

                find(header, "allOffButton").clicked();
                expect(probeWindow.sent.length === 2
                       && JSON.stringify(probeWindow.sent[1].ids) === '["chronyc","nm"]'
                       && probeWindow.sent[1].on === false,
                       "Turn all off: " + JSON.stringify(probeWindow.sent));

                // Keep: only for a rule already in the firewall, not chosen yet.
                expect(!find(row(1), "keepButton").visible, "Keep on a chosen rule");
                send([entry("flatpak", false, "off"), entry("chronyc", true, "installed"),
                      entry("nm", true, "editedByYou"), entry("old", true, "inFirewall")]);
                const keep = find(row(3), "keepButton");
                expect(keep.visible, "no Keep on an undecided rule");
                keep.clicked();
                expect(probeWindow.sent.length === 3
                       && JSON.stringify(probeWindow.sent[2].ids) === '["old"]'
                       && probeWindow.sent[2].on === true,
                       "Keep: " + JSON.stringify(probeWindow.sent));
                probeWindow.sent.pop();

                // Remove: only for the edited rule, and only once confirmed.
                expect(!find(row(1), "removeButton").visible, "Remove on an unedited rule");
                const remove = find(row(2), "removeButton");
                expect(remove.visible, "no Remove on an edited rule");
                remove.clicked();
                expect(probeWindow.sent.length === 2, "removed before the user confirmed");
                expect(find(row(2), "removeQuestion").visible, "no confirmation question");
                find(row(2), "removeCancel").clicked();
                expect(probeWindow.sent.length === 2 && find(row(2), "removeButton").visible,
                       "Cancel didn't cancel");
                find(row(2), "removeButton").clicked();
                find(row(2), "removeConfirm").clicked();
                expect(probeWindow.sent.length === 3
                       && probeWindow.sent[2].action === "removeCuratedDefault"
                       && probeWindow.sent[2].id === "nm",
                       "Remove: " + JSON.stringify(probeWindow.sent));

                // A broad entry (any address, S6) is never turned on or adopted by
                // "Turn all on", and "Turn all off" includes it.
                send([entry("flatpak", false, "off"), entry("dns", false, "off", true),
                      entry("dns2", true, "inFirewall", true),
                      entry("chronyc", true, "installed")]);
                const asked = probeWindow.sent.length;
                find(header, "allOnButton").clicked();
                expect(probeWindow.sent.length === asked + 1
                       && JSON.stringify(probeWindow.sent[asked].ids) === '["flatpak"]'
                       && probeWindow.sent[asked].on === true,
                       "Turn all on and a broad entry: " + JSON.stringify(probeWindow.sent));
                find(header, "allOffButton").clicked();
                expect(probeWindow.sent.length === asked + 2
                       && JSON.stringify(probeWindow.sent[asked + 1].ids) === '["dns2","chronyc"]'
                       && probeWindow.sent[asked + 1].on === false,
                       "Turn all off and a broad entry: " + JSON.stringify(probeWindow.sent));
                probeWindow.sent.length = asked;

                // Old bridge refuses singleton broad On and Keep; Off remains.
                send([entry("dns-resolved", false, "off", true)]);
                const beforeDns = probeWindow.sent.length;
                curated.setEntry("dns-resolved", true);
                expect(probeWindow.sent.length === beforeDns && !curated.consentOpen,
                       "old bridge received DNS enable");
                curated.applyServerMessageJson(JSON.stringify({action:"authenticated", capabilities:["curatedBroadOptInV1"]}));
                curated.setEntry("dns-resolved", true);
                expect(curated.consentOpen && !curated.consentReady, "no waiting DNS confirmation");
                const prepare = probeWindow.sent[beforeDns];
                expect(prepare.action === "prepareCuratedBroadOptIn" && prepare.purpose === "enable", "wrong prepare");
                const dialog = page.dnsConsentDialog;
                const confirmDns = find(dialog.contentItem, "dnsConsentConfirm");
                const cancelDns = find(dialog.contentItem, "dnsConsentCancel");
                expect(dialog.visible && !confirmDns.enabled, "confirmation visible before preparation");
                const disclosure = find(dialog.contentItem, "dnsConsentDisclosure").text;
                expect(find(dialog.contentItem, "dnsConsentDisclosure").textFormat === Text.PlainText, "DNS disclosure is markup");
                for (const word of ["/usr/lib/systemd/systemd-resolved", "UID 193", "TCP/UDP", "53", "IPv4/IPv6", "any destination", "data inside DNS lookups"])
                    expect(disclosure.indexOf(word) >= 0, "missing disclosure " + word);
                curated.confirmBroadConsent();
                expect(probeWindow.sent.length === beforeDns + 1, "confirmed before a ticket");
                curated.setEntry("dns-resolved", true);
                const intent = probeWindow.sent[probeWindow.sent.length - 1];
                curated.applyServerMessageJson(JSON.stringify({action:"curatedBroadOptInPrepared", requestId:intent.requestId,
                    id:"dns-resolved", purpose:"enable", ticket:"a".repeat(64)}));
                expect(curated.consentReady && confirmDns.enabled, "valid private ticket not accepted");
                confirmDns.clicked();
                const enable = probeWindow.sent[probeWindow.sent.length - 1];
                expect(enable.action === "enableCuratedBroadDefault" && enable.consent === "resolver-any-address-v1", "no reviewed enable");
                const afterConfirm = probeWindow.sent.length;
                curated.confirmBroadConsent();
                expect(probeWindow.sent.length === afterConfirm, "confirmation replayed");
                curated.applyServerMessageJson(JSON.stringify({action:"curatedDefaultsRequestResult", requestId:intent.requestId,
                    results:[{id:"dns-resolved", outcome:"choiceAccepted", choiceAccepted:true, persisted:false, reason:"session only"}]}));
                expect(curated.consentResult.indexOf("not saved") >= 0 && curated.consentResult.indexOf("Installation status follows") >= 0,
                       "result claimed persistent installation");
                // Keep-disabled has its own disclosure and cannot become enable.
                send([entry("dns-resolved", true, "inFirewallButOff", true)]);
                curated.keepEntry("dns-resolved");
                const keepIntent = probeWindow.sent[probeWindow.sent.length - 1];
                expect(keepIntent.purpose === "keep", "disabled Keep became enable");
                cancelDns.clicked();
                expect(probeWindow.sent[probeWindow.sent.length - 1].action === "cancelCuratedBroadOptIn", "cancel did not cancel ticket");
                curated.reenableDns();
                expect(probeWindow.sent[probeWindow.sent.length - 1].purpose === "reenable", "no dedicated reenable intent");
                curated.resetBridgeSession();
                expect(!curated.consentOpen && !curated.broadSupported, "session loss retained consent");
                const beforeReplay = probeWindow.sent.length;
                curated.confirmBroadConsent();
                expect(probeWindow.sent.length === beforeReplay, "disconnect replayed confirmation");
                probeWindow.sent.length = beforeDns;

                // #158: on, but held back on this computer (its account not
                // confirmed): the switch stays on, the status says so rather
                // than the inert bridge's sentence, and the reason is shown.
                const held = entry("dns-resolved", true, "unavailable", true);
                held.problem = "Snitchwatch couldn't confirm the account.";
                send([held]);
                expect(find(row(0), "entrySwitch").checked, "a held-back entry shows off");
                expect(find(row(0), "statusLabel").text
                       === "On, but Snitchwatch holds this rule back on this computer; see why below.",
                       "held back: " + find(row(0), "statusLabel").text);
                expect(find(row(0), "problemLabel").visible
                       && find(row(0), "problemLabel").text === held.problem,
                       "held back without its reason");
                // #158: a refusal note the bridge cleared (Remove, reconnect)
                // leaves nothing behind.
                const refused = entry("flatpak", false, "off");
                refused.problem = "The firewall service refused the rule.";
                send([refused]);
                expect(find(row(0), "problemLabel").visible, "refusal note not shown");
                send([entry("flatpak", false, "off")]);
                expect(!find(row(0), "problemLabel").visible, "a cleared note lingers");
                expect(find(row(0), "statusLabel").text === "Off.", "off: "
                       + find(row(0), "statusLabel").text);

                // A bridge that never adds them: nothing can be turned on.
                send([entry("flatpak", false, "unavailable")], "Needs the system service.");
                expect(find(header, "unavailableBanner").visible, "no unavailable banner");
                expect(find(header, "unavailableReason").text === "Needs the system service.",
                       "reason not shown");
                expect(!find(header, "allOnButton").visible, "Turn all on while unavailable");
                expect(!find(row(0), "entrySwitch").enabled, "switch enabled while unavailable");
                curated.setEntry("flatpak", true);
                curated.removeEntry("flatpak");
                expect(probeWindow.sent.length === 3, "asked while unavailable");
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
        if root_ok.load(Ordering::SeqCst) {
            if let Some(app) = app.as_mut() {
                app.exec();
            }
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "recommended rules probe failed to load (QML parse error)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains(SHELL_QML_PREFIX))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "recommended rules probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}
