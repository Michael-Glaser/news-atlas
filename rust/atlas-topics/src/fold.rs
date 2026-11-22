//! Case and accent folding: `_fold(text)` from `topics.py`.
//!
//! Decides the one canonical form a keyword and a text are compared in:
//! lowercase (including Python's word-final sigma), the `_FOLD` substitutions
//! (`ß` -> `ss`, `’` -> `'`, `–` -> `-`, …), then NFKD with the combining marks
//! dropped — and the order of those three steps, which is load-bearing.
//!
//! Does not decide: what matched. Folding runs on the keyword and on the text,
//! so matching stays consistent even where the fold is lossy (Vietnamese stacked
//! tones, Hangul syllables and halfwidth katakana keep their shape).
//!
//! The `NFKD` table is data, generated from Python's `unicodedata`; the logic
//! around it is four small functions.

use std::cmp::Ordering;

/// Characters `NFKD` leaves alone but readers still expect to fold together
/// (`_FOLD` in topics.py), applied *after* lowercasing and *before* NFKD —
/// the order matters, `´` becomes an apostrophe, not a space.
///
/// Deliberate departure from Python, and the reason the Greek branch of a topic
/// works: `ς` (final sigma) and `σ` are one letter, and `str.lower()` only
/// unifies them when it has the uppercase form to work from. A keyword typed
/// with the keyboard's `σ` therefore missed every headline written with `ς`
/// until both spellings were folded here.
fn substitute(ch: char) -> Option<&'static str> {
    match ch {
        'ß' => Some("ss"),
        'ø' => Some("o"),
        'æ' => Some("ae"),
        'œ' => Some("oe"),
        'ł' => Some("l"),
        'đ' => Some("d"),
        'ı' => Some("i"),
        'ς' => Some("σ"),
        '\u{2019}' | '\u{2018}' | '\u{b4}' => Some("'"),
        '\u{2013}' | '\u{2014}' => Some("-"),
        '\u{a0}' | '\u{2009}' => Some(" "),
        _ => None,
    }
}

/// `unicodedata.normalize("NFKD", ch)` with `unicodedata.combining()` chars
/// dropped, for the single characters a news keyword or title can hold.
///
/// The table covers Latin-1, Latin Extended-A/B, Greek and Cyrillic (the
/// ranges generated from Python's `unicodedata`); fullwidth ASCII is folded
/// by the range rule below. Codepoints outside it pass through unchanged.
fn nfkd(ch: char) -> Option<&'static str> {
    let cp = ch as u32;
    if !(0xa0..=0x45e).contains(&cp) {
        return None;
    }
    match NFKD.binary_search_by_key(&cp, |&(key, _)| key) {
        Ok(i) => Some(NFKD[i].1),
        Err(_) => None,
    }
}

/// Fullwidth ASCII (`ＡＩ` → `ai`) — NFKD compatibility mapping, by offset.
fn fullwidth(ch: char) -> Option<char> {
    let cp = ch as u32;
    if (0xff01..=0xff5e).contains(&cp) {
        char::from_u32(cp - 0xfee0)
    } else {
        None
    }
}

/// `unicodedata.combining(ch) != 0`, approximated by the combining blocks
/// that can appear next to real word characters.
fn is_combining(ch: char) -> bool {
    const RANGES: &[(u32, u32)] = &[
        (0x0300, 0x036f), // combining diacritical marks
        (0x0483, 0x0489), // Cyrillic
        (0x0591, 0x05bd), // Hebrew points
        (0x05bf, 0x05c7),
        (0x0610, 0x061a), // Arabic harakat
        (0x064b, 0x065f),
        (0x0670, 0x0670),
        (0x06d6, 0x06ed),
        (0x0730, 0x074a), // Syriac
        (0x0900, 0x0903), // Devanagari
        (0x093a, 0x094d),
        (0x0951, 0x0957),
        (0x0962, 0x0963),
        (0x0e31, 0x0e4e), // Thai
        (0x1ab0, 0x1aff),
        (0x1dc0, 0x1dff), // combining diacritical marks supplement
        (0x20d0, 0x20f0),
        (0xfe20, 0xfe2f), // combining half marks
    ];
    let cp = ch as u32;
    RANGES
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                Ordering::Greater
            } else if cp > hi {
                Ordering::Less
            } else {
                Ordering::Equal
            }
        })
        .is_ok()
}

