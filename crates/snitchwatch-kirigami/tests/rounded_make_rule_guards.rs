//! Restyle of `MakeRuleSheet.qml`: exactly which controls are rounded, and
//! that no plain `Controls.*` control is left (there is nothing that must
//! stay plain: the sheet has no field, switch, check box or list row). The
//! SelectableText texts stay. A swap that is undone, or a new plain
//! control, changes a count and fails here.
use std::path::Path;

fn count(src: &str, opener: &str) -> usize {
    src.lines().filter(|l| l.contains(opener)).count()
}

#[test]
fn make_rule_sheet_uses_rounded_controls_only() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("qml/MakeRuleSheet.qml");
    let src = std::fs::read_to_string(&path).unwrap();
    // Open button, Allow, Deny.
    assert_eq!(count(&src, "RoundedButton {"), 3);
    // Scope and duration.
    assert_eq!(count(&src, "RoundedComboBox {"), 2);
    // Held-key semantics unchanged: the sheet never used GuardedButton.
    assert_eq!(count(&src, "GuardedButton {"), 0);
    assert_eq!(count(&src, "Controls."), 0, "a plain Controls.* control is back");
    assert_eq!(count(&src, "SelectableText {"), 7);
}
