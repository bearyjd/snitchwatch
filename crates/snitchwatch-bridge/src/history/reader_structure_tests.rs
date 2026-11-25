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

/// `source` with comments and the insides of string and char literals
/// blanked to spaces (newlines kept), byte for byte the same length, so an
/// offset into it is an offset into `source`. Handles line and nested block
/// comments, escapes, byte strings, raw strings with any number of `#`, and
/// told apart char literals (`'"'`) from lifetimes (`'a`).
fn mask(source: &str) -> String {
    let b = source.as_bytes();
    let mut out = b.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for byte in &mut out[from..to] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    let ident = |i: usize| i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_');
    let mut i = 0;
    while i < b.len() {
        let start = i;
        if b[i..].starts_with(b"//") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            blank(&mut out, start, i);
        } else if b[i..].starts_with(b"/*") {
            let mut depth = 0usize;
            while i < b.len() {
                if b[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            blank(&mut out, start, i);
        } else if let Some((body, hashes)) =
            raw_string_open(b, i).filter(|_| i == 0 || !ident(i - 1))
        {
            let close: Vec<u8> = std::iter::once(b'"')
                .chain(std::iter::repeat_n(b'#', hashes))
                .collect();
            i = body;
            while i < b.len() && !b[i..].starts_with(&close) {
                i += 1;
            }
            i = (i + close.len()).min(b.len());
            blank(&mut out, start, i);
        } else if b[i] == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i = (i + 1).min(b.len());
            blank(&mut out, start, i);
        } else if b[i] == b'\'' {
            i = char_literal_end(source, i).unwrap_or(i + 1);
            blank(&mut out, start, i);
        } else {
            i += 1;
        }
    }
    String::from_utf8(out).expect("blanking whole bytes between ASCII delimiters")
}

/// If a raw string opens at `i` (`r"`, `r#"`, `br##"` ...): the index just past
/// the opening quote and the number of `#`.
fn raw_string_open(b: &[u8], i: usize) -> Option<(usize, usize)> {
    let mut at = i;
    if matches!(b.get(at), Some(b'b' | b'c')) {
        at += 1;
    }
    if b.get(at) != Some(&b'r') {
        return None;
    }
    at += 1;
    let hashes = b[at..].iter().take_while(|&&c| c == b'#').count();
    (b.get(at + hashes) == Some(&b'"')).then_some((at + hashes + 1, hashes))
}

/// Index past a char literal opening at `i`, or None for a lifetime.
fn char_literal_end(source: &str, i: usize) -> Option<usize> {
    let rest = &source[i + 1..];
    let first = rest.chars().next()?;
    if first == '\\' {
        let after = &rest[1..];
        let escaped = after.chars().next()?;
        let tail = &after[escaped.len_utf8()..];
        return tail.find('\'').map(|n| i + 2 + escaped.len_utf8() + n + 1);
    }
    rest[first.len_utf8()..]
        .starts_with('\'')
        .then(|| i + 1 + first.len_utf8() + 1)
}

/// `(start, end, normalised text)` of every attribute in masked source: the
/// `#[...]` / `#![...]` span and its content with whitespace removed.
fn attributes(m: &str) -> Vec<(usize, usize, String)> {
    let b = m.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'#' {
            let open = i + 1 + usize::from(b.get(i + 1) == Some(&b'!'));
            if b.get(open) == Some(&b'[') {
                let mut depth = 0usize;
                let mut j = open;
                while j < b.len() {
                    match b[j] {
                        b'[' => depth += 1,
                        b']' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                let end = (j + 1).min(b.len());
                let text: String = m[open + 1..j.min(b.len())].split_whitespace().collect();
                found.push((i, end, text));
                i = end;
                continue;
            }
        }
        i += 1;
    }
    found
}

/// Does a `cfg(...)` / `cfg_attr(...)` attribute name the `test` token
/// anywhere, including `not(test)` (which also makes test and production
/// builds differ)?
fn names_test(attr: &str) -> bool {
    (attr.starts_with("cfg(") || attr.starts_with("cfg_attr("))
        && attr
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|t| t == "test")
}

