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
const OWNERS: &[&str] = &[
    "reader.rs",
    "reader_registry.rs",
    "reader_registry_tests.rs",
    "reader_fixture.rs",
    "reader_tests.rs",
    "reader_paging_tests.rs",
    "reader_limits_tests.rs",
    "reader_coverage_tests.rs",
    "capture.rs",
    "capture_writer.rs",
    "capture_tests.rs",
    "guard.rs",
    "hook_tests.rs",
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
    "gaps.rs",
    "gaps_tests.rs",
    "gaps_repair_tests.rs",
    "pinned_queue.rs",
    "retention.rs",
    "retention_room.rs",
    "retention_tests.rs",
    "retention_expiry_tests.rs",
    "retention_pause_tests.rs",
    "retention_room_tests.rs",
    "retention_crash_tests.rs",
    "retention_writer_tests.rs",
    "retention_support.rs",
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
const DURABLE_TYPES: [&str; 18] = [
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
    "flush_counter_at",
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
const DURABLE_OWNERS: [&str; 43] = [
    "timeline.rs",
    "timeline_tests.rs",
    "gap_counts.rs",
    "gap_counts_tests.rs",
    "gap_counts_merge_tests.rs",
    "reader_ceiling_tests.rs",
    "reader_cut_tests.rs",
    "reader.rs",
    "reader_page.rs",
    "reader_sql.rs",
    "reader_fixture.rs",
    "reader_tests.rs",
    "reader_paging_tests.rs",
    "reader_progress_tests.rs",
    "reader_limits_tests.rs",
    "reader_coverage_tests.rs",
    "reader_stage_tests.rs",
    "reader_fit_tests.rs",
    "capture.rs",
    "capture_writer.rs",
    "capture_tests.rs",
    "hook_tests.rs",
    "snapshots_tests.rs",
    "ledger.rs",
    "ledger_tests.rs",
    "session.rs",
    "durable.rs",
    "durable_tests.rs",
    "crash_tests.rs",
    "gaps.rs",
    "gaps_tests.rs",
    "gaps_repair_tests.rs",
    "pinned_queue.rs",
    "retention.rs",
    "retention_room.rs",
    "retention_tests.rs",
    "retention_expiry_tests.rs",
    "retention_pause_tests.rs",
    "retention_room_tests.rs",
    "retention_crash_tests.rs",
    "retention_writer_tests.rs",
    "retention_support.rs",
    "dormant_tests.rs",
];

/// PR 5: capture and admission. Whole identifiers that name them, a
/// violation anywhere outside `history/`.
const CAPTURE_TYPES: [&str; 4] = [
    "CaptureStats",
    "CaptureConfig",
    "queues_allocated",
    "AdmissionConfig",
];
/// Their modules and common words (`capture`, `Refusal`, `start`): counted
/// only in a file that names `history`.
const CAPTURE_MODULES: &[&str] = &[
    "capture",
    "admission",
    "Admission",
    "Capture",
    "start",
    "Mapped",
    "Refusal",
    "GLOBAL_RATE",
    "GLOBAL_BURST",
    "PROCESS_RATE",
    "PROCESS_BURST",
    "SHARE_PERCENT",
    "MAX_BUCKETS",
    "IDLE_MS",
    "DecisionGuard",
    "DecisionOutcome",
    "begin_decision",
];
/// The files that implement and test capture and admission.
const CAPTURE_OWNERS: &[&str] = &[
    "ping.rs",
    "capture.rs",
    "capture_writer.rs",
    "capture_stats.rs",
    "capture_tests.rs",
    "admission.rs",
    "admission_tests.rs",
    "retention.rs",
    "retention_room.rs",
    "retention_tests.rs",
    "retention_expiry_tests.rs",
    "retention_pause_tests.rs",
    "retention_room_tests.rs",
    "retention_crash_tests.rs",
    "retention_writer_tests.rs",
    "retention_support.rs",
    "guard.rs",
    "hook_tests.rs",
    "gaps.rs",
    "gaps_tests.rs",
    "gaps_repair_tests.rs",
    "dormant_tests.rs",
    "mod.rs",
];

/// The production hook sites PR 5 and PR 7 wire (the watchdog's down and up
/// edges), and the only paths each may name:
/// `UiService` holds the capture and uses it for pings and prompt outcomes.
/// The bridge binary asks `start` for one.
const HOOK_FILES: [(&str, &[&str]); 3] = [
    (
        "snitchwatch-bridge/src/daemon_watchdog.rs",
        &["history::capture::Capture"],
    ),
    (
        "snitchwatch-bridge/src/grpc_server.rs",
        &[
            "history::capture::Capture",
            "history::guard::DecisionOutcome",
            "history::record::AnsweredBy",
        ],
    ),
    (
        "snitchwatch-bridge-cli/src/lib.rs",
        &[
            "history::capture::Capture",
            "history::capture::start",
            "history::Mode",
        ],
    ),
];

/// `text` with each `path` that is not the start of a longer identifier
/// (`history::capture::Capture` inside `history::capture::CaptureStats`)
/// replaced by a space.
fn replace_whole_path(text: &str, path: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(path) {
        let end = at + path.len();
        out.push_str(&rest[..at]);
        if rest[end..].chars().next().is_some_and(is_ident) {
            out.push_str(path);
        } else {
            out.push(' ');
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// [`outside_hits`] for `file`, with the paths its hook is allowed to name
/// taken out first.
fn outside_hits_for(file: &Path, src: &str) -> Vec<String> {
    let allowed = HOOK_FILES
        .iter()
        .find(|(name, _)| file.ends_with(name))
        .map_or(&[][..], |(_, paths)| *paths);
    let mut text = src.to_owned();
    for path in allowed {
        text = replace_whole_path(&text, path);
    }
    outside_hits(&text)
}

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
            || CAPTURE_TYPES.contains(&w.as_str())
            || (names_history
                && (w == "history"
                    || WIRING_HISTORY_ONLY.contains(&w.as_str())
                    || DURABLE_MODULES.contains(&w.as_str())
                    || CAPTURE_MODULES.contains(&w.as_str())));
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
        let text = fs::read_to_string(&file).expect("read source");
        for hit in outside_hits_for(&file, &text) {
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
                || ["new", "record", "pending_count", "as_str", "parse", "ceiling", "from_budget", "is_uncertain", "bounded", "record_since"]
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

// ---- PR 5: the hooks ----

/// What a non-owner file inside `history/` may not name of capture and
/// admission: their types and constants anywhere, and the modules as a path.
fn capture_inside_hits(src: &str) -> Vec<String> {
    let mut hits: Vec<String> = Vec::new();
    for w in code_words(src) {
        let gated = (CAPTURE_TYPES.contains(&w.as_str()) || CAPTURE_MODULES.contains(&w.as_str()))
            && !["start", "capture", "Capture"].contains(&w.as_str());
        if gated && !hits.contains(&w) {
            hits.push(w);
        }
    }
    // The common words only as a path into the module.
    let stripped = strip(src);
    for path in [
        "capture::",
        "admission::",
        "::capture",
        "::admission",
        "super::guard",
    ] {
        if stripped
            .lines()
            .filter(|l| !is_mod_decl(l))
            .any(|l| l.contains(path))
        {
            hits.push(path.to_owned());
        }
    }
    hits
}

#[test]
fn the_capture_half_is_named_only_by_its_owners() {
    let mut offenders = Vec::new();
    for file in workspace_sources() {
        if !in_history(&file) || CAPTURE_OWNERS.contains(&file_name(&file)) {
            continue;
        }
        let text = fs::read_to_string(&file).expect("read source");
        for hit in capture_inside_hits(&text) {
            offenders.push(format!("{} uses {hit}", file.display()));
        }
    }
    assert!(offenders.is_empty(), "{offenders:#?}");
}

#[test]
fn the_capture_guard_catches_a_planted_use_inside_history() {
    for planted in [
        "use super::capture::Capture;",
        "use super::capture;",
        "use super::capture as c;\nfn f() { c::start(); }",
        "use super::admission::Admission;",
        "fn f(c: CaptureStats) {}",
        "fn f() { let _ = GLOBAL_RATE; }",
        "fn f(r: Refusal) {}",
        "fn f() { crate::history::admission::x(); }",
    ] {
        assert!(
            !capture_inside_hits(planted).is_empty(),
            "missed: {planted}"
        );
    }
    assert!(capture_inside_hits("pub mod capture;\npub mod admission;").is_empty());
}

/// Every `pub` item of `capture.rs` and `admission.rs` is guarded, so a new
/// one cannot slip in unscanned.
#[test]
fn capture_names_cover_every_pub_item() {
    let mut missing = Vec::new();
    for file in ["capture.rs", "capture_stats.rs", "admission.rs", "guard.rs"] {
        for name in pub_items(file, false) {
            let listed =
                CAPTURE_TYPES.contains(&name.as_str()) || CAPTURE_MODULES.contains(&name.as_str());
            if !listed {
                missing.push(format!("{file}: {name}"));
            }
        }
    }
    assert!(missing.is_empty(), "unguarded capture items: {missing:#?}");
}

fn hook(name: &str) -> PathBuf {
    PathBuf::from("crates").join(name)
}

#[test]
fn the_hook_files_may_name_exactly_their_paths() {
    let grpc = hook("snitchwatch-bridge/src/grpc_server.rs");
    let ok = "pub struct S { h: Option<Arc<crate::history::capture::Capture>> }";
    assert!(outside_hits_for(&grpc, ok).is_empty());
    let prompt =
        "use crate::history::guard::DecisionOutcome; use crate::history::record::AnsweredBy;";
    assert!(outside_hits_for(&grpc, prompt).is_empty());
    let cli = hook("snitchwatch-bridge-cli/src/lib.rs");
    assert!(!outside_hits_for(&cli, prompt).is_empty());
    let ok = "fn f() { let _ = snitchwatch_bridge::history::capture::start(snitchwatch_bridge::history::Mode::User, None); }";
    assert!(
        outside_hits_for(&cli, ok).is_empty(),
        "{:?}",
        outside_hits_for(&cli, ok)
    );
}

/// The relaxation must not let a hook file reach the pieces PR 5 does not
/// wire (gaps, reader, control) or the switches (`enable`, the store,
/// the writer, the durable half), nor let any other file name capture.
#[test]
fn the_relaxed_guard_still_catches_what_is_not_wired() {
    let grpc = hook("snitchwatch-bridge/src/grpc_server.rs");
    let cli = hook("snitchwatch-bridge-cli/src/lib.rs");
    let elsewhere = hook("snitchwatch-bridge/src/ws_server.rs");
    for planted in [
        "use crate::history::guard::DecisionGuard;",
        "fn f() { crate::history::gaps::label(); }",
        "fn f() { crate::history::reader::page(); }",
        "fn f() { crate::history::control::clear(); }",
        "fn f() { crate::history::open_store(&l); }",
        "fn f() { crate::history::enable(&l, c); }",
        "use crate::history::writer::Writer;",
        "use crate::history::durable::PinnedBatch;",
        "use crate::history::lifecycle::ReaderGate;",
        "use crate::history::{capture, Mode};",
        "fn f() { crate::history::capture::queues_allocated(); }",
        "fn f(s: crate::history::capture::CaptureStats) {}",
        "use crate::history::Settings;",
    ] {
        // The allowed names themselves are fine; anything else is not.
        let with_allowed = format!("{planted}\nfn g(_: crate::history::capture::Capture) {{}}");
        assert!(
            !outside_hits_for(&grpc, &with_allowed).is_empty(),
            "grpc hook missed: {planted}"
        );
        assert!(
            !outside_hits_for(&cli, &with_allowed).is_empty(),
            "cli hook missed: {planted}"
        );
        assert!(
            !outside_hits_for(&elsewhere, &with_allowed).is_empty(),
            "another file missed: {planted}"
        );
    }
    // Any other file naming even the allowed paths is a violation.
    let allowed = "fn f(_: crate::history::capture::Capture) {}";
    assert!(!outside_hits_for(&elsewhere, allowed).is_empty());
    assert!(!outside_hits_for(
        &hook("snitchwatch-bridge/src/ws_server.rs"),
        "use crate::history::Mode;"
    )
    .is_empty());
}
