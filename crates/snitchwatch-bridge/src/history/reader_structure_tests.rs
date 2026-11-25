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

fn production(source: &str) -> &str {
    source.split("#[cfg(test)]").next().unwrap()
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
