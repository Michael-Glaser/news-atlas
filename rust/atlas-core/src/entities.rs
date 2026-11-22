//! `entities` — proper-noun phrases, counted.
//!
//! Decides what a phrase is (up to four capitalised tokens, sentence-start
//! stopwords dropped), what counts as an occurrence (case-insensitive, on word
//! boundaries), how a known name and a discovered name are merged, and how the
//! result is ordered and capped.
//!
//! It does not decide what a phrase means, whether it is a real-world entity, or
//! which of two names are the same thing: a caller supplies the known names and
//! reads the counts the way it likes.

use std::collections::{HashMap, HashSet};

use crate::json::{self, Json};
use crate::text;

const MAX_PHRASE_TOKENS: usize = 4;

/// Capitalised words that are almost never entities at sentence start.
/// Comparison is case-insensitive, so the lowercase forms are covered.
const STOPWORDS: &[&str] = &[
    "The", "This", "That", "It", "He", "She", "They", "We", "You", "But", "And", "However", "Meanwhile",
    "After", "Before", "New", "More", "Most", "Also", "Still", "Now", "Then", "There", "Here", "What", "When",
    "Where", "Why", "How", "Just", "Even", "Only", "Very", "First", "Last", "Next", "One", "Two", "Three",
    "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday", "January", "February",
    "March", "April", "May", "June", "July", "August", "September", "October", "November", "December",
];

pub struct Candidate {
    pub name: String,
    pub count: usize,
    pub known: bool,
}

struct Entry {
    count: usize,
    known: bool,
    display: String,
}

struct Tok {
    text: String,
    sentence_start: bool,
    capitalised: bool,
}

pub fn run(root: &Json) -> Result<String, String> {
    let source = match root.get("text").and_then(Json::as_str) {
        Some(t) => t,
        None => return Err("missing \"text\" string".to_string()),
    };
    let mut known: Vec<String> = Vec::new();
    if let Some(Json::Arr(items)) = root.get("known") {
        for item in items {
            if let Json::Str(raw) = item {
                let name = text::collapse_ws(raw);
                if !name.is_empty() {
                    known.push(name);
                }
            }
        }
    }
    let min_count = root
        .get("min_count")
        .and_then(Json::as_usize)
        .unwrap_or(1);
    let max = root.get("max").and_then(Json::as_usize).unwrap_or(40);

    let found = extract(source, &known, min_count, max);

    let mut out = String::from("{\"entities\":[");
    for (i, candidate) in found.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"name\":");
        json::escape_str(&mut out, &candidate.name);
        out.push_str(",\"count\":");
        out.push_str(&candidate.count.to_string());
        out.push_str(",\"known\":");
        out.push_str(if candidate.known { "true" } else { "false" });
        out.push('}');
    }
    out.push_str("]}");
    Ok(out)
}

