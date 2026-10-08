//! Source-level guards for the "honest UI" fixes: surfaces that must not claim
//! more than the bridge actually delivers, and labels that must not render
//! daemon-derived strings as markup.
//!
//! Deliberately crude text assertions, for the same reason as
//! `qml_source_guards.rs`: cxx-qt-lib exposes no way to inspect a rendered
//! `Label`'s `textFormat` or an `InlineMessage`'s `showCloseButton` from Rust,
//! and `qmltestrunner` cannot load `com.snitchwatch.shell`. These keep a
//! regression from silently re-introducing what the fixes removed;
//! `honest_ui_pages_qml.rs` separately proves the edited pages still load.

mod qml_guard_support;
use qml_guard_support::*;

const BLOCKLISTS_PAGE: &str = include_str!("../qml/BlocklistsPage.qml");
const PROFILES_PAGE: &str = include_str!("../qml/ProfilesPage.qml");
const RULES_PAGE: &str = include_str!("../qml/RulesPage.qml");
const SIMULATOR_SHEET: &str = include_str!("../qml/RuleSimulatorSheet.qml");
const IMPORT_SHEET: &str = include_str!("../qml/RulesImportSheet.qml");
const EDITOR_SHEET: &str = include_str!("../qml/RuleEditorSheet.qml");
const PENDING_SHEET: &str = include_str!("../qml/PendingDecisionSheet.qml");
const MAKE_RULE_SHEET: &str = include_str!("../qml/MakeRuleSheet.qml");
const MAKE_RULE_OUTCOMES: &str = include_str!("../qml/MakeRuleOutcomes.qml");
const MAKE_RULE_CONTROLLER_RS: &str = include_str!("../src/make_rule_controller.rs");
const CONNECTIONS_PAGE: &str = include_str!("../qml/ConnectionsPage.qml");
const MAIN_QML: &str = include_str!("../qml/main.qml");
const SIZED_SHEET: &str = include_str!("../qml/SizedOverlaySheet.qml");

/// Every QML file the shell ships, for the guards that must hold everywhere
/// (checked against the directory by `all_qml_files_are_covered`).
const ALL_QML: &[(&str, &str)] = &[
    ("BlocklistsPage.qml", BLOCKLISTS_PAGE),
    ("ConnectionsPage.qml", CONNECTIONS_PAGE),
    (
        "DecideLaterButton.qml",
        include_str!("../qml/DecideLaterButton.qml"),
    ),
    (
        "DaemonHealthPage.qml",
        include_str!("../qml/DaemonHealthPage.qml"),
    ),
    (
        "DiagnosticsPage.qml",
        include_str!("../qml/DiagnosticsPage.qml"),
    ),
    ("GeoPage.qml", include_str!("../qml/GeoPage.qml")),
    (
        "InlineVerdicts.qml",
        include_str!("../qml/InlineVerdicts.qml"),
    ),
    ("main.qml", MAIN_QML),
    ("MakeRuleOutcomes.qml", MAKE_RULE_OUTCOMES),
    (
        "MakeRuleSheet.qml",
        include_str!("../qml/MakeRuleSheet.qml"),
    ),
    (
        "OnboardingPage.qml",
        include_str!("../qml/OnboardingPage.qml"),
    ),
    ("PendingDecisionSheet.qml", PENDING_SHEET),
    ("ProfilesPage.qml", PROFILES_PAGE),
    (
        "PromptSlotBanner.qml",
        include_str!("../qml/PromptSlotBanner.qml"),
    ),
    ("RuleEditorSheet.qml", EDITOR_SHEET),
    ("RuleSimulatorSheet.qml", SIMULATOR_SHEET),
    ("RulesImportSheet.qml", IMPORT_SHEET),
    ("RulesPage.qml", RULES_PAGE),
    ("ScannerPage.qml", include_str!("../qml/ScannerPage.qml")),
    ("SizedOverlaySheet.qml", SIZED_SHEET),
    ("TrafficPage.qml", include_str!("../qml/TrafficPage.qml")),
    ("TrayMenu.qml", include_str!("../qml/TrayMenu.qml")),
];

