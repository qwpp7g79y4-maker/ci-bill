//! Escaping shared by the text and Markdown reports.
//!
//! CSV cells, scanned YAML, and fetched API fields all land in these
//! reports unvalidated, so a control character or a Unicode bidi override
//! in one of them must never reach the terminal or a Markdown table raw.

use std::fmt::Write;

/// Replaces every C0 control character, DEL, and Unicode bidi override with
/// a visible escape (`\n`, `\t`, `\xHH`, `\u{HHHH}`). Everything else,
/// including other printable Unicode, passes through unchanged. JSON output
/// does not use this: `serde_json` already escapes control characters.
pub fn escape_display(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c if is_bidi_override(c) => {
                let _ = write!(out, "\\u{{{:04x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Explicit directional-formatting characters, the ones that can make text
/// render in a different order than it reads (e.g. U+202E RIGHT-TO-LEFT
/// OVERRIDE).
fn is_bidi_override(c: char) -> bool {
    matches!(c as u32, 0x202a..=0x202e | 0x2066..=0x2069)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_and_bidi_overrides_become_visible() {
        assert_eq!(escape_display("plain"), "plain");
        assert_eq!(escape_display("a\nb"), "a\\nb");
        assert_eq!(escape_display("a\rb"), "a\\rb");
        assert_eq!(escape_display("a\tb"), "a\\tb");
        assert_eq!(escape_display("a\x1bb"), "a\\x1bb");
        assert_eq!(escape_display("a\x07b"), "a\\x07b");
        assert_eq!(escape_display("a\x7fb"), "a\\x7fb");
        assert_eq!(escape_display("a\0b"), "a\\x00b");
        assert_eq!(escape_display("a\u{202e}b"), "a\\u{202e}b");
        assert_eq!(escape_display("a\u{2066}b"), "a\\u{2066}b");
        assert_eq!(escape_display("caf\u{e9}"), "caf\u{e9}");
    }
}
