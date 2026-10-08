//! Making text from outside safe to show: in a desktop notification body
//! (rendered as a markup subset), a log line or a plain-text label.
//! Moved out of `translator::verdict`, which re-exports the two public
//! functions under their old paths.

/// Sanitize an attacker-controlled string before it's shown in a desktop
/// notification body or sent to the WS client as protocol text — issue #14
/// security review round 2, MEDIUM-1. See `translator::verdict::ScopeDegradation`'s doc
/// comment for why this exists. Strips control characters (including the
/// ANSI `ESC` byte, newlines, carriage returns — bridge-cli logs apply
/// terminal escape sequences), HTML-entity-escapes the markup
/// metacharacters `<`/`>`/`&` (so a literal `<b>` in a hostname displays as
/// the text `<b>` rather than being interpreted as bold by a freedesktop
/// notification daemon), and caps the result to `max_len` **characters**
/// (not bytes — truncating mid-codepoint would corrupt multi-byte UTF-8).
pub fn sanitize_for_display(input: &str, max_len: usize) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    for c in input.chars() {
        if count >= max_len {
            out.push('…');
            break;
        }
        if c.is_control() || is_display_hazard(c) {
            continue;
        }
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            other => out.push(other),
        }
        count += 1;
    }
    out
}

/// Plain-text form of attacker-influenced text for a `Text.PlainText` label:
/// drops control characters and [`is_display_hazard`] ones, escapes nothing.
pub fn strip_display_hazards(input: &str) -> String {
    input
        .chars()
        .filter(|&c| !c.is_control() && !is_display_hazard(c))
        .collect()
}

/// Unicode characters `char::is_control()` (category Cc only) doesn't
/// catch, but that still let attacker-controlled text visually lie about
/// itself once rendered in a notification body or log — issue #14 security
/// review round 2, LOW. Bidi format controls (category Cf) can reorder or
/// hide surrounding text: e.g. a hostname containing U+202E
/// RIGHT-TO-LEFT OVERRIDE can make the *displayed* text read as a
/// different, more trustworthy-looking domain than the bytes actually are.
/// The zero-width/invisible-joiner controls (also Cf) can hide characters
/// entirely or defeat naive substring-based review. The line/paragraph
/// separators (category Zl/Zp) can inject a visual line break a
/// control-char-only strip wouldn't catch, splitting a notification body
/// across lines the caller didn't intend.
///
/// Since the PR #100 review it is every format character (general category
/// Cf: soft hyphen, Arabic letter mark, tag characters, and the rest) plus
/// those two separators. The Cf ranges are generated from Python's
/// `unicodedata` (Unicode 16.0.0). Since its re-review it is also the
/// characters outside Cf that render as nothing: the combining grapheme
/// joiner, the Hangul fillers and the variation selectors.
fn is_display_hazard(c: char) -> bool {
    matches!(c,
        '\u{2028}' // LINE SEPARATOR (Zl)
        | '\u{2029}' // PARAGRAPH SEPARATOR (Zp)
        // Invisible, but not Cf:
        | '\u{034F}' // COMBINING GRAPHEME JOINER (Mn)
        | '\u{115F}'..='\u{1160}' // HANGUL CHOSEONG/JUNGSEONG FILLER (Lo)
        | '\u{3164}' // HANGUL FILLER (Lo)
        | '\u{FFA0}' // HALFWIDTH HANGUL FILLER (Lo)
        | '\u{FE00}'..='\u{FE0F}' // VARIATION SELECTOR-1..16 (Mn)
        | '\u{E0100}'..='\u{E01EF}' // VARIATION SELECTOR-17..256 (Mn)
        // General category Cf, Unicode 16.0.0:
        | '\u{00AD}'
        | '\u{0600}'..='\u{0605}'
        | '\u{061C}'
        | '\u{06DD}'
        | '\u{070F}'
        | '\u{0890}'..='\u{0891}'
        | '\u{08E2}'
        | '\u{180E}'
        | '\u{200B}'..='\u{200F}' // zero-width space/ZWNJ/ZWJ, LRM, RLM
        | '\u{202A}'..='\u{202E}' // LRE, RLE, PDF, LRO, RLO
        | '\u{2060}'..='\u{2064}' // word joiner, invisible operators
        | '\u{2066}'..='\u{206F}' // LRI, RLI, FSI, PDI, deprecated format
        | '\u{FEFF}' // BOM / zero-width no-break space
        | '\u{FFF9}'..='\u{FFFB}'
        | '\u{110BD}'
        | '\u{110CD}'
        | '\u{13430}'..='\u{1343F}'
        | '\u{1BCA0}'..='\u{1BCA3}'
        | '\u{1D173}'..='\u{1D17A}'
        | '\u{E0001}'
        | '\u{E0020}'..='\u{E007F}' // tag characters
    )
}

/// Like [`sanitize_for_display`], but a long `input` keeps its END: at most
/// `max_len` characters, with a leading "…". For a host, where the end
/// (the registered domain) is what identifies it, and a right-truncated
/// `login.microsoft.com.<padding>.evil.tld` would hide the real domain (PR
/// #100 review). A program path wants [`sanitize_ends_for_display`].
/// Escapes after truncating, so an entity is never cut in half.
pub fn sanitize_tail_for_display(input: &str, max_len: usize) -> String {
    let kept = shown_chars(input);
    if kept.len() <= max_len {
        return escape_markup(&kept);
    }
    format!("…{}", escape_markup(&kept[kept.len() - max_len..]))
}

