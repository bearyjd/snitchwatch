//! Restyle batch D source guard: in each batch-D file, how many controls are
//! now rounded and exactly which plain `Controls.*` ones remain, and why. A
//! swap that is undone, or a new plain control, changes a count and fails
//! here. `MakeRuleSheet.qml` is deferred (selectable batch 7 owns it first)
//! and must stay untouched. Also: the new text components hold no literal
//! colour or radius and no input area of their own.
use std::path::Path;

fn qml_source(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("qml")
        .join(format!("{name}.qml"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn count(src: &str, opener: &str) -> usize {
    src.lines().filter(|l| l.contains(opener)).count()
}

/// Plain controls that must be gone from every batch-D file.
const SWAPPED: [&str; 8] = [
    "Controls.Button {",
    "Controls.TextField {",
    "Controls.TextArea {",
    "Controls.Switch {",
    "Controls.RadioButton {",
    "Controls.ToolButton {",
    "Controls.CheckBox {",
    "Controls.ComboBox {",
];

/// Plain controls that stay, with the reason:
/// - `SpinBox`: no wrapper is planned for it.
/// - `ItemDelegate`: list rows belong to restyle batch E.
const PLAIN: [&str; 2] = [
    "Controls.SpinBox {",
    "Controls.ItemDelegate {",
];

struct Expect {
    file: &'static str,
    rounded_button: usize,
    rounded_field: usize,
    rounded_area: usize,
    guarded_switch: usize,
    plain: [usize; 2], rounded_check: usize, rounded_combo: usize,
}

const FILES: [Expect; 8] = [
    Expect { file: "RulesPage", rounded_button: 7, rounded_field: 0, rounded_area: 0, guarded_switch: 0, plain: [0, 1], rounded_check: 0, rounded_combo: 0 },
    Expect { file: "RuleEditorSheet", rounded_button: 6, rounded_field: 4, rounded_area: 0, guarded_switch: 3, plain: [0, 0], rounded_check: 1, rounded_combo: 4 },
    Expect { file: "RulesImportSheet", rounded_button: 2, rounded_field: 0, rounded_area: 0, guarded_switch: 0, plain: [0, 0], rounded_check: 1, rounded_combo: 0 },
    Expect { file: "RuleSimulatorSheet", rounded_button: 2, rounded_field: 11, rounded_area: 2, guarded_switch: 0, plain: [1, 0], rounded_check: 1, rounded_combo: 2 },
    Expect { file: "RulesInspectorSheet", rounded_button: 5, rounded_field: 0, rounded_area: 0, guarded_switch: 1, plain: [0, 0], rounded_check: 0, rounded_combo: 0 },
    Expect { file: "BlocklistsPage", rounded_button: 0, rounded_field: 1, rounded_area: 0, guarded_switch: 0, plain: [0, 0], rounded_check: 0, rounded_combo: 0 },
    Expect { file: "ProfilesPage", rounded_button: 0, rounded_field: 3, rounded_area: 0, guarded_switch: 0, plain: [0, 0], rounded_check: 0, rounded_combo: 0 },
    Expect { file: "RecommendedRulesPage", rounded_button: 0, rounded_field: 0, rounded_area: 0, guarded_switch: 1, plain: [0, 0], rounded_check: 0, rounded_combo: 0 },
];

#[test]
fn batch_d_files_use_the_rounded_controls_and_keep_only_the_documented_plain_ones() {
    for e in &FILES {
        let src = qml_source(e.file);
        for opener in SWAPPED {
            assert_eq!(count(&src, opener), 0, "{}: plain `{opener}` is back", e.file);
        }
        assert_eq!(count(&src, "RoundedButton {"), e.rounded_button, "{}: RoundedButton", e.file);
        assert_eq!(count(&src, "RoundedField {"), e.rounded_field, "{}: RoundedField", e.file);
        assert_eq!(count(&src, "RoundedTextArea {"), e.rounded_area, "{}: RoundedTextArea", e.file);
        assert_eq!(count(&src, "GuardedSwitch {"), e.guarded_switch, "{}: GuardedSwitch", e.file);
        assert_eq!(count(&src, "RoundedCheckBox {"), e.rounded_check, "{}: RoundedCheckBox", e.file);
        assert_eq!(count(&src, "RoundedComboBox {"), e.rounded_combo, "{}: RoundedComboBox", e.file);
        for (opener, want) in PLAIN.iter().zip(e.plain) {
            assert_eq!(count(&src, opener), want, "{}: plain `{opener}`", e.file);
        }
    }
}

#[test]
fn make_rule_sheet_is_deferred_and_untouched() {
    let src = qml_source("MakeRuleSheet");
    assert_eq!(count(&src, "Controls.Button {"), 3);
    assert_eq!(count(&src, "Controls.ComboBox {"), 2);
    assert_eq!(count(&src, "RoundedButton {"), 0);
}

#[test]
fn the_text_components_take_radii_and_colours_from_theme_and_the_palette() {
    for name in ["RoundedFieldFrame", "RoundedField", "RoundedTextArea"] {
        let src = qml_source(name);
        for (n, line) in src.lines().enumerate() {
            let t = line.trim();
            if t.starts_with("//") {
                continue;
            }
            assert!(
                !(t.starts_with("radius:")
                    && t["radius:".len()..].trim().starts_with(|c: char| c.is_ascii_digit())),
                "{name}:{n}: literal radius: {t}"
            );
            assert!(
                !t.contains('#') || t.starts_with("//"),
                "{name}:{n}: literal colour: {t}"
            );
            assert!(
                !t.starts_with("MouseArea") && !t.starts_with("TapHandler"),
                "{name}:{n}: the frame must take no input of its own: {t}"
            );
        }
    }
}
