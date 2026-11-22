//! The user's keywords, compiled: from a comma-separated string to a matcher.
//!
//! Decides, in order: how the `keywords` and `exclude` strings are split,
//! trimmed, lowered and de-duplicated (`terms`); which spellings of one word
//! count as the same keyword family (`stem_key`); which endings a keyword admits
//! (`rate` -> `rate|rates|rated|rating`, `policies` -> `policy|policies|…`); and
//! how that matcher is run over an already-folded text (`search`) — a word
//! boundary before and after, `[\s\-]+` between the words of a phrase, no
//! backtracking and no recursion.
//!
//! Does not decide: what a match is worth or whether it clears a threshold. That
//! is `crate::score`. There is no stemmer and no news vocabulary here: the only
//! relevance signal is the list the user typed.

use std::collections::HashSet;

use crate::chars;

/// `_terms(raw)`: split a comma/semicolon/newline separated keyword string
/// into clean lowercased terms, in the user's order, duplicates dropped.
pub fn terms(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for chunk in raw.split(|c: char| c == ',' || c == ';' || c == '\n') {
        // Python: " ".join(chunk.split()).strip().strip('"').lower()
        let collapsed = chars::collapse(chunk);
        let term = collapsed.trim_matches('"').to_lowercase();
        if chars::char_count(&term) >= 2 && seen.insert(term.clone()) {
            out.push(term);
        }
    }
    out
}

/// `_stem_key(term)`: one folded form per keyword family, so "war" and
/// "wars" inside one topic are counted once.
///
/// // BUG: (mirrored from Python) the rule is not symmetric across both
/// sides: "taxes" stems to "taxe" while "tax" stems to "tax", so a topic
/// holding both counts two keywords whose patterns disagree about "tax".
pub fn stem_key(term: &str) -> String {
    let word = crate::fold::text(term);
    let len = chars::char_count(&word);
    if word.ends_with("ies") && len > 5 {
        let mut out = word;
        out.truncate(out.len() - 3);
        out.push('y');
        return out;
    }
    if word.ends_with('s') && !word.ends_with("ss") && len > 4 {
        return word[..word.len() - 1].to_string();
    }
    word
}

/// One keyword word: the literal that has to appear, plus the endings the
/// Python pattern builder allows after it. An empty ending means "stop
/// here"; `EXACT` (`[""]`) is a word with no endings at all.
pub struct WordPat {
    prefix: String,
    tails: &'static [&'static str],
}

/// One keyword (possibly several words) compiled for a boolean search.
/// An empty `words` list is Python's `(?!)\` — it matches nothing.
pub struct TermPat {
    words: Vec<WordPat>,
}

const EXACT: &[&str] = &[""];
const PLURAL: &[&str] = &["s", "es", "ed", "ing", ""];
const Y_ENDING: &[&str] = &["y", "ies", "ied", "ying"];
const E_ENDING: &[&str] = &["e", "es", "ed", "ing"];

/// `_compile(term)`: word-boundary pattern, multi-word terms tolerating
/// extra whitespace and dashes. `term` may arrive un-folded.
pub fn compile(term: &str) -> TermPat {
    let folded = crate::fold::text(term);
    let words: Vec<WordPat> = folded
        .split(chars::is_space)
        .filter(|w| !w.is_empty())
        .map(word_pat)
        .collect();
    TermPat { words }
}

