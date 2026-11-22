//! `text` — HTML entities to characters, shared by `cleantext` and `sanitize`.
//!
//! Decides what `&amp;`, `&#39;` and `&#x2014;` stand for: one left-to-right pass,
//! one bounded scan per `&`, an unknown name left exactly as written.
//!
//! **It agrees with a browser about what a reference is, and that is a security
//! property rather than a comfort.** The decoder that disagrees with the parser
//! is the decoder that hands the parser something it thought it had removed.
//! Measured before the scanner below was written, against a browser's own rules:
//!
//! * `&#60script` — no `;` — is `<script` to every browser, because a missing
//!   terminator on a numeric reference is a parse error a browser reports and
//!   then ignores. This decoder demanded the `;`, so
//!   `<p>&#60script&#62alert(1)&#60/script&#62</p>` came out of `sanitize_html`
//!   still looking encoded — and any renderer decoded it into live markup. That
//!   is markup the sanitiser believed it had dropped, which is the worst shape a
//!   sanitiser bug can have.
//! * `&ltscript` is *not* a reference to a browser: a name with no `;` that runs
//!   straight into another name character is the "ambiguous ampersand" rule, the
//!   one that stops `?a=1&copy=2` from being read as `?a=1©=2`. So it stays as
//!   written, here as there.
//!
//! Both rules are now the same here as in the HTML tokenizer: the answer to "what
//! does this text say" cannot be a matter of opinion, or `sanitize` ends up
//! scrubbing a string nobody will ever display.
//!
//! Decoding is exactly **one** pass, which is what a browser does: `&amp;lt;`
//! becomes `&lt;` and not `<`, because the DOM for the source `&amp;lt;` holds the
//! five characters `&lt;`. A second pass here would invent markup no browser ever
//! saw. Text that is already text (a stored field, model output, a query) must not
//! be decoded at all — that is [`crate::cleantext::plain`], and `cleantext::inline`
//! is the HTML-facing wrapper that decodes once.
//!
//! It does not decide whether the surrounding text is fit to keep (that is
//! `cleantext`) and it knows nothing about markup: `sanitize` finds and removes
//! the tags, this turns their entities back into characters.

/// The longest name a browser knows is 32 characters; the longest one named below
/// is 6. The bound is what keeps one stray `&` from scanning the rest of a page.
const MAX_NAME: usize = 32;

/// A code point needs 7 decimal digits (`1114111`) or 6 hex (`10FFFF`). Nine is
/// already past `u32`, so a longer run of digits is not a reference at all.
const MAX_DIGITS: usize = 9;

/// Decode the common HTML entities plus `&#NN;` / `&#xHH;`, once.
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut last = 0usize;
    let mut i = 0usize;
    while i < s.len() {
        // `&` is ASCII and every UTF-8 continuation byte is >= 0x80, so walking
        // bytes finds exactly the `&` characters and never lands mid-character.
        if s.as_bytes()[i] != b'&' {
            i += 1;
            continue;
        }
        match reference(s, i) {
            Some((end, value)) => {
                if let Some(prefix) = s.get(last..i) {
                    out.push_str(prefix);
                }
                out.push_str(&value);
                i = end;
                last = end;
            }
            None => i += 1,
        }
    }
    if let Some(tail) = s.get(last..) {
        out.push_str(tail);
    }
    out
}

