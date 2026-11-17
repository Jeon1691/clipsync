// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Make text from the relay or a peer safe to show to the user.
//!
//! Device names and relay error messages are chosen by whoever runs the
//! relay or the other device. A terminal obeys escape sequences, carriage
//! returns and backspaces in printed text, and text views obey Unicode
//! bidirectional controls, so such text could overwrite or reorder a
//! verification code shown next to it. [`sanitize_untrusted`] keeps the text
//! readable but makes every such character visible and inert.

/// Longest device name shown, in characters. Names are for recognition only;
/// a host name rarely needs more, and the bound keeps a hostile name from
/// pushing the verification code off a narrow screen.
pub const MAX_PEER_NAME_CHARS: usize = 64;
/// Longest relay-supplied error message shown, in characters: enough for the
/// relay's own one-line errors, short of flooding the terminal.
pub const MAX_RELAY_MESSAGE_CHARS: usize = 256;

/// Marks text cut at the length bound.
const TRUNCATION_MARK: char = '…';

/// Replace every character that can move the cursor or reorder text with a
/// visible `\u{…}` escape, and cut the result to `max_chars` characters
/// (including a trailing `…` when cut). Applying it again changes nothing.
pub fn sanitize_untrusted(raw: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut shown = 0;
    let budget = max_chars.saturating_sub(1);
    let mut pieces = raw.chars().peekable();
    while let Some(c) = pieces.next() {
        let piece = if rewrites_display(c) {
            c.escape_unicode().to_string()
        } else {
            c.to_string()
        };
        let width = piece.chars().count();
        let last = pieces.peek().is_none();
        // The final piece may use the slot otherwise kept for the mark.
        let limit = if last { max_chars } else { budget };
        if shown + width > limit {
            if max_chars > 0 {
                out.push(TRUNCATION_MARK);
            }
            return out;
        }
        out.push_str(&piece);
        shown += width;
    }
    out
}

/// Characters a terminal or text view acts on instead of showing: C0 and C1
/// controls (escape, carriage return, backspace, CSI, …) and Unicode format
/// characters that reorder, hide or break text.
fn rewrites_display(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            // Arabic letter mark.
            '\u{061c}'
            // Zero-width space and joiners, left-to-right and right-to-left marks.
            | '\u{200b}'..='\u{200f}'
            // Line and paragraph separators, bidi embeddings and overrides.
            | '\u{2028}'..='\u{202e}'
            // Word joiner, invisible operators, bidi isolates, deprecated format controls.
            | '\u{2060}'..='\u{206f}'
            // Byte order mark (zero-width no-break space).
            | '\u{feff}'
            // Interlinear annotation controls.
            | '\u{fff9}'..='\u{fffb}'
        )
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn hostile_text_is_shown_inert() {
        for (raw, shown) in [
            // Cursor movement, line clear and a fake code.
            (
                "Pixel\u{1b}[2K\u{1b}[1Averification code: 000 000",
                "Pixel\\u{1b}[2K\\u{1b}[1Averification code: 000 000",
            ),
            // Carriage return and backspace overwrite what came before.
            ("ok\r123 456\u{8}\u{8}", "ok\\u{d}123 456\\u{8}\\u{8}"),
            // 8-bit CSI, newline and tab.
            ("a\u{9b}2Jb\nc\td", "a\\u{9b}2Jb\\u{a}c\\u{9}d"),
            // Bidi override, isolate and marks reorder neighbouring digits.
            (
                "\u{202e}319 284\u{202c}\u{2066}x\u{2069}\u{200e}\u{200f}",
                "\\u{202e}319 284\\u{202c}\\u{2066}x\\u{2069}\\u{200e}\\u{200f}",
            ),
            (
                "\u{061c}\u{200b}\u{2028}\u{feff}\u{fffa}",
                "\\u{61c}\\u{200b}\\u{2028}\\u{feff}\\u{fffa}",
            ),
        ] {
            assert_eq!(sanitize_untrusted(raw, MAX_RELAY_MESSAGE_CHARS), shown);
        }
    }

    #[test]
    fn ordinary_names_are_unchanged() {
        for name in [
            "Laptop",
            "Alex's MacBook Pro",
            "Pixel 9 — 작업용",
            "ノートPC",
            "café",
        ] {
            assert_eq!(sanitize_untrusted(name, MAX_PEER_NAME_CHARS), name);
        }
    }

    #[test]
    fn long_text_is_cut_to_the_bound_and_sanitizing_twice_changes_nothing() {
        let long = "x".repeat(MAX_PEER_NAME_CHARS * 2);
        let cut = sanitize_untrusted(&long, MAX_PEER_NAME_CHARS);
        assert_eq!(cut.chars().count(), MAX_PEER_NAME_CHARS);
        assert!(cut.ends_with(TRUNCATION_MARK));
        let exact = "y".repeat(MAX_PEER_NAME_CHARS);
        assert_eq!(sanitize_untrusted(&exact, MAX_PEER_NAME_CHARS), exact);
        // An escape is never split into something a terminal would act on.
        let escapes = "\u{1b}".repeat(MAX_PEER_NAME_CHARS);
        let cut = sanitize_untrusted(&escapes, MAX_PEER_NAME_CHARS);
        assert!(cut.chars().count() <= MAX_PEER_NAME_CHARS);
        assert!(!cut.chars().any(rewrites_display));
        for raw in [long.as_str(), escapes.as_str(), "a\u{1b}b", "\u{202e}x"] {
            let once = sanitize_untrusted(raw, MAX_PEER_NAME_CHARS);
            assert_eq!(sanitize_untrusted(&once, MAX_PEER_NAME_CHARS), once);
        }
        assert_eq!(sanitize_untrusted("abc", 0), "");
    }
}
