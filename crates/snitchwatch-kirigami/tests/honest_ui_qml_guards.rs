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

const BLOCKLISTS_PAGE: &str = include_str!("../qml/BlocklistsPage.qml");
const PROFILES_PAGE: &str = include_str!("../qml/ProfilesPage.qml");
const RULES_PAGE: &str = include_str!("../qml/RulesPage.qml");
const SIMULATOR_SHEET: &str = include_str!("../qml/RuleSimulatorSheet.qml");
const IMPORT_SHEET: &str = include_str!("../qml/RulesImportSheet.qml");
const EDITOR_SHEET: &str = include_str!("../qml/RuleEditorSheet.qml");
const PENDING_SHEET: &str = include_str!("../qml/PendingDecisionSheet.qml");
const CONNECTIONS_PAGE: &str = include_str!("../qml/ConnectionsPage.qml");
const MAIN_QML: &str = include_str!("../qml/main.qml");
const SIZED_SHEET: &str = include_str!("../qml/SizedOverlaySheet.qml");

/// Every QML file the shell ships, for the guards that must hold everywhere
/// (checked against the directory by `all_qml_files_are_covered`).
const ALL_QML: &[(&str, &str)] = &[
    ("BlocklistsPage.qml", BLOCKLISTS_PAGE),
    ("ConnectionsPage.qml", CONNECTIONS_PAGE),
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

/// Drop whole-line `//` comments so a guard can't trip over prose that merely
/// names the thing it forbids (same helper shape as `qml_source_guards.rs`).
fn code_lines(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `<open> ... }` body in `source` (e.g. open = `Controls.Label {`),
/// found by brace counting (braces inside double-quoted strings are ignored).
/// Good enough for the flat, one-property-per-line style these pages are
/// written in.
fn blocks(source: &str, open: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = source;
    while let Some(start) = rest.find(open) {
        let body_start = start + open.len();
        let mut depth = 1usize;
        let mut in_string = false;
        let mut end = rest.len();
        for (i, ch) in rest[body_start..].char_indices() {
            match ch {
                '"' => in_string = !in_string,
                '{' if !in_string => depth += 1,
                '}' if !in_string => {
                    depth -= 1;
                    if depth == 0 {
                        end = body_start + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        found.push(rest[body_start..end].to_string());
        rest = &rest[end..];
    }
    found
}

/// The block's own `text:` binding: the `text:` line at the block's top
/// property indent (so a nested child's `text:` — an action inside `actions:`,
/// say — is never picked up) plus any continuation lines indented deeper than
/// it (how multi-line ternaries / `+` chains are written here), so a later
/// `color:` line can't be mistaken for part of the text.
fn text_binding(block: &str) -> Option<String> {
    let lines: Vec<&str> = block.lines().collect();
    let indent = |l: &str| l.len() - l.trim_start().len();
    let base = indent(lines.iter().find(|l| !l.trim().is_empty())?);
    let start = lines
        .iter()
        .position(|l| indent(l) == base && l.trim_start().starts_with("text:"))?;
    let mut binding = lines[start].to_string();
    for line in &lines[start + 1..] {
        if line.trim().is_empty() || indent(line) <= base {
            break;
        }
        binding.push('\n');
        binding.push_str(line);
    }
    Some(binding)
}

/// Whether a `text:` binding is nothing but double-quoted string literals
/// joined by `+` — no identifiers, property reads, calls or ternaries, so it
/// cannot carry data.
fn is_fixed_text(binding: &str) -> bool {
    let Some(expr) = binding.trim_start().strip_prefix("text:") else {
        return false;
    };
    let mut chars = expr.chars();
    let mut literals = 0;
    while let Some(ch) = chars.next() {
        match ch {
            c if c.is_whitespace() || c == '+' => {}
            '"' => {
                literals += 1;
                loop {
                    match chars.next() {
                        Some('\\') => {
                            chars.next();
                        }
                        Some('"') => break,
                        Some(_) => {}
                        None => return false,
                    }
                }
            }
            _ => return false,
        }
    }
    literals > 0
}

/// Every `Controls.Label` whose `text:` binding mentions one of `data_exprs`
/// (daemon / subscription / remote-derived strings) must set
/// `textFormat: Text.PlainText`. `min_checked` stops the guard passing
/// vacuously if the matcher drifts from the page.
fn assert_data_labels_plain_text(
    page_name: &str,
    source: &str,
    data_exprs: &[&str],
    min_checked: usize,
) {
    let code = code_lines(source);
    let mut checked = 0;
    for block in blocks(&code, "Controls.Label {") {
        let Some(binding) = text_binding(&block) else {
            continue;
        };
        if !data_exprs.iter().any(|d| binding.contains(d)) {
            continue;
        }
        checked += 1;
        assert!(
            block.contains("textFormat: Text.PlainText"),
            "{page_name} label shows data without `textFormat: Text.PlainText` — \
             `Controls.Label` defaults to AutoText, which renders markup in daemon-derived \
             strings (issue #51):\n{block}"
        );
    }
    assert!(
        checked >= min_checked,
        "expected at least {min_checked} data-bearing labels in {page_name}, found {checked} — \
         did the guard's matcher drift from the page?"
    );
}

/// The one line of `block` that is exactly `line` (trimmed).
fn has_line(block: &str, line: &str) -> bool {
    block.lines().any(|l| l.trim() == line)
}

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

/// Issue #46 Part 1: profiles are saved when the bridge says so, but never
/// applied to the firewall. The Profiles header holds two fixed-text
/// warnings, neither dismissable nor jargon-laden:
/// - one shown unconditionally (`visible: true`), saying profiles are not
///   applied to the firewall and activating one installs no rules, until
///   profiles can hold rules of their own;
/// - one keyed on `!page.storagePersistent` (false only while there is no
///   model), saying profiles are lost on restart;
///
/// plus a note keyed on `page.storagePersistent` saying they are saved, and
/// the storage problem's reason in a PlainText label (data never goes in an
/// InlineMessage, issue #51).
fn assert_profiles_banners(page_name: &str, source: &str) {
    let code = code_lines(source);
    for line in [
        "readonly property bool storagePersistent: page.model ? page.model.storagePersistent : false",
        "readonly property string storageReason: page.model ? page.model.storageReason : \"\"",
    ] {
        assert!(has_line(&code, line), "{page_name} must declare `{line}`");
    }
    let header = blocks(&code, "header: ColumnLayout {");
    assert_eq!(header.len(), 1, "{page_name} lost its banner header");
    let banners = blocks(&header[0], "Kirigami.InlineMessage {");
    assert_eq!(banners.len(), 2, "{page_name}: expected two warnings");
    assert_plain_warnings(page_name, &banners);
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
    keyed(
        "true",
        &["not applied to the firewall", "no firewall rules"],
    );
    keyed("!page.storagePersistent", &["memory only", "restart"]);
    assert_profiles_storage_labels(page_name, &blocks(&header[0], "Controls.Label {"));
}

/// Every banner is a Warning, can't be dismissed and says "Snitchwatch's
/// background service", not "bridge".
fn assert_plain_warnings(page_name: &str, banners: &[String]) {
    for banner in banners {
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
            "{page_name}'s banner uses internal jargon (\"bridge\"); say \"Snitchwatch's \
             background service\" instead"
        );
    }
}

/// The Profiles header's labels: a fixed-text note shown while profiles are
/// saved, and exactly one PlainText label for the storage reason.
fn assert_profiles_storage_labels(page_name: &str, labels: &[String]) {
    let saved = labels
        .iter()
        .find(|l| has_line(l, "visible: page.storagePersistent"))
        .unwrap_or_else(|| panic!("{page_name}: no note saying profiles are saved"));
    assert!(
        saved.contains("are saved") && is_fixed_text(&text_binding(saved).unwrap_or_default()),
        "{page_name}: the saved note must be fixed text saying profiles are saved:\n{saved}"
    );
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
}

/// Issue #46: profiles install no daemon rules, and are lost on restart
/// unless the bridge says they are saved.
#[test]
fn profiles_page_warns_it_is_not_enforced() {
    assert_profiles_banners("ProfilesPage.qml", PROFILES_PAGE);
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

#[test]
fn profiles_page_labels_showing_profile_data_are_plain_text() {
    assert_data_labels_plain_text(
        "ProfilesPage.qml",
        PROFILES_PAGE,
        &["row.name", "row.networkMatchers", "page.storageReason"],
        3,
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
