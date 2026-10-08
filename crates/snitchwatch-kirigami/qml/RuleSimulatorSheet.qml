// Rule-match simulator sheet (Little-Snitch-parity "Simulate" panel), opened
// from RulesPage's header. Synchronous evaluation over the already cached
// rules (`RulesModel.simulate` -> `rules::simulator::simulate`): it never
// touches the network, but it does run on the UI thread, and compiling every
// regular expression it meets takes a moment (a pattern at the simulator's
// size cap about a quarter of a second), so a click can pause briefly.
//
// Every result is a simulation, never a live daemon verdict (see the
// `rules::simulator` module docs for what is and isn't reproduced). A blank
// field is UNKNOWN: rules with a condition on it are reported as not
// evaluated instead of being guessed.
//
// Rule names and operands in the result are data, so they only ever go into
// PlainText labels (issue #51).
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

SizedOverlaySheet {
    id: sheet
    title: "Simulate rule match"
    preferredWidth: Kirigami.Units.gridUnit * 22

    // Injected by the page that owns the sheet.
    property RulesModel model

    // `simulateRan` distinguishes "never run" from "ran, no match" so the
    // result section only appears after a real attempt.
    property bool simulateRan: false
    property string simulateMatchedRule: ""
    property string simulateAction: ""
    property int simulatePrecedence: -1
    // Plain-text lines built from the simulator's result. Conditions it can't
    // simulate at all, with the reason.
    property string simulateUnsupported: ""
    // Conditions left undecided because an input was left blank...
    property string simulateUnevaluated: ""
    // ...or typed but not usable (not an IP address).
    property string simulateInvalid: ""
    // Notes on how the deciding rule matched (hash conditions).
    property string simulateWarnings: ""
    // Rules that couldn't be decided are treated as not matching, so the
    // headline is only conditional on them.
    readonly property bool simulateUndecided: sheet.simulateUnsupported.length > 0
        || sheet.simulateUnevaluated.length > 0
        || sheet.simulateInvalid.length > 0

    function actionColor(action) {
        return action === "allow" ? Kirigami.Theme.positiveTextColor : Kirigami.Theme.negativeTextColor;
    }

    // At most ten entries, then a count of the rest.
    function lines(entries, describe) {
        const shown = entries.slice(0, 10).map(describe);
        if (entries.length > shown.length) {
            shown.push("and " + (entries.length - shown.length) + " more");
        }
        return shown.join("\n");
    }

    // Run the rule-match simulator (Qt-free logic in `rules::simulator`)
    // against the sheet's inputs and populate the result section.
    function runSimulation() {
        if (!sheet.model) return;
        const json = sheet.model.simulate(JSON.stringify({
            processPath: simProcessPath.text,
            destHost: simHost.text,
            destPort: simPort.value,
            protocol: simProtocol.currentText,
            parentPaths: simParentPaths.text,
            command: simCommand.text,
            pid: simPid.text,
            uid: simUid.text,
            env: simEnv.text,
            srcIp: simSrcIp.text,
            srcPort: simSrcPort.text,
            destIp: simDestIp.text,
            ifaceIn: simIfaceIn.text,
            ifaceOut: simIfaceOut.text,
            checksums: simChecksums.modes[simChecksums.currentIndex],
            md5: simMd5.text
        }));
        if (!json) return;
        const result = JSON.parse(json);
        sheet.simulateMatchedRule = result.matchedRule || "";
        sheet.simulateAction = result.action || "";
        sheet.simulatePrecedence = (result.precedence === undefined || result.precedence === null)
            ? -1 : result.precedence;
        sheet.simulateUnsupported = sheet.lines(result.unsupportedOperands || [],
            function (u) { return u.operand + " — " + u.reason; });
        const undecided = result.unevaluated || [];
        sheet.simulateUnevaluated = sheet.lines(
            undecided.filter(function (u) { return !u.invalid; }),
            function (u) { return u.rule + ": " + u.operand + " — input missing: " + u.missing; });
        sheet.simulateInvalid = sheet.lines(
            undecided.filter(function (u) { return u.invalid; }),
            function (u) { return u.rule + ": " + u.operand + " — the " + u.missing + " typed isn't valid"; });
        sheet.simulateWarnings = (result.warnings || []).join("\n");
        sheet.simulateRan = true;
    }

    ColumnLayout {
        Layout.preferredWidth: sheet.preferredWidth
        spacing: Kirigami.Units.largeSpacing

        Controls.Label {
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            opacity: 0.7
            font: Kirigami.Theme.smallFont
            text: "Evaluates a candidate connection against the currently cached rules, using opensnitchd's own precedence rules. This is a simulation over cached data, not a live daemon verdict. A blank field is unknown, so rules with a condition on it are reported as not evaluated instead of being guessed; the one exception is the destination host, where blank means a connection to a bare IP address."
        }
        Controls.Label {
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            opacity: 0.7
            font: Kirigami.Theme.smallFont
            text: "Regular expressions are matched the way opensnitchd's Go engine (RE2) does, as closely as this simulator can. A pattern it can't read, such as a character class with a literal dash, an escaped character other than \\d \\w \\s, or non-ASCII text, is reported as not simulated."
        }

        Kirigami.FormLayout {
            Layout.fillWidth: true

            Controls.TextField {
                id: simProcessPath
                Kirigami.FormData.label: "Process path"
                placeholderText: "/usr/bin/curl"
                Layout.fillWidth: true
            }
            Controls.TextField {
                id: simHost
                Kirigami.FormData.label: "Destination host"
                placeholderText: "github.com"
                Layout.fillWidth: true
            }
            Controls.SpinBox {
                id: simPort
                Kirigami.FormData.label: "Destination port"
                from: 0
                to: 65535
                value: 443
            }
            Controls.ComboBox {
                id: simProtocol
                Kirigami.FormData.label: "Protocol"
                // opensnitchd names IPv6 flows tcp6/udp6/...; a rule for
                // `tcp` does not match `tcp6`.
                model: ["tcp", "tcp6", "udp", "udp6", "udplite", "udplite6",
                        "sctp", "sctp6", "icmp", "icmp6"]
            }
        }

        Controls.Button {
            Layout.fillWidth: true
            text: advancedInputs.visible ? "Hide advanced inputs" : "Advanced inputs"
            icon.name: advancedInputs.visible ? "arrow-up" : "arrow-down"
            onClicked: advancedInputs.visible = !advancedInputs.visible
        }

        // Everything opensnitchd can match on beyond the four fields above.
        // `runSimulation` sends them as typed.
        ColumnLayout {
            id: advancedInputs
            Layout.fillWidth: true
            visible: false
            spacing: Kirigami.Units.largeSpacing

            Controls.Label {
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                opacity: 0.7
                font: Kirigami.Theme.smallFont
                text: "Everything else opensnitchd can match on. Leave a field blank if you don't know it. The network aliases LAN and MULTICAST use opensnitchd's default definitions; the alias file on the daemon host may differ."
            }

            Kirigami.FormLayout {
                Layout.fillWidth: true

                Controls.TextArea {
                    id: simParentPaths
                    Kirigami.FormData.label: "Parent programs"
                    placeholderText: "One path per line, nearest first"
                    Layout.fillWidth: true
                    Layout.preferredHeight: Kirigami.Units.gridUnit * 4
                }
                Controls.TextField {
                    id: simCommand
                    Kirigami.FormData.label: "Command line"
                    placeholderText: "/usr/bin/curl -s https://github.com"
                    Layout.fillWidth: true
                }
                Controls.TextField {
                    id: simPid
                    Kirigami.FormData.label: "Process ID"
                    validator: IntValidator { bottom: 0; top: 2147483647 }
                    Layout.fillWidth: true
                }
                Controls.TextField {
                    id: simUid
                    objectName: "simUid"
                    Kirigami.FormData.label: "User ID"
                    placeholderText: "1000"
                    validator: IntValidator { bottom: 0; top: 2147483647 }
                    Layout.fillWidth: true
                }
                Controls.TextArea {
                    id: simEnv
                    Kirigami.FormData.label: "Environment"
                    placeholderText: "NAME=value, one per line"
                    Layout.fillWidth: true
                    Layout.preferredHeight: Kirigami.Units.gridUnit * 4
                }
                Controls.Label {
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    opacity: 0.7
                    font: Kirigami.Theme.smallFont
                    text: "Once you type a variable here, every variable you don't list counts as unset."
                }
                Controls.TextField {
                    id: simSrcIp
                    Kirigami.FormData.label: "Source IP"
                    placeholderText: "192.168.1.10"
                    Layout.fillWidth: true
                }
                Controls.TextField {
                    id: simSrcPort
                    Kirigami.FormData.label: "Source port"
                    validator: IntValidator { bottom: 0; top: 65535 }
                    Layout.fillWidth: true
                }
                Controls.TextField {
                    id: simDestIp
                    objectName: "simDestIp"
                    Kirigami.FormData.label: "Destination IP"
                    placeholderText: "93.184.216.34"
                    Layout.fillWidth: true
                }
                Controls.TextField {
                    id: simIfaceIn
                    Kirigami.FormData.label: "Inbound interface"
                    placeholderText: "eth0"
                    Layout.fillWidth: true
                }
                Controls.TextField {
                    id: simIfaceOut
                    Kirigami.FormData.label: "Outbound interface"
                    placeholderText: "eth0"
                    Layout.fillWidth: true
                }
                Controls.ComboBox {
                    id: simChecksums
                    objectName: "simChecksums"
                    Kirigami.FormData.label: "Checksums"
                    // Parallel to `modes`, which is what the simulator reads.
                    readonly property var modes: ["unknown", "off", "on", "on-none"]
                    model: ["Unknown", "Off", "On, program's MD5 below",
                            "On, program has none recorded"]
                }
                Controls.TextField {
                    id: simMd5
                    Kirigami.FormData.label: "Program MD5"
                    placeholderText: "Leave blank if unknown"
                    enabled: simChecksums.currentIndex === 2
                    Layout.fillWidth: true
                }
            }
        }

        Controls.Button {
            Layout.fillWidth: true
            text: "Run simulation"
            icon.name: "system-run"
            onClicked: sheet.runSimulation()
        }

        Kirigami.Separator {
            Layout.fillWidth: true
            visible: sheet.simulateRan
        }

        ColumnLayout {
            Layout.fillWidth: true
            visible: sheet.simulateRan
            spacing: Kirigami.Units.smallSpacing

            Controls.Label {
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                opacity: 0.7
                font: Kirigami.Theme.smallFont
                text: "Simulated result: not a live daemon verdict."
            }
            Controls.Label {
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                font.bold: true
                textFormat: Text.PlainText
                text: sheet.simulateMatchedRule.length > 0
                      ? ((sheet.simulateUndecided ? "If the rules below don't match: " : "")
                         + "Matched: " + sheet.simulateMatchedRule)
                      : (sheet.simulateUndecided
                         ? "If the rules below don't match: No rule matched"
                         : "No rule matched")
                color: sheet.simulateMatchedRule.length > 0 && !sheet.simulateUndecided
                       ? sheet.actionColor(sheet.simulateAction)
                       : Kirigami.Theme.neutralTextColor
            }
            Controls.Label {
                visible: sheet.simulateMatchedRule.length > 0
                textFormat: Text.PlainText
                text: "Action: " + sheet.simulateAction + "  ·  Position " + (sheet.simulatePrecedence + 1)
                color: sheet.simulateUndecided
                       ? Kirigami.Theme.neutralTextColor
                       : sheet.actionColor(sheet.simulateAction)
            }
            Controls.Label {
                Layout.fillWidth: true
                visible: sheet.simulateMatchedRule.length === 0
                wrapMode: Text.Wrap
                opacity: 0.8
                font: Kirigami.Theme.smallFont
                text: "What happens next depends on the daemon, which this simulation can't see. It usually asks you (if the Snitchwatch window is connected and no other prompt is open); otherwise the daemon's configured default action applies. That default is a setting the simulator doesn't know: Snitchwatch ships deny, opensnitchd's own default is allow."
            }
            Controls.Label {
                Layout.fillWidth: true
                visible: sheet.simulateWarnings.length > 0
                wrapMode: Text.Wrap
                opacity: 0.8
                font: Kirigami.Theme.smallFont
                color: Kirigami.Theme.neutralTextColor
                textFormat: Text.PlainText
                text: sheet.simulateWarnings
            }
            Controls.Label {
                Layout.fillWidth: true
                visible: sheet.simulateUnevaluated.length > 0
                wrapMode: Text.Wrap
                opacity: 0.8
                font: Kirigami.Theme.smallFont
                color: Kirigami.Theme.neutralTextColor
                textFormat: Text.PlainText
                text: "Not evaluated, because an input was left blank. The result assumes these rules did not match:\n"
                      + sheet.simulateUnevaluated
            }
            Controls.Label {
                Layout.fillWidth: true
                visible: sheet.simulateInvalid.length > 0
                wrapMode: Text.Wrap
                opacity: 0.8
                font: Kirigami.Theme.smallFont
                color: Kirigami.Theme.neutralTextColor
                textFormat: Text.PlainText
                text: "Not evaluated, because an input isn't valid. The result assumes these rules did not match:\n"
                      + sheet.simulateInvalid
            }
            Controls.Label {
                Layout.fillWidth: true
                visible: sheet.simulateUnsupported.length > 0
                wrapMode: Text.Wrap
                opacity: 0.8
                font: Kirigami.Theme.smallFont
                color: Kirigami.Theme.neutralTextColor
                textFormat: Text.PlainText
                text: "Can't simulate these conditions. The result assumes the rules using them did not match:\n"
                      + sheet.simulateUnsupported
            }
        }
    }
}
