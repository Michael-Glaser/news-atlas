//! `norm_title` — the key that decides whether two headlines are one story.
//!
//! `db.py` does `str.lower()` → `unicodedata.normalize("NFKD")` → drop every
//! combining mark → replace non-alphanumerics with a space → collapse spaces.
//!
//! The standard library has no Unicode normalisation tables and this workspace
//! must build offline, so the decomposition step is replaced by two written-out
//! pieces of data: a folding table for Latin letters with diacritics (plus the
//! fullwidth forms and the NFKD ligatures), and the combining-diacritic blocks
//! to drop when a caller hands us text that is already decomposed.
//!
//! What that means in practice: Latin-script headlines fold exactly like
//! `db.py`, so "Café" and "cafe" are the same key. A script whose NFKD
//! decomposition is not listed here (Hangul syllables, for example) does not
//! decompose, so two spellings of the same headline in it stay distinct — a
//! missed duplicate, never a wrong merge.

/// Precomposed characters and what `NFKD` would reduce them to.
const FOLD: &[(char, &str)] = &[
    // A
    ('À', "A"), ('Á', "A"), ('Â', "A"), ('Ã', "A"), ('Ä', "A"), ('Å', "A"), ('Ā', "A"),
    ('Ă', "A"), ('Ą', "A"), ('Ǎ', "A"), ('Ǟ', "A"), ('Ǡ', "A"), ('Ǻ', "A"),
    ('Ȁ', "A"), ('Ȃ', "A"), ('Ȧ', "A"), ('Ạ', "A"), ('Ả', "A"), ('Ấ', "A"), ('Ầ', "A"),
    ('Ẩ', "A"), ('Ẫ', "A"), ('Ậ', "A"), ('Ắ', "A"), ('Ằ', "A"), ('Ẳ', "A"), ('Ẵ', "A"),
    ('Ặ', "A"),
    ('à', "a"), ('á', "a"), ('â', "a"), ('ã', "a"), ('ä', "a"), ('å', "a"), ('ā', "a"),
    ('ă', "a"), ('ą', "a"), ('ǎ', "a"), ('ǟ', "a"), ('ǡ', "a"), ('ǻ', "a"), ('ȁ', "a"),
    ('ȃ', "a"), ('ȧ', "a"), ('ḁ', "a"), ('ạ', "a"), ('ả', "a"), ('ấ', "a"), ('ầ', "a"),
    ('ẩ', "a"), ('ẫ', "a"), ('ậ', "a"), ('ắ', "a"), ('ằ', "a"), ('ẳ', "a"), ('ẵ', "a"),
    ('ặ', "a"),
    // C, D
    ('Ç', "C"), ('Ć', "C"), ('Ĉ', "C"), ('Ċ', "C"), ('Č', "C"), ('Ȼ', "C"),
    ('ç', "c"), ('ć', "c"), ('ĉ', "c"), ('ċ', "c"), ('č', "c"), ('ȼ', "c"),
    ('Ď', "D"), ('Đ', "D"), ('Ð', "D"), ('ď', "d"), ('đ', "d"), ('ð', "d"),
    // E
    ('È', "E"), ('É', "E"), ('Ê', "E"), ('Ë', "E"), ('Ē', "E"), ('Ĕ', "E"), ('Ė', "E"),
    ('Ę', "E"), ('Ě', "E"), ('Ȅ', "E"), ('Ȇ', "E"), ('Ȩ', "E"), ('Ẹ', "E"), ('Ẻ', "E"),
    ('Ẽ', "E"), ('Ế', "E"), ('Ề', "E"), ('Ể', "E"), ('Ễ', "E"), ('Ệ', "E"),
    ('è', "e"), ('é', "e"), ('ê', "e"), ('ë', "e"), ('ē', "e"), ('ĕ', "e"), ('ė', "e"),
    ('ę', "e"), ('ě', "e"), ('ȅ', "e"), ('ȇ', "e"), ('ȩ', "e"), ('ẹ', "e"), ('ẻ', "e"),
    ('ẽ', "e"), ('ế', "e"), ('ề', "e"), ('ể', "e"), ('ễ', "e"), ('ệ', "e"),
    // G, H
    ('Ĝ', "G"), ('Ğ', "G"), ('Ġ', "G"), ('Ģ', "G"), ('Ǥ', "G"),
    ('ĝ', "g"), ('ğ', "g"), ('ġ', "g"), ('ģ', "g"), ('ǥ', "g"),
    ('Ĥ', "H"), ('Ħ', "H"), ('ĥ', "h"), ('ħ', "h"),
    // I
    ('Ì', "I"), ('Í', "I"), ('Î', "I"), ('Ï', "I"), ('Ĩ', "I"), ('Ī', "I"), ('Ĭ', "I"),
    ('Į', "I"), ('İ', "I"), ('Ǐ', "I"), ('Ȉ', "I"), ('Ȋ', "I"), ('Ỉ', "I"), ('Ị', "I"),
    ('ì', "i"), ('í', "i"), ('î', "i"), ('ï', "i"), ('ĩ', "i"), ('ī', "i"), ('ĭ', "i"),
    ('į', "i"), ('ı', "i"), ('ǐ', "i"), ('ȉ', "i"), ('ȋ', "i"), ('ỉ', "i"), ('ị', "i"),
    // J, K
    ('Ĵ', "J"), ('ĵ', "j"),
    ('Ķ', "K"), ('Ǩ', "K"), ('ķ', "k"), ('ǩ', "k"),
    // L
    ('Ĺ', "L"), ('Ļ', "L"), ('Ľ', "L"), ('Ŀ', "L"), ('Ł', "L"),
    ('ĺ', "l"), ('ļ', "l"), ('ľ', "l"), ('ŀ', "l"), ('ł', "l"),
    // N
    ('Ñ', "N"), ('Ń', "N"), ('Ņ', "N"), ('Ň', "N"), ('Ŋ', "N"),
    ('ñ', "n"), ('ń', "n"), ('ņ', "n"), ('ň', "n"), ('ŋ', "n"),
    // O
    ('Ò', "O"), ('Ó', "O"), ('Ô', "O"), ('Õ', "O"), ('Ö', "O"), ('Ø', "O"), ('Ō', "O"),
    ('Ŏ', "O"), ('Ő', "O"), ('Ơ', "O"), ('Ǒ', "O"), ('Ǫ', "O"), ('Ǭ', "O"), ('Ȍ', "O"),
    ('Ȏ', "O"), ('Ȫ', "O"), ('Ȭ', "O"), ('Ȯ', "O"), ('Ȱ', "O"), ('Ọ', "O"), ('Ỏ', "O"),
    ('Ố', "O"), ('Ồ', "O"), ('Ổ', "O"), ('Ỗ', "O"), ('Ộ', "O"), ('Ớ', "O"), ('Ờ', "O"),
    ('Ở', "O"), ('Ỡ', "O"), ('Ợ', "O"),
    ('ò', "o"), ('ó', "o"), ('ô', "o"), ('õ', "o"), ('ö', "o"), ('ø', "o"), ('ō', "o"),
    ('ŏ', "o"), ('ő', "o"), ('ơ', "o"), ('ǒ', "o"), ('ǫ', "o"), ('ǭ', "o"), ('ȍ', "o"),
    ('ȏ', "o"), ('ȫ', "o"), ('ȭ', "o"), ('ȯ', "o"), ('ȱ', "o"), ('ọ', "o"), ('ỏ', "o"),
    ('ố', "o"), ('ồ', "o"), ('ổ', "o"), ('ỗ', "o"), ('ộ', "o"), ('ớ', "o"), ('ờ', "o"),
    ('ở', "o"), ('ỡ', "o"), ('ợ', "o"),
    // R, S
    ('Ŕ', "R"), ('Ŗ', "R"), ('Ř', "R"), ('Ȑ', "R"), ('Ȓ', "R"),
    ('ŕ', "r"), ('ŗ', "r"), ('ř', "r"), ('ȑ', "r"), ('ȓ', "r"),
    ('Ś', "S"), ('Ŝ', "S"), ('Ş', "S"), ('Š', "S"), ('Ș', "S"), ('ſ', "s"),
    ('ś', "s"), ('ŝ', "s"), ('ş', "s"), ('š', "s"), ('ș', "s"),
    // T
    ('Ţ', "T"), ('Ť', "T"), ('Ŧ', "T"), ('Ț', "T"),
    ('ţ', "t"), ('ť', "t"), ('ŧ', "t"), ('ț', "t"),
    // U
    ('Ù', "U"), ('Ú', "U"), ('Û', "U"), ('Ü', "U"), ('Ũ', "U"), ('Ū', "U"), ('Ŭ', "U"),
    ('Ů', "U"), ('Ű', "U"), ('Ų', "U"), ('Ư', "U"), ('Ǔ', "U"), ('Ǖ', "U"), ('Ǘ', "U"),
    ('Ǚ', "U"), ('Ǜ', "U"), ('Ȕ', "U"), ('Ȗ', "U"), ('Ụ', "U"), ('Ủ', "U"), ('Ứ', "U"),
    ('Ừ', "U"), ('Ử', "U"), ('Ữ', "U"), ('Ự', "U"),
    ('ù', "u"), ('ú', "u"), ('û', "u"), ('ü', "u"), ('ũ', "u"), ('ū', "u"), ('ŭ', "u"),
    ('ů', "u"), ('ű', "u"), ('ų', "u"), ('ư', "u"), ('ǔ', "u"), ('ǖ', "u"), ('ǘ', "u"),
    ('ǚ', "u"), ('ǜ', "u"), ('ȕ', "u"), ('ȗ', "u"), ('ụ', "u"), ('ủ', "u"), ('ứ', "u"),
    ('ừ', "u"), ('ử', "u"), ('ữ', "u"), ('ự', "u"),
    // W, Y, Z
    ('Ŵ', "W"), ('ŵ', "w"),
    ('Ý', "Y"), ('Ŷ', "Y"), ('Ÿ', "Y"), ('Ȳ', "Y"), ('Ỳ', "Y"), ('Ỵ', "Y"), ('Ỷ', "Y"),
    ('Ỹ', "Y"),
    ('ý', "y"), ('ÿ', "y"), ('ŷ', "y"), ('ȳ', "y"), ('ỳ', "y"), ('ỵ', "y"), ('ỷ', "y"),
    ('ỹ', "y"),
    ('Ź', "Z"), ('Ż', "Z"), ('Ž', "Z"), ('Ȥ', "Z"),
    ('ź', "z"), ('ż', "z"), ('ž', "z"), ('ȥ', "z"),
    // Digraphs and letters NFKD does not decompose but a headline mixes in.
    ('Æ', "AE"), ('æ', "ae"), ('Œ', "OE"), ('œ', "oe"), ('Þ', "TH"), ('þ', "th"),
    ('Ĳ', "IJ"), ('ĳ', "ij"), ('ß', "ss"),
    // NFKD ligatures.
    ('\u{FB00}', "ff"), ('\u{FB01}', "fi"), ('\u{FB02}', "fl"), ('\u{FB03}', "ffi"),
    ('\u{FB04}', "ffl"), ('\u{FB05}', "ft"), ('\u{FB06}', "st"),
];

