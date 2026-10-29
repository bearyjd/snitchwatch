//! PR 3a stays dormant: the lifecycle primitives are constructed nowhere in
//! production code, so a bridge started without history enabled never builds
//! them (the no-`history/`-directory half is `tests.rs`).

use std::fs;
use std::path::{Path, PathBuf};

const PRIMITIVES: [&str; 8] = [
    "lifecycle::",
    "pins::",
    "PinRegistry",
    "ReaderGate",
    "ReaderPermit",
    "AcceptanceSeq",
    "Generation::",
    "PinId",
];

/// Files allowed to mention the primitives: their own modules and tests.
const OWNERS: [&str; 5] = [
    "lifecycle.rs",
    "lifecycle_tests.rs",
    "pins.rs",
    "pins_tests.rs",
    "dormant_tests.rs",
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

#[test]
fn primitives_have_no_production_call_sites() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for krate in fs::read_dir(&crates).expect("crates dir") {
        let src = krate.expect("entry").path().join("src");
        if src.is_dir() {
            rust_files(&src, &mut files);
        }
    }
    assert!(files.len() > 20, "scan found the workspace sources");
    let mut offenders = Vec::new();
    for file in files {
        let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let in_history = file.components().any(|c| c.as_os_str() == "history");
        if in_history && OWNERS.contains(&name) {
            continue;
        }
        let text = fs::read_to_string(&file).expect("read source");
        // `mod.rs` may only declare the modules.
        for needle in PRIMITIVES {
            if text.lines().any(|l| {
                l.contains(needle) && !l.trim_start().starts_with("//") && !l.contains("pub mod ")
            }) {
                offenders.push(format!("{} uses {needle}", file.display()));
            }
        }
    }
    assert!(offenders.is_empty(), "{offenders:#?}");
}
