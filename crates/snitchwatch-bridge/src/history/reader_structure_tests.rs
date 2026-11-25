//! The reader has no write path: its sources name no writer type and no
//! statement that changes data, and nothing in them is compiled only for
//! tests. A structural proof, beside the behavioural ones.

const SOURCES: [(&str, &str); 5] = [
    ("reader.rs", include_str!("reader.rs")),
    ("reader_open.rs", include_str!("reader_open.rs")),
    ("reader_page.rs", include_str!("reader_page.rs")),
    ("reader_size.rs", include_str!("reader_size.rs")),
    ("reader_sql.rs", include_str!("reader_sql.rs")),
];

const TEST_ATTR: &str = "#[cfg(test)]";

/// The source minus its `#[cfg(test)] mod name { ... }` (or `mod name;`)
/// items, found by brace matching. Any other `#[cfg(test)]`, wherever it
/// sits, stays in the result, so a test-only seam cannot hide behind an
/// earlier test module. Braces inside plain string literals are skipped.
fn production(source: &str) -> String {
    let mut out = String::new();
    let mut rest = source;
    while let Some(at) = rest.find(TEST_ATTR) {
        let after = &rest[at + TEST_ATTR.len()..];
        match test_module_len(after) {
            Some(len) => {
                out.push_str(&rest[..at]);
                rest = &after[len..];
            }
            None => {
                out.push_str(&rest[..at + TEST_ATTR.len()]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Length of the `mod name { ... }` / `mod name;` item that starts `after`
/// (past whitespace), or None if the attribute is on anything else.
fn test_module_len(after: &str) -> Option<usize> {
    let body = after.trim_start();
    let lead = after.len() - body.len();
    let decl = body.strip_prefix("mod ")?;
    let head = decl.find(['{', ';'])?;
    if decl.as_bytes()[head] == b';' {
        return Some(lead + "mod ".len() + head + 1);
    }
    let (mut depth, mut in_str, mut escaped) = (0usize, false, false);
    for (i, ch) in decl[head..].char_indices() {
        match (in_str, ch) {
            (true, _) if escaped => escaped = false,
            (true, '\\') => escaped = true,
            (true, '"') => in_str = false,
            (true, _) => {}
            (false, '"') => in_str = true,
            (false, '{') => depth += 1,
            (false, '}') => {
                depth -= 1;
                if depth == 0 {
                    return Some(lead + "mod ".len() + head + i + 1);
                }
            }
            (false, _) => {}
        }
    }
    None
}

#[test]
fn the_reader_names_no_writer_type_and_no_statement_that_changes_data() {
    let forbidden = [
        "Engine",
        "Store",
        "transact(",
        "execute(",
        "execute_batch",
        "INSERT",
        "DELETE",
        "UPDATE",
        "CREATE",
        "DROP",
        "ALTER",
        "VACUUM",
        "ATTACH",
        "BEGIN",
        "COMMIT",
        "open_owner_only(",
        "set_permissions",
        "record_gap",
        "start_session",
        "end_session",
        "write_watermark",
        "pragma_update",
    ];
    for (name, source) in SOURCES {
        let code: String = production(source)
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for word in forbidden {
            assert!(!code.contains(word), "{name} mentions {word}");
        }
    }
}

#[test]
fn nothing_in_the_read_path_is_compiled_only_for_tests() {
    for (name, source) in SOURCES {
        let code = production(source);
        assert!(
            !code.contains("cfg(test)") && !code.contains("for_test"),
            "{name}"
        );
        assert!(!code.contains("thread::sleep"), "{name} sleeps");
    }
}

const SEAM_AFTER_A_TEST_MODULE: &str = "fn a() {}\n\
    #[cfg(test)]\n\
    mod tests {\n    fn t() { let _ = \"}\"; }\n}\n\
    #[cfg(test)]\n\
    fn seam() {}\n\
    fn writes() { conn.execute(\"INSERT\"); }\n";

#[test]
fn a_test_only_seam_after_a_test_module_is_still_production_code() {
    let code = production(SEAM_AFTER_A_TEST_MODULE);
    assert!(code.contains("cfg(test)"), "the seam must be seen");
    assert!(
        code.contains("fn writes"),
        "the code after it must be scanned"
    );
    assert!(
        !code.contains("fn t()"),
        "the test module itself is dropped"
    );
}

#[test]
fn a_test_only_seam_before_any_test_module_is_still_production_code() {
    let code = production("#[cfg(test)]\nfn seam() {}\nfn b() {}\n");
    assert!(code.contains("cfg(test)") && code.contains("fn b"));
}

#[test]
fn only_a_whole_test_module_is_dropped() {
    let code = production("fn a() {}\n#[cfg(test)]\nmod tests;\nfn b() {}\n");
    assert!(!code.contains("cfg(test)") && code.contains("fn a") && code.contains("fn b"));
    let code = production("#[cfg(test)]\nmod tests { mod inner { } fn x() {} }\nfn c() {}\n");
    assert!(!code.contains("fn x") && code.contains("fn c"));
}
