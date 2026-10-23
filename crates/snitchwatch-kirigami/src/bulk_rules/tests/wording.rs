//! The wording guard for this module: facts, never verdicts. Every string
//! literal the sequencer can emit (the module's own source, test code cut
//! off) is scanned with the same list as the learning review's guard.

const VERDICT_WORDS: [&str; 21] = [
    "safe",
    "malicious",
    "suspicious",
    "normal",
    "trusted",
    "trustworthy",
    "legitimate",
    "benign",
    "harmless",
    "dangerous",
    "malware",
    "virus",
    "verified",
    "genuine",
    "bad",
    "good",
    "clean",
    "infected",
    "attack",
    "unsafe",
    "suspect",
];

const SOURCES: [(&str, &str); 8] = [
    ("bulk_rules.rs", include_str!("../../bulk_rules.rs")),
    ("compare.rs", include_str!("../compare.rs")),
    ("plan.rs", include_str!("../plan.rs")),
    ("progress.rs", include_str!("../progress.rs")),
    ("run.rs", include_str!("../run.rs")),
    ("sink.rs", include_str!("../sink.rs")),
    ("snapshot.rs", include_str!("../snapshot.rs")),
    ("bridge_sink.rs", include_str!("../bridge_sink.rs")),
];

/// The string literals of Rust source (comments skipped, escapes kept).
fn literals(source: &str) -> Vec<String> {
    let chars: Vec<char> = source.chars().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '\'' => {
                if chars.get(i + 1) == Some(&'\\') && chars.get(i + 3) == Some(&'\'') {
                    i += 4;
                } else if chars.get(i + 2) == Some(&'\'') {
                    i += 3;
                } else {
                    i += 1;
                }
            }
            '"' => {
                let mut text = String::new();
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' {
                        text.push(chars[i]);
                        i += 1;
                    }
                    if i < chars.len() {
                        text.push(chars[i]);
                    }
                    i += 1;
                }
                found.push(text);
                i += 1;
            }
            _ => i += 1,
        }
    }
    found
}

fn without_tests(source: &str) -> &str {
    source.split("#[cfg(test)]").next().unwrap_or(source)
}

fn verdicts_in(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| VERDICT_WORDS.contains(&w.as_str()))
        .collect()
}

#[test]
fn the_scanner_finds_literals_and_verdicts() {
    assert_eq!(
        literals(
            r#"// "safe"
        let a = "added"; let q = '"';"#
        ),
        ["added"]
    );
    assert_eq!(verdicts_in("A Safe rule, not unsafe"), ["safe", "unsafe"]);
    assert!(verdicts_in("Already present").is_empty());
}

#[test]
fn no_string_the_sequencer_can_emit_passes_judgement() {
    let mut total = 0;
    for (file, source) in SOURCES {
        for text in literals(without_tests(source)) {
            total += 1;
            assert_eq!(verdicts_in(&text), Vec::<String>::new(), "{file}: {text:?}");
        }
    }
    assert!(
        total > 30,
        "the scan reached the module's strings ({total})"
    );
}
