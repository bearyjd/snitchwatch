//! The scanner test seams (`force_transport_for_tests`, `run_deep_scan_via`)
//! must stay out of production builds: they can override transport selection.
//! Integration tests are separate crates, so `cfg(test)` alone cannot reach
//! them; the `test-seams` feature does, and it is enabled only through the
//! self dev-dependency. These text guards fail if the gate is dropped or the
//! feature becomes a default.
const SCANNER: &str = include_str!("../src/scanner.rs");
const MANIFEST: &str = include_str!("../Cargo.toml");
const GATE: &str = "#[cfg(any(test, feature = \"test-seams\"))]";

fn lines_before<'a>(needle: &str) -> Vec<Vec<&'a str>> {
    let lines: Vec<&str> = SCANNER.lines().collect();
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains(needle) && !l.trim_start().starts_with("//"))
        .map(|(i, _)| lines[..i].iter().rev().take(4).copied().collect())
        .collect()
}

#[test]
fn every_test_seam_item_is_behind_the_feature_gate() {
    for needle in [
        "static FORCED_TRANSPORT",
        "pub fn force_transport_for_tests",
        "pub fn run_deep_scan_via",
    ] {
        let hits = lines_before(needle);
        assert_eq!(hits.len(), 1, "{needle} should be defined exactly once");
        assert!(
            hits[0].iter().any(|l| l.trim() == GATE),
            "{needle} is not gated by {GATE}"
        );
    }
    let detect = SCANNER
        .split("fn detect_transport")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("detect_transport");
    let override_use = detect.find("FORCED_TRANSPORT").expect("override lookup");
    assert!(
        detect[..override_use].contains(GATE)
            && detect[..override_use].rfind(GATE) > detect[..override_use].rfind(';'),
        "detect_transport consults the override outside the gate"
    );
}

#[test]
fn the_feature_is_not_a_default_and_only_the_dev_dependency_enables_it() {
    let features = MANIFEST
        .split("[features]")
        .nth(1)
        .and_then(|s| s.split("\n[").next())
        .expect("[features] section");
    assert!(
        features.lines().any(|l| l.trim_start().starts_with("test-seams")),
        "test-seams feature missing"
    );
    assert!(
        !features.lines().any(|l| l.trim_start().starts_with("default")),
        "a default feature set would ship the test seams"
    );
    let (_, dev) = MANIFEST.split_once("[dev-dependencies]").expect("dev-dependencies");
    assert!(dev.contains("features = [\"test-seams\"]"), "dev-dependency self-reference missing");
    let (normal, _) = MANIFEST.split_once("[dev-dependencies]").unwrap();
    assert!(!normal.contains("test-seams\"]"), "test-seams enabled outside dev-dependencies");
}
