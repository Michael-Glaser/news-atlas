//! Which charset a response is in, and how its bytes become text.
//!
//! Two decisions, both of them guesses a browser makes and an article needs:
//! [`detect_charset`] (header, then BOM, then a `<meta charset>` scan of the first
//! [`META_SCAN_BYTES`] bytes, then UTF-8) and [`decode`] (the handful of encodings
//! a news page really uses, decoded lossily).
//!
//! This crate is the only decoder on the path. The workspace builds `ureq` with
//! `default-features = false`, so its `charset` feature is not compiled in and the
//! bytes [`decode`] is handed are the page's own; when that feature was on, the
//! transport converted a `text/*` body to UTF-8 first and every page that declared a
//! legacy charset in its header came out as mojibake (`caf’` → `cafâ€™`).
//! `crate::tests::the_transport_hands_over_the_bytes_the_page_sent` fails if a
//! decoder is ever put back in front of this one.
//!
//! It does not: fetch anything, validate a body, or ever fail. An encoding this
//! crate carries no table for is read as UTF-8, so a wrong guess costs one
//! replacement character per bad byte, never an error and never a panic.

/// Bytes of body scanned for `<meta charset=…>` — Python's `data[:4000]`.
pub const META_SCAN_BYTES: usize = 4096;

/// Charset for one response: the header first, then a BOM, then a `<meta charset>`
/// scan of the first [`META_SCAN_BYTES`] bytes, then UTF-8.
///
/// The meta scan is Python's `re.search(rb'charset=["\']?([\w-]+)', data[:4000])`,
/// which is why it also catches `<meta http-equiv=… content="text/html;
/// charset=…">` — that form contains `charset=` too.
pub fn detect_charset(content_type: &str, body: &[u8]) -> String {
    if let Some(name) = charset_in(content_type) {
        return name;
    }
    if body.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return "utf-8".to_string();
    }
    if body.starts_with(&[0xFF, 0xFE]) {
        return "utf-16le".to_string();
    }
    if body.starts_with(&[0xFE, 0xFF]) {
        return "utf-16be".to_string();
    }
    let head = &body[..body.len().min(META_SCAN_BYTES)];
    // `charset=` is ASCII, so a lossy view of the bytes is exact for the search
    // and can never panic on a body that is not valid in any encoding.
    let text = String::from_utf8_lossy(head);
    if let Some(name) = charset_in(&text) {
        return name;
    }
    "utf-8".to_string()
}

/// The `charset=` token of `text`, lower-cased, or `None`.
fn charset_in(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let marker = "charset=";
    let bytes = text.as_bytes();
    let mut from = 0usize;
    while let Some(offset) = lower[from..].find(marker) {
        let start = from + offset + marker.len();
        let mut i = start;
        // A quoted value (`charset="utf-8"`) is legal and common.
        while i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'' || bytes[i] == b' ') {
            i += 1;
        }
        let mut j = i;
        while j < bytes.len()
            && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'-' || bytes[j] == b'_')
        {
            j += 1;
        }
        if j > i {
            return Some(text[i..j].to_ascii_lowercase());
        }
        from = start;
    }
    None
}

/// The encodings this crate can read, named by the labels a page uses.
///
/// One table, one question — what does this label mean for [`decode`] — and a label
/// this crate does not know is [`Encoding::Utf8`], because that is how it is read,
/// lossily, and because guessing wrong twice is worse than one honest substitution per
/// bad byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    /// `utf-8` and its aliases, and `ascii`, which is a subset of it.
    Utf8,
    /// `windows-1252`, and the labels a page uses for "latin-1", which a browser
    /// decodes as windows-1252.
    Cp1252,
    /// `utf-16` in either byte order.
    Utf16Le,
    Utf16Be,
}

