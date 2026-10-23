//! The wording guard for the session's own strings: facts, never verdicts.

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

const SOURCES: [(&str, &str); 4] = [
    ("summary.rs", include_str!("../summary.rs")),
    ("view.rs", include_str!("../view.rs")),
    ("batch.rs", include_str!("../batch.rs")),
    ("session", include_str!("../../learning_review_session.rs")),
];

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .collect()
}

/// Whole words only: "unverified" is not "verified" here.
fn literal_texts(source: &str) -> Vec<String> {
    let code = source.split("#[cfg(test)]").next().unwrap_or(source);
    code.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .flat_map(|l| {
            l.split('"')
                .skip(1)
                .step_by(2)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn no_string_the_session_can_emit_is_a_verdict() {
    for (file, source) in SOURCES {
        for literal in literal_texts(source) {
            for word in words(&literal) {
                assert!(
                    !VERDICT_WORDS.contains(&word.as_str()),
                    "{file}: {literal:?} says {word:?}"
                );
            }
        }
    }
}

#[test]
fn the_guard_can_fail() {
    assert!(words("a good rule")
        .iter()
        .any(|w| VERDICT_WORDS.contains(&w.as_str())));
    assert!(!words("an unverified rule")
        .iter()
        .any(|w| VERDICT_WORDS.contains(&w.as_str())));
}
