//! Fitting text to a column.
//!
//! By display width rather than by character throughout: a CJK branch
//! name or a log line in Japanese counts two columns a glyph, and
//! counting `chars()` overruns the column beside it.

use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

/// Cuts text down to the room there is for it, saying that it did.
pub fn fit(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }

    let mut out = String::new();
    let mut used = 0usize;

    for ch in text.chars() {
        let next = used + ch.width().unwrap_or(0);
        if next > width - 1 {
            break;
        }

        out.push(ch);
        used = next;
    }

    out.push('…');
    out
}

/// Pads out to a column.
pub fn pad(text: &str, width: usize) -> String {
    let mut out = text.to_string();
    out.push_str(&" ".repeat(width.saturating_sub(text.width())));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitting_counts_columns_rather_than_characters() {
        assert_eq!(fit("web", 10), "web");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert!(fit("日本語のブランチ", 6).width() <= 6);
    }

    #[test]
    fn padding_counts_columns_too() {
        assert_eq!(pad("ab", 5).width(), 5);
        assert_eq!(pad("日本", 6).width(), 6);
        assert_eq!(pad("far too long", 3), "far too long");
    }
}