/// Which encoding a label names.
fn encoding_of(label: &str) -> Encoding {
    match label.trim().to_ascii_lowercase().as_str() {
        "iso-8859-1" | "iso8859-1" | "iso_8859-1" | "latin1" | "latin-1" | "windows-1252"
        | "cp1252" | "cp-1252" | "x-cp1252" => Encoding::Cp1252,
        "utf-16" | "utf16" | "utf-16le" | "utf16le" => Encoding::Utf16Le,
        "utf-16be" | "utf16be" => Encoding::Utf16Be,
        // `utf-8`, `utf8`, `unicode-1-1-utf-8`, `ascii`, `us-ascii`,
        // `ansi_x3.4-1968`, and everything this crate has no table for.
        _ => Encoding::Utf8,
    }
}

/// Bytes → text, lossily, for the handful of encodings a news page actually uses.
///
/// * `utf-8` (and its aliases, and `ascii`, which is a subset) — invalid sequences
///   become U+FFFD, never an error and never a panic.
/// * `windows-1252` / `iso-8859-1` / `latin1` — decoded with the 0x80–0x9F table,
///   because that is what a browser does with a page that says "latin1".
/// * `utf-16`, `utf-16le`, `utf-16be` — code units, odd trailing byte becomes U+FFFD.
/// * anything else — decoded as UTF-8: this crate carries no other table.
///
/// A leading byte-order mark is dropped. [`detect_charset`] has already used it to
/// pick the encoding, so keeping it would put an invisible U+FEFF at the front of
/// the article text — which the extractor would then carry into the title, the
/// preview and the model's input.
pub fn decode(body: &[u8], charset: &str) -> String {
    let text = match encoding_of(charset) {
        Encoding::Utf8 => String::from_utf8_lossy(body).into_owned(),
        Encoding::Cp1252 => decode_cp1252(body),
        Encoding::Utf16Le => decode_utf16(body, true),
        Encoding::Utf16Be => decode_utf16(body, false),
    };
    strip_bom(text)
}

/// The body without the byte-order mark that already did its job.
fn strip_bom(mut text: String) -> String {
    if text.starts_with('\u{FEFF}') {
        text.remove(0);
    }
    text
}

/// The five undefined slots of windows-1252 (0x81, 0x8D, 0x8F, 0x90, 0x9D) keep
/// their C1 code point, which is what a browser shows for them.
const CP1252_HIGH: [u16; 32] = [
    0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160,
    0x2039, 0x0152, 0x008D, 0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022,
    0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
];

fn decode_cp1252(body: &[u8]) -> String {
    let mut out = String::with_capacity(body.len());
    for &byte in body {
        let code = match byte {
            0x80..=0x9F => CP1252_HIGH[(byte - 0x80) as usize],
            _ => byte as u16,
        };
        out.push(char::from_u32(code as u32).unwrap_or('\u{FFFD}'));
    }
    out
}

