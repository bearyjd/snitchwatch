//! History stays dormant: the lifecycle primitives (PR 3a), the durable
//! lifecycle (PR 3b) and everything that would switch history on (`enable(`,
//! `open_store`, the writer, `insert_event`) are named nowhere outside their
//! owner files, so a bridge started without history enabled never builds
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

/// Exactly a module declaration (`pub mod x;`, `mod x;`): the one thing a
/// parent module may say about a gated module. A line that merely contains
/// `pub mod ` (`pub mod a { use ...::pins::PinId; }`) is code and is scanned.
fn is_mod_decl(line: &str) -> bool {
    let t = line.trim();
    let rest = ["pub mod ", "pub(crate) mod ", "mod "]
        .iter()
        .find_map(|p| t.strip_prefix(p));
    rest.and_then(|r| r.strip_suffix(';'))
        .is_some_and(|name| !name.is_empty() && name.chars().all(is_ident))
}

/// Every whole identifier in the code of `src` (comments, strings and char
/// literals blanked), module declarations excepted.
fn code_words(src: &str) -> Vec<String> {
    strip(src)
        .lines()
        .filter(|l| !is_mod_decl(l))
        .flat_map(|l| l.split(|ch: char| !is_ident(ch)))
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// What a non-owner source file references: lifecycle/pins types or modules.
/// Module declaration lines (the history `mod.rs`) are exempt.
fn offending(src: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for line in strip(src).lines().filter(|l| !is_mod_decl(l)) {
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

/// Files allowed to mention the primitives: their own modules and tests, and
/// the durable half (PR 3b), which holds and releases pins.
const OWNERS: [&str; 11] = [
    "snapshots_tests.rs",
    "lifecycle.rs",
    "lifecycle_tests.rs",
    "pins.rs",
    "pins_tests.rs",
    "durable.rs",
    "durable_tests.rs",
    "ledger.rs",
    "ledger_tests.rs",
    "crash_tests.rs",
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

fn workspace_sources() -> Vec<PathBuf> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for krate in fs::read_dir(&crates).expect("crates dir") {
        let src = krate.expect("entry").path().join("src");
        if src.is_dir() {
            rust_files(&src, &mut files);
        }
    }
    assert!(files.len() > 20, "scan found the workspace sources");
    files
}

fn in_history(file: &Path) -> bool {
    file.components().any(|c| c.as_os_str() == "history")
}

fn file_name(file: &Path) -> &str {
    file.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

#[test]
fn primitives_have_no_production_call_sites() {
    let mut offenders = Vec::new();
    for file in workspace_sources() {
        if in_history(&file) && OWNERS.contains(&file_name(&file)) {
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

// ---- PR 3b: the durable lifecycle, and the switches that turn history on ----

/// What switches history on or writes to it. Named outside `history/` it
/// would be a call site: whole identifiers, anywhere in code.
const WIRING: [&str; 2] = ["open_store", "insert_event"];
/// Also a switch, but other writers exist (the Kirigami seen-programs
/// writer), so outside `history/` it counts only in a file naming `history`.
const WIRING_HISTORY_ONLY: [&str; 1] = ["Writer"];
/// The 3b names with no other use in the workspace: types and functions of
/// `ledger`, `session` and `durable`. Whole identifiers, anywhere in code.
/// `durable_names_cover_every_pub_item` keeps this and the next list complete.
const DURABLE_TYPES: [&str; 17] = [
    "LossCounter",
    "LedgerLimits",
    "LedgerOverflow",
    "GapCause",
    "SessionEnd",
    "PinnedBatch",
    "DurableError",
    "record_gap",
    "read_gaps",
    "enforce_limits",
    "read_watermark",
    "write_watermark",
    "read_sessions",
    "commit_pinned",
    "commit_gap",
    "flush_counter",
    "begin_session",
];
/// The other 3b names: the modules and common words (`session`, `Gap`,
/// `end_session`) that other code uses for other things, so outside
/// `history/` they count only in a file that names `history` too.
const DURABLE_MODULES: [&str; 10] = [
    "ledger",
    "session",
    "durable",
    "Gap",
    "Session",
    "start_session",
    "end_session",
    "heartbeat",
    "row_count",
    "ROW_BYTES",
];

/// The files that implement and test the durable half.
const DURABLE_OWNERS: [&str; 8] = [
    "snapshots_tests.rs",
    "ledger.rs",
    "ledger_tests.rs",
    "session.rs",
    "durable.rs",
    "durable_tests.rs",
    "crash_tests.rs",
    "dormant_tests.rs",
];

/// What a file outside `history/` may not name: the module itself (its only
/// permitted mention is the `pub mod history;` declaration), the switches,
/// and the 3b names. `enable` counts only as a call, in a file naming
/// `history` (`Choices::enable` and friends exist elsewhere).
fn outside_hits(src: &str) -> Vec<String> {
    let words = code_words(src);
    let names_history = words.iter().any(|w| w == "history");
    let mut hits: Vec<String> = Vec::new();
    for w in &words {
        let gated = WIRING.contains(&w.as_str())
            || DURABLE_TYPES.contains(&w.as_str())
            || (names_history
                && (w == "history"
                    || WIRING_HISTORY_ONLY.contains(&w.as_str())
                    || DURABLE_MODULES.contains(&w.as_str())));
        if gated && !hits.contains(w) {
            hits.push(w.clone());
        }
    }
    if names_history && strip(src).contains("enable(") {
        hits.push("enable(".to_owned());
    }
    hits
}

/// What a non-owner file inside `history/` may not name of the durable half.
fn inside_hits(src: &str) -> Vec<String> {
    let mut hits: Vec<String> = Vec::new();
    for w in code_words(src) {
        let gated = DURABLE_TYPES.contains(&w.as_str()) || DURABLE_MODULES.contains(&w.as_str());
        if gated && !hits.contains(&w) {
            hits.push(w);
        }
    }
    hits
}

#[test]
fn nothing_outside_history_names_the_switches_or_the_durable_half() {
    let mut offenders = Vec::new();
    for file in workspace_sources() {
        if in_history(&file) {
            continue;
        }
        for hit in outside_hits(&fs::read_to_string(&file).expect("read source")) {
            offenders.push(format!("{} uses {hit}", file.display()));
        }
    }
    assert!(offenders.is_empty(), "{offenders:#?}");
}

#[test]
fn the_durable_half_is_named_only_by_its_owners() {
    let mut offenders = Vec::new();
    for file in workspace_sources() {
        if !in_history(&file) || DURABLE_OWNERS.contains(&file_name(&file)) {
            continue;
        }
        for hit in inside_hits(&fs::read_to_string(&file).expect("read source")) {
            offenders.push(format!("{} uses {hit}", file.display()));
        }
    }
    assert!(offenders.is_empty(), "{offenders:#?}");
}

#[test]
fn the_outside_guard_catches_each_not_yet_wired_piece() {
    for planted in [
        "fn f() { let _ = crate::history::open_store(&l); }",
        "fn f() { insert_event(&c, &e); }",
        "use crate::history::writer::Writer;\nfn f(w: &Writer) {}",
        "use snitchwatch_bridge::history::{enable};\nfn f() { enable(&l, c); }",
        "fn f(c: LossCounter) {}",
        "fn f(l: LedgerLimits) {}",
        "fn f(c: GapCause) {}",
        "use crate::history::ledger;",
        "use crate::history::session::x;",
        "use crate::history::durable::y;",
        "use crate::history as h;",
    ] {
        assert!(!outside_hits(planted).is_empty(), "missed: {planted}");
    }
}

#[test]
fn the_outside_guard_leaves_unrelated_code_alone() {
    assert!(outside_hits("fn f() { Choices::default().enable(ID); }").is_empty());
    assert!(outside_hits("mod session; fn f(s: Session) { s.start(); }").is_empty());
    assert!(outside_hits("pub mod history;").is_empty());
    assert!(outside_hits("// history::open_store\nlet s = \"history\";").is_empty());
    assert!(outside_hits("fn f(w: BufWriter) {}").is_empty());
    assert!(outside_hits("fn f(w: &Writer) {}").is_empty());
}

#[test]
fn the_inside_guard_catches_a_stray_durable_reference() {
    assert!(!inside_hits("use super::ledger::LossCounter;").is_empty());
    assert!(!inside_hits("fn f() { session::start(); }").is_empty());
    assert!(inside_hits("pub mod ledger;\npub mod session;").is_empty());
}

#[test]
fn a_module_declaration_line_does_not_exempt_code_after_it() {
    assert!(!offending("pub mod a { use crate::history::pins::PinId; }").is_empty());
    assert!(!outside_hits("pub mod a { fn f() { open_store(); } }").is_empty());
    assert!(offending("pub mod pins;\nmod lifecycle;").is_empty());
}

#[test]
fn blanking_a_multi_line_comment_keeps_its_newlines() {
    assert_eq!(strip("a /* x\ny\nz */ b\nc").lines().count(), 4);
    assert_eq!(strip("a \"x\ny\" b\nc").lines().count(), 3);
}

#[test]
fn scanner_sees_a_use_after_an_escaped_quote_in_a_string_literal() {
    let src = "fn f() { let _s = \"a\\\"b\"; let _g: ReaderGate; }";
    assert!(!offending(src).is_empty());
    let apostrophe = "fn f() { let _s = \"it\\'s\"; let _g: PinRegistry; }";
    assert!(!offending(apostrophe).is_empty());
    // A quote inside the string is not the end of it: the text after the
    // escape is still string, not code.
    assert!(offending("fn f() { let _s = \"a\\\" ReaderGate\"; }").is_empty());
}

/// Every `pub` item the three owner files define, so the lists above cannot
/// silently fall behind the code they guard. Free items are at column 0;
/// `durable.rs` is one `impl Engine`, so all of its `pub` items count.
fn pub_items(file: &str, all_indents: bool) -> Vec<String> {
    let text = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/history")
            .join(file),
    )
    .expect("read owner file");
    let mut items = Vec::new();
    for line in strip(&text).lines() {
        let line = if all_indents { line.trim_start() } else { line };
        let Some(rest) = line.strip_prefix("pub ") else {
            continue;
        };
        let mut words = rest.split(|c: char| !is_ident(c)).filter(|w| !w.is_empty());
        let kind = words.next().unwrap_or("");
        if ["struct", "enum", "fn", "const", "type"].contains(&kind) {
            if let Some(name) = words.next() {
                items.push(name.to_owned());
            }
        }
    }
    items
}

#[test]
fn durable_names_cover_every_pub_item() {
    let mut missing = Vec::new();
    for (file, all) in [
        ("ledger.rs", false),
        ("session.rs", false),
        ("durable.rs", true),
    ] {
        for name in pub_items(file, all) {
            let listed = DURABLE_TYPES.contains(&name.as_str())
                || DURABLE_MODULES.contains(&name.as_str())
                // Methods of `LossCounter` and `Engine` used through values.
                || ["new", "record", "pending_count", "as_str", "parse", "ceiling", "from_budget", "is_uncertain"]
                    .contains(&name.as_str())
                || ["DEFAULT_MAX_ROWS", "MIN_CEILING"].contains(&name.as_str());
            if !listed {
                missing.push(format!("{file}: {name}"));
            }
        }
    }
    assert!(missing.is_empty(), "unguarded durable items: {missing:#?}");
}

#[test]
fn the_outside_guard_catches_the_real_durable_names() {
    for name in DURABLE_TYPES {
        assert!(
            !outside_hits(&format!("fn f() {{ let _ = {name}; }}")).is_empty(),
            "{name}"
        );
    }
    for name in DURABLE_MODULES {
        let planted = format!("use crate::history as h;\nfn f() {{ let _ = {name}; }}");
        assert!(outside_hits(&planted).contains(&name.to_string()), "{name}");
        assert!(
            !inside_hits(&format!("fn f() {{ let _ = {name}; }}")).is_empty(),
            "{name}"
        );
    }
}
