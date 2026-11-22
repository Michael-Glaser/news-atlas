//! `json` — the little of JSON this crate touches: one field out of the request,
//! one escaped string into the answer.
//!
//! Decides how a field is found (`"key":` at the top level of a flat object),
//! how a string value is unescaped (`\n`, `\t`, `\uXXXX`), how a numeric field
//! is read, and how a value is escaped on the way out (quotes, backslashes, the
//! control characters).
//!
//! The escaping is spelled exactly as `atlas-lib`'s `json::escape_str` spells it
//! — `\u00XX` for a control character, not a substituted space — because the same
//! `{"error": …}` line has to come out identical whether this crate's binary
//! prints it or an in-process caller builds it (`crate::refusal`, and see
//! `atlas-ingest`'s `rules::tests::the_text_and_core_envelopes_are_the_binaries`).
//!
//! It does not parse JSON: no nesting, no arrays, no validation, no error. A
//! request that is not a flat object of strings and numbers reads as empty
//! fields, which is what the operations already tolerate. The real parser is
//! `atlas-core`'s `json`.

pub fn get(text: &str, key: &str) -> String {
    // Minimal reader: the app sends flat objects of strings/numbers, nothing else.
    let needle = format!("\"{}\"", key);
    let mut at = match text.find(&needle) {
        Some(i) => i + needle.len(),
        None => return String::new(),
    };
    while let Some(c) = text[at..].chars().next() {
        if c == ':' {
            at += 1;
            break;
        }
        at += c.len_utf8();
    }
    let tail = text[at..].trim_start();
    if let Some(stripped) = tail.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = stripped.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('u') => {
                        let hex: String = chars.by_ref().take(4).collect();
                        if let Ok(code) = u32::from_str_radix(&hex, 16) {
                            if let Some(ch) = char::from_u32(code) {
                                out.push(ch);
                            }
                        }
                    }
                    Some(other) => out.push(other),
                    None => break,
                },
                other => out.push(other),
            }
        }
        return out;
    }
    // number / true / false
    tail.chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-' || *c == '.')
        .collect()
}

pub fn num(text: &str, key: &str) -> Option<usize> {
    get(text, key).parse::<usize>().ok()
}

pub fn esc(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 8);
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            // The remaining C0 controls are spelled `\u00XX`, exactly like the
            // workspace's shared escaper (`atlas-lib`'s `json::escape_str`) and
            // like `serde_json`. They used to become a space, which is shorter and
            // lossy — and it made one refusal two different byte strings: this
            // binary printed `{"error":"… …"}` while an in-process caller building
            // the same envelope from the same message produced
            // `{"error":"…\u0001…"}`. One spelling, so a refusal is the same bytes
            // wherever it is assembled.
            c if (c as u32) < 0x20 => {
                let code = c as u32;
                out.push_str("\\u");
                for shift in [12u32, 8, 4, 0] {
                    let digit = (code >> shift) & 0xf;
                    let letter = if digit < 10 {
                        (b'0' + digit as u8) as char
                    } else {
                        (b'a' + (digit as u8 - 10)) as char
                    };
                    out.push(letter);
                }
            }
            c => out.push(c),
        }
    }
    out
}
