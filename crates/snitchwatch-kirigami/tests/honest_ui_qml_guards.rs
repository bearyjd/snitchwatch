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