/// `_word(word)` for one already-folded word.
fn word_pat(word: &str) -> WordPat {
    let mut w = word.to_string();
    let len = chars::char_count(&w);
    // Under three characters, or anything that is not a pure letter run
    // ("gpt-4", "h2", "ai"), is matched literally with no endings.
    if len < 3 || !w.chars().all(|c| c.is_alphabetic()) {
        return WordPat {
            prefix: w,
            tails: EXACT,
        };
    }
    if w.ends_with("ies") && len > 5 {
        w.truncate(w.len() - 3);
        w.push('y'); // policies -> policy
    } else if (w.ends_with("ses")
        || w.ends_with("xes")
        || w.ends_with("ches")
        || w.ends_with("shes"))
        && len > 5
    {
        w.truncate(w.len() - 2); // taxes -> tax(e)
    } else if w.ends_with('s') && !w.ends_with("ss") && len > 4 {
        w.truncate(w.len() - 1); // rates -> rate
    }
    let len = chars::char_count(&w);
    if w.ends_with('y') && len >= 4 {
        let before = chars::take_chars(&w, len - 1).chars().next_back().unwrap_or(' ');
        if !is_vowel(before) {
            w.truncate(w.len() - 1); // policy -> polic + (y|ies|ied|ying)
            return WordPat {
                prefix: w,
                tails: Y_ENDING,
            };
        }
    }
    if w.ends_with('e') && len >= 4 {
        w.truncate(w.len() - 1); // rate -> rat + (e|es|ed|ing)
        return WordPat {
            prefix: w,
            tails: E_ENDING,
        };
    }
    WordPat {
        prefix: w,
        tails: PLURAL,
    }
}

fn is_vowel(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')
}

/// Does `term` occur anywhere in `folded` (which must already be folded)?
///
/// Candidate positions come from the first word's literal, so the scan over
/// the body is linear; each candidate costs a handful of byte comparisons.
/// No backtracking engine, no recursion, and — for the single-word keywords
/// that nearly every topic is made of — no allocation at all.
pub fn search(folded: &str, term: &TermPat) -> bool {
    let words = &term.words;
    let Some(first) = words.first() else {
        return false;
    };
    // One scratch stack for the whole scan, not one per candidate position:
    // a body word that repeats the stem ("oils … oils …") produced one heap
    // allocation per occurrence before. Multi-word phrases are the only
    // caller of it, and `Vec::new` does not allocate until the first push.
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let mut from = 0usize;
    loop {
        let Some(offset) = folded.get(from..).and_then(|tail| tail.find(first.prefix.as_str()))
        else {
            return false;
        };
        let at = from + offset;
        if matches_at(folded, at, words, &mut stack) {
            return true;
        }
        // Advance one character, staying on a character boundary.
        let step = folded[at..].chars().next().map_or(1, char::len_utf8);
        from = at + step;
        if from > folded.len() {
            return false;
        }
    }
}

/// The Python pattern's `(?<!\w)` … `(?!\w)` with `[\s\-]+` between words.
///
/// Depth-first over (word index, position) using `stack` as the frontier, so
/// a phrase of any length is matched without recursion.
fn matches_at(
    folded: &str,
    start: usize,
    words: &[WordPat],
    stack: &mut Vec<(usize, usize)>,
) -> bool {
    if words.is_empty() || !opens_word(folded, start) {
        return false;
    }
    let last = words.len() - 1;
    if last == 0 {
        // The common case: one word, so no frontier is needed.
        let word = &words[0];
        return match word_end(folded, start, word) {
            Some(base) => finishes(folded, base, word),
            None => false,
        };
    }
    stack.clear();
    stack.push((0, start));
    while let Some((index, pos)) = stack.pop() {
        let word = &words[index];
        let Some(base) = word_end(folded, pos, word) else {
            continue;
        };
        for tail in word.tails {
            let Some(end) = append(folded, base, tail) else {
                continue;
            };
            if index == last {
                if ends_word(folded, end) {
                    return true;
                }
            } else if let Some(next) = skip_separator(folded, end) {
                stack.push((index + 1, next));
            }
        }
    }
    false
}

/// Where `word`'s literal stops when it starts at `pos`, or `None`.
fn word_end(folded: &str, pos: usize, word: &WordPat) -> Option<usize> {
    append(folded, pos, &word.prefix)
}

/// `folded[at..]` starts with `part`, so `at` is a character boundary.
fn append(folded: &str, at: usize, part: &str) -> Option<usize> {
    let tail = folded.get(at..)?;
    if tail.starts_with(part) {
        Some(at + part.len())
    } else {
        None
    }
}

