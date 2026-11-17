// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Make text from the relay or a peer safe to show to the user.
//!
//! Device names, relay error messages and IDs are chosen by whoever runs the
//! relay or the other device. A terminal obeys escape sequences, carriage
//! returns and backspaces in printed text, and text views obey Unicode
//! bidirectional controls, so such text could overwrite or reorder a
//! verification code shown next to it. Blank-looking or invisible characters
//! (ideographic space, braille blank, Hangul fillers, zero-width and other
//! default-ignorable code points) could pad a name until it wraps into a row
//! that looks like the code. [`sanitize_untrusted`] keeps the text readable
//! but makes every such character visible and inert, and bounds how many
//! terminal columns it can fill.

/// Widest device name shown, in terminal columns. Names are for recognition
/// only, and a host name rarely needs more. With the CLI's
/// `other device: "…"` label the line stays within 56 columns, so it cannot
/// wrap on a terminal of that width or more; on any terminal at least 30
/// columns wide the name is too short to wrap into a row that starts with a
/// forged `verification code: ddd ddd` (26 columns).
pub const MAX_PEER_NAME_COLUMNS: usize = 40;
/// Widest relay-supplied error message shown, in terminal columns: enough for
/// the relay's own one-line errors, short of flooding the terminal.
pub const MAX_RELAY_MESSAGE_COLUMNS: usize = 256;
/// Longest identifier (room or device ID) accepted from the relay or a peer.
/// IDs are 26-character ULIDs; the bound leaves room for older or test IDs
/// while keeping a printed line short.
pub const MAX_IDENTIFIER_LEN: usize = 64;

/// Marks text cut at the width bound.
const TRUNCATION_MARK: char = '…';
/// Columns counted for an ASCII character.
const ASCII_COLUMNS: usize = 1;
/// Columns counted for any other character. East Asian wide characters and
/// emoji take two terminal columns and none takes more, so this bounds the
/// rendered width without a width table (narrow characters are overcounted).
const NON_ASCII_COLUMNS: usize = 2;

/// Terminal columns `c` may fill, rounded up (see [`NON_ASCII_COLUMNS`]).
fn columns(c: char) -> usize {
    if c.is_ascii() {
        ASCII_COLUMNS
    } else {
        NON_ASCII_COLUMNS
    }
}

/// Replace every character that can move the cursor, reorder text, or show
/// as blank or nothing with a visible `\u{…}` escape, collapse runs of spaces
/// to one, and cut the result to at most `max_columns` terminal columns
/// (including a trailing `…` when cut). Applying it again changes nothing.
pub fn sanitize_untrusted(raw: &str, max_columns: usize) -> String {
    let budget = max_columns.saturating_sub(columns(TRUNCATION_MARK));
    let mut out = String::new();
    let mut used = 0;
    // Length of `out` before the first piece that overflows `budget`: where
    // the text is cut, leaving room for the mark, if it ends up too wide.
    let mut cut_at = None;
    for c in raw.chars() {
        // Padding made of plain spaces collapses to one.
        if c == ' ' && out.ends_with(' ') {
            continue;
        }
        let piece = if must_escape(c) {
            c.escape_unicode().to_string()
        } else {
            c.to_string()
        };
        let width: usize = piece.chars().map(columns).sum();
        if used + width > budget && cut_at.is_none() {
            cut_at = Some(out.len());
        }
        if used + width > max_columns {
            // Cut between pieces, so an escape is never split.
            out.truncate(cut_at.unwrap_or(out.len()));
            if columns(TRUNCATION_MARK) <= max_columns {
                out.push(TRUNCATION_MARK);
            }
            return out;
        }
        out.push_str(&piece);
        used += width;
    }
    out
}

/// A room or device ID made of ASCII letters, digits, `-`, `_` and `.` only,
/// at most [`MAX_IDENTIFIER_LEN`] long: safe to print as is.
pub fn is_plain_identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_IDENTIFIER_LEN
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// An ID for printing: unchanged when [`is_plain_identifier`], otherwise
/// sanitized (for example one stored by an older version).
pub fn shown_identifier(id: &str) -> String {
    if is_plain_identifier(id) {
        id.to_owned()
    } else {
        sanitize_untrusted(id, MAX_IDENTIFIER_LEN)
    }
}