/// One already-lowercased character, folded: `_FOLD` substitutions, then the
/// `NFKD` decomposition with combining marks dropped, then fullwidth ASCII.
fn push_folded(lower: char, out: &mut String) {
    if let Some(rep) = substitute(lower) {
        out.push_str(rep);
        return;
    }
    if lower.is_ascii() {
        out.push(lower);
        return;
    }
    if let Some(rep) = nfkd(lower) {
        push_expansion(rep, out);
        return;
    }
    if let Some(ascii) = fullwidth(lower) {
        // The compatibility form of a fullwidth letter is ASCII, which needs no
        // further folding.
        out.push(ascii);
        return;
    }
    if !is_combining(lower) {
        out.push(lower);
    }
}

/// The characters an `NFKD` expansion is made of, folded once more.
///
/// Without this second look the fold was not a fixed point: `ǣ` expands to
/// `æ`, which the substitution table itself rewrites to `ae`, so a keyword
/// written `Æ` (→ `ae`) and a text written `ǣ` (→ `æ`) landed on different forms
/// and never matched. The same happened to `ǿ`/`Ø`, and to `ϒ`, whose expansion
/// is the capital `Υ` — the one table entry that is not already lowercase.
fn push_expansion(rep: &str, out: &mut String) {
    for ch in rep.chars() {
        if let Some(again) = substitute(ch) {
            out.push_str(again);
        } else if ch.is_uppercase() {
            for lowered in ch.to_lowercase() {
                match substitute(lowered) {
                    Some(again) => out.push_str(again),
                    None => out.push(lowered),
                }
            }
        } else {
            out.push(ch);
        }
    }
}

/// `_fold(text)` from topics.py: lowercase, fold the hard cases, then strip
/// accents so `Élysée`, `Elysee` and `ÉLYSÉE` are one keyword.
///
/// Two deliberate departures from Python's `_fold`, both widening a match:
/// the two Greek sigmas fold together (see [`substitute`]) and the result is a
/// fixed point (see [`push_expansion`]), so text and keyword agree no matter
/// which spelling of a letter either side used.
pub fn text(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }
    if input.is_ascii() {
        // The common path: no `_FOLD` key and no accent is ASCII.
        return input.to_ascii_lowercase();
    }
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii() {
            out.push(ch.to_ascii_lowercase());
            continue;
        }
        // `char::to_lowercase` has no context, so the word-final shape of a
        // capital sigma is decided by the substitution table instead: both
        // sigmas fold to `σ`, which is what makes Greek matching work in
        // both directions (Python's `str.lower()` produced `ς` here).
        for lower in ch.to_lowercase() {
            push_folded(lower, &mut out);
        }
    }
    out
}