/// Issue #45 PR B: blocklists are enforced through daemon rules, but only a
/// list whose rule the daemon accepted says so. The Blocklists header holds
/// two warnings, neither dismissable nor jargon-laden:
/// - one keyed on `page.anyNotEnforced` (any list not "rule installed",
///   defaulting to false only while there is no model), saying lists are
///   not blocking and where to see why;
/// - one keyed on `!page.storagePersistent`, saying subscriptions are lost
///   on restart.
///
/// No warning may still claim blocklists are never applied ("Preview", "not
/// applied to the firewall yet"). The storage problem's reason is data, so
/// it goes in a PlainText label, never an InlineMessage.
fn assert_enforcement_keyed_banners(page_name: &str, source: &str) {
    let code = code_lines(source);
    for (property, default) in [
        ("storagePersistent", "false"),
        ("storageUnreadable", "false"),
        ("anyNotEnforced", "false"),
        ("perUserBlocklists", "false"),
        ("anyOverLimit", "false"),
    ] {
        let line = format!(
            "readonly property bool {property}: page.model ? page.model.{property} : {default}"
        );
        assert!(has_line(&code, &line), "{page_name} must declare `{line}`");
    }
    let header = blocks(&code, "header: ColumnLayout {");
    assert_eq!(header.len(), 1, "{page_name} lost its banner header");
    let banners = blocks(&header[0], "Kirigami.InlineMessage {");
    assert_eq!(banners.len(), 5, "{page_name}: expected five warnings");
    for banner in &banners {
        assert!(
            banner.contains("type: Kirigami.MessageType.Warning"),
            "{page_name}'s banner is no longer a Warning:\n{banner}"
        );
        assert!(
            !banner.contains("showCloseButton: true") && !banner.contains("actions:"),
            "{page_name}'s banner must not be dismissable:\n{banner}"
        );
        assert!(
            !banner.contains("bridge"),
            "{page_name}'s banner uses internal jargon (\"bridge\")"
        );
        assert!(
            !banner.contains("Preview") && !banner.contains("not applied to the firewall yet"),
            "{page_name}: blocklists are enforced now; no banner may say they never are:\n{banner}"
        );
    }
    let keyed = |visible: &str, says: &[&str]| {
        let banner = banners
            .iter()
            .find(|b| has_line(b, &format!("visible: {visible}")))
            .unwrap_or_else(|| panic!("{page_name}: no warning keyed on `{visible}`"));
        for phrase in says {
            assert!(
                banner.contains(phrase),
                "{page_name}: the `{visible}` warning must say \"{phrase}\":\n{banner}"
            );
        }
    };
    // A pending list isn't known not to block, only not confirmed to.
    keyed("page.anyNotEnforced", &["aren't confirmed"]);
    keyed(
        "page.perUserBlocklists",
        &["system-wide", "doesn't apply blocklists"],
    );
    keyed("page.anyOverLimit", &["2,000,000", "aren't applied"]);
    keyed(
        "page.storageUnreadable",
        &["couldn't read its saved blocklists"],
    );
    keyed("!page.storagePersistent", &["restart"]);
    let labels = blocks(&header[0], "Controls.Label {");
    let reasons: Vec<_> = labels
        .iter()
        .filter(|l| l.contains("page.storageReason"))
        .collect();
    assert_eq!(
        reasons.len(),
        1,
        "{page_name}: expected one storage-reason label"
    );
    assert!(
        reasons[0].contains("textFormat: Text.PlainText"),
        "{page_name}: the storage reason must be a PlainText label:\n{}",
        reasons[0]
    );
    // `lists.domains` is an exact lookup: say so whenever there are lists,
    // not only in the empty state.
    let exact = labels
        .iter()
        .find(|l| l.contains("exact name"))
        .unwrap_or_else(|| panic!("{page_name}: no exact-name note in the header"));
    assert!(
        exact.contains("not subdomains")
            && has_line(exact, "visible: page.model && page.model.count > 0"),
        "{page_name}: the exact-name note must show whenever lists exist:\n{exact}"
    );
}

