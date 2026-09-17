//! Terminal-safe rendering of untrusted device text.

/// Longest device-controlled value shown in full; longer ones are capped with an ellipsis.
const MAX_DISPLAY_CHARS: usize = 256;

// Bidi embeddings, overrides and isolates reorder what a terminal shows, and the line and
// paragraph separators break a line; `char::is_control` covers only Cc. Zero-width joiners
// and non-joiners stay: emoji sequences and Indic or Arabic script need them.
const fn is_reordering(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// Renders untrusted text safe for a terminal.
///
/// Control characters, which include ESC, and bidi controls become the replacement
/// character, so no ANSI sequence survives and displayed order matches byte order; longer
/// values are capped. Matching and filesystem writes always use the original text.
#[must_use]
pub fn sanitize_for_display(value: &str) -> String {
    let mut out = String::with_capacity(value.len().min(MAX_DISPLAY_CHARS));
    for (index, c) in value.chars().enumerate() {
        if index == MAX_DISPLAY_CHARS {
            out.push('…');
            break;
        }
        let hidden = c.is_control() || is_reordering(c);
        out.push(if hidden { '\u{FFFD}' } else { c });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_passes_clean_text_through() {
        assert_eq!(sanitize_for_display("Google Pixel 9"), "Google Pixel 9");
        assert_eq!(sanitize_for_display(""), "");
        assert_eq!(sanitize_for_display("Fotos/año"), "Fotos/año");
    }

    #[test]
    fn sanitize_neutralizes_control_characters_and_ansi() {
        assert_eq!(
            sanitize_for_display("a\x1b[2Jb\nc\td"),
            "a\u{FFFD}[2Jb\u{FFFD}c\u{FFFD}d"
        );
        assert_eq!(sanitize_for_display("\x07"), "\u{FFFD}");
    }

    #[test]
    fn sanitize_neutralizes_bidi_controls_and_line_separators() {
        assert_eq!(
            sanitize_for_display("photo\u{202E}gpj.exe"),
            "photo\u{FFFD}gpj.exe"
        );
        assert_eq!(
            sanitize_for_display("a\u{2066}b\u{200F}c\u{2028}d\u{061C}e"),
            "a\u{FFFD}b\u{FFFD}c\u{FFFD}d\u{FFFD}e"
        );
    }

    #[test]
    fn sanitize_keeps_joiners_that_scripts_and_emoji_need() {
        let family = "\u{1F468}\u{200D}\u{1F469}";
        assert_eq!(sanitize_for_display(family), family);
        let farsi = "می\u{200C}خواهم";
        assert_eq!(sanitize_for_display(farsi), farsi);
    }

    #[test]
    fn sanitize_caps_long_values_with_an_ellipsis() {
        let shown = sanitize_for_display(&"x".repeat(300));
        assert_eq!(shown.chars().count(), 257);
        assert!(shown.ends_with('…'), "{shown}");
    }

    #[test]
    fn sanitize_keeps_values_at_the_cap_whole() {
        assert_eq!(sanitize_for_display(&"y".repeat(256)).chars().count(), 256);
    }
}
