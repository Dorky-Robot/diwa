//! Display-time sanitization of untrusted text.
//!
//! Insight titles, bodies, and tags originate from commit messages and PR
//! comments, pass through Claude, and end up printed to the user's terminal.
//! If any of them contain ANSI escape sequences or other C0/C1 control
//! characters, the terminal renders them — historically that's been a
//! vector for setting window titles, remapping keys, or clearing the
//! screen to hide real output. We strip these before display.
//!
//! Tabs and newlines are preserved (they're legitimate in commit bodies);
//! everything else in the ASCII control range (0x00-0x1F, 0x7F) and the
//! C1 range (0x80-0x9F) is dropped.

/// Remove control characters that could alter terminal rendering.
pub fn strip_display_controls(s: &str) -> String {
    s.chars()
        .filter(|c| {
            let code = *c as u32;
            match *c {
                '\t' | '\n' | '\r' => true,
                _ => code >= 0x20 && !(0x7F..=0x9F).contains(&code),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_color() {
        let s = "hello \x1b[31mred\x1b[0m world";
        assert_eq!(strip_display_controls(s), "hello [31mred[0m world");
    }

    #[test]
    fn strips_c1_controls() {
        let s = "a\u{0085}b\u{009b}c";
        assert_eq!(strip_display_controls(s), "abc");
    }

    #[test]
    fn preserves_tabs_and_newlines() {
        let s = "a\tb\nc\r\nd";
        assert_eq!(strip_display_controls(s), "a\tb\nc\r\nd");
    }

    #[test]
    fn strips_bel_and_del() {
        let s = "a\x07b\x7fc";
        assert_eq!(strip_display_controls(s), "abc");
    }

    #[test]
    fn preserves_regular_text() {
        let s = "Why did we switch to pull-based rendering? Because…";
        assert_eq!(strip_display_controls(s), s);
    }
}
