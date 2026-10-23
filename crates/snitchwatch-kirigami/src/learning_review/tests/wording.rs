//! The wording guard: facts, never verdicts. Every string literal this
//! module can emit is scanned (the module's own source, test code cut off).

/// Words that pass judgement. Kept here, not in the module, so the guard
/// does not match itself.
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

const SOURCES: [(&str, &str); 9] = [
    (
        "learning_review.rs",
        include_str!("../../learning_review.rs"),
    ),
    ("build.rs", include_str!("../build.rs")),
    ("directory.rs", include_str!("../directory.rs")),
    ("dual_use.rs", include_str!("../dual_use.rs")),
    ("flags.rs", include_str!("../flags.rs")),
    ("paths.rs", include_str!("../paths.rs")),
    ("present.rs", include_str!("../present.rs")),
    ("rule.rs", include_str!("../rule.rs")),
    ("types.rs", include_str!("../types.rs")),
];

/// The string literals of Rust source: comments and character literals
/// skipped, escapes kept as written.
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
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            '\'' => {
                // A character literal ('"', '\'') or a lifetime.
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

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn verdicts_in(text: &str) -> Vec<String> {
    words(text)
        .into_iter()
        .filter(|w| VERDICT_WORDS.contains(&w.as_str()))
        .collect()
}

#[test]
fn the_scanner_finds_literals_and_skips_comments_and_chars() {
    let sample = r#"
        // "safe" in a comment
        let a = "runs from"; /* "normal" */
        let q = '"'; let b = "say \"hi\"";
        let c = format!("{} of {}", x, y);
    "#;
    assert_eq!(literals(sample), ["runs from", r#"say \"hi\""#, "{} of {}"]);
    assert_eq!(verdicts_in("This is Safe, not unsafe"), ["safe", "unsafe"]);
    assert!(verdicts_in("an uncommon port").is_empty());
}

#[test]
fn no_string_the_review_can_emit_passes_judgement() {
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

#[test]
fn the_fixed_sentences_are_covered_by_the_scan() {
    let all: Vec<String> = SOURCES
        .iter()
        .flat_map(|(_, s)| literals(without_tests(s)))
        .collect();
    for needle in [
        "Blocking this may stop networking or updates.",
        "runs from ",
        "uses an uncommon port",
    ] {
        assert!(all.iter().any(|l| l.contains(needle)), "{needle}");
    }
}
