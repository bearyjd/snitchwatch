//! Honest-UI guards for the make-rule flow and the Connections page labels:
//! only what the bridge answered is said, and notices are fixed text.

mod qml_guard_support;
use qml_guard_support::*;

/// Same hazard on the Connections tab: process names, hosts and matched-rule
/// names come straight from the daemon / the connecting program.
#[test]
fn connections_page_labels_showing_connection_data_are_plain_text() {
    assert_data_labels_plain_text(
        "ConnectionsPage.qml",
        CONNECTIONS_PAGE,
        &[
            "row.process",
            "row.host",
            "row.verdict",
            "row.groupLabel",
            "part.src.programLabel",
            "part.src.programPathTail",
            "page.inspectHost",
            "page.inspectIp",
            "page.inspectProtocol",
            "page.inspectVerdict",
            "page.inspectMatchedRuleDisplay",
            // Selectable-text batch 8: the row's host line and verdict word
            // (row.host, row.verdict) are built by ConnectionActions, which
            // "Copy row text" shares.
            "row.actions.hostLine(row)",
            "row.actions.verdictText(row)",
        ],
        10,
    );
}

/// A result that never comes must still end the wait: each rule-command sheet
/// polls its controller every second while it is busy, and the controller
/// gives up after its deadline (`NO_ANSWER_AFTER`).
#[test]
fn rule_command_sheets_poll_their_controller_while_busy() {
    for (name, source, running, triggered) in [
        (
            "MakeRuleOutcomes.qml",
            MAKE_RULE_OUTCOMES,
            "running: !!outcomes.controller && outcomes.controller.busy",
            "onTriggered: outcomes.controller.poll()",
        ),
        (
            "RuleEditorSheet.qml",
            EDITOR_SHEET,
            "running: !!sheet.controller && sheet.controller.busy",
            "onTriggered: sheet.controller.poll()",
        ),
    ] {
        let polling = blocks(&code_lines(source), "Timer {")
            .into_iter()
            .filter(|timer| {
                has_line(timer, "interval: 1000")
                    && has_line(timer, "repeat: true")
                    && has_line(timer, running)
                    && has_line(timer, triggered)
            })
            .count();
        assert_eq!(
            polling, 1,
            "{name} must poll its controller every second while busy"
        );
    }
}

/// M1 (PR #108 security review): "Make a rule…" never claims the rule exists
/// by itself. Its only outcome text is `MakeRuleController`'s, which says
/// "created" only for the bridge's Ok result, shown as plain text.
#[test]
fn make_rule_sheet_says_only_what_the_bridge_answered() {
    let code = code_lines(MAKE_RULE_SHEET);
    for claim in ["created", "was sent", "sent to"] {
        assert!(
            !code.contains(claim),
            "MakeRuleSheet.qml says `{claim}` itself; only MakeRuleController's result may"
        );
    }
    assert!(code.contains(
        "readonly property string result: !!sheet.controller && sheet.controller.rowId === sheet.rowId\n        ? sheet.controller.statusText : \"\""
    ));
    let label = ["Controls.Label {", "SelectableText {"]
        .iter()
        .flat_map(|open| blocks(&code, open))
        .find(|block| has_line(block, "objectName: \"makeRuleResult\""))
        .expect("the result label");
    assert!(has_line(&label, "textFormat: Text.PlainText"), "{label}");
    assert!(has_line(&label, "text: sheet.result"), "{label}");

    // While another row's request waits, Allow and Deny are disabled; one
    // fixed line says why.
    let busy = ["Controls.Label {", "SelectableText {"]
        .iter()
        .flat_map(|open| blocks(&code, open))
        .find(|block| has_line(block, "objectName: \"makeRuleBusyElsewhere\""))
        .expect("the busy-elsewhere label");
    assert!(
        has_line(
            &busy,
            "visible: form.visible && sheet.bindableProcessPath && !!sheet.controller && sheet.controller.busy && sheet.controller.rowId !== sheet.rowId"
        ),
        "{busy}"
    );
    assert!(has_line(&busy, "textFormat: Text.PlainText"), "{busy}");
    let binding = text_binding(&busy).expect("the label has a text");
    assert!(is_fixed_text(&binding), "{binding}");
    assert!(
        binding.contains("Another rule is still being sent. Try again in a moment."),
        "{binding}"
    );
}

