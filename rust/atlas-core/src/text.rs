//! `text` — the string helpers the scanner and the entity counter share.
//!
//! Decides what an HTML entity means, how whitespace collapses, what counts as a
//! word, and which short line is prose rather than a nav label.
//!
//! It does not decide what is an article (that is `extract`) and it does not
//! decide which words matter (that is `entities`): both ask this module, so the
//! two never drift apart.

/// Decode the common HTML entities plus `&#NN;` / `&#xHH;`.
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    let mut last = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'&' {
            i += 1;
            continue;
        }
        let limit = (i + 32).min(bytes.len());
        let mut j = i + 1;
        let mut semi: Option<usize> = None;
        while j < limit {
            let c = bytes[j];
            if c == b';' {
                semi = Some(j);
                break;
            }
            if !(c.is_ascii_alphanumeric() || c == b'#') {
                break;
            }
            j += 1;
        }
        if let Some(end) = semi {
            if let Some(name) = s.get(i + 1..end) {
                if let Some(replacement) = entity_value(name) {
                    if let Some(prefix) = s.get(last..i) {
                        out.push_str(prefix);
                    }
                    out.push_str(&replacement);
                    i = end + 1;
                    last = i;
                    continue;
                }
            }
        }
        i += 1;
    }
    if let Some(tail) = s.get(last..) {
        out.push_str(tail);
    }
    out
}

fn entity_value(name: &str) -> Option<String> {
    if let Some(number) = name.strip_prefix('#') {
        let parsed = if let Some(hex) = number
            .strip_prefix('x')
            .or_else(|| number.strip_prefix('X'))
        {
            u32::from_str_radix(hex, 16).ok()
        } else {
            number.parse::<u32>().ok()
        };
        return match parsed {
            // NUL and surrogates are not characters a page can mean: use the
            // replacement char the way a browser does.
            Some(0) => Some("\u{FFFD}".to_string()),
            Some(code) => char::from_u32(code).map(|c| c.to_string()),
            None => None,
        };
    }
    // Named entities that actually show up in news pages: punctuation, the
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
        "dagger" => "\u{2020}",
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

/// Collapse every run of whitespace into one space and trim the ends.
pub fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch);
        }
    }
    out
}

/// Decode entities, then collapse whitespace.
pub fn normalize(s: &str) -> String {
    collapse_ws(&decode_entities(s))
}

pub fn word_count(s: &str) -> usize {
    s.split_whitespace()
        .filter(|w| w.chars().any(|c| c.is_alphanumeric()))
        .count()
}

/// A short line survives only when it reads like a sentence.
pub fn is_sentence_like(s: &str) -> bool {
    let trimmed = s.trim_end();
    if word_count(trimmed) < 4 {
        return false;
    }
    let ends = trimmed.ends_with('.')
        || trimmed.ends_with('!')
        || trimmed.ends_with('?')
        || trimmed.ends_with('\u{2026}')
        || trimmed.ends_with(':');
    let has = trimmed.contains('.') || trimmed.contains('!') || trimmed.contains('?');
    ends || has
}

/// Keep lines of 25+ characters; drop shorter lines unless sentence-like.
pub fn keep_line(s: &str) -> bool {
    if s.chars().count() >= 25 {
        return true;
    }
    is_sentence_like(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entities_decode_to_unicode() {
        let input = "a &amp; b &lt;c&gt; &#39;q&#x27; &mdash; &nbsp;x";
        let expected = "a & b <c> 'q' \u{2014}  x";
        assert_eq!(decode_entities(input), expected);
    }

    #[test]
    fn entities_decode_latin1_and_currency() {
        assert_eq!(
            decode_entities("caf&eacute; &euro;5 &copy; 20&deg;"),
            "caf\u{e9} \u{20ac}5 \u{a9} 20\u{b0}"
        );
    }

    #[test]
    fn whitespace_collapses_and_short_lines_are_filtered() {
        assert_eq!(collapse_ws("  a\n\tb   c "), "a b c");
        assert!(!keep_line("Hi there"));
        assert!(keep_line("A very concise sentence that ends well."));
        assert!(keep_line("Yes it is fine, really."));
    }
}