/// The reference starting at `start` (which holds `&`): the byte just past it and
/// the text it stands for, or `None` when a browser leaves it as written.
///
/// The three shapes a browser knows are decided here, in its order: a number, a
/// name, or nothing.
fn reference(s: &str, start: usize) -> Option<(usize, String)> {
    let b = s.as_bytes();
    let mut i = start + 1;
    let first = *b.get(i)?;
    if first == b'#' {
        i += 1;
        let hex = matches!(b.get(i), Some(&b'x') | Some(&b'X'));
        if hex {
            i += 1;
        }
        let digits = i;
        while i < b.len()
            && i - digits < MAX_DIGITS
            && (if hex {
                b[i].is_ascii_hexdigit()
            } else {
                b[i].is_ascii_digit()
            })
        {
            i += 1;
        }
        if i == digits {
            return None;
        }
        let number = s.get(digits..i)?;
        // The terminator is optional for a number: `&#60script` is `<script`.
        let end = if b.get(i) == Some(&b';') { i + 1 } else { i };
        return Some((end, numeric_value(number, hex)));
    }
    if !first.is_ascii_alphanumeric() {
        return None;
    }
    let names = i;
    while i < b.len() && i - names < MAX_NAME && b[i].is_ascii_alphanumeric() {
        i += 1;
    }
    let name = s.get(names..i)?;
    match b.get(i) {
        Some(&b';') => named_value(name).map(|value| (i + 1, value)),
        // A name with no `;` followed by `=` is the query-string case the
        // ambiguous-ampersand rule exists for, so it is not a reference.
        Some(&b'=') => None,
        // No `;` and no name character after it: a browser decodes it (`&lt` at
        // the end of a line is `<`), so this does too.
        _ => named_value(name).map(|value| (i, value)),
    }
}

/// What a numeric reference means, in a browser's answer: anything that is not a
/// character (NUL, a lone surrogate, past U+10FFFF) is U+FFFD. U+FFFD is not `<`,
/// so no spelling of a reference can become markup by arithmetic.
fn numeric_value(number: &str, hex: bool) -> String {
    let parsed = if hex {
        u32::from_str_radix(number, 16).ok()
    } else {
        number.parse::<u32>().ok()
    };
    match parsed.and_then(char::from_u32) {
        Some(c) if c != '\0' => c.to_string(),
        _ => "\u{FFFD}".to_string(),
    }
}