/// Issue #45: a blocklist row says "Rule installed" only for a list the
/// daemon accepted; every other list is called out at the top of the page.
#[test]
fn blocklists_page_warns_while_any_list_is_not_enforced() {
    assert_enforcement_keyed_banners("BlocklistsPage.qml", BLOCKLISTS_PAGE);
}

/// Issue #45 (S2): the bridge never pushes a whole entry list (it overflowed
/// GUI clients), so the inspector must ask for the first page when it opens.
#[test]
fn blocklists_inspector_requests_entries_when_it_opens() {
    let code = code_lines(BLOCKLISTS_PAGE);
    let open = &code[code
        .find("function openInspector(row) {")
        .expect("openInspector moved")..];
    let body = &open[..open.find("\n    }").unwrap_or(open.len())];
    let expect = body
        .find("page.entriesModel.expectEntries(row.listId)")
        .unwrap_or_else(|| panic!("openInspector must note which list it wants:\n{body}"));
    let request = body
        .find("page.model.requestEntries(row.listId, 0)")
        .unwrap_or_else(|| panic!("openInspector must request the first entries page:\n{body}"));
    assert!(
        expect < request,
        "the wanted list must be set before its page can arrive"
    );
}

/// The empty-state copy sits right under the banner, so it must not promise
/// what the banner disclaims.
#[test]
fn blocklists_empty_state_does_not_promise_filtering() {
    assert!(
        !code_lines(BLOCKLISTS_PAGE).contains("start filtering"),
        "BlocklistsPage.qml's empty-state text promises filtering while the page's banner says \
         subscriptions are not applied to the firewall"
    );
}

/// Issue #51: `Controls.Label` defaults to `Text.AutoText`, which renders
/// anything that looks like HTML as rich text. Rule names, operator data and
/// blocklist ids come from the daemon / subscription URLs, so every label that
/// shows them must opt into `Text.PlainText`. Numeric / static expressions
/// (`row.precedence`, `row.enabled`, `sourceLabel(...)`) are intentionally
/// absent from the list.
#[test]
fn rules_page_labels_showing_rule_data_are_plain_text() {
    assert_data_labels_plain_text(
        "RulesPage.qml",
        RULES_PAGE,
        &[
            "row.name",
            "row.displayName",
            "row.operatorSummary",
            "row.ruleAction",
            "row.blocklistId",
            "page.inspectName",
            "page.inspectDisplayName",
            "page.inspectReadOnlyReason",
            "page.inspectSource",
            "page.inspectAction",
            "page.inspectDuration",
            "page.inspectOperatorSummary",
            // Issue #44: names the destination of an all-apps rule.
            "row.allAppsHint",
            // P2.7: export/import outcomes, which carry bridge reasons.
            "rulesIo.statusText",
            // P2.1: why the editor can't change a rule (bridge reasons).
            "page.inspectNotEditable",
            // P2.1: the editor's last result (bridge reasons).
            "ruleEditorController.statusText",
        ],
        11,
    );
}

/// The import preview shows names, conditions and reasons from an untrusted
/// file and the firewall service, so every label in it is plain text, and no
/// checkbox carries text (a CheckBox's text is AutoText).
#[test]
fn import_sheet_labels_are_all_plain_text_and_checkboxes_carry_no_text() {
    let code = code_lines(IMPORT_SHEET);
    let labels = blocks(&code, "Controls.Label {");
    assert!(labels.len() >= 12, "found {} labels", labels.len());
    for block in &labels {
        assert!(
            block.contains("textFormat: Text.PlainText"),
            "RulesImportSheet.qml has a label without PlainText:\n{block}"
        );
    }
    let checkboxes = blocks(&code, "Controls.CheckBox {");
    assert!(!checkboxes.is_empty());
    for block in checkboxes {
        assert!(
            text_binding(&block).is_none(),
            "RulesImportSheet.qml puts text on a CheckBox (AutoText):\n{block}"
        );
    }
}