pub fn extract(source: &str, known: &[String], min_count: usize, max: usize) -> Vec<Candidate> {
    let tokens = tokenize(source);
    let mut entries: HashMap<String, Entry> = HashMap::new();
    // phrase key -> next token key -> (next display form, times followed)
    let mut follows: HashMap<String, HashMap<String, (String, usize)>> = HashMap::new();

    let mut i = 0usize;
    while i < tokens.len() {
        if !tokens[i].capitalised {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < tokens.len() && tokens[j].capitalised {
            j += 1;
        }
        let run = &tokens[i..j];
        let max_len = run.len().min(MAX_PHRASE_TOKENS);
        for len in 1..=max_len {
            for offset in 0..=(run.len() - len) {
                let window = &run[offset..offset + len];
                if len == 1 && window[0].sentence_start && is_stopword(&window[0].text) {
                    continue;
                }
                let phrase = join_tokens(window);
                if phrase.is_empty() {
                    continue;
                }
                let key = phrase.to_lowercase();
                match entries.get_mut(&key) {
                    Some(entry) => entry.count += 1,
                    None => {
                        entries.insert(
                            key.clone(),
                            Entry {
                                count: 1,
                                known: false,
                                display: phrase,
                            },
                        );
                    }
                }
                if len <= 2 {
                    if let Some(next) = run.get(offset + len) {
                        let next_key = next.text.to_lowercase();
                        let inner = follows.entry(key).or_insert_with(HashMap::new);
                        let slot = inner
                            .entry(next_key)
                            .or_insert_with(|| (next.text.clone(), 0usize));
                        slot.1 += 1;
                    }
                }
            }
        }
        i = j;
    }

    // "Always followed by the same capitalised token" -> add merged phrase.
    let mut merged: Vec<(String, String)> = Vec::new();
    for (key, nexts) in follows.iter() {
        if nexts.len() != 1 {
            continue;
        }
        let count = match entries.get(key) {
            Some(entry) => entry.count,
            None => continue,
        };
        if count < 2 {
            continue;
        }
        if let Some((next_display, next_count)) = nexts.values().next() {
            if *next_count == count {
                merged.push((key.clone(), next_display.clone()));
            }
        }
    }
    for (key, next_display) in merged {
        let phrase_display = match entries.get(&key) {
            Some(entry) => entry.display.clone(),
            None => continue,
        };
        let count = match entries.get(&key) {
            Some(entry) => entry.count,
            None => continue,
        };
        let merged_key = format!("{} {}", key, next_display.to_lowercase());
        let merged_display = format!("{} {}", phrase_display, next_display);
        match entries.get_mut(&merged_key) {
            Some(entry) => {
                if count > entry.count {
                    entry.count = count;
                }
            }
            None => {
                entries.insert(
                    merged_key,
                    Entry {
                        count,
                        known: false,
                        display: merged_display,
                    },
                );
            }
        }
    }

    // Known names: counted case-insensitively on word boundaries, always kept.
    for name in known {
        if name.is_empty() {
            continue;
        }
        let key = name.to_lowercase();
        let hits = count_occurrences(source, name);
        match entries.get_mut(&key) {
            Some(entry) => {
                entry.known = true;
                entry.display = name.clone();
                if hits > entry.count {
                    entry.count = hits;
                }
            }
            None => {
                entries.insert(
                    key,
                    Entry {
                        count: hits,
                        known: true,
                        display: name.clone(),
                    },
                );
            }
        }
    }

    // A phrase that only ever occurs inside a longer one carries no extra
    // information: "Nvidia Corp" should not also report "Corp", nor
    // "New York Times" report "York" and "Times". Phrases are at most
    // MAX_PHRASE_TOKENS long, so this stays a cheap lookup per candidate.
    let mut covered: HashSet<String> = HashSet::new();
    for (key, entry) in entries.iter() {
        let words: Vec<&str> = key.split(' ').collect();
        if words.len() < 2 || words.len() > MAX_PHRASE_TOKENS {
            continue;
        }
        for start in 0..words.len() {
            for len in 1..=(words.len() - start) {
                if len == words.len() {
                    continue; // the phrase itself
                }
                let mut part = String::new();
                for (idx, word) in words[start..start + len].iter().enumerate() {
                    if idx > 0 {
                        part.push(' ');
                    }
                    part.push_str(word);
                }
                if let Some(short) = entries.get(&part) {
                    if short.count <= entry.count {
                        covered.insert(part);
                    }
                }
            }
        }
    }

    let mut list: Vec<Candidate> = entries
        .into_iter()
        .filter(|(key, entry)| {
            if entry.count == 0 {
                // A known name that never occurs is not an entity of this text.
                return false;
            }
            if entry.known {
                return true;
            }
            entry.count >= min_count && !covered.contains(key)
        })
        .map(|(_, entry)| Candidate {
            name: entry.display,
            count: entry.count,
            known: entry.known,
        })
        .collect();
    list.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    if list.len() > max {
        list.truncate(max);
    }
    list
}

fn is_stopword(token: &str) -> bool {
    STOPWORDS.iter().any(|w| w.eq_ignore_ascii_case(token))
}

fn join_tokens(window: &[Tok]) -> String {
    let mut out = String::new();
    for tok in window {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&tok.text);
    }
    out
}

/// Split text into word tokens, tracking sentence starts and capitalisation.
fn tokenize(source: &str) -> Vec<Tok> {
    let chars: Vec<char> = source.chars().collect();
    let mut out: Vec<Tok> = Vec::new();
    let mut i = 0usize;
    let mut sentence_start = true;
    while i < chars.len() {
        let c = chars[i];
        if c == '.' || c == '!' || c == '?' || c == '\n' || c == '\r' || c == '\u{2026}' || c == ':' {
            sentence_start = true;
            i += 1;
            continue;
        }
        if !c.is_alphanumeric() {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < chars.len() {
            let ch = chars[i];
            if ch.is_alphanumeric() {
                i += 1;
                continue;
            }
            let joiner = ch == '-' || ch == '\'' || ch == '\u{2019}' || ch == '.' || ch == '&';
            if joiner && i + 1 < chars.len() && chars[i + 1].is_alphanumeric() {
                i += 1;
                continue;
            }
            break;
        }
        let token: String = chars[start..i].iter().collect();
        let capitalised = token
            .chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false);
        out.push(Tok {
            text: token,
            sentence_start,
            capitalised,
        });
        sentence_start = false;
    }
    out
}

