//! Honest-UI guards for the rule buttons and the map band.

mod qml_guard_support;
use qml_guard_support::*;

/// The rule button's message bar and confirm dialog show the model's refusal
/// reason and the bridge's status (data): both must be plain text.
#[test]
fn level_rule_actions_labels_showing_bridge_text_are_plain_text() {
    assert_data_labels_plain_text(
        "LevelRuleActions.qml",
        include_str!("../qml/LevelRuleActions.qml"),
        &[
            "actions.message",
            "actions.confirmText",
            "actions.asked.program",
        ],
        3,
    );
    assert_data_labels_plain_text(
        "RuleButton.qml",
        include_str!("../qml/RuleButton.qml"),
        &["root.tipText", "root.glyph", "root.stateLabel"],
        3,
    );
}

/// Colour is never the only cue (Phase 2 risks): every rule button state has
/// its own glyph and label, none shared, and the label is drawn next to the
/// glyph. The live state table is checked in `rule_button_qml.rs`.
#[test]
fn rule_button_states_each_have_a_distinct_glyph_and_label() {
    let code = code_lines(include_str!("../qml/RuleButton.qml"));
    let mut glyphs = Vec::new();
    let mut labels = Vec::new();
    for state in ["allow", "deny", "partial", "none", "pending", "unavailable"] {
        let line = code
            .lines()
            .find(|l| l.trim_start().starts_with(&format!("{state}: {{")))
            .unwrap_or_else(|| panic!("no table row for {state}"));
        let field = |name: &str| {
            let rest = line.split(&format!("{name}: \"")).nth(1);
            rest.and_then(|r| r.split('"').next()).map(str::to_owned)
        };
        glyphs.push(field("glyph").unwrap_or_else(|| panic!("{state}: no glyph")));
        labels.push(field("label").unwrap_or_else(|| panic!("{state}: no label")));
    }
    for set in [&glyphs, &labels] {
        let mut unique = set.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), set.len(), "duplicate in {set:?}");
        assert!(set.iter().all(|t| !t.is_empty()), "empty text in {set:?}");
    }
    assert!(code.contains("text: root.glyph") && code.contains("text: root.stateLabel"));
}

/// The map band must say what its markers are and are not (phase 2 risks):
/// the country is approximate (CDN and anycast addresses can mislead), pulses
/// mean a connection event and not data volume, and the shapes' source. The
/// wording is pinned in one `text:` line of `MapBand.qml`, always visible
/// while the map is shown, and rendered as plain text.
#[test]
fn map_footer_states_what_the_markers_mean() {
    const FOOTER: &str = "Markers show approximate database locations of observed destination IPs, not physical endpoints (CDN and anycast addresses can mislead). Coincident IPs share a selectable stack; missing city/coordinates stay in Destinations. Pulses mean a connection event, not data volume. Country shapes: Natural Earth.";
    let code = code_lines(include_str!("../qml/MapBand.qml"));
    let footers: Vec<String> = blocks(&code, "Controls.Label {")
        .into_iter()
        .filter(|b| b.contains("id: honestText"))
        .collect();
    assert_eq!(footers.len(), 1, "MapBand.qml lost its honest footer");
    assert_eq!(
        text_binding(&footers[0]).as_deref().map(str::trim),
        Some(format!("text: \"{FOOTER}\"").as_str()),
        "the map footer wording changed; update this pin deliberately"
    );
    assert!(footers[0].contains("textFormat: Text.PlainText"));
    assert!(
        !footers[0].contains("visible:") && !footers[0].contains("opacity: 0\n"),
        "the footer must not be hideable on its own"
    );
}

/// The missing-database state reuses the Geography page's wording and offers
/// no download.
#[test]
fn map_setup_state_matches_the_geography_page_and_never_downloads() {
    let map = code_lines(include_str!("../qml/MapView.qml"));
    assert!(map.contains("No GeoIP database installed"));
    assert!(map.contains("Snitchwatch never downloads one automatically."));
    let lower = map.to_lowercase();
    assert!(!lower.contains("download button") && !lower.contains("onclicked: download"));
    assert!(include_str!("../qml/GeoPage.qml").contains("No GeoIP database installed"));
}

/// Colour is never the only cue on a map marker: a distinct shape per
/// verdict, plus a glyph, plus an accessible name that states the counts.
#[test]
fn map_markers_encode_verdict_by_shape_and_glyph_not_colour_alone() {
    let code = code_lines(include_str!("../qml/MapMarker.qml"));
    assert!(code_lines(include_str!("../qml/MapView.qml")).contains("longDuration <= 1"));
    for needle in [
        "dominant === \"pending\" ? 45 : 0",
        "dominant === \"denied\" ? \"×\"",
        "dominant === \"allowed\" ? \"✓\"",
        "Accessible.name: cluster.summary",
        "visible: marker.cluster.mixed",
    ] {
        assert!(code.contains(needle), "MapMarker.qml lost `{needle}`");
    }
}

