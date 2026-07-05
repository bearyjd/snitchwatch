//! ISO 3166-1 alpha-2 country code -> flag emoji.
//!
//! Flag emoji are pairs of Unicode "regional indicator symbol" codepoints
//! (`U+1F1E6..=U+1F1FF`, one per ASCII letter `A..=Z`); rendering to an actual
//! flag glyph is a font/rendering concern outside this crate's control, but
//! the codepoint pairing itself is a pure, well-defined function of the two
//! letters.

/// Base codepoint for the regional indicator symbol matching ASCII `'A'`.
const REGIONAL_INDICATOR_BASE: u32 = 0x1F1E6;

/// Convert a 2-letter ISO 3166-1 alpha-2 country code into its flag emoji.
///
/// Accepts either case; non-alphabetic input or input that isn't exactly two
/// ASCII letters returns `None` rather than guessing — callers (the geo
/// aggregate store) fall back to a neutral glyph for the "Local network" /
/// "Unknown" buckets, which never have a real ISO code.
pub fn flag_emoji(country_code: &str) -> Option<String> {
    let mut chars = country_code.chars();
    let a = chars.next()?;
    let b = chars.next()?;
    if chars.next().is_some() {
        return None; // more than two characters
    }
    if !a.is_ascii_alphabetic() || !b.is_ascii_alphabetic() {
        return None;
    }
    let first = regional_indicator(a.to_ascii_uppercase());
    let second = regional_indicator(b.to_ascii_uppercase());
    Some([first, second].into_iter().collect())
}

fn regional_indicator(upper_ascii_letter: char) -> char {
    let offset = upper_ascii_letter as u32 - 'A' as u32;
    char::from_u32(REGIONAL_INDICATOR_BASE + offset)
        .expect("regional indicator symbols are a contiguous, valid Unicode range for A..=Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_country_code_produces_two_regional_indicators() {
        let flag = flag_emoji("US").unwrap();
        let chars: Vec<char> = flag.chars().collect();
        assert_eq!(chars.len(), 2);
        assert_eq!(chars[0], '\u{1F1FA}'); // regional indicator U
        assert_eq!(chars[1], '\u{1F1F8}'); // regional indicator S
    }

    #[test]
    fn lowercase_input_is_normalised() {
        assert_eq!(flag_emoji("us").unwrap(), flag_emoji("US").unwrap());
    }

    #[test]
    fn mixed_case_input_is_normalised() {
        assert_eq!(flag_emoji("De").unwrap(), flag_emoji("DE").unwrap());
    }

    #[test]
    fn wrong_length_returns_none() {
        assert_eq!(flag_emoji("USA"), None);
        assert_eq!(flag_emoji("U"), None);
        assert_eq!(flag_emoji(""), None);
    }

    #[test]
    fn non_alphabetic_returns_none() {
        assert_eq!(flag_emoji("U1"), None);
        assert_eq!(flag_emoji("--"), None);
    }

    #[test]
    fn every_letter_pair_round_trips_through_the_regional_indicator_range() {
        for a in 'A'..='Z' {
            for b in 'A'..='Z' {
                let code: String = [a, b].into_iter().collect();
                let flag = flag_emoji(&code).unwrap();
                assert_eq!(flag.chars().count(), 2);
            }
        }
    }
}