/// Generated from Python's `unicodedata`: every codepoint in these ranges
/// whose NFKD form differs once combining marks are dropped.
const NFKD: &[(u32, &str)] = &[
    (0x00A0, " "), (0x00A8, " "), (0x00AA, "a"), (0x00AF, " "), (0x00B2, "2"), (0x00B3, "3"),
    (0x00B4, " "), (0x00B5, "\u{03BC}"), (0x00B8, " "), (0x00B9, "1"), (0x00BA, "o"),
    (0x00BC, "1\u{2044}4"), (0x00BD, "1\u{2044}2"), (0x00BE, "3\u{2044}4"), (0x00C0, "A"),
    (0x00C1, "A"), (0x00C2, "A"), (0x00C3, "A"), (0x00C4, "A"), (0x00C5, "A"), (0x00C7, "C"),
    (0x00C8, "E"), (0x00C9, "E"), (0x00CA, "E"), (0x00CB, "E"), (0x00CC, "I"), (0x00CD, "I"),
    (0x00CE, "I"), (0x00CF, "I"), (0x00D1, "N"), (0x00D2, "O"), (0x00D3, "O"), (0x00D4, "O"),
    (0x00D5, "O"), (0x00D6, "O"), (0x00D9, "U"), (0x00DA, "U"), (0x00DB, "U"), (0x00DC, "U"),
    (0x00DD, "Y"), (0x00E0, "a"), (0x00E1, "a"), (0x00E2, "a"), (0x00E3, "a"), (0x00E4, "a"),
    (0x00E5, "a"), (0x00E7, "c"), (0x00E8, "e"), (0x00E9, "e"), (0x00EA, "e"), (0x00EB, "e"),
    (0x00EC, "i"), (0x00ED, "i"), (0x00EE, "i"), (0x00EF, "i"), (0x00F1, "n"), (0x00F2, "o"),
    (0x00F3, "o"), (0x00F4, "o"), (0x00F5, "o"), (0x00F6, "o"), (0x00F9, "u"), (0x00FA, "u"),
    (0x00FB, "u"), (0x00FC, "u"), (0x00FD, "y"), (0x00FF, "y"), (0x0100, "A"), (0x0101, "a"),
    (0x0102, "A"), (0x0103, "a"), (0x0104, "A"), (0x0105, "a"), (0x0106, "C"), (0x0107, "c"),
    (0x0108, "C"), (0x0109, "c"), (0x010A, "C"), (0x010B, "c"), (0x010C, "C"), (0x010D, "c"),
    (0x010E, "D"), (0x010F, "d"), (0x0112, "E"), (0x0113, "e"), (0x0114, "E"), (0x0115, "e"),
    (0x0116, "E"), (0x0117, "e"), (0x0118, "E"), (0x0119, "e"), (0x011A, "E"), (0x011B, "e"),
    (0x011C, "G"), (0x011D, "g"), (0x011E, "G"), (0x011F, "g"), (0x0120, "G"), (0x0121, "g"),
    (0x0122, "G"), (0x0123, "g"), (0x0124, "H"), (0x0125, "h"), (0x0128, "I"), (0x0129, "i"),
    (0x012A, "I"), (0x012B, "i"), (0x012C, "I"), (0x012D, "i"), (0x012E, "I"), (0x012F, "i"),
    (0x0130, "I"), (0x0132, "IJ"), (0x0133, "ij"), (0x0134, "J"), (0x0135, "j"), (0x0136, "K"),
    (0x0137, "k"), (0x0139, "L"), (0x013A, "l"), (0x013B, "L"), (0x013C, "l"), (0x013D, "L"),
    (0x013E, "l"), (0x013F, "L\u{00B7}"), (0x0140, "l\u{00B7}"), (0x0143, "N"), (0x0144, "n"),
    (0x0145, "N"), (0x0146, "n"), (0x0147, "N"), (0x0148, "n"), (0x0149, "\u{02BC}n"),
    (0x014C, "O"), (0x014D, "o"), (0x014E, "O"), (0x014F, "o"), (0x0150, "O"), (0x0151, "o"),
    (0x0154, "R"), (0x0155, "r"), (0x0156, "R"), (0x0157, "r"), (0x0158, "R"), (0x0159, "r"),
    (0x015A, "S"), (0x015B, "s"), (0x015C, "S"), (0x015D, "s"), (0x015E, "S"), (0x015F, "s"),
    (0x0160, "S"), (0x0161, "s"), (0x0162, "T"), (0x0163, "t"), (0x0164, "T"), (0x0165, "t"),
    (0x0168, "U"), (0x0169, "u"), (0x016A, "U"), (0x016B, "u"), (0x016C, "U"), (0x016D, "u"),
    (0x016E, "U"), (0x016F, "u"), (0x0170, "U"), (0x0171, "u"), (0x0172, "U"), (0x0173, "u"),
    (0x0174, "W"), (0x0175, "w"), (0x0176, "Y"), (0x0177, "y"), (0x0178, "Y"), (0x0179, "Z"),
    (0x017A, "z"), (0x017B, "Z"), (0x017C, "z"), (0x017D, "Z"), (0x017E, "z"), (0x017F, "s"),
    (0x01A0, "O"), (0x01A1, "o"), (0x01AF, "U"), (0x01B0, "u"), (0x01C4, "DZ"), (0x01C5, "Dz"),
    (0x01C6, "dz"), (0x01C7, "LJ"), (0x01C8, "Lj"), (0x01C9, "lj"), (0x01CA, "NJ"), (0x01CB, "Nj"),
    (0x01CC, "nj"), (0x01CD, "A"), (0x01CE, "a"), (0x01CF, "I"), (0x01D0, "i"), (0x01D1, "O"),
    (0x01D2, "o"), (0x01D3, "U"), (0x01D4, "u"), (0x01D5, "U"), (0x01D6, "u"), (0x01D7, "U"),
    (0x01D8, "u"), (0x01D9, "U"), (0x01DA, "u"), (0x01DB, "U"), (0x01DC, "u"), (0x01DE, "A"),
    (0x01DF, "a"), (0x01E0, "A"), (0x01E1, "a"), (0x01E2, "\u{00C6}"), (0x01E3, "\u{00E6}"),
    (0x01E6, "G"), (0x01E7, "g"), (0x01E8, "K"), (0x01E9, "k"), (0x01EA, "O"), (0x01EB, "o"),
    (0x01EC, "O"), (0x01ED, "o"), (0x01EE, "\u{01B7}"), (0x01EF, "\u{0292}"), (0x01F0, "j"),
    (0x01F1, "DZ"), (0x01F2, "Dz"), (0x01F3, "dz"), (0x01F4, "G"), (0x01F5, "g"), (0x01F8, "N"),
    (0x01F9, "n"), (0x01FA, "A"), (0x01FB, "a"), (0x01FC, "\u{00C6}"), (0x01FD, "\u{00E6}"),
    (0x01FE, "\u{00D8}"), (0x01FF, "\u{00F8}"), (0x0200, "A"), (0x0201, "a"), (0x0202, "A"),
    (0x0203, "a"), (0x0204, "E"), (0x0205, "e"), (0x0206, "E"), (0x0207, "e"), (0x0208, "I"),
    (0x0209, "i"), (0x020A, "I"), (0x020B, "i"), (0x020C, "O"), (0x020D, "o"), (0x020E, "O"),
    (0x020F, "o"), (0x0210, "R"), (0x0211, "r"), (0x0212, "R"), (0x0213, "r"), (0x0214, "U"),
    (0x0215, "u"), (0x0216, "U"), (0x0217, "u"), (0x0218, "S"), (0x0219, "s"), (0x021A, "T"),
    (0x021B, "t"), (0x021E, "H"), (0x021F, "h"), (0x0226, "A"), (0x0227, "a"), (0x0228, "E"),
    (0x0229, "e"), (0x022A, "O"), (0x022B, "o"), (0x022C, "O"), (0x022D, "o"), (0x022E, "O"),
    (0x022F, "o"), (0x0230, "O"), (0x0231, "o"), (0x0232, "Y"), (0x0233, "y"),
    (0x0374, "\u{02B9}"), (0x037A, " "), (0x037E, ";"), (0x0384, " "), (0x0385, " "),
    (0x0386, "\u{0391}"), (0x0387, "\u{00B7}"), (0x0388, "\u{0395}"), (0x0389, "\u{0397}"),
    (0x038A, "\u{0399}"), (0x038C, "\u{039F}"), (0x038E, "\u{03A5}"), (0x038F, "\u{03A9}"),
    (0x0390, "\u{03B9}"), (0x03AA, "\u{0399}"), (0x03AB, "\u{03A5}"), (0x03AC, "\u{03B1}"),
    (0x03AD, "\u{03B5}"), (0x03AE, "\u{03B7}"), (0x03AF, "\u{03B9}"), (0x03B0, "\u{03C5}"),
    (0x03CA, "\u{03B9}"), (0x03CB, "\u{03C5}"), (0x03CC, "\u{03BF}"), (0x03CD, "\u{03C5}"),
    (0x03CE, "\u{03C9}"), (0x03D0, "\u{03B2}"), (0x03D1, "\u{03B8}"), (0x03D2, "\u{03A5}"),
    (0x03D3, "\u{03A5}"), (0x03D4, "\u{03A5}"), (0x03D5, "\u{03C6}"), (0x03D6, "\u{03C0}"),
    (0x03F0, "\u{03BA}"), (0x03F1, "\u{03C1}"), (0x03F2, "\u{03C2}"), (0x03F4, "\u{0398}"),
    (0x03F5, "\u{03B5}"), (0x03F9, "\u{03A3}"), (0x0400, "\u{0415}"), (0x0401, "\u{0415}"),
    (0x0403, "\u{0413}"), (0x0407, "\u{0406}"), (0x040C, "\u{041A}"), (0x040D, "\u{0418}"),
    (0x040E, "\u{0423}"), (0x0419, "\u{0418}"), (0x0439, "\u{0438}"), (0x0450, "\u{0435}"),
    (0x0451, "\u{0435}"), (0x0453, "\u{0433}"), (0x0457, "\u{0456}"), (0x045C, "\u{043A}"),
    (0x045D, "\u{0438}"), (0x045E, "\u{0443}"),
];