fn must_escape(c: char) -> bool {
    rewrites_display(c) || looks_blank_or_invisible(c)
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

/// Characters that render as blank space or as nothing, so they can pad a
/// name invisibly: Unicode `White_Space` other than the ASCII space (no-break
/// and ideographic spaces, the en/em space family, …), the remaining
/// `Default_Ignorable_Code_Point` characters, and blank-looking letters and
/// symbols.
fn looks_blank_or_invisible(c: char) -> bool {
    (c.is_whitespace() && c != ' ')
        || matches!(
            c,
            // Soft hyphen.
            '\u{00ad}'
            // Combining grapheme joiner.
            | '\u{034f}'
            // Hangul choseong and jungseong fillers.
            | '\u{115f}' | '\u{1160}'
            // Khmer inherent vowels.
            | '\u{17b4}' | '\u{17b5}'
            // Mongolian free variation selectors and vowel separator.
            | '\u{180b}'..='\u{180f}'
            // Braille pattern blank.
            | '\u{2800}'
            // Hangul filler.
            | '\u{3164}'
            // Variation selectors.
            | '\u{fe00}'..='\u{fe0f}'
            // Halfwidth Hangul filler.
            | '\u{ffa0}'
            // Reserved default-ignorable specials.
            | '\u{fff0}'..='\u{fff8}'
            // Shorthand format controls.
            | '\u{1bca0}'..='\u{1bca3}'
            // Musical symbol format controls.
            | '\u{1d173}'..='\u{1d17a}'
            // Tags, variation selectors supplement and reserved ignorables.
            | '\u{e0000}'..='\u{e0fff}'
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
            assert_eq!(sanitize_untrusted(raw, MAX_RELAY_MESSAGE_COLUMNS), shown);
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
            assert_eq!(sanitize_untrusted(name, MAX_PEER_NAME_COLUMNS), name);
        }
    }

    #[test]
    fn long_text_is_cut_to_the_bound_and_sanitizing_twice_changes_nothing() {
        let long = "x".repeat(MAX_PEER_NAME_COLUMNS * 2);
        let cut = sanitize_untrusted(&long, MAX_PEER_NAME_COLUMNS);
        assert_eq!(rendered_columns(&cut), MAX_PEER_NAME_COLUMNS);
        assert!(cut.ends_with(TRUNCATION_MARK));
        let exact = "y".repeat(MAX_PEER_NAME_COLUMNS);
        assert_eq!(sanitize_untrusted(&exact, MAX_PEER_NAME_COLUMNS), exact);
        // An escape is never split into something a terminal would act on.
        let escapes = "\u{1b}".repeat(MAX_PEER_NAME_COLUMNS);
        let cut = sanitize_untrusted(&escapes, MAX_PEER_NAME_COLUMNS);
        assert!(rendered_columns(&cut) <= MAX_PEER_NAME_COLUMNS);
        assert!(!cut.chars().any(must_escape));
        for raw in [long.as_str(), escapes.as_str(), "a\u{1b}b", "\u{202e}x"] {
            let once = sanitize_untrusted(raw, MAX_PEER_NAME_COLUMNS);
            assert_eq!(sanitize_untrusted(&once, MAX_PEER_NAME_COLUMNS), once);
        }
        assert_eq!(sanitize_untrusted("abc", 0), "");
        // Too narrow for the mark: cut without it.
        assert_eq!(sanitize_untrusted("abc", 1), "");
    }

    /// Columns a terminal needs for `text`, at most two per character.
    fn rendered_columns(text: &str) -> usize {
        text.chars().map(columns).sum()
    }

    #[test]
    fn blank_and_invisible_characters_are_shown_as_escapes() {
        let blank = [
            '\u{a0}',
            '\u{85}',
            '\u{1680}',
            '\u{2000}',
            '\u{2007}',
            '\u{200a}',
            '\u{2028}',
            '\u{2029}',
            '\u{202f}',
            '\u{205f}',
            '\u{3000}',
            '\u{2800}',
            '\u{3164}',
            '\u{ffa0}',
            '\u{115f}',
            '\u{1160}',
            '\u{ad}',
            '\u{34f}',
            '\u{17b4}',
            '\u{180e}',
            '\u{200b}',
            '\u{200d}',
            '\u{2060}',
            '\u{fe0f}',
            '\u{feff}',
            '\u{fff0}',
            '\u{1bca0}',
            '\u{1d173}',
            '\u{e0001}',
            '\u{e0041}',
            '\u{e01ef}',
        ];
        for c in blank {
            let shown = sanitize_untrusted(&format!("a{c}b"), MAX_RELAY_MESSAGE_COLUMNS);
            assert_eq!(shown, format!("a{}b", c.escape_unicode()), "{c:?}");
            assert!(shown.is_ascii(), "{c:?}");
        }
        // Visible letters that merely look unusual stay as they are.
        for c in ['\u{3042}', '\u{2801}', '\u{3165}', 'é', '—'] {
            assert_eq!(
                sanitize_untrusted(&c.to_string(), MAX_PEER_NAME_COLUMNS),
                c.to_string()
            );
        }
        // Runs of plain spaces collapse, so they cannot pad a name either.
        assert_eq!(
            sanitize_untrusted("a     b  c", MAX_PEER_NAME_COLUMNS),
            "a b c"
        );
    }

    #[test]
    fn width_is_bounded_in_terminal_columns_not_characters() {
        // Two columns each: half as many wide characters fit as ASCII ones.
        let wide = "\u{30ce}".repeat(MAX_PEER_NAME_COLUMNS);
        let cut = sanitize_untrusted(&wide, MAX_PEER_NAME_COLUMNS);
        assert!(rendered_columns(&cut) <= MAX_PEER_NAME_COLUMNS, "{cut:?}");
        assert!(cut.ends_with(TRUNCATION_MARK));
        assert_eq!(
            cut.chars().filter(|&c| c == '\u{30ce}').count(),
            (MAX_PEER_NAME_COLUMNS - NON_ASCII_COLUMNS) / NON_ASCII_COLUMNS
        );
        let exact = "\u{30ce}".repeat(MAX_PEER_NAME_COLUMNS / NON_ASCII_COLUMNS);
        assert_eq!(sanitize_untrusted(&exact, MAX_PEER_NAME_COLUMNS), exact);
        // The reviewer's padded fake code cannot fill a line any more.
        let forged = format!("{}verification code: 111 222", "\u{3000}".repeat(33));
        let shown = sanitize_untrusted(&forged, MAX_PEER_NAME_COLUMNS);
        assert!(!shown.contains('\u{3000}'), "{shown:?}");
        assert!(!shown.contains("verification code"), "{shown:?}");
        assert!(rendered_columns(&shown) <= MAX_PEER_NAME_COLUMNS);
        for raw in [wide.as_str(), forged.as_str(), "a   b\u{3000}\u{3000}c"] {
            let once = sanitize_untrusted(raw, MAX_PEER_NAME_COLUMNS);
            assert_eq!(sanitize_untrusted(&once, MAX_PEER_NAME_COLUMNS), once);
        }
    }

    #[test]
    fn identifiers_are_printed_only_when_plain() {
        for id in ["01JC8Z3V7Q4N2M5K6P8R9S0T1W", "sas-peer-remote", "a_b.c"] {
            assert!(is_plain_identifier(id), "{id}");
            assert_eq!(shown_identifier(id), id);
        }
        let too_long = "a".repeat(MAX_IDENTIFIER_LEN + 1);
        for id in [
            "",
            "room\u{1b}]52;c;aGk=\u{7}",
            "a b",
            "Ｒoom",
            too_long.as_str(),
        ] {
            assert!(!is_plain_identifier(id), "{id:?}");
        }
        let shown = shown_identifier("room\u{1b}]8;;https://x\u{1b}\\");
        assert!(!shown.chars().any(must_escape), "{shown:?}");
        assert!(shown.starts_with("room\\u{1b}"), "{shown:?}");
    }
}