/// The fold for one character, if the table has it.
fn fold(ch: char) -> Option<&'static str> {
    // Fullwidth ASCII is a mechanical offset under NFKD, so it needs no table row.
    let n = ch as u32;
    if (0xFF01..=0xFF5E).contains(&n) {
        return Some(fullwidth_ascii(n));
    }
    FOLD.iter().find(|(c, _)| *c == ch).map(|(_, to)| *to)
}

/// `U+FF01..U+FF5E` are `U+0021..U+007E` shifted by `0xFEE0`.
fn fullwidth_ascii(code: u32) -> &'static str {
    // Alphanumerics are the ones that matter (a fullwidth space is whitespace
    // either way, and fullwidth punctuation becomes a separator in both
    // implementations), so the table only holds ASCII letters and digits.
    let plain = (code - 0xFEE0) as u8;
    match plain {
        b'0' => "0", b'1' => "1", b'2' => "2", b'3' => "3", b'4' => "4",
        b'5' => "5", b'6' => "6", b'7' => "7", b'8' => "8", b'9' => "9",
        b'A' => "A", b'B' => "B", b'C' => "C", b'D' => "D", b'E' => "E",
        b'F' => "F", b'G' => "G", b'H' => "H", b'I' => "I", b'J' => "J",
        b'K' => "K", b'L' => "L", b'M' => "M", b'N' => "N", b'O' => "O",
        b'P' => "P", b'Q' => "Q", b'R' => "R", b'S' => "S", b'T' => "T",
        b'U' => "U", b'V' => "V", b'W' => "W", b'X' => "X", b'Y' => "Y",
        b'Z' => "Z",
        b'a' => "a", b'b' => "b", b'c' => "c", b'd' => "d", b'e' => "e",
        b'f' => "f", b'g' => "g", b'h' => "h", b'i' => "i", b'j' => "j",
        b'k' => "k", b'l' => "l", b'm' => "m", b'n' => "n", b'o' => "o",
        b'p' => "p", b'q' => "q", b'r' => "r", b's' => "s", b't' => "t",
        b'u' => "u", b'v' => "v", b'w' => "w", b'x' => "x", b'y' => "y",
        b'z' => "z",
        _ => " ",
    }
}