/// Any of the endings the keyword admits fits here, and the word ends after it.
fn finishes(folded: &str, base: usize, word: &WordPat) -> bool {
    word.tails
        .iter()
        .any(|tail| append(folded, base, tail).is_some_and(|end| ends_word(folded, end)))
}

/// `(?<!\w)`: the character before `start` is not a word character.
fn opens_word(folded: &str, start: usize) -> bool {
    match folded.get(..start) {
        Some(head) => match chars::last_char(head) {
            Some(prev) => !chars::is_word_char(prev),
            None => true,
        },
        None => false,
    }
}

/// `(?!\w)`: the character at `end` is not a word character (or there is none).
fn ends_word(folded: &str, end: usize) -> bool {
    match folded.get(end..) {
        Some(rest) => match rest.chars().next() {
            Some(next) => !chars::is_word_char(next),
            None => true,
        },
        None => false,
    }
}

/// `[\s\-]+`: at least one whitespace or hyphen, consumed.
fn skip_separator(folded: &str, pos: usize) -> Option<usize> {
    let mut bytes = 0;
    for ch in folded[pos..].chars() {
        if ch == '-' || chars::is_space(ch) {
            bytes += ch.len_utf8();
        } else {
            break;
        }
    }
    if bytes == 0 {
        None
    } else {
        Some(pos + bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::{compile, search, stem_key, terms};

    #[test]
    fn terms_are_split_cleaned_lowercased_and_deduped() {
        assert_eq!(
            terms("Oil price, OPEC; refinery\n\"Quoted\""),
            vec!["oil price", "opec", "refinery", "quoted"]
        );
        assert_eq!(terms("war, war, WAR"), vec!["war"]);
        // A one-character chunk is dropped, like Python's len(t) >= 2.
        assert_eq!(terms("  a b  ,  x  "), vec!["a b"]);
        assert_eq!(terms(""), Vec::<String>::new());
        assert_eq!(terms("x, , , "), Vec::<String>::new());
        assert_eq!(terms("ab, ab"), vec!["ab"]);
    }

    #[test]
    fn stem_key_groups_one_family() {
        // Only words longer than four characters lose their plural "s".
        assert_eq!(stem_key("rates"), "rate");
        assert_eq!(stem_key("rate"), "rate");
        assert_eq!(stem_key("policies"), "policy");
        assert_eq!(stem_key("policy"), "policy");
        assert_eq!(stem_key("war"), "war");
        assert_eq!(stem_key("wars"), "wars"); // four chars: left as is
        // BUG (kept from Python): "taxes" does not stem to "tax".
        assert_eq!(stem_key("taxes"), "taxe");
        assert_eq!(stem_key("tax"), "tax");
    }

    // -- patterns: exactly the cases tools/topics_check.py asserts ----------

    fn hits(term: &str, article: &str) -> bool {
        search(&crate::fold::text(article), &compile(term))
    }

    #[test]
    fn a_keyword_inside_a_longer_word_does_not_match() {
        assert!(!hits("art", "Artificial intelligence lifts the market"));
        assert!(!hits("war", "warfare"));
        assert!(!hits("ai", "saigon falls"));
    }

    #[test]
    fn plural_and_verb_endings_behave_like_python() {
        assert!(hits("rate", "The rate rises"));
        assert!(hits("rate", "Rates are rising"));
        assert!(hits("rate", "The central bank rated it"));
        assert!(hits("rate", "Rating agencies agree"));
        assert!(!hits("rate", "The rater left"));
        assert!(hits("policies", "New policies land"));
        assert!(hits("policies", "One policy lands"));
        assert!(!hits("policies", "Policed"));
        // BUG (kept from Python): a keyword typed as "taxes" cannot match "tax".
        assert!(!hits("taxes", "tax"));
        assert!(hits("taxes", "taxes rise"));
    }

    #[test]
    fn multi_word_terms_tolerate_space_hyphen_and_newline() {
        assert!(hits("chip export", "New chip export rules land"));
        assert!(hits("chip export", "New chip-export rules land"));
        assert!(hits("chip export", "New chip\nexport rules land"));
        assert!(hits("chip export", "New chips export rules land"));
        assert!(!hits("chip export", "New export chip rules land"));
        assert!(!hits("chip export", "New chip exportation rules land"));
    }

    #[test]
    fn short_and_punctuated_terms_are_literal() {
        assert!(hits("ai", "AI-driven chips"));
        assert!(hits("gpt-4", "GPT-4 ships"));
        assert!(hits("h2", "The h2 market"));
        assert!(!hits("ai", "railway"));
    }

    #[test]
    fn accent_folding_works_both_ways() {
        assert!(hits("elysee", "Élysée palace confirms the visit"));
        assert!(hits("Élysée", "elysee palace confirms the visit"));
        assert!(hits("cafe", "The cafe on the corner closed"));
        assert!(!hits("elysee", "Élyséens protest"));
    }

    #[test]
    fn cyrillic_keywords_and_boundaries_work() {
        assert!(hits("война", "Война продолжается, обстрелы усилились"));
        assert!(!hits("война", "Войнах больше нет"));
        assert!(!hits("перемирие", "Война: обстрел продолжается"));
    }

    #[test]
    fn every_ending_of_the_ending_table_is_reachable() {
        // "burn" keeps the PLURAL table (five endings, one of them empty). Every
        // one of them has to be tried: a matcher that stopped at four would
        // silently lose "burning" and report the topic as a miss.
        assert!(hits("burn", "burn"));
        assert!(hits("burn", "burns"));
        assert!(hits("burn", "burnes")); // mechanical, not English: the table says so
        assert!(hits("burn", "burned"));
        assert!(hits("burn", "burning"));
        assert!(!hits("burn", "burnish"));
        // Y_ENDING and E_ENDING are reached from their own spellings.
        assert!(hits("carry", "carried"));
        assert!(hits("carry", "carrying"));
        assert!(hits("rate", "rates"));
        assert!(hits("rate", "rated"));
        assert!(hits("rate", "rating"));
    }

    #[test]
    fn a_stem_repeated_all_over_the_body_stays_linear() {
        // The worst case for the candidate loop: the stem of the keyword occurs
        // hundreds of times and no occurrence ever completes the word, so every
        // one is a failed candidate. Before the frontier was hoisted out of the
        // loop this allocated one Vec per occurrence; the scan is now
        // allocation-free for a single-word keyword and ~45% faster in release
        // (measured 35.9 us -> 19.6 us per 20 kB scan, `cargo test --release`).
        let mut body = String::new();
        while body.len() < 20_000 {
            body.push_str("oils are trading and the barrels move ");
        }
        let folded = crate::fold::text(&body);
        let miss = compile("oily"); // stem "oil", never completes
        let hit = compile("trading and"); // two words, adjacent once per sentence
        let candidates = folded.matches("oil").count();
        assert!(candidates > 400, "body should repeat the stem, got {}", candidates);

        let started = std::time::Instant::now();
        let scans = 20;
        for _ in 0..scans {
            assert!(!search(&folded, &miss));
        }
        let elapsed = started.elapsed();
        println!(
            "{} failed scans of a {} byte body with {} stem occurrences: {:?} ({:?} each)",
            scans,
            folded.len(),
            candidates,
            elapsed,
            elapsed / scans
        );
        assert!(search(&folded, &hit));
        // A canary, not a pin: the point is that a pathological body cannot turn
        // a miss into seconds. Debug builds are ~7x slower than release here.
        let budget_ms = if cfg!(debug_assertions) { 40 } else { 5 };
        assert!(
            elapsed.as_millis() < budget_ms,
            "{} failed scans took {:?}",
            scans,
            elapsed
        );
    }
}

#[cfg(test)]
mod differential {
    //! `search` against a brute-force scan of the same pattern.
    //!
    //! `search` is an optimised matcher (candidate positions from the first
    //! word's literal, one frontier for the whole scan). The reference below
    //! tries *every* start position and every ending, recursively, with no
    //! pre-scan at all — slow, obviously correct, and only reachable from
    //! tests. A rewrite of the fast path that misses a position or drops an
    //! ending shows up here as a disagreement.

    use super::{compile, search, TermPat, WordPat};
    use crate::chars;

    fn brute(folded: &str, term: &TermPat) -> bool {
        (0..=folded.len())
            .filter(|start| folded.is_char_boundary(*start))
            .any(|start| chain(folded, start, &term.words))
    }

    fn chain(folded: &str, pos: usize, words: &[WordPat]) -> bool {
        let Some((word, rest)) = words.split_first() else {
            return false;
        };
        if !folded[pos..].starts_with(word.prefix.as_str()) {
            return false;
        }
        let base = pos + word.prefix.len();
        for tail in word.tails {
            if !folded[base..].starts_with(tail) {
                continue;
            }
            let end = base + tail.len();
            if rest.is_empty() {
                let before = pos == 0
                    || folded[..pos]
                        .chars()
                        .next_back()
                        .map_or(true, |c| !chars::is_word_char(c));
                let after = folded[end..]
                    .chars()
                    .next()
                    .map_or(true, |c| !chars::is_word_char(c));
                if before && after {
                    return true;
                }
            } else {
                let mut at = end;
                let mut seen = false;
                while let Some(ch) = folded[at..].chars().next() {
                    if ch == '-' || chars::is_space(ch) {
                        at += ch.len_utf8();
                        seen = true;
                    } else {
                        break;
                    }
                }
                if seen && chain(folded, at, rest) {
                    return true;
                }
            }
        }
        false
    }

    const TERMS: &[&str] = &[
        "war",
        "wars",
        "rate",
        "policies",
        "taxes",
        "art",
        "ai",
        "gpt-4",
        "chip export",
        "oil price",
        "peace talks resume",
        "война",
        "ΛΟΓΟΣ",
        "elysee",
        "carry",
        "burn",
        "h2",
        "İstanbul",
        // Self-overlapping literals: the only shape where a candidate can be
        // skipped by advancing one character too far, so they are what pins the
        // candidate walk itself (see `the_reference_is_not_simply_always_false`).
        "aa",
        "aaa",
    ];

    const TEXTS: &[&str] = &[
        "",
        "wars are wars",
        "warfare and warship",
        "art artificial art",
        "The rate rises, rates fell, rating agencies rated it",
        "New policies land where one policy landed",
        "taxes rise while tax does not",
        "a chip-export ban on chip  export rules",
        "chip\n\nexport and chip -- export",
        "oil price, oil  prices, oil-price cap",
        "peace talks resume in the spring",
        "Война продолжается, войнах нет",
        "Ο ΛΟΓΟΣ και ο λόγος",
        "elysee Élysée ÉLYSÉE",
        "carry carried carrying carries",
        "burn burns burnes burned burning burnish",
        "the h2 market, h2o, ai-driven AI",
        "istanbul İSTANBUL İstanbul",
        "СЛОВО",
        "🙂 war 🙂 war🙂",
        "aaa",
        "aaaa",
        "a aa aaa",
        "aa",
    ];

    #[test]
    fn search_agrees_with_a_brute_force_scan() {
        let mut checked = 0;
        for term in TERMS {
            let pattern: TermPat = compile(term);
            for text in TEXTS {
                let folded = crate::fold::text(text);
                let fast = search(&folded, &pattern);
                let slow = brute(&folded, &pattern);
                assert_eq!(
                    fast, slow,
                    "term {:?} against {:?} (folded {:?}): fast {} reference {}",
                    term, text, folded, fast, slow
                );
                checked += 1;
            }
        }
        assert_eq!(checked, TERMS.len() * TEXTS.len());
    }

    #[test]
    fn the_reference_is_not_simply_always_false() {
        // A reference that never matched would make the comparison above empty.
        let pattern = compile("war");
        assert!(brute(&crate::fold::text("war and peace"), &pattern));
        assert!(!brute(&crate::fold::text("warfare"), &pattern));
    }
}