fn decode_utf16(body: &[u8], little_endian: bool) -> String {
    let mut units: Vec<u16> = Vec::with_capacity(body.len() / 2 + 1);
    let mut i = 0usize;
    while i + 1 < body.len() {
        let pair = [body[i], body[i + 1]];
        units.push(if little_endian {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        });
        i += 2;
    }
    let mut text = String::from_utf16_lossy(&units);
    if i < body.len() {
        // Half a code unit left over: a truncated body, not a crash.
        text.push('\u{FFFD}');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

#[test]
fn the_header_wins_over_the_body() {
    assert_eq!(
        detect_charset("text/html; charset=ISO-8859-1", b"<meta charset=utf-8>"),
        "iso-8859-1"
    );
    assert_eq!(
        detect_charset("text/html; charset=\"UTF-8\"", b""),
        "utf-8"
    );
    assert_eq!(detect_charset("application/xhtml+xml;charset= utf-8", b""), "utf-8");
}


#[test]
fn without_a_header_the_meta_tag_decides() {
    let body = br#"<html><head><meta charset="windows-1252"><title>x</title></head></html>"#;
    assert_eq!(detect_charset("text/html", body), "windows-1252");
    let http_equiv = br#"<meta http-equiv="Content-Type" content="text/html; charset=iso-8859-1">"#;
    assert_eq!(detect_charset("", http_equiv), "iso-8859-1");
    // The scan looks at the first 4 kB only, as Python does.
    let mut late = vec![b' '; META_SCAN_BYTES + 8];
    late.extend_from_slice(b"<meta charset=koi8-r>");
    assert_eq!(detect_charset("", &late), "utf-8");
}


#[test]
fn a_bom_is_decisive_and_utf8_is_the_default() {
    assert_eq!(detect_charset("", b"\xEF\xBB\xBF<html>"), "utf-8");
    assert_eq!(
        detect_charset("", b"\xFF\xFE<\x00h\x00t\x00m\x00l\x00>\x00"),
        "utf-16le"
    );
    assert_eq!(detect_charset("", b"<html>no clue at all</html>"), "utf-8");
    assert_eq!(detect_charset("text/html", b""), "utf-8");
}


#[test]
fn decoding_never_panics_and_never_errors() {
    // Valid UTF-8 with a multi-byte character.
    assert_eq!(decode("héllo wörld".as_bytes(), "utf-8"), "héllo wörld");
    // Invalid UTF-8 becomes the replacement character.
    assert_eq!(decode(&[0x61, 0xFF, 0x62], "utf-8"), "a\u{FFFD}b");
    // windows-1252 needs the 0x80–0x9F table, not a latin-1 byte cast.
    assert_eq!(decode(&[0x80], "windows-1252"), "€");
    assert_eq!(decode(&[0x92], "iso-8859-1"), "\u{2019}");
    assert_eq!(decode(&[0xE9], "latin1"), "é");
    // UTF-16, both endiannesses, including the stray trailing byte.
    assert_eq!(decode(&[0x68, 0x00, 0x69, 0x00], "utf-16le"), "hi");
    assert_eq!(decode(&[0x00, 0x68, 0x00, 0x69], "utf-16be"), "hi");
    // One code unit plus a stray byte: the half unit becomes one replacement char.
    assert_eq!(decode(&[0x68, 0x00, 0x69], "utf-16le"), "h\u{FFFD}");
    // A lone surrogate is replaced, not fatal.
    assert_eq!(decode(&[0x00, 0xD8], "utf-16le"), "\u{FFFD}");
    // An encoding this crate carries no table for is read as UTF-8, lossily.
    assert_eq!(decode("ok".as_bytes(), "shift_jis"), "ok");
    assert_eq!(decode(&[], "utf-8"), "");
}


#[test]
fn the_byte_order_mark_that_chose_the_encoding_is_not_text() {
    // A UTF-8 BOM decided the encoding, so it must not become the first character
    // of the article: an invisible U+FEFF in the text ends up in the title, the
    // preview and the model's input.
    let utf8_bom = b"\xEF\xBB\xBF<html>story</html>";
    assert_eq!(detect_charset("", utf8_bom), "utf-8");
    assert_eq!(decode(utf8_bom, "utf-8"), "<html>story</html>");
    // The same through the header path, and through `text/html` with no BOM
    // mention at all: the mark is dropped either way.
    assert_eq!(decode(utf8_bom, "utf-8"), decode(&utf8_bom[3..], "utf-8"));
    // UTF-16 keeps its BOM as U+FEFF when decoded as code units; it is dropped here.
    let utf16 = b"\xFF\xFEh\x00i\x00";
    assert_eq!(detect_charset("", utf16), "utf-16le");
    assert_eq!(decode(utf16, "utf-16le"), "hi");
    assert_eq!(decode(b"\xFE\xFF\x00h\x00i", "utf-16be"), "hi");
    // A mark in the middle of a body is data, not a mark: untouched.
    assert_eq!(decode("a\u{FEFF}b".as_bytes(), "utf-8"), "a\u{FEFF}b");
}

}
