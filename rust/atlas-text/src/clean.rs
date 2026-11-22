//! `clean` — what may be stored at all: the character and layout rules.
//!
//! Decides which characters are dropped (C0/C1 controls, zero-width spaces and
//! joiners, the bidi overrides, the BOM, the soft hyphen, private use), how
//! whitespace is normalised (tabs and space runs to one space, at most one blank
//! line, ends trimmed), where an over-long text is cut (`MAX_FIELD`, on a
//! character boundary so no character is split), and how many characters were
//! removed.
//!
//! It does not decide what the text means: no word is dropped or lowercased, no
//! markup is interpreted, nothing is translated. `atlas-guard`'s `cleantext`
//! applies the same rules before a model sees a page; this module is the copy the
//! rest of the pipeline calls directly.

/// How many characters one field may hold; past it, `clean_text` cuts and says so
/// (`truncated`).
///
/// Public because it is the number the request envelope is built from
/// ([`crate::MAX_INPUT`] is `4 * MAX_FIELD`), so a caller that caps a field before
/// sending it can cap it at the engine's own limit instead of at a second literal
/// that drifts.
pub const MAX_FIELD: usize = 200_000;

fn is_droppable(c: char) -> bool {
    let n = c as u32;
    c.is_control()
        || n == 0x00AD            // soft hyphen
        || n == 0x200B            // zero width space
        || n == 0x200C
        || n == 0x200D
        || n == 0x200E
        || n == 0x200F
        || n == 0xFEFF            // BOM
        || (0x202A..=0x202E).contains(&n)  // bidi overrides
        || (0x2066..=0x2069).contains(&n)
        || (0xE000..=0xF8FF).contains(&n)  // private use
        // BUG: this range is empty (0xC0 > 0x9F), so the clause never fires — the
        // comment says C1 controls, the code says nothing. Nothing is lost today
        // because `c.is_control()` above already covers C1 (U+0080..=U+009F), but a
        // future reader who trims that first test would silently stop dropping
        // them. Left exactly as found: fixing it would change what this refactor
        // is allowed to change.
        || (0xC0..=0x9F).contains(&n)      // C1 controls
}

/// C0/C1 controls, zero-width and bidi characters out, layout normalised, capped.
pub fn clean_text(input: &str) -> (String, usize, bool) {
    let mut removed = 0usize;
    let mut kept: Vec<char> = Vec::with_capacity(input.len().min(MAX_FIELD));
    let mut truncated = false;
    for (i, ch) in input.chars().enumerate() {
        if i >= MAX_FIELD {
            truncated = true;
            break;
        }
        if ch == '\n' {
            kept.push('\n');
            continue;
        }
        if ch == '\t' {
            removed += 1;
            kept.push(' ');
            continue;
        }
        if is_droppable(ch) {
            removed += 1;          // dropped, not turned into a space: "a\u{0}b" is "ab"
            continue;
        }
        kept.push(ch);
    }
    let joined: String = kept.into_iter().collect();
    let mut out = String::with_capacity(joined.len());
    let mut spaces = 0usize;
    let mut breaks = 0usize;
    for ch in joined.chars() {
        if ch == '\n' {
            breaks += 1;
            spaces = 0;
            while out.ends_with(' ') {
                out.pop();
            }
            if breaks <= 2 {
                out.push('\n');
            }
            continue;
        }
        breaks = 0;
        if ch.is_whitespace() {
            spaces += 1;
            if spaces == 1 && !out.ends_with('\n') && !out.is_empty() {
                out.push(' ');
            }
            continue;
        }
        spaces = 0;
        out.push(ch);
    }
    let trimmed = out.trim().to_string();
    if trimmed.chars().count() > MAX_FIELD {
        return (trimmed.chars().take(MAX_FIELD).collect(), removed, true);
    }
    (trimmed, removed, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_and_bidi_are_removed() {
        let (text, removed, _) = clean_text("a\u{0}b\u{202e}c\u{200b}d\u{e9}\u{7f}");
        assert_eq!(text, "abcdé");
        assert_eq!(removed, 4);
    }

    #[test]
    fn layout_is_normalised() {
        let (text, _, _) = clean_text("  one   two \n\n\n\n three  ");
        assert_eq!(text, "one two\n\nthree");
    }
}