/// What a named reference means, or `None` for a name no browser knows.
fn named_value(name: &str) -> Option<String> {
    // Entities that actually show up in news pages: punctuation, the
    // Latin-1 accents of European feeds, currency and legal marks.
    let decoded = match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => " ",
        "ensp" => "\u{2002}",
        "emsp" => "\u{2003}",
        "thinsp" => "\u{2009}",
        "shy" => "\u{ad}",
        "mdash" => "\u{2014}",
        "ndash" => "\u{2013}",
        "minus" => "\u{2212}",
        "rsquo" => "\u{2019}",
        "lsquo" => "\u{2018}",
        "sbquo" => "\u{201a}",
        "ldquo" => "\u{201c}",
        "rdquo" => "\u{201d}",
        "bdquo" => "\u{201e}",
        "laquo" => "\u{ab}",
        "raquo" => "\u{bb}",
        "hellip" => "\u{2026}",
        "middot" => "\u{b7}",
        "bull" => "\u{2022}",
        "deg" => "\u{b0}",
        "plusmn" => "\u{b1}",
        "times" => "\u{d7}",
        "divide" => "\u{f7}",
        "frac12" => "\u{bd}",
        "frac14" => "\u{bc}",
        "frac34" => "\u{be}",
        "micro" => "\u{b5}",
        "para" => "\u{b6}",
        "sect" => "\u{a7}",
        "copy" => "\u{a9}",
        "reg" => "\u{ae}",
        "trade" => "\u{2122}",
        "cent" => "\u{a2}",
        "pound" => "\u{a3}",
        "yen" => "\u{a5}",
        "euro" => "\u{20ac}",
        "aacute" => "\u{e1}",
        "agrave" => "\u{e0}",
        "acirc" => "\u{e2}",
        "auml" => "\u{e4}",
        "aring" => "\u{e5}",
        "atilde" => "\u{e3}",
        "aelig" => "\u{e6}",
        "ccedil" => "\u{e7}",
        "eacute" => "\u{e9}",
        "egrave" => "\u{e8}",
        "ecirc" => "\u{ea}",
        "euml" => "\u{eb}",
        "iacute" => "\u{ed}",
        "igrave" => "\u{ec}",
        "icirc" => "\u{ee}",
        "iuml" => "\u{ef}",
        "ntilde" => "\u{f1}",
        "oacute" => "\u{f3}",
        "ograve" => "\u{f2}",
        "ocirc" => "\u{f4}",
        "ouml" => "\u{f6}",
        "otilde" => "\u{f5}",
        "oslash" => "\u{f8}",
        "uacute" => "\u{fa}",
        "ugrave" => "\u{f9}",
        "ucirc" => "\u{fb}",
        "uuml" => "\u{fc}",
        "yacute" => "\u{fd}",
        "szlig" => "\u{df}",
        _ => return None,
    };
    Some(decoded.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_entities_a_news_page_actually_uses() {
        assert_eq!(
            decode_entities("a &amp; b &lt;c&gt; &#39;q&#x27; &mdash; &nbsp;x"),
            "a & b <c> 'q' \u{2014}  x",
            "`&nbsp;` is a space, so the two spaces around it are both kept"
        );
        assert_eq!(decode_entities("M&uuml;nchen &euro;5 &hellip;"), "München €5 …");
    }

    #[test]
    fn a_name_that_is_not_an_entity_is_left_exactly_as_written() {
        assert_eq!(decode_entities("&notanentity; & &; &#; &x;"), "&notanentity; & &; &#; &x;");
        // The ambiguous-ampersand rule: a name with no `;` that runs into
        // another name character is not a reference, which is what keeps
        // `&copy` in a query string from turning into a copyright sign.
        assert_eq!(decode_entities("&ltscript &ampx &copy2"), "&ltscript &ampx &copy2");
    }

    #[test]
    fn a_numeric_reference_is_decoded_without_its_terminator_as_well() {
        // Measured against a browser before this test was written: `&#60` with
        // no `;` is `<`. Requiring the `;` left markup in what the sanitiser
        // called plain text, and a renderer then decoded it.
        assert_eq!(decode_entities("&#60script&#62"), "<script>");
        assert_eq!(decode_entities("&#x3C/script&#X3E"), "</script>");
        // A named reference without its terminator is decoded the same way, as
        // long as no name character or `=` follows (`&lt/` is `<` then `/`).
        assert_eq!(decode_entities("&lt/script"), "</script");
        assert_eq!(decode_entities("x &amp"), "x &");
    }

    #[test]
    fn a_number_that_is_not_a_character_becomes_the_replacement_character() {
        // NUL, a lone surrogate and a code point past the last one: a browser
        // substitutes U+FFFD, and U+FFFD is not `<`, so arithmetic cannot
        // smuggle markup through a reference either.
        assert_eq!(decode_entities("a&#0;b"), "a\u{FFFD}b");
        assert_eq!(decode_entities("a&#xD800;b"), "a\u{FFFD}b");
        assert_eq!(decode_entities("a&#x110000;b"), "a\u{FFFD}b");
        // 21 bits is the whole of Unicode: the last code point still decodes.
        assert_eq!(decode_entities("&#x10FFFF;").chars().count(), 1);
    }

    #[test]
    fn decoding_is_one_pass_so_no_markup_appears_that_no_browser_saw() {
        // A browser renders `&amp;lt;` as the four characters `&lt;`: its parser
        // decodes once. A decoder that runs to a fixed point would turn the same
        // page into `<`, i.e. it would invent a tag. This asserts one pass, and
        // `sanitize`'s inert pass is what makes the one layer that survives
        // harmless.
        assert_eq!(decode_entities("&amp;lt;script&amp;gt;"), "&lt;script&gt;");
        assert_eq!(decode_entities("&amp;amp;lt;"), "&amp;lt;");
    }

    #[test]
    fn a_long_or_malformed_reference_does_not_scan_a_whole_page() {
        let long_name = format!("&{}{}", "a".repeat(100_000), ";");
        assert_eq!(decode_entities(&long_name), long_name);
        // A run of digits is capped, so one `&` costs a constant amount of work.
        // What is left over is ordinary text, and it contains no markup.
        let long_number = format!("&#{};", "9".repeat(100_000));
        let out = decode_entities(&long_number);
        assert!(
            out.starts_with('\u{FFFD}'),
            "past the last code point is U+FFFD, not a 100k-digit parse: {:?}",
            &out[..8.min(out.len())]
        );
        assert!(out.chars().count() < long_number.chars().count());
        let amps = "&".repeat(100_000);
        assert_eq!(decode_entities(&amps), amps);
    }
}