fn lower_char(c: char) -> char {
    let mut it = c.to_lowercase();
    match it.next() {
        Some(lower) => lower,
        None => c,
    }
}

/// Case-insensitive count of `needle` in `hay` on word boundaries.
fn count_occurrences(hay: &str, needle: &str) -> usize {
    let needle_norm: Vec<char> = needle
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ")
        .chars()
        .map(lower_char)
        .collect();
    if needle_norm.is_empty() {
        return 0;
    }
    let mut hay_norm: Vec<char> = Vec::with_capacity(hay.len());
    let mut prev_space = false;
    for c in hay.chars() {
        let lower = lower_char(c);
        if lower.is_whitespace() {
            if !prev_space {
                hay_norm.push(' ');
                prev_space = true;
            }
        } else {
            hay_norm.push(lower);
            prev_space = false;
        }
    }
    if needle_norm.len() > hay_norm.len() {
        return 0;
    }
    let mut count = 0usize;
    let mut i = 0usize;
    while i + needle_norm.len() <= hay_norm.len() {
        let prev_ok = i == 0 || !hay_norm[i - 1].is_alphanumeric();
        if prev_ok {
            let mut matched = true;
            for (k, want) in needle_norm.iter().enumerate() {
                if hay_norm[i + k] != *want {
                    matched = false;
                    break;
                }
            }
            if matched {
                let end = i + needle_norm.len();
                let next_ok = end >= hay_norm.len() || !hay_norm[end].is_alphanumeric();
                if next_ok {
                    count += 1;
                    i = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_counts_repeated_names_and_known_names() {
        let body = "Nvidia makes chips. Nvidia sells chips. The company Nvidia grows. \
                    TSMC is a foundry. tsmc again.";
        let known = vec!["TSMC".to_string()];
        let list = extract(body, &known, 1, 20);
        let nvidia = list
            .iter()
            .find(|e| e.name == "Nvidia")
            .expect("Nvidia present");
        assert_eq!(nvidia.count, 3);
        assert!(!nvidia.known);
        let tsmc = list
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case("tsmc"))
            .expect("TSMC present");
        assert!(tsmc.known);
        assert_eq!(tsmc.count, 2);
        assert!(list.iter().all(|e| e.name != "The"));
        assert!(list.iter().all(|e| e.name != "The company"));
    }

    #[test]
    fn entity_sentence_start_stopword_keeps_multiword_phrase() {
        let list = extract("New York is loud.", &[], 1, 20);
        assert!(list.iter().all(|e| e.name != "New"));
        assert!(list.iter().any(|e| e.name == "New York"));
    }

    #[test]
    fn entity_tokenizer_keeps_internal_punctuation() {
        let list = extract("OpenAI released GPT-4 for U.S. labs.", &[], 1, 20);
        assert!(
            list.iter().any(|e| e.name == "OpenAI"),
            "got {:?}",
            names(&list)
        );
        assert!(list.iter().any(|e| e.name == "GPT-4"), "got {:?}", names(&list));
        assert!(list.iter().any(|e| e.name == "U.S"), "got {:?}", names(&list));
    }

    fn names(list: &[Candidate]) -> Vec<String> {
        list.iter().map(|e| e.name.clone()).collect()
    }

    #[test]
    fn entity_min_count_and_max_are_respected() {
        let body = "Alpha said. Alpha said. Alpha said. Beta said once. Gamma said once.";
        let list = extract(body, &[], 2, 10);
        assert!(list.iter().all(|e| e.count >= 2));
        assert!(list.iter().any(|e| e.name == "Alpha"));
        let capped = extract(body, &[], 1, 1);
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].name, "Alpha");
    }

    #[test]
    fn entity_fragment_inside_a_longer_phrase_is_reported_once() {
        let body = "Nvidia Corp said demand rose. Nvidia Corp kept hiring engineers.";
        let list = extract(body, &[], 1, 20);
        let found = names(&list);
        assert!(found.iter().any(|n| n.as_str() == "Nvidia Corp"), "got {:?}", found);
        assert!(found.iter().all(|n| n.as_str() != "Corp"), "got {:?}", found);
    }

    #[test]
    fn known_name_that_never_occurs_is_not_reported() {
        let known = vec!["Zeta Corp".to_string()];
        let list = extract("Nothing here mentions that company.", &known, 1, 20);
        assert!(list.iter().all(|e| e.name != "Zeta Corp"), "got {:?}", names(&list));
    }
}
