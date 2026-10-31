//! Honest-UI guards for the Blocklists, Rules, simulator, import/editor and
//! pending-decision pages: what they say must match what the bridge delivers,
//! and labels showing daemon-derived text stay plain text.

mod qml_guard_support;
use qml_guard_support::*;

/// Issue #45 PR B: blocklists are enforced through daemon rules, but only a
/// list whose rule the daemon accepted says so. The Blocklists header holds
/// two warnings, neither dismissable nor jargon-laden:
/// - one keyed on `page.anyNotEnforced` (and not the unreadable store) (any list not "rule installed",
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
    keyed(
        "page.anyNotEnforced && !page.storageUnreadable",
        &["aren't confirmed"],
    );
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
    let mut labels = blocks(&header[0], "Controls.Label {");
    labels.extend(blocks(&header[0], "SelectableText {"));
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
/// (`row.precedence`, `row.enabled`) are intentionally
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
            "host.inspectName",
            "page.inspectDisplayName",
            "host.inspectDisplayName",
            "page.inspectReadOnlyReason",
            "host.inspectReadOnlyReason",
            "page.inspectSource",
            "host.inspectSource",
            "page.inspectAction",
            "host.inspectAction",
            "page.inspectDuration",
            "host.inspectDuration",
            "page.inspectOperatorSummary",
            "host.inspectOperatorSummary",
            // Issue #44: names the destination of an all-apps rule.
            "row.allAppsHint",
            // Issue #64: the flagged row's badge.
            "row.flagBadge",
            // Issue #61: what the list leaves out.
            "page.model.notShownText",
            // The inspector's Source, from Rust (`rules::sections`).
            "page.inspectSourceLabel",
            "host.inspectSourceLabel",
            // P2.7: export/import outcomes, which carry bridge reasons.
            "rulesIo.statusText",
            // P2.1: why the editor can't change a rule (bridge reasons).
            "page.inspectNotEditable",
            "host.inspectNotEditable",
            // P2.1: the editor's last result (bridge reasons).
            "ruleEditorController.statusText",
            // P2.6: a finding names the covering rule, which is rule data.
            "row.shadowText",
        ],
        12,
    );
}

/// The import preview shows names, conditions and reasons from an untrusted
/// file and the firewall service, so every label in it is plain text, and no
/// checkbox carries text (a CheckBox's text is AutoText).
#[test]
fn import_sheet_labels_are_all_plain_text_and_checkboxes_carry_no_text() {
    let code = code_lines(IMPORT_SHEET);
    let mut labels = blocks(&code, "Controls.Label {");
    labels.extend(blocks(&code, "SelectableText {"));
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
    let mut labels = blocks(&code, "Controls.Label {");
    labels.extend(blocks(&code, "SelectableText {"));
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

/// Issue #102: only deny, reject and decide-first rules stop opensnitchd's
/// check, so the inspector never says "first match wins" of every rule.
#[test]
fn the_rule_inspector_says_how_each_rule_decides() {
    let code = code_lines(RULES_PAGE);
    assert!(
        !code.contains("first match wins"),
        "RulesPage.qml claims first match wins"
    );
    assert!(code.contains("page.inspectHowItDecides"));
}

/// Issue #61: "No rules yet" never shows under "N rules aren't listed";
/// `rules_all_apps_qml` checks the property, this its use.
#[test]
fn the_empty_rules_placeholder_follows_what_is_not_listed() {
    let code = code_lines(RULES_PAGE);
    assert!(code.contains("visible: page.showsEmptyPlaceholder"));
    assert!(code.contains("&& !page.showsNotShown"));
    assert!(code.contains("RulesEmptyPlaceholder {\n            parent: list"));
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
            "page.leftoverReason",
            "text: host",
        ],
        13,
    );
}

/// Prompt-slot D: each entry's program, what it allows, why, its status and
/// problem, and the bridge's reasons are the bridge's text; so are a "No
/// longer recommended" rule's name (#113: the daemon's), status and problem.
#[test]
fn recommended_rules_page_labels_showing_bridge_text_are_plain_text() {
    assert_data_labels_plain_text(
        "RecommendedRulesPage.qml",
        RECOMMENDED_PAGE,
        &[
            "row.program",
            "row.allows",
            "row.why",
            "row.statusText",
            "row.problem",
            "page.unavailableReason",
            "orphanRow.modelData.ruleName",
            "orphanRow.modelData.statusText",
            "orphanRow.modelData.problem",
        ],
        9,
    );
}

/// The first-run groups step shows the bridge's group titles and descriptions
/// and each entry's program, what it allows, why, and status.
#[test]
fn curated_groups_step_labels_showing_bridge_text_are_plain_text() {
    assert_data_labels_plain_text(
        "CuratedGroupsStep.qml",
        include_str!("../qml/CuratedGroupsStep.qml"),
        &[
            "groupRow.modelData.title",
            "groupRow.modelData.description",
            "modelData.program",
            "modelData.allows",
            "modelData.why",
            "modelData.statusText",
            "step.model.consentResult",
        ],
        7,
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

/// A per-user service reaches the firewall service over the legacy TCP
/// connection, where nothing is offered to remove (any local process could
/// pose as the daemon). Its banner says so in fixed text, since there is no
/// other sign on the page.
#[test]
fn the_per_user_banner_says_leftover_rules_cant_be_checked_from_here() {
    let code = code_lines(BLOCKLISTS_PAGE);
    let banner = blocks(&code, "Kirigami.InlineMessage {")
        .into_iter()
        .find(|block| block.contains("objectName: \"perUserBanner\""))
        .expect("the per-user banner");
    let text = text_binding(&banner).unwrap_or_default();
    assert!(is_fixed_text(&text), "the banner text is fixed:\n{text}");
    for part in [
        "rules a system-wide Snitchwatch service left in the firewall can't be checked ",
        "or removed from here; start the system-wide service to remove them.",
    ] {
        assert!(text.contains(part), "banner lacks '{part}':\n{text}");
    }
}