/// L4 (PR #111 review): the shortened deadline is for the headless probes
/// only; the app always waits `NO_ANSWER_AFTER` for a rule command result.
#[test]
fn no_shipped_qml_shortens_the_make_rule_deadline() {
    for (name, source) in ALL_QML {
        assert!(
            !source.contains("shortenDeadlineForTests"),
            "{name} shortens MakeRuleController's deadline; only the probes may"
        );
    }
}

/// H1 (PR #111 review): a passive notification renders rich text, and a
/// refusal's reason is bridge text, so the notice for an outcome off screen is
/// one of three FIXED strings. `finished` carries no text at all; the reason
/// stays in the sheet's plain-text result.
#[test]
fn make_rule_notices_are_fixed_text_only() {
    assert!(
        MAKE_RULE_CONTROLLER_RS.contains(
            "fn finished(self: Pin<&mut MakeRuleController>, row_id: QString, ending: i32);"
        ),
        "MakeRuleController.finished must carry no text"
    );
    let outcomes = code_lines(MAKE_RULE_OUTCOMES);
    for line in [
        "readonly property string createdText: \"The rule was created.\"",
        "readonly property string notCreatedText: \"A rule couldn't be created. Open that connection to see why.\"",
        "readonly property string unknownText: \"The firewall service didn't confirm the rule. Open that connection to see more.\"",
        "outcomes.notice(ending === 1 ? outcomes.createdText : ending === 0 ? outcomes.notCreatedText : outcomes.unknownText);",
    ] {
        assert!(has_line(&outcomes, line), "MakeRuleOutcomes.qml lost `{line}`");
    }
    assert!(has_line(&outcomes, "signal notice(string text)"));
    assert_eq!(
        outcomes.matches(".notice(").count(),
        1,
        "MakeRuleOutcomes.qml emits a notice other than the fixed one"
    );
    assert_eq!(
        outcomes.matches("readonly property string ").count(),
        3,
        "MakeRuleOutcomes.qml has exactly three fixed notices"
    );
    for data in ["status", "reason", "statusText"] {
        assert!(
            !outcomes.contains(data),
            "MakeRuleOutcomes.qml reads `{data}`: notices are fixed text only"
        );
    }
    // The sheet notifies nobody, and the window passes the fixed text on.
    let sheet = code_lines(MAKE_RULE_SHEET);
    for forbidden in ["explained", "showPassive", "finished"] {
        assert!(
            !sheet.contains(forbidden),
            "MakeRuleSheet.qml has `{forbidden}`: only MakeRuleOutcomes gives notices"
        );
    }
    assert!(has_line(
        &code_lines(MAIN_QML),
        "onNotice: text => root.showPassiveNotification(text, \"long\")"
    ));
}

/// E3 (PR #108 review): a put-off row's inspector says the firewall may list
/// the same connection again. One fixed sentence, plain text, shown only
/// where the row model says so (ConnectionsPage passes `rowDetailsJson`'s
/// `alsoListedByDefault` in).
#[test]
fn the_two_rows_hint_is_one_fixed_plain_text_line() {
    assert!(has_line(
        &code_lines(CONNECTIONS_PAGE),
        "alsoListedByDefault: page.inspectAlsoListedByDefault"
    ));
    let code = code_lines(MAKE_RULE_SHEET);
    let hints: Vec<String> = ["Controls.Label {", "SelectableText {"]
        .iter()
        .flat_map(|open| blocks(&code, open))
        .filter(|block| has_line(block, "id: alsoListedNote"))
        .collect();
    assert_eq!(hints.len(), 1, "expected one two-rows hint label");
    let hint = &hints[0];
    assert!(has_line(hint, "textFormat: Text.PlainText"), "{hint}");
    assert!(
        has_line(hint, "visible: sheet.alsoListedByDefault"),
        "{hint}"
    );
    let binding = text_binding(hint).expect("the hint has a text");
    assert!(is_fixed_text(&binding), "{binding}");
    // The literals joined, as the label renders them.
    let shown: String = binding.split('"').skip(1).step_by(2).collect();
    assert_eq!(
        shown,
        "The firewall may also list this connection, and its retries, separately as decided \
         by its default action."
    );
}