/// The rule editor shows rule names, values and reasons from the firewall
/// service, so every label in it is plain text, and no checkbox carries text
/// (a CheckBox's text is AutoText).
#[test]
fn editor_sheet_labels_are_all_plain_text_and_checkboxes_carry_no_text() {
    let code = code_lines(EDITOR_SHEET);
    let labels = blocks(&code, "Controls.Label {");
    assert!(labels.len() >= 8, "found {} labels", labels.len());
    for block in &labels {
        assert!(
            block.contains("textFormat: Text.PlainText"),
            "RuleEditorSheet.qml has a label without PlainText:\n{block}"
        );
    }
    let checkboxes = blocks(&code, "Controls.CheckBox {");
    assert!(!checkboxes.is_empty());
    for block in checkboxes {
        assert!(
            text_binding(&block).is_none(),
            "RuleEditorSheet.qml puts text on a CheckBox (AutoText):\n{block}"
        );
    }
}

/// The Simulate sheet shows rule names and operands from the daemon, and
/// lines the simulator built from them.
#[test]
fn simulator_sheet_labels_showing_rule_data_are_plain_text() {
    assert_data_labels_plain_text(
        "RuleSimulatorSheet.qml",
        SIMULATOR_SHEET,
        &[
            "sheet.simulateMatchedRule",
            "sheet.simulateAction",
            "sheet.simulateUnsupported",
            "sheet.simulateUnevaluated",
            "sheet.simulateInvalid",
            "sheet.simulateWarnings",
        ],
        6,
    );
}

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
            "page.inspectHost",
            "page.inspectIp",
            "page.inspectProtocol",
            "page.inspectVerdict",
            "page.inspectMatchedRuleDisplay",
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
    let label = blocks(&code, "Controls.Label {")
        .into_iter()
        .find(|block| has_line(block, "objectName: \"makeRuleResult\""))
        .expect("the result label");
    assert!(has_line(&label, "textFormat: Text.PlainText"), "{label}");
    assert!(has_line(&label, "text: sheet.result"), "{label}");

    // While another row's request waits, Allow and Deny are disabled; one
    // fixed line says why.
    let busy = blocks(&code, "Controls.Label {")
        .into_iter()
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
        "outcomes.notice(ending === 1 ? outcomes.createdText : ending === 2 ? outcomes.unknownText : outcomes.notCreatedText);",
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
    let hints: Vec<String> = blocks(&code, "Controls.Label {")
        .into_iter()
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

/// Blocklist names, URLs, fetch-failure reasons and (above all) the host
/// entries are fetched from remote subscription URLs.
#[test]
fn blocklists_page_labels_showing_subscription_data_are_plain_text() {
    assert_data_labels_plain_text(
        "BlocklistsPage.qml",
        BLOCKLISTS_PAGE,
        &[
            "row.displayName",
            "row.url",
            "row.status",
            "row.enforcement",
            "page.inspectUrl",
            "page.inspectStatus",
            "page.inspectEnforcement",
            "page.inspectLastUpdated",
            "page.inspectLastFailureReason",
            "page.storageReason",
            "text: host",
        ],
        12,
    );
}

/// The decision prompt: the connecting program's name and the destination host
/// (plus the reverse-DNS / RDAP answers, which are fully remote-controlled)
/// are attacker-influenced text on the one surface the user must read to
/// decide safely.
#[test]
fn pending_decision_sheet_labels_showing_remote_data_are_plain_text() {
    assert_data_labels_plain_text(
        "PendingDecisionSheet.qml",
        PENDING_SHEET,
        &[
            "sheet.process",
            "insight.hostname",
            "insight.org",
            "insight.registrar",
            "insight.country",
        ],
        5,
    );
}