/// Like [`sanitize_tail_for_display`], but a long `input` keeps its START
/// and its END: at most `max_len` characters, half from each (the start
/// gets the smaller half), with "…" between. For a program path: the end
/// names the program and the start says where it lives, so a padded
/// `/tmp/x/<padding>/usr/lib64/firefox/firefox` can't pass for Firefox by
/// its end alone (PR #100 re-review). A host keeps its end only.
pub fn sanitize_ends_for_display(input: &str, max_len: usize) -> String {
    let kept = shown_chars(input);
    if kept.len() <= max_len {
        return escape_markup(&kept);
    }
    let start = max_len / 2;
    let end = kept.len() - (max_len - start);
    format!(
        "{}…{}",
        escape_markup(&kept[..start]),
        escape_markup(&kept[end..])
    )
}

/// `input` without control characters or [`is_display_hazard`] ones.
fn shown_chars(input: &str) -> Vec<char> {
    input
        .chars()
        .filter(|&c| !c.is_control() && !is_display_hazard(c))
        .collect()
}

/// `chars` with the markup metacharacters `<`, `>` and `&` escaped.
fn escape_markup(chars: &[char]) -> String {
    let mut out = String::new();
    for &c in chars {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_format_character_is_a_hazard() {
        for c in [
            '\u{00AD}',
            '\u{061C}',
            '\u{200B}',
            '\u{2060}',
            '\u{FEFF}',
            '\u{E0001}',
            '\u{E0020}',
            '\u{E0041}',
            '\u{E007F}',
            '\u{2069}',
            '\u{206F}',
            '\u{1D173}',
            '\u{2028}',
            '\u{2029}',
        ] {
            assert!(is_display_hazard(c), "U+{:04X}", c as u32);
            let shown = sanitize_for_display(&format!("a{c}b"), 64);
            assert_eq!(shown, "ab", "U+{:04X}", c as u32);
            assert_eq!(strip_display_hazards(&format!("a{c}b")), "ab");
        }
        for c in ['a', 'é', '中', '-', '.', ' ', '\u{2070}', '\u{FFFC}'] {
            assert!(!is_display_hazard(c), "U+{:04X}", c as u32);
        }
    }

    #[test]
    fn invisible_characters_outside_cf_are_hazards_too() {
        for c in [
            '\u{034F}',  // COMBINING GRAPHEME JOINER
            '\u{115F}',  // HANGUL CHOSEONG FILLER
            '\u{1160}',  // HANGUL JUNGSEONG FILLER
            '\u{3164}',  // HANGUL FILLER
            '\u{FFA0}',  // HALFWIDTH HANGUL FILLER
            '\u{FE00}',  // VARIATION SELECTOR-1
            '\u{FE0F}',  // VARIATION SELECTOR-16
            '\u{E0100}', // VARIATION SELECTOR-17
            '\u{E01EF}', // VARIATION SELECTOR-256
        ] {
            assert!(is_display_hazard(c), "U+{:04X}", c as u32);
            assert_eq!(
                sanitize_for_display(&format!("a{c}b"), 64),
                "ab",
                "U+{:04X}",
                c as u32
            );
            assert_eq!(strip_display_hazards(&format!("a{c}b")), "ab");
        }
        // Their neighbours are ordinary.
        for c in [
            '\u{034E}',
            '\u{0350}',
            '\u{1161}',
            '\u{3165}',
            '\u{FE10}',
            '\u{E01F0}',
        ] {
            assert!(!is_display_hazard(c), "U+{:04X}", c as u32);
        }
    }

    #[test]
    fn a_long_host_or_path_keeps_its_end() {
        let host = format!("login.microsoft.com.{}.evil.tld", "a".repeat(200));
        let shown = sanitize_tail_for_display(&host, 64);
        assert!(
            shown.starts_with('…') && shown.ends_with(".evil.tld"),
            "{shown}"
        );
        assert_eq!(shown.chars().count(), 65);
        assert_eq!(
            sanitize_tail_for_display("/tmp/x/firefox", 64),
            "/tmp/x/firefox"
        );
        // Hazards go before counting; escaping comes after cutting.
        assert_eq!(sanitize_tail_for_display("a\u{202e}b", 64), "ab");
        assert_eq!(sanitize_tail_for_display("x&y<z>", 3), "…&lt;z&gt;");
        assert_eq!(sanitize_tail_for_display("&&&&&", 2), "…&amp;&amp;");
    }

    #[test]
    fn a_long_path_keeps_its_start_and_its_end() {
        let path = format!("/tmp/x/{}/usr/lib64/firefox/firefox", "d/".repeat(200));
        let shown = sanitize_ends_for_display(&path, 64);
        assert!(shown.starts_with("/tmp/x/d/"), "{shown}");
        assert!(shown.ends_with("/usr/lib64/firefox/firefox"), "{shown}");
        assert_eq!(shown.matches('…').count(), 1, "{shown}");
        assert_eq!(shown.chars().count(), 65);
        assert_eq!(
            sanitize_ends_for_display("/tmp/x/firefox", 64),
            "/tmp/x/firefox"
        );
        // The start gets the smaller half of an odd budget.
        assert_eq!(sanitize_ends_for_display("abcdefgh", 5), "ab…fgh");
        // Hazards go before counting; escaping comes after cutting.
        assert_eq!(sanitize_ends_for_display("a\u{202e}b", 64), "ab");
        assert_eq!(
            sanitize_ends_for_display("<&abcd&>", 4),
            "&lt;&amp;…&amp;&gt;"
        );
    }
}