#[cfg(test)]
mod tests {
    use super::text;

    #[test]
    fn fold_lowercases_and_strips_accents() {
        assert_eq!(text("Élysée"), "elysee");
        assert_eq!(text("ART"), "art");
        assert_eq!(text("Straße"), "strasse");
        assert_eq!(text("Œuvre"), "oeuvre");
        assert_eq!(text("İstanbul"), "istanbul");
        assert_eq!(text("Война"), "воина"); // NFKD drops the breve of й
        // Was "λογος" with a word-final sigma, which is what `str.lower()`
        // produces and what Python's `_fold` then kept. The fold now unifies the
        // two sigmas (see `substitute`), because a Greek keyword typed with σ
        // has to match a headline written with ς: that is the whole point of
        // folding a case, and the expectation changed on purpose.
        assert_eq!(text("ΛΟΓΟΣ"), "λογοσ");
        assert_eq!(text("ΣΟΦΙΑ"), "σοφια");
        assert_eq!(text(""), "");
    }

    #[test]
    fn the_two_greek_sigmas_are_one_letter() {
        // A final sigma, a normal sigma, and a capital sigma at the end of a
        // word all fold to the same character, so a keyword matches whichever
        // spelling the publisher used.
        assert_eq!(text("ς"), text("σ"));
        assert_eq!(text("ς"), "σ");
        assert_eq!(text("λόγος"), text("λόγοσ"));
        assert_eq!(text("ΛΟΓΟΣ"), text("λογοσ"));
        assert_eq!(text("ΛΟΓΟΣ"), text("λόγος"));
    }