/// `Kirigami.InlineMessage` renders its `text` through a `SelectableLabel`
/// (AutoText) with no `textFormat` hook, and HTML-escaping doesn't fix that:
/// AutoText only decodes entities when it already judges the string to be
/// markup, so an escaped `&lt;b&gt;` shows up literally. Data therefore never
/// goes into an InlineMessage — only string literals joined by `+` do.
///
/// Scope: the decision prompt, Connections, Blocklists, Profiles and Rules.
/// `honest_ui_external_text_guards.rs` keeps the external text on `main.qml`
/// and the diagnostics pages out of InlineMessages; the banners left there
/// (`statusSummary`, the pending-age count) show only text built locally.
#[test]
fn inline_messages_carry_only_fixed_text() {
    let mut checked = 0;
    for (name, source) in [
        ("PendingDecisionSheet.qml", PENDING_SHEET),
        ("ConnectionsPage.qml", CONNECTIONS_PAGE),
        ("BlocklistsPage.qml", BLOCKLISTS_PAGE),
        ("ProfilesPage.qml", PROFILES_PAGE),
        ("RulesPage.qml", RULES_PAGE),
        ("RuleSimulatorSheet.qml", SIMULATOR_SHEET),
        ("RulesImportSheet.qml", IMPORT_SHEET),
        ("RuleEditorSheet.qml", EDITOR_SHEET),
    ] {
        for block in blocks(&code_lines(source), "Kirigami.InlineMessage {") {
            checked += 1;
            let binding = text_binding(&block).unwrap_or_default();
            assert!(
                is_fixed_text(&binding),
                "{name} has an InlineMessage whose text is not purely string literals joined by \
                 `+`. InlineMessage can't render data as plain text (issue #51); put the data \
                 in a `Controls.Label` with `textFormat: Text.PlainText` instead:\n{binding}"
            );
        }
    }
    assert!(
        checked >= 3,
        "expected the decision-prompt, Blocklists and Profiles InlineMessages, found {checked}"
    );
}

#[test]
fn fixed_text_matcher_accepts_literals_and_rejects_data() {
    assert!(is_fixed_text("text: \"a\""));
    assert!(is_fixed_text("text: \"a \\\" b\"\n    + \"c\""));
    assert!(!is_fixed_text("text: sheet.process"));
    assert!(!is_fixed_text("text: \"a\" + sheet.host"));
    assert!(!is_fixed_text("text: cond ? \"a\" : \"b\""));
    assert!(!is_fixed_text(""));
}

#[test]
fn text_binding_ignores_nested_children() {
    let block = "\n    type: Warning\n    actions: [\n        Kirigami.Action {\n            \
                 text: \"nested\"\n        }\n    ]\n    text: \"own\"\n        + \"more\"\n    \
                 visible: true\n";
    let binding = text_binding(block).unwrap();
    assert!(
        binding.contains("own") && binding.contains("more"),
        "{binding}"
    );
    assert!(!binding.contains("nested") && !binding.contains("visible"));
}

/// Tooltips: the style's default `ToolTip` text item (reached via the attached
/// `ToolTip.text`) is AutoText under Basic/Fusion, so hovering an elided,
/// markup-named title would render the markup — including remote `<img>`
/// loads. Nothing in the shell may use the attached text; a tooltip is
/// declared explicitly with a PlainText `contentItem`.
#[test]
fn tooltips_render_plain_text_only() {
    for (name, source) in ALL_QML {
        let code = code_lines(source);
        for forbidden in ["ToolTip.text", "ToolTip.toolTip"] {
            assert!(
                !code.contains(forbidden),
                "{name} uses `{forbidden}`, which draws through the style's AutoText tooltip \
                 label (issue #51). Declare `Controls.ToolTip {{ contentItem: Controls.Label {{ \
                 textFormat: Text.PlainText; ... }} }}` instead"
            );
        }
        for block in blocks(&code, "ToolTip {") {
            assert!(
                block.contains("contentItem: Controls.Label {")
                    && block.contains("textFormat: Text.PlainText"),
                "{name} declares a ToolTip without a PlainText Controls.Label contentItem \
                 (issue #51):\n{block}"
            );
        }
    }
}