/// The map band's collapse state and height are session-only: a fresh start
/// shows the map. Nothing in the band may read or write settings.
#[test]
fn map_band_state_is_never_persisted() {
    for (name, source) in [
        ("MapBandState.qml", include_str!("../qml/MapBandState.qml")),
        ("MapBand.qml", include_str!("../qml/MapBand.qml")),
        ("MonitorPane.qml", include_str!("../qml/MonitorPane.qml")),
    ] {
        let lower = code_lines(source).to_lowercase();
        for forbidden in ["settings", "localstorage", "qt.labs"] {
            assert!(
                !lower.contains(forbidden),
                "{name} must not persist state ({forbidden})"
            );
        }
    }
    assert!(code_lines(include_str!("../qml/MapBandState.qml"))
        .contains("property bool collapsed: false"));
}

/// The map is featured in the LEFT pane: the top of a vertical SplitView,
/// above the tree, never in the page header or the top bar, and the inspector
/// stays the right-hand column. Dragging to zero collapses it.
#[test]
fn map_band_sits_above_the_tree_in_the_left_pane() {
    let pane = code_lines(include_str!("../qml/MonitorPane.qml"));
    assert!(
        pane.contains("orientation: Qt.Vertical"),
        "the map/tree split must be vertical"
    );
    let band = pane
        .find("MapBand {")
        .expect("MonitorPane lost its MapBand");
    let tree = pane
        .find("ConnectionList {")
        .expect("MonitorPane lost its tree");
    assert!(band < tree, "the map band must sit above the tree");
    assert!(
        pane.contains("band.followDrag(band.height)")
            && code_lines(include_str!("../qml/MapBand.qml"))
                .contains("MapBandState.collapsed = body < band.collapseBelow"),
        "dragging to zero must collapse"
    );
    assert!(pane.contains("Controls.SplitView.fillHeight: true"));
    for (name, source) in [
        (
            "ConnectionsTopBar.qml",
            include_str!("../qml/ConnectionsTopBar.qml"),
        ),
        (
            "ConnectionsFilterBar.qml",
            include_str!("../qml/ConnectionsFilterBar.qml"),
        ),
    ] {
        assert!(
            !code_lines(source).contains("MapBand"),
            "{name} must not host the map"
        );
    }
    let page = code_lines(include_str!("../qml/ConnectionsPage.qml"));
    assert!(
        !page.contains("MapBand {"),
        "the page hosts the map through MonitorPane"
    );
    let monitor = page
        .find("MonitorPane {")
        .expect("the page lost MonitorPane");
    let inspector = page
        .find("InspectorPanel {")
        .expect("the page lost its inspector");
    assert!(
        monitor < inspector,
        "the inspector stays the right-hand panel"
    );
}

/// The splitter's height cap in `MonitorPane.qml`: while a handle drag is in
/// progress the band may grow to its full expanded size even when it starts
/// collapsed, and it never takes the tree's minimum. `tests/qml/
/// tst_map_split_drag.qml` mirrors these exact expressions; keep them in step.
#[test]
fn map_band_height_cap_lets_a_collapsed_band_reopen() {
    let pane: String = code_lines(include_str!("../qml/MonitorPane.qml"))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for needle in [
        "Controls.SplitView.maximumHeight: Math.min( split.resizing ? band.chromeHeight + band.maximumBody : band.maximumHeight, Math.max(split.resizing ? band.collapsedHeight : band.minimumHeight, split.height - pane.treeMinimum - pane.handleHeight))",
        "Controls.SplitView.minimumHeight: split.resizing ? band.collapsedHeight : band.minimumHeight",
        "onPreferredHeightChanged: if (!split.resizing) pane.applyPreferred()",
        "onResizingChanged: if (!split.resizing) pane.applyPreferred()",
    ] {
        assert!(pane.contains(needle), "MonitorPane.qml lost `{needle}`");
    }
    let mirror: String = include_str!("qml/tst_map_split_drag.qml")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for needle in [
        "Controls.SplitView.maximumHeight: Math.min( split.resizing ? root.chrome + root.maxBody : root.maximumHeight, Math.max(split.resizing ? root.header : root.minimumHeight, split.height - root.treeMin - root.handleHeight))",
        "Controls.SplitView.minimumHeight: split.resizing ? root.header : root.minimumHeight",
        "onPreferredChanged: if (!split.resizing)",
    ] {
        assert!(
            mirror.contains(needle),
            "tst_map_split_drag.qml drifted from MonitorPane.qml: `{needle}`"
        );
    }
}

#[test]
fn geographic_destination_labels_are_plain_text() {
    assert_data_labels_plain_text(
        "MapDestinationsPopup.qml",
        include_str!("../qml/MapDestinationsPopup.qml"),
        &["modelData.name"],
        1,
    );
    assert_data_labels_plain_text(
        "MapBand.qml",
        include_str!("../qml/MapBand.qml"),
        &["map.activeGeoScope.name"],
        1,
    );
    assert_data_labels_plain_text(
        "MapMarker.qml",
        include_str!("../qml/MapMarker.qml"),
        &["marker.cluster.members[0].name"],
        1,
    );
}
