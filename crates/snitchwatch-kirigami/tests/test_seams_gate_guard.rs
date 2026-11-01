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
        features
            .lines()
            .any(|l| l.trim_start().starts_with("test-seams")),
        "test-seams feature missing"
    );
    assert!(
        !features
            .lines()
            .any(|l| l.trim_start().starts_with("default")),
        "a default feature set would ship the test seams"
    );
    let (_, dev) = MANIFEST
        .split_once("[dev-dependencies]")
        .expect("dev-dependencies");
    assert!(
        dev.contains("features = [\"test-seams\"]"),
        "dev-dependency self-reference missing"
    );
    let (normal, _) = MANIFEST.split_once("[dev-dependencies]").unwrap();
    assert!(
        !normal.contains("test-seams\"]"),
        "test-seams enabled outside dev-dependencies"
    );
}

/// The auto-repeat key filter (C++ bridge) is compiled only for the feature,
/// and nothing in the shipped crate roots refers to it.
#[test]
fn the_key_repeat_bridge_is_compiled_only_under_the_feature() {
    let build = include_str!("../build.rs");
    let needle = "\"src/key_repeat_test.rs\"";
    let hits: Vec<_> = build.match_indices(needle).collect();
    assert_eq!(
        hits.len(),
        1,
        "key_repeat bridge should be registered exactly once"
    );
    let before = &build[..hits[0].0];
    let gate = before
        .rfind("CARGO_FEATURE_TEST_SEAMS")
        .expect("bridge is not behind the feature check");
    assert!(
        before[gate..].matches('{').count() > before[gate..].matches('}').count(),
        "bridge registration is outside the CARGO_FEATURE_TEST_SEAMS block"
    );
    for (name, src) in [
        ("lib.rs", include_str!("../src/lib.rs")),
        ("main.rs", include_str!("../src/main.rs")),
    ] {
        assert!(
            !src.contains("key_repeat"),
            "{name} references the test-only key filter"
        );
    }
}
