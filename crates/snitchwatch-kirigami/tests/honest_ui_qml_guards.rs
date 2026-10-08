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
const PENDING_SHEET: &str = include_str!("../qml/PendingDecisionSheet.qml");
const CONNECTIONS_PAGE: &str = include_str!("../qml/ConnectionsPage.qml");
const MAIN_QML: &str = include_str!("../qml/main.qml");

/// Drop whole-line `//` comments so a guard can't trip over prose that merely
/// names the thing it forbids (same helper shape as `qml_source_guards.rs`).
fn code_lines(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `Controls.Label { ... }` body in `source`, found by brace counting
/// (braces inside double-quoted strings are ignored). Good enough for the flat,
/// one-property-per-line style these pages are written in.
fn label_blocks(source: &str) -> Vec<String> {
    const OPEN: &str = "Controls.Label {";
    let mut blocks = Vec::new();
    let mut rest = source;
    while let Some(start) = rest.find(OPEN) {
        let body_start = start + OPEN.len();
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
        blocks.push(rest[body_start..end].to_string());
        rest = &rest[end..];
    }
    blocks
}

/// The banner must be an `InlineMessage` of type Warning, shown
/// unconditionally and without a close button — a dismissable "this isn't
/// enforced" notice defeats its purpose.
fn assert_preview_banner(page_name: &str, source: &str) {
    let code = code_lines(source);
    let start = code
        .find("Kirigami.InlineMessage {")
        .unwrap_or_else(|| panic!("{page_name} lost its not-enforced InlineMessage banner"));
    let banner = &code[start..];
    let banner = &banner[..banner.find("\n    }").unwrap_or(banner.len())];
    assert!(
        banner.contains("type: Kirigami.MessageType.Warning"),
        "{page_name}'s banner is no longer a Warning"
    );
    assert!(
        banner.contains("visible: true"),
        "{page_name}'s banner must be unconditionally visible"
    );
    assert!(
        !banner.contains("showCloseButton: true") && !banner.contains("actions:"),
        "{page_name}'s banner must not be dismissable"
    );
    assert!(
        banner.contains("not applied") && banner.contains("restart"),
        "{page_name}'s banner must say the data is not applied to the firewall and is lost on \
         bridge restart"
    );
}

/// Issues #45/#46: production wires no-op rule sinks and in-memory stores
/// (`snitchwatch-bridge-cli/src/lib.rs`), so these tabs look functional while
/// enforcing nothing and forgetting everything on restart.
#[test]
fn blocklists_page_warns_it_is_not_enforced() {
    assert_preview_banner("BlocklistsPage.qml", BLOCKLISTS_PAGE);
}

#[test]
fn profiles_page_warns_it_is_not_enforced() {
    assert_preview_banner("ProfilesPage.qml", PROFILES_PAGE);
}

/// Issue #51: `Controls.Label` defaults to `Text.AutoText`, which renders
/// anything that looks like HTML as rich text. Rule names, operator data and
/// blocklist ids come from the daemon / subscription URLs, so every label that
/// shows them must opt into `Text.PlainText`.
#[test]
fn rules_page_labels_showing_rule_data_are_plain_text() {
    // Expressions that carry rule-derived strings. Numeric / static ones
    // (`row.precedence`, `row.enabled`, `sourceLabel(...)`) are intentionally
    // absent.
    const RULE_DATA: &[&str] = &[
        "row.name",
        "row.operatorSummary",
        "row.ruleAction",
        "row.blocklistId",
        "page.inspectName",
        "page.inspectSource",
        "page.inspectAction",
        "page.inspectDuration",
        "page.inspectOperatorSummary",
        "page.simulateMatchedRule",
        "page.simulateAction",
        "page.simulateUnsupported",
    ];

    let code = code_lines(RULES_PAGE);
    let mut checked = 0;
    for block in label_blocks(&code) {
        let shows_rule_data = block
            .lines()
            .filter(|l| l.contains("text:") || l.trim_start().starts_with('+'))
            .any(|l| RULE_DATA.iter().any(|d| l.contains(d)));
        if !shows_rule_data {
            continue;
        }
        checked += 1;
        assert!(
            block.contains("textFormat: Text.PlainText"),
            "RulesPage.qml label shows rule data without `textFormat: Text.PlainText`:\n{block}"
        );
    }
    assert!(
        checked >= 11,
        "expected at least 11 rule-data labels in RulesPage.qml (row name/summary/action, \
         inspector name/source/action/duration/target, simulator matched/action/unsupported), \
         found {checked} — did the guard's matcher drift from the page?"
    );
}

/// The inspector sheet's title is the rule name. `Kirigami.OverlaySheet`
/// draws `title` with its own default `Kirigami.Heading` (AutoText, no way to
/// set `textFormat` through `title:`), so RulesPage supplies its own heading
/// via `header:` — otherwise a rule named `<b>x</b>` renders as markup there
/// even though every `Controls.Label` below it is plain text.
#[test]
fn rules_page_inspector_title_is_plain_text() {
    let code = code_lines(RULES_PAGE);
    let start = code
        .find("header: Kirigami.Heading {")
        .expect("RulesPage.qml's inspector lost its PlainText header override");
    let header = &code[start..];
    let header = &header[..header.find("\n        }").unwrap_or(header.len())];
    assert!(
        header.contains("textFormat: Text.PlainText") && header.contains("inspector.title"),
        "RulesPage.qml's inspector header must render `inspector.title` with \
         `textFormat: Text.PlainText`:\n{header}"
    );
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
