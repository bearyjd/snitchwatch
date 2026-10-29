//! PR 4a stays dormant: the identity functions are called nowhere outside
//! their own module and tests, so a bridge started without history enabled
//! never builds one (the no-`history/`-directory half is `tests.rs`,
//! `history_that_was_never_enabled_creates_nothing`).

use std::fs;
use std::path::{Path, PathBuf};

const IDENTITY: [&str; 6] = [
    "rule_identity",
    "identify(",
    "RuleIdentity",
    "IdentityError",
    "Qualifiers",
    "RuleClass::of",
];

const OWNERS: [&str; 4] = [
    "rule_identity.rs",
    "rule_identity_fuzz_tests.rs",
    "rule_identity_tests.rs",
    "identity_dormant_tests.rs",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn offenders_in(files: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (name, text) in files {
        for needle in IDENTITY {
            let hit = text.lines().any(|l| {
                l.contains(needle)
                    && !l.trim_start().starts_with("//")
                    && !l.contains("pub mod ")
                    && !l.contains("mod rule_identity_tests;")
                    && !l.contains("mod rule_identity_fuzz_tests;")
                    && !l.contains("mod identity_dormant_tests;")
            });
            if hit {
                out.push(format!("{name} uses {needle}"));
            }
        }
    }
    out
}

#[test]
fn identity_functions_have_no_production_call_sites() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut paths = Vec::new();
    for krate in fs::read_dir(&crates).expect("crates dir") {
        let src = krate.expect("entry").path().join("src");
        if src.is_dir() {
            rust_files(&src, &mut paths);
        }
    }
    assert!(paths.len() > 20, "scan found the workspace sources");
    let files: Vec<(String, String)> = paths
        .iter()
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let in_history = p.components().any(|c| c.as_os_str() == "history");
            !(in_history && OWNERS.contains(&name))
        })
        .map(|p| {
            (
                p.display().to_string(),
                fs::read_to_string(p).expect("read"),
            )
        })
        .collect();
    let found = offenders_in(&files);
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn the_scan_notices_a_call_site() {
    let planted = [("x.rs".to_owned(), "let _ = identify(&rule);".to_owned())];
    assert_eq!(offenders_in(&planted).len(), 1);
}
