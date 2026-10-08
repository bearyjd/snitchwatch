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
const SIZED_SHEET: &str = include_str!("../qml/SizedOverlaySheet.qml");

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

/// The block's `text:` binding: the `text:` line plus any continuation lines
/// indented deeper than it (how multi-line ternaries / `+` chains are written
/// here), so a later `color:` line can't be mistaken for part of the text.
fn text_binding(block: &str) -> Option<String> {
    let lines: Vec<&str> = block.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim_start().starts_with("text:"))?;
    let indent = |l: &str| l.len() - l.trim_start().len();
    let base = indent(lines[start]);
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
        ],
        11,
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
            "page.inspectUrl",
            "page.inspectStatus",
            "page.inspectLastUpdated",
            "page.inspectLastFailureReason",
            "text: host",
        ],
        8,
    );
}

#[test]
fn profiles_page_labels_showing_profile_data_are_plain_text() {
    assert_data_labels_plain_text(
        "ProfilesPage.qml",
        PROFILES_PAGE,
        &["row.name", "row.networkMatchers"],
        2,
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
/// goes into an InlineMessage — only fixed text does.
#[test]
fn inline_messages_carry_only_fixed_text() {
    for (name, source) in [
        ("PendingDecisionSheet.qml", PENDING_SHEET),
        ("ConnectionsPage.qml", CONNECTIONS_PAGE),
        ("BlocklistsPage.qml", BLOCKLISTS_PAGE),
        ("ProfilesPage.qml", PROFILES_PAGE),
        ("RulesPage.qml", RULES_PAGE),
    ] {
        for block in blocks(&code_lines(source), "Kirigami.InlineMessage {") {
            let binding = text_binding(&block).unwrap_or_default();
            for forbidden in ["sheet.", "page.", "row.", "model."] {
                assert!(
                    !binding.contains(forbidden),
                    "{name} interpolates `{forbidden}...` data into an InlineMessage's text, \
                     which can't be rendered as plain text (issue #51). Put the data in a \
                     `Controls.Label` with `textFormat: Text.PlainText` instead:\n{binding}"
                );
            }
        }
    }
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
