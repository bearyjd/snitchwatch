//! PR 3a stays dormant: the lifecycle primitives are constructed nowhere in
//! production code, so a bridge started without history enabled never builds
//! them (the no-`history/`-directory half is `tests.rs`).

use std::fs;
use std::path::{Path, PathBuf};

/// Type names, matched as substrings.
const TYPES: [&str; 6] = [
    "PinRegistry",
    "PinId",
    "ReaderGate",
    "ReaderPermit",
    "AcceptanceSeq",
    "Generation",
];
/// Module names, matched as whole identifiers anywhere (paths, `use` lists,
/// aliases such as `use ...::lifecycle as hl`).
const MODULES: [&str; 2] = ["lifecycle", "pins"];

/// Source with comments (line and nested block), string literals (plain and
/// raw) and char literals blanked out.
fn strip(src: &str) -> String {
    let c: Vec<char> = src.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        let start = i;
        if c[i] == '/' && c.get(i + 1).copied() == Some('/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if c[i] == '/' && c.get(i + 1).copied() == Some('*') {
            let mut depth = 1;
            i += 2;
            while i < c.len() && depth > 0 {
                if c[i] == '/' && c.get(i + 1).copied() == Some('*') {
                    depth += 1;
                    i += 2;
                } else if c[i] == '*' && c.get(i + 1).copied() == Some('/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            out.push(' ');
        } else if c[i] == 'r' && matches!(c.get(i + 1).copied(), Some('"') | Some('#')) {
            let hashes = c[i + 1..].iter().take_while(|&&h| h == '#').count();
            if c.get(i + 1 + hashes) == Some(&'"') {
                let close: Vec<char> = std::iter::once('"')
                    .chain(std::iter::repeat_n('#', hashes))
                    .collect();
                i += 2 + hashes;
                while i < c.len() && c[i..].get(..close.len()) != Some(&close[..]) {
                    i += 1;
                }
                i += close.len();
                out.push(' ');
            } else {
                out.push('r');
                i += 1;
            }
        } else if c[i] == '"' {
            i += 1;
            while i < c.len() && c[i] != '"' {
                i += if c[i] == '\\' { 2 } else { 1 };
            }
            i += 1;
            out.push(' ');
        } else if c[i] == '\''
            && (c.get(i + 1).copied() == Some('\\') || c.get(i + 2).copied() == Some('\''))
        {
            // A char literal, not a lifetime.
            // An escape (`'\''`, `'\\'`) consumes the character after the
            // backslash, so an escaped apostrophe does not end the literal.
            i += if c[i + 1] == '\\' { 3 } else { 2 };
            while i < c.len() && c[i] != '\'' {
                i += 1;
            }
            i += 1;
            out.push(' ');
        } else {
            out.push(c[i]);
            i += 1;
        }
        // Blanking must not join lines: keep every newline it swallowed, or
        // a `pub mod` line could hide the next line from the scan.
        if i > start + 1 {
            out.extend(c[start..i.min(c.len())].iter().filter(|&&ch| ch == '\n'));
        }
    }
    out
}

fn is_ident(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// What a non-owner source file references: lifecycle/pins types or modules.
/// `pub mod` declaration lines (the history `mod.rs`) are exempt.
fn offending(src: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for line in strip(src).lines().filter(|l| !l.contains("pub mod ")) {
        hits.extend(
            TYPES
                .iter()
                .filter(|t| line.contains(**t))
                .map(|t| t.to_string()),
        );
        for word in line.split(|ch: char| !is_ident(ch)) {
            if MODULES.contains(&word) {
                hits.push(word.to_string());
            }
        }
    }
    hits
}

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
        for hit in offending(&text) {
            offenders.push(format!("{} uses {hit}", file.display()));
        }
    }
    assert!(offenders.is_empty(), "{offenders:#?}");
}

#[test]
fn scanner_flags_an_aliased_module_import() {
    let src = "use crate::history::lifecycle as hl;\nfn f() { let _g: hl::Generation = Default::default(); }";
    assert!(!offending(src).is_empty());
    assert!(!offending("use crate::history::{pins, open};").is_empty());
    assert!(!offending("fn f(r: &PinRegistry) {}").is_empty());
}

#[test]
fn scanner_ignores_comments_and_strings() {
    let src = "/* ReaderGate */ // lifecycle::Generation\n/* a /* nested */ pins */\n/// PinRegistry\nfn f() { let s = \"pins::PinId\"; let r = r#\"lifecycle\"#; let c = '\"'; }";
    assert_eq!(offending(src), Vec::<String>::new());
    // Code after a comment is still scanned.
    assert!(!offending("/* x */ use crate::history::pins::PinId;").is_empty());
    // A lifetime is not a char literal.
    assert!(!offending("fn f<'a>(x: &'a ReaderGate) {}").is_empty());
}

#[test]
fn scanner_sees_a_call_after_an_escaped_apostrophe_char_literal() {
    let src = "fn f() { let _q = ['\\'','\"']; let _g: ReaderGate; }";
    assert!(!offending(src).is_empty());
    assert!(!offending("fn f() { let _b = '\\\\'; let _g: PinRegistry; }").is_empty());
}

#[test]
fn a_block_comment_does_not_join_lines_and_hide_the_next_use() {
    let src = "pub mod a; /* one\ntwo */\nuse crate::history::pins::PinId;";
    assert!(!offending(src).is_empty());
    let multi = "pub mod a; /* one\ntwo\nthree */ use crate::history::pins::PinId;";
    assert!(!offending(multi).is_empty());
}