/// `ALL_QML` must not silently miss a QML file added later.
#[test]
fn all_qml_files_are_covered() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/qml");
    let mut on_disk: Vec<String> = std::fs::read_dir(dir)
        .expect("read qml dir")
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".qml"))
        .collect();
    on_disk.sort();
    let mut listed: Vec<String> = ALL_QML.iter().map(|(n, _)| n.to_string()).collect();
    listed.sort();
    assert_eq!(
        on_disk, listed,
        "ALL_QML in honest_ui_qml_guards.rs is out of sync with qml/"
    );
}

/// Sheet titles are data too (process name, rule name, subscription name,
/// profile name). `Kirigami.OverlaySheet` draws `title` with its own default
/// header `Heading` (AutoText, no `textFormat` hook through `title:`), so
/// `SizedOverlaySheet` supplies a PlainText header once for every sheet — and
/// no page may bypass it with a bare `Kirigami.OverlaySheet`.
#[test]
fn overlay_sheet_titles_are_plain_text() {
    let code = code_lines(SIZED_SHEET);
    let start = code
        .find("header: Kirigami.Heading {")
        .expect("SizedOverlaySheet.qml lost its PlainText header override");
    let header = &code[start..];
    let header = &header[..header.find("\n    }").unwrap_or(header.len())];
    assert!(
        header.contains("textFormat: Text.PlainText") && header.contains("sheet.title"),
        "SizedOverlaySheet.qml's header must render `sheet.title` with \
         `textFormat: Text.PlainText`:\n{header}"
    );
    for (name, source) in [
        ("ConnectionsPage.qml", CONNECTIONS_PAGE),
        ("BlocklistsPage.qml", BLOCKLISTS_PAGE),
        ("ProfilesPage.qml", PROFILES_PAGE),
        ("RulesPage.qml", RULES_PAGE),
        ("RuleSimulatorSheet.qml", SIMULATOR_SHEET),
        ("RulesImportSheet.qml", IMPORT_SHEET),
        ("RuleEditorSheet.qml", EDITOR_SHEET),
    ] {
        assert!(
            !code_lines(source).contains("Kirigami.OverlaySheet {"),
            "{name} declares a bare Kirigami.OverlaySheet, bypassing SizedOverlaySheet's \
             PlainText title header (issue #51)"
        );
    }
}

/// Issue #49: the bridge hardcodes per-connection bytes to 0 and opensnitchd
/// has no per-connection counters, so the "This connection: 0 B sent / 0 B
/// received" readout and the sparkline fed by those same zeros were always
/// false. They stay hidden until a real byte source exists.
#[test]
fn per_connection_traffic_readouts_stay_hidden() {
    let sheet = code_lines(PENDING_SHEET);
    for forbidden in [
        "This connection",
        "bytesSent",
        "bytesReceived",
        "Canvas",
        "seriesJson",
    ] {
        assert!(
            !sheet.contains(forbidden),
            "PendingDecisionSheet.qml shows `{forbidden}` again — the bridge reports zero \
             per-connection bytes, so this would display a fabricated 0 B readout / flat \
             sparkline (issue #49)"
        );
    }
    let page = code_lines(CONNECTIONS_PAGE);
    for forbidden in [
        "inspectBytes",
        "bytesSent:",
        "bytesReceived:",
        "trafficModel",
    ] {
        assert!(
            !page.contains(forbidden),
            "ConnectionsPage.qml feeds `{forbidden}` to the inspector again (issue #49)"
        );
    }
    let main = code_lines(MAIN_QML);
    assert!(
        !main.contains("trafficModelRef"),
        "main.qml threads the zero-fed TrafficModel into ConnectionsPage again (issue #49)"
    );
}