/// Combining marks: what is left of `NFKD` once the base letter has split off,
/// and exactly what `unicodedata.combining` drops.
fn is_combining(ch: char) -> bool {
    let n = ch as u32;
    (0x0300..=0x036F).contains(&n)      // combining diacritical marks
        || (0x1AB0..=0x1AFF).contains(&n) // ... extended
        || (0x1DC0..=0x1DFF).contains(&n) // ... supplement
        || (0x20D0..=0x20FF).contains(&n) // ... for symbols
        || (0xFE20..=0xFE2F).contains(&n) // combining half marks
}

/// Normalised headline: the key used to recognise the same story twice.
///
/// `db.py: norm_title` — case, accents, punctuation, quotes and spacing are
/// flattened, so `"Chip exports hit a record - Reuters"` and `"chip exports hit
/// a record"` match. Returns `""` when there is nothing worth comparing.
pub fn norm_title(title: &str) -> String {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(trimmed.len());
    let mut pending_space = false;
    for ch in trimmed.chars() {
        // Case first, then fold: `to_lowercase` can expand (İ is i + U+0307) and
        // the expansion has to be folded and stripped like any other input.
        for lowered in ch.to_lowercase() {
            if is_combining(lowered) {
                continue;
            }
            let piece: &str = match fold(lowered) {
                Some(text) => text,
                None if lowered.is_alphanumeric() => {
                    if pending_space && !out.is_empty() {
                        out.push(' ');
                    }
                    pending_space = false;
                    out.push(lowered);
                    continue;
                }
                None => "",
            };
            let alphabetic = piece.chars().any(|c| c.is_alphanumeric());
            if !alphabetic {
                // A separator: one space, and never at the start.
                pending_space = !out.is_empty();
                continue;
            }
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push_str(piece);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

#[test]
fn norm_title_flattens_what_db_py_flattens() {
    // `db.py`'s docstring claims "... - Reuters" and the bare headline match;
    // its code keeps "reuters", because a hyphen becomes a space and letters
    // survive. The port follows the code, not the docstring (the collector strips
    // the publisher from the title before this ever runs).
    assert_eq!(
        norm_title("Chip exports hit a record - Reuters"),
        "chip exports hit a record reuters"
    );
    assert_eq!(
        norm_title("Chip  exports hit a record -- Reuters!"),
        norm_title("chip exports hit a record reuters")
    );
    assert_eq!(
        norm_title("Café  CULTURE!"),
        "cafe culture",
        "accents and case fold away"
    );
    assert_eq!(norm_title("  "), "");
    assert_eq!(norm_title("“Quoted” headline"), "quoted headline");
    // Already-decomposed input folds too.
    assert_eq!(norm_title("cafe\u{0301}"), "cafe");
    assert_eq!(norm_title("Straße"), "strasse");
    assert_eq!(norm_title("\u{FB01}ne"), "fine");
    assert_eq!(norm_title("ＦＵＬＬ"), "full");
    assert_eq!(norm_title("Île-de-France"), "ile de france");
}

}