    #[test]
    fn precomposed_spellings_fold_like_the_letters_they_stand_for() {
        // Ā macron vowels and friends expand to a character the substitution
        // table rewrites again; folding the expansion is what makes the two
        // spellings meet.
        assert_eq!(text("Ǣ"), text("Æ"));
        assert_eq!(text("Ǣ"), "ae");
        assert_eq!(text("Ǿ"), text("Ø"));
        assert_eq!(text("ǿ"), "o");
        // The one NFKD entry that expands to a capital (ϒ -> Υ) is lowered too.
        assert_eq!(text("ϒ"), text("Υ"));
        assert_eq!(text("ϒ"), "υ");
    }

    #[test]
    fn folding_an_already_folded_text_changes_nothing() {
        // The matcher compares one canonical form; if folding were not a fixed
        // point, a keyword and a text could fold to different forms of the same
        // letter (the ǣ/Æ case above) and silently never match.
        for cp in 0x20u32..0x3000 {
            let Some(ch) = char::from_u32(cp) else {
                continue;
            };
            let once = text(&ch.to_string());
            let twice = text(&once);
            assert_eq!(twice, once, "U+{:04X} folds to {:?}, which folds to {:?}", cp, once, twice);
        }
    }

    #[test]
    fn upper_and_lower_case_agree_inside_the_table() {
        // Every cased character the table covers has to fold to the same thing
        // in either case: that is what "case folding" means, and it is the
        // property the Greek sigma broke before. (Outside the covered ranges
        // the fold is documented as lossy: Vietnamese stacked tones, the Latin
        // Extended Additional block and halfwidth katakana keep their shape.)
        let covered = |cp: u32| (0x20..=0x45e).contains(&cp) || (0xff01..=0xff5e).contains(&cp);
        for cp in 0x20u32..=0xff5e {
            if !covered(cp) {
                continue;
            }
            let Some(ch) = char::from_u32(cp) else {
                continue;
            };
            // A combining mark has no case pair to compare: on its own it folds
            // to nothing, and its uppercase partner (U+0345 -> Ι) is a letter.
            if super::is_combining(ch) {
                continue;
            }
            let upper: String = ch.to_uppercase().collect();
            let lower: String = ch.to_lowercase().collect();
            assert_eq!(
                text(&upper),
                text(&lower),
                "U+{:04X} {:?}/{:?} fold differently",
                cp,
                upper,
                lower
            );
        }
    }

    #[test]
    fn fold_handles_the_hard_substitutions_and_spacing() {
        assert_eq!(text("don’t"), "don't");
        assert_eq!(text("a–b"), "a-b");
        assert_eq!(text("a\u{a0}b"), "a b");
        assert_eq!(text("ＡＩ model"), "ai model"); // fullwidth
        assert_eq!(text("e\u{301}"), "e"); // decomposed é
        assert_eq!(text("🙂"), "🙂");
    }
}