/// The source minus its `#[cfg(test)] mod name { ... }` (or `mod name;`)
/// items. Any other attribute naming `test`, wherever it sits, stays in the
/// result for `is_test_gated`, so a test-only seam cannot hide behind an
/// earlier test module. Scanning is on the masked text, so nothing inside a
/// comment or literal is ever taken for an attribute or a brace.
fn production(source: &str) -> String {
    let m = mask(source);
    let mut out = String::new();
    let mut kept = 0;
    let mut from = 0;
    for (start, end, text) in attributes(&m) {
        if start < from || text != "cfg(test)" || m[start..].starts_with("#!") {
            continue;
        }
        if let Some(stop) = test_module_end(&m, end) {
            out.push_str(&source[kept..start]);
            kept = stop;
            from = stop;
        }
    }
    out.push_str(&source[kept..]);
    out
}

/// End of the `mod name { ... }` / `mod name;` item that follows `at`
/// (past whitespace and further attributes), or None if it is anything else.
fn test_module_end(m: &str, at: usize) -> Option<usize> {
    let mut pos = at;
    for (start, end, _) in attributes(&m[at..]) {
        if m[pos..at + start].trim().is_empty() {
            pos = at + end;
        }
    }
    let decl = m[pos..].trim_start().strip_prefix("mod ")?;
    let head = m.len() - decl.len();
    let open = head + decl.find(['{', ';'])?;
    if m.as_bytes()[open] == b';' {
        return Some(open + 1);
    }
    let mut depth = 0usize;
    for (i, ch) in m[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Does any attribute left in `code` name `test` in a `cfg` / `cfg_attr`?
fn is_test_gated(code: &str) -> bool {
    attributes(&mask(code))
        .iter()
        .any(|(_, _, t)| names_test(t))
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
            !is_test_gated(&code) && !code.contains("for_test"),
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

#[test]
fn every_cfg_or_cfg_attr_that_names_test_is_a_violation() {
    for attr in [
        "#[cfg(all(test, unix))]",
        "#[cfg(any(test, unix))]",
        "#[cfg_attr(test, derive(Debug))]",
        "#[cfg( test )]",
        "#[cfg(not(test))]",
        "#[cfg(\n    test\n)]",
    ] {
        let code = production(&format!(
            "fn a() {{}}\n{attr}\nfn seam() {{}}\nfn b() {{}}\n"
        ));
        assert!(is_test_gated(&code), "{attr} must be flagged");
        assert!(code.contains("fn b"), "{attr}: the code after is kept");
    }
}

#[test]
fn a_test_module_with_hostile_lexical_content_is_dropped_exactly() {
    let src = "fn a<'a>(x: &'a str) -> &'a str { x }\n\
        #[cfg(test)]\n\
        mod tests {\n\
        \x20   //! an odd \" quote and a } brace\n\
        \x20   /// doc \" quote\n\
        \x20   /* block \" } /* nested \" */ still comment \" */\n\
        \x20   const Q: char = '\"';\n\
        \x20   const E: char = '\\'';\n\
        \x20   const B: &[u8] = b\"}\\\"}\";\n\
        \x20   const R: &str = r#\"}\"#;\n\
        \x20   const R2: &str = r##\"}\" # }\"##;\n\
        \x20   fn lt<'a>(x: &'a str) -> &'a str { x }\n\
        \x20   mod inner { fn deep() { 'l: loop { break 'l} } }\n\
        }\n\
        fn writes() { conn.execute(\"INSERT\"); }\n";
    let code = production(src);
    assert!(code.contains("fn writes"), "production after it survives");
    assert!(code.contains("INSERT") && code.contains("fn a<"));
    assert!(!code.contains("fn deep") && !code.contains("const R2"));
    assert!(!is_test_gated(&code));
}

#[test]
fn attributes_named_in_comments_and_strings_are_not_attributes() {
    let src = "fn a() { let _ = \"#[cfg(test)] mod m {\"; }\n\
        // see #[cfg(test)] mod tests {\n\
        /* #[cfg(all(test))] */\n\
        fn b() {}\n";
    let code = production(src);
    assert!(code.contains("fn a") && code.contains("fn b"));
    assert!(!is_test_gated(&code));
}

#[test]
fn a_clean_file_passes_and_is_returned_whole() {
    let src =
        "use x::y;\n#[derive(Debug)]\nstruct S;\n#[test_case]\nfn f() -> &'static str { \"ok\" }\n";
    let code = production(src);
    assert_eq!(code, src);
    assert!(!is_test_gated(&code));
}

#[test]
fn a_spaced_or_multiline_cfg_test_module_is_still_dropped() {
    let code = production(
        "#[cfg( test )]\nmod t { fn x() {} }\n#[cfg(\n  test\n)]\nmod u { fn y() {} }\nfn z() {}\n",
    );
    assert!(!code.contains("fn x") && !code.contains("fn y") && code.contains("fn z"));
}
