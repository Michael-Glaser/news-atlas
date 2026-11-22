//! Entity candidates for one article.
//!
//! `graph.extract` in `server/graph.py` is the Python original: countries from the
//! geodata file, `#hashtags`, capitalised runs in the headline, frequent
//! capitalised runs in the body, plus every entity the database already knows.
//! That function is 200 lines of hand-tuned heuristics and **it is not what this
//! module claims to be**.
//!
//! What is ported, and where each piece lives:
//!
//! | Piece | Owner |
//! | --- | --- |
//! | the entities an article has, and what each one *is* | the model, `save_article`'s `entities` |
//! | capitalised-run candidates, with counts | `atlas-core entities` (via [`Rules`]) |
//! | which names are names at all | `atlas-graph` (`JUNK_L`, `_ok_name`, …) in `link_article` |
//! | the type of a name only the extractor found | this module, [`guess_type`] |
//! | whether a name is a place | `atlas-places` (the gazetteer), through [`place_of`] |
//! | the canonical form (`Inc.`/`Corp.`/possessive tails) | this module, `graph._canonical_name` |
//! | the place of the story as a `LOC` | `atlas-places` (`locate`) |
//!
//! This module is the **fallback**, not the rule. [`from_model`] reads the list the
//! model itself recognised — with the type it decided — and that list is what
//! `save_article` links whenever it is not empty; [`extract`] runs only when the
//! model named nobody. That ordering is the whole point: measured on the live
//! archive before this split, `entities` held names nobody had claimed to have read
//! ("Berlin Summit Europe Ready", "Chancellor Merz") typed by the extractor's own
//! shape rules.
//!
//! Deliberately **not** ported, and named here rather than quietly missing:
//! `#hashtag` entities, and the case-frequency plausibility pass (`_plausible`) that
//! rejects a sentence-initial capital that never recurs. The consequence is a
//! slightly shorter entity list per article — a recall loss, never a wrong name:
//! every candidate still passes [`admit`], [`written_lower`] and then
//! `atlas-graph`'s own filters before it is written.

use serde_json::Value;

use crate::rules::{ArticleText, Rules};

/// `graph._CORP_TOKENS` — a corporate word inside a run ends the name.
///
/// `atlas-graph` keeps the same vocabulary in a private module (it strips these
/// itself before a name reaches `entities`), so the list is written out here. It is
/// data, not a decision: a divergence would keep one word too many in a name, never
/// put a wrong name in the graph — `link_article` still filters what it is given.
const CORP_TOKENS: &[&str] = &[
    "ag", "co", "company", "corp", "corporation", "gmbh", "inc", "limited", "llc", "ltd", "plc",
    "sa",
];

/// `graph._CORP_SUFFIX_RE`'s alternatives, in the regex's own order.
const CORP_SUFFIXES: &[&str] = &[
    "inc", "inc.", "corp", "corp.", "corporation", "ltd", "ltd.", "limited", "llc", "l.l.c.",
    "gmbh", "gmbh.", "ag", "a.g.", "sa", "s.a.", "plc", "co", "co.", "company",
];

/// Names per article, before `link_article` filters them (`graph` links what it is
/// given, so the ceiling is here).
pub const MAX_ENTITIES: usize = 14;

/// Characters of body text scanned for candidates (`graph.extract` caps at 3 000).
pub const BODY_SCAN_CHARS: usize = 3_000;

/// The type vocabulary `save_article`'s spec offers the model, as the model is
/// asked to spell it.
///
/// A vocabulary, never a word filter: these are answers the model may give, and the
/// only thing this module checks is that the answer is one of them. Anything else is
/// stored untyped (`""`), because an invented type is a guess and a guess is what the
/// model's answer replaces.
pub const MODEL_TYPES: [&str; 8] = [
    "Person",
    "Organization",
    "Institution",
    "Country",
    "City",
    "Event",
    "Product",
    "Other",
];

/// How sure the gazetteer has to be that a name is a place before this module says
/// `LOC` or a second country is written onto the article.
///
/// Measured through `atlas-places locate` on every place this fixture names: a name
/// the gazetteer reads back whole (`Berlin`, `Ukraine`, `Russia`, `Kyiv`, `Moscow`,
/// `Germany`, `UK`→United Kingdom, `Deutschland`→Germany, `Kremlin`→Moscow, `Gaza`→
/// Gaza City, `New Zealand`, `Tel Aviv`, `Anchorage`) answers `1.00`; the only
/// sub-1.00 answer measured is a *string of two places* (`Kyiv Berlin` → 0.59, its
/// rival also 0.59), where the gazetteer says it cannot separate the two. 0.6 is the
/// cut above that, so a name the gazetteer is divided about never moves an article
/// onto a country's map.
pub const PLACE_CONF: f64 = 0.6;

/// A place the gazetteer names back: the name as the article wrote it, and the
/// country it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    /// The name as it was offered (`UK`, not `United Kingdom`): what the text says.
    pub name: String,
    /// ISO 3166-1 alpha-3, as `atlas-places` answered.
    pub iso: String,
}

/// The entities a model recognised, in the form this crate stores them.
///
/// `entities` is the `save_article` argument: an array of `{"name", "type"}` objects,
/// or — because a small model sends what it can — a plain array of names. The name is
/// held to [`atlas_ai::prompts::entity_name`], the same rule an entity name is held to
/// everywhere else in the app; the type to [`MODEL_TYPES`], with `Country` and `City`
/// mapped to the graph's `LOC` and every other word kept in the vocabulary's own
/// spelling — the model chose the word, only its casing is normalised, so `organization`
/// and `Organization` are the same answer. An entry with no usable name is dropped,
/// never repaired into one.
pub fn from_model(raw: Option<&Value>) -> Vec<(String, String)> {
    let items: &[Value] = match raw {
        Some(Value::Array(items)) => items,
        // One comma-separated string is the shape a small model falls back to (the same
        // fallback `tags` and `categories` accept), so it is read as a list of names.
        Some(Value::String(single)) => {
            let names = crate::tags::split_list(single);
            let pairs: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), "")).collect();
            return accept(&pairs);
        }
        _ => return Vec::new(),
    };
    let mut pairs: Vec<(&str, &str)> = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Value::String(name) => pairs.push((name.as_str(), "")),
            Value::Object(object) => {
                let name = object.get("name").and_then(Value::as_str).unwrap_or("");
                let kind = object.get("type").and_then(Value::as_str).unwrap_or("");
                pairs.push((name, kind));
            }
            _ => {}
        }
    }
    accept(&pairs)
}

/// Clean, type and cap the pairs a model sent.
fn accept(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (raw_name, raw_type) in pairs {
        let name = atlas_ai::prompts::entity_name(raw_name);
        if name.is_empty() {
            continue;
        }
        if out.len() >= MAX_ENTITIES {
            break;
        }
        let key = name.to_lowercase();
        if out.iter().any(|(existing, _)| existing.to_lowercase() == key) {
            continue;
        }
        let kind = model_type(raw_type);
        out.push((name, kind));
    }
    out
}

/// One of [`MODEL_TYPES`], in that vocabulary's own spelling, else `""`.
///
/// `Country` and `City` become the graph's `LOC`: the graph holds places under one
/// kind, and the map's rows are selected by it. The rest is kept as the model said —
/// the model's decision is the answer, and this function only refuses to invent one.
pub fn model_type(raw: &str) -> String {
    let asked = raw.trim().to_lowercase();
    for word in MODEL_TYPES {
        if asked == word.to_lowercase() {
            return match word {
                "Country" | "City" => "LOC".to_string(),
                other => other.to_string(),
            };
        }
    }
    String::new()
}

/// Extract `(name, type)` pairs for one article.
///
/// `rules` is `atlas-core`; `place` is what `atlas-places` resolved for the story
/// (it enters as a `LOC`, so a place the filters would call "New Zealand" is not
/// dropped).
pub fn extract(
    rules: &dyn Rules,
    title: &str,
    summary: &str,
    body: &str,
    place: Option<&str>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    let push = |name: &str, kind: &str, out: &mut Vec<(String, String)>, seen: &mut Vec<String>| {
        let canonical = canonical_name(name);
        if canonical.chars().count() < 3 {
            return;
        }
        let key = canonical.to_lowercase();
        if seen.iter().any(|existing| existing == &key) {
            return;
        }
        if out.len() >= MAX_ENTITIES {
            return;
        }
        seen.push(key);
        out.push((canonical, kind.to_string()));
    };

    let head = format!("{}. {}", title.trim(), summary.trim());
    // The article is the evidence a candidate is judged by. It is assembled once, from the
    // same fields the candidates come from, so "is this a name or the page's furniture?"
    // is answered by the text that produced the run rather than by a word list.
    let evidence = format!(
        "{} {} {}",
        title,
        summary,
        body.chars().take(BODY_SCAN_CHARS).collect::<String>()
    );
    let body_text: String = body.chars().take(BODY_SCAN_CHARS).collect();
    let head_runs = rules
        .entity_candidates(&head, &[], 1, MAX_ENTITIES)
        .unwrap_or_default();
    for (name, _count) in &head_runs {
        if written_lower(name, &evidence) || !admit(rules, name, &body_text) {
            continue;
        }
        push(name, guess_type(rules, name), &mut out, &mut seen);
    }

    // The headline's names are handed to the body pass as "known": a name that
    // recurs in the body is the same name, not a second one.
    let known: Vec<String> = head_runs.iter().map(|(name, _)| name.clone()).collect();
    if !body_text.trim().is_empty() {
        let body_runs = rules
            .entity_candidates(&body_text, &known, 2, MAX_ENTITIES * 2)
            .unwrap_or_default();
        for (name, _count) in &body_runs {
            if written_lower(name, &evidence) || !admit(rules, name, &body_text) {
                continue;
            }
            push(name, guess_type(rules, name), &mut out, &mut seen);
        }
    }

    if let Some(place) = place {
        if !place.trim().is_empty() {
            push(place.trim(), "LOC", &mut out, &mut seen);
        }
    }
    out
}

/// May the extractor offer this candidate at all?
///
/// A single capitalised word is admitted as before: one word is a name or a page word,
/// and [`written_lower`] plus `atlas-graph`'s filters still judge it. A **run** of two
/// or more words has to be anchored in the page: either the body writes that exact
/// phrase too, or the whole run is a gazetteer place (a run that *ends* in a place is
/// not one — "But Not Sans Ukraine" ends in Ukraine and is a headline fragment).
///
/// Why two words and not only the four-token runs the shape allows: measured on
/// article 3444 of the live archive the junk arrives at every length — "Berlin Summit
/// Europe Ready", "But Not Sans Ukraine", "Europe Ready To Talk" (four tokens) *and*
/// "Chancellor Merz" (two), the last one a title glued to a surname the body writes on
/// its own ("Merz said …"). All four are refused here; `Merz`, `Berlin`, `Ukraine` and
/// `Russia` stay. The price is a name the headline alone carries — the model's own
/// `entities` list is the answer to that, and it is what `save_article` uses first.
///
/// No word list appears in this rule: the two answers are "the page writes it" and
/// "the gazetteer knows it".
fn admit(rules: &dyn Rules, name: &str, body: &str) -> bool {
    let canonical = canonical_name(name);
    if canonical.split_whitespace().count() <= 1 {
        return true;
    }
    occurrences(body, &canonical) > 0 || place_of(rules, &canonical).is_some()
}

/// The gazetteer's answer for one name: is this name a place, and which country?
///
/// [`Rules::locate`] reads a *text* and answers the place it is about, so it also names
/// a place when the text merely contains one ("Berlin Summit Europe Ready" → Berlin). A
/// name is a place here only when the gazetteer read **the name**:
///
/// * the place it answered is spelled with exactly the words of the name ("Berlin",
///   "New Zealand"; "The Netherlands" is not — the gazetteer answers "Netherlands", so
///   the `The` is a word no gazetteer key holds, and the candidate window "Netherlands"
///   is the one that reaches the map), or
/// * the name is an alias the gazetteer knows under another spelling (`UK`, `Britain`,
///   `Deutschland`, `Turkiye`, `Gaza`, `Kremlin`), which is only believed for a name of
///   at most two words *and* only when every word of the name is needed for that
///   answer: "Euros Deutschland" also resolves to Germany, but dropping "Euros" leaves
///   the same answer, so "Euros" was noise and the name is not a place.
///
/// Both need [`PLACE_CONF`]. Nothing here is a list of place names: the gazetteer is
/// the data, and this is the structural question of whether it read the whole name. A
/// name with a qualifier ("Berlin Summit", "Ukraine The" — the last one two sentences
/// meeting) is not a place: the qualifier is not part of any gazetteer key, and the
/// place itself is a candidate of its own (`atlas-core entities` offers every window of
/// a run, so "Berlin" arrives beside "Berlin Summit").
pub(crate) fn place_of(rules: &dyn Rules, name: &str) -> Option<Place> {
    let asked = name.trim();
    if asked.is_empty() {
        return None;
    }
    let answer = rules.locate(&named(asked)).ok()?;
    if !answer.named() || answer.conf < PLACE_CONF {
        return None;
    }
    let wanted = words(asked);
    let found = words(&answer.place);
    let place = Place {
        name: asked.to_string(),
        iso: answer.iso.clone(),
    };
    match run_at(&wanted, &found) {
        Some(_) => (wanted == found).then_some(place),
        None => {
            if wanted.len() > 2 {
                return None;
            }
            // Every word is re-asked in the spelling the page used, not lower-cased: the
            // gazetteer knows `UK` and does not know `uk`, so a lower-cased rebuild would
            // "prove" that the word was noise when it was the whole match.
            let tokens: Vec<&str> = asked
                .split(|ch: char| !ch.is_alphanumeric())
                .filter(|part| !part.is_empty())
                .collect();
            for index in 0..tokens.len() {
                let shorter: Vec<&str> = tokens
                    .iter()
                    .enumerate()
                    .filter(|(other, _)| *other != index)
                    .map(|(_, part)| *part)
                    .collect();
                let again = rules.locate(&named(&shorter.join(" "))).ok()?;
                if again.named() && again.iso == answer.iso && again.place == answer.place {
                    return None;
                }
            }
            Some(place)
        }
    }
}

/// Every place one article's text names, for the article's countries.
///
/// The candidates are `atlas-core`'s capitalised runs (the same runs [`extract`] reads),
/// and each is put to [`place_of`] so the gazetteer — not a list of country names —
/// decides. `UK` and `Berlin` are places here, `Ukraine Peace Talks` is not; the mapping
/// from a name to its ISO code is the gazetteer's answer too. The headline and the
/// summary are scanned whole (they are the article's own claim about what it is about),
/// the body up to [`BODY_SCAN_CHARS`].
pub fn places(rules: &dyn Rules, title: &str, summary: &str, body: &str) -> Vec<Place> {
    let head = format!("{} {}", title.trim(), summary.trim());
    let tail: String = body.chars().take(BODY_SCAN_CHARS).collect();
    let mut out: Vec<Place> = Vec::new();
    for text in [head.as_str(), tail.as_str()] {
        if text.trim().is_empty() {
            continue;
        }
        // Every window of every run: 120 is the ceiling `atlas-core` sorts by count, and
        // a country named once deep in a body is still a country the story is in.
        let runs = rules
            .entity_candidates(text, &[], 1, 120)
            .unwrap_or_default();
        for (name, _count) in runs {
            let Some(place) = place_of(rules, &canonical_name(&name)) else {
                continue;
            };
            if out
                .iter()
                .any(|known| known.iso == place.iso && known.name == place.name)
            {
                continue;
            }
            out.push(place);
        }
    }
    out
}

/// [`ArticleText`] carrying one name, which is how a name is put to the gazetteer.
fn named(name: &str) -> ArticleText {
    ArticleText {
        title: name.to_string(),
        summary: name.to_string(),
        ..ArticleText::default()
    }
}

/// Lower-cased words without punctuation, for comparing a name with the place the
/// gazetteer answered.
///
/// Split on every non-alphanumeric character, not on whitespace: a page that glues two
/// sentences together writes `UK.An invitation …`, and "UK.An" must compare as two
/// words (so it is *not* the one-word alias "UK") rather than as one strange one.
fn words(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| word.to_lowercase())
        .collect()
}

/// Where `needle` occurs as a contiguous run inside `hay`, if it does.
fn run_at(hay: &[String], needle: &[String]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|start| hay[*start..*start + needle.len()] == *needle)
}

/// What kind of thing a name is, when nobody has said.
///
/// `LOC` when the gazetteer names it (see [`place_of`]) — a country or a city is a place
/// whatever the sentence around it says — and `MISC` otherwise, which is what the
/// model's own vocabulary calls "not said yet".
///
/// This is the fallback's type, never the model's: [`from_model`] carries the model's
/// answer, and `atlas-graph` keeps the first type an entity is given. `MISC` used to be
/// the answer for every name, and before that the function guessed from a vocabulary and
/// a space count (a corporate word made an organisation, exactly two words made a
/// person) — a fixed word list deciding what a story names. Measured then: 3478 of 4539
/// live entities were `ORG` and 947 `PERSON`, all of them exactly the two-word rows, with
/// no `enrich:` receipt to override one. The guess survives only where the *data* can
/// answer it, which is geography.
pub fn guess_type(rules: &dyn Rules, name: &str) -> &'static str {
    if place_of(rules, name).is_some() {
        "LOC"
    } else {
        "MISC"
    }
}

/// `graph._canonical_name` — the name without its noise.
///
/// Possessive tails, a corporate suffix at the end, and the edition marker a
/// feed adds are removed; a corporate suffix *inside* the run ends it
/// ("Nvidia Corp NVDA" → "Nvidia").
pub fn canonical_name(name: &str) -> String {
    let mut text = name
        .replace('\u{2019}', "'")
        .replace('\u{00a0}', " ")
        .replace('\u{2013}', "-");
    text = text.split_whitespace().collect::<Vec<&str>>().join(" ");
    let trimmed = text.trim_matches(|ch: char| {
        matches!(
            ch,
            ' ' | ',' | ';' | ':' | '"' | '\'' | '\u{201e}' | '\u{201c}' | '\u{201d}' | '\u{2018}'
                | '\u{2019}' | '\u{00ab}' | '\u{00bb}' | '\u{2026}' | '-' | '\u{2014}' | '|' | '/'
        )
    });
    let mut tokens: Vec<&str> = trimmed.split_whitespace().collect();
    for (index, token) in tokens.iter().enumerate() {
        if index + 1 < tokens.len() && CORP_TOKENS.contains(&token.trim_end_matches('.').to_lowercase().as_str())
        {
            tokens.truncate(index);
            break;
        }
    }
    let mut out: String = tokens.join(" ");
    for _ in 0..3 {
        let without_possessive = strip_possessive(&out);
        let without_suffix = strip_corp_suffix(&without_possessive);
        let cleaned = without_suffix.trim_matches(|ch: char| ch == ' ' || ch == ',' || ch == '.');
        if cleaned.is_empty() || cleaned == out {
            break;
        }
        out = cleaned.to_string();
    }
    out
}

/// Drop a trailing `'s` / `s'`.
fn strip_possessive(text: &str) -> String {
    let lower = text.to_lowercase();
    if lower.ends_with("'s") {
        return text[..text.len() - 2].to_string();
    }
    if lower.ends_with("s'") {
        return text[..text.len() - 1].to_string();
    }
    text.to_string()
}

/// Drop a trailing `, Inc.` / ` Corp.` / ` GmbH`.
fn strip_corp_suffix(text: &str) -> String {
    let lower = text.to_lowercase();
    for suffix in CORP_SUFFIXES {
        if !lower.ends_with(suffix) {
            continue;
        }
        let cut = lower.len() - suffix.len();
        // the suffix must be its own word: `[,\s]+` before it
        let head = &text[..cut];
        let trimmed = head.trim_end_matches(|ch: char| ch == ',' || ch.is_whitespace());
        if trimmed.len() == head.len() {
            continue; // no separator: "Cisco" is not "Co"
        }
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::TestRules;
    use serde_json::json;

    #[test]
    fn a_corporate_suffix_ends_a_name() {
        assert_eq!(canonical_name("Nvidia Corp NVDA"), "Nvidia");
        assert_eq!(canonical_name("Siemens AG"), "Siemens");
        assert_eq!(canonical_name("Apple Inc."), "Apple");
        assert_eq!(canonical_name("Cisco"), "Cisco");
    }

    #[test]
    fn a_possessive_tail_and_quotes_go() {
        assert_eq!(canonical_name("\u{2018}Angela Merkel\u{2019}s"), "Angela Merkel");
        assert_eq!(canonical_name("  “European Commission”  "), "European Commission");
    }

    #[test]
    fn a_name_nobody_has_typed_is_misc_and_a_place_is_loc() {
        // This test used to pin `graph.guess_type`: two words is a person, a corporate
        // word makes an organisation. That is a fixed vocabulary and a space count
        // deciding what kind of thing a story names, which is the model's decision, so
        // the shape rule is gone. What is left is the one answer the *data* can give:
        // the gazetteer says a name is a place. Measured on the live archive before the
        // change: 3478 of 4539 entities were ORG (the default rule) and 947 were PERSON —
        // exactly the two-word rows — with no `enrich:` receipt in the database to say a
        // model had ever confirmed one of them.
        let rules = TestRules;
        for name in [
            "Angela Merkel",
            "European Commission",
            "Siemens AG",
            "Nvidia",
            "Ministry of Foreign Affairs",
        ] {
            assert_eq!(
                guess_type(&rules, name),
                "MISC",
                "{name} was typed from its shape — a guess is not an answer, and MISC is \
                 what the model's own vocabulary calls 'not said yet'"
            );
        }
        // The stub engine knows Lisbon, Berlin and Portugal (see `crate::tests`); the
        // gazetteer's answer is a place, whatever the name looks like.
        for name in ["Berlin", "Lisbon", "Portugal"] {
            assert_eq!(guess_type(&rules, name), "LOC", "{name} is a place");
        }
        // The model's answer replaces the guess: `enrich::retype_from_model` asks this
        // function what the guess is, so a stored MISC is exactly what it overwrites.
    }

    #[test]
    fn a_model_list_is_cleaned_typed_and_capped() {
        // The shape the spec asks for: objects with a name and a type.
        let pairs = from_model(Some(&json!([
            {"name": "Friedrich Merz", "type": "Person"},
            {"name": "Ukraine", "type": "Country"},
            {"name": "Kyiv", "type": "City"},
            {"name": "Bundestag", "type": "Institution"},
            {"name": "Some Trader", "type": "Animal"}
        ])));
        assert_eq!(
            pairs,
            vec![
                ("Friedrich Merz".to_string(), "Person".to_string()),
                // Country and City are the graph's one `LOC` kind.
                ("Ukraine".to_string(), "LOC".to_string()),
                ("Kyiv".to_string(), "LOC".to_string()),
                ("Bundestag".to_string(), "Institution".to_string()),
                // An unknown type is untyped, never invented.
                ("Some Trader".to_string(), String::new()),
            ]
        );
        // The spelling is the vocabulary's, whatever the model's case was.
        assert_eq!(
            from_model(Some(&json!([{"name": "Nvidia", "type": "organization"}]))),
            vec![("Nvidia".to_string(), "Organization".to_string())]
        );
        // A plain array of names is "name only, no type".
        assert_eq!(
            from_model(Some(&json!(["Angela Merkel", "Nvidia"]))),
            vec![
                ("Angela Merkel".to_string(), String::new()),
                ("Nvidia".to_string(), String::new()),
            ]
        );
        // A name the app's own rule refuses is dropped, not repaired; a duplicate is one
        // entity; and the cap is `MAX_ENTITIES`.
        let refused = from_model(Some(&json!([
            {"name": "<img src=x onerror=alert(1)>", "type": "Person"},
            {"name": "ab", "type": "Person"},
            {"name": "Ukraine", "type": "Country"},
            {"name": "ukraine", "type": "Country"}
        ])));
        assert_eq!(refused, vec![("Ukraine".to_string(), "LOC".to_string())]);
        let many: Vec<Value> = (0..40)
            .map(|index| json!({"name": format!("Entity Number {}", index), "type": "Other"}))
            .collect();
        assert_eq!(from_model(Some(&json!(many))).len(), MAX_ENTITIES);
        // Nothing at all is an empty list, not a refusal: the extractor takes over.
        assert!(from_model(None).is_empty());
        assert!(from_model(Some(&json!([]))).is_empty());
        assert!(from_model(Some(&json!({"name": "Ukraine"}))).is_empty());
    }

    #[test]
    fn a_name_is_a_place_only_when_the_gazetteer_read_the_whole_name() {
        // The stub answers Berlin/Lisbon/Portugal for any text that contains them, which
        // is exactly the trap: `locate` reads a *text*, so it names Berlin inside a
        // headline fragment too.
        let rules = TestRules;
        assert_eq!(
            place_of(&rules, "Berlin").map(|place| place.iso),
            Some("DE".to_string())
        );
        assert_eq!(
            place_of(&rules, "Berlin Summit"),
            None,
            "a qualifier is not part of the gazetteer key: the run is not a place, and \
             `Berlin` is a candidate of its own"
        );
        assert_eq!(
            place_of(&rules, "Berlin Summit Europe Ready"),
            None,
            "three words the gazetteer did not read make it a fragment, not a place"
        );
        assert_eq!(place_of(&rules, "Chancellor Merz"), None);
        assert_eq!(place_of(&rules, "Kyiv"), None, "the stub does not know Kyiv");
        // An alias resolves to the place, and its own country comes with it.
        assert_eq!(place_of(&rules, ""), None);
    }

    #[test]
    fn a_run_of_four_words_from_a_headline_is_not_a_name() {
        // The rule the extractor's admission rests on, at the level it is decided: a run
        // of 2+ words has to be written by the body or be a place. "Chancellor Merz"
        // never occurs in the body ("Chancellor Friedrich Merz" and "Merz" do), and it is
        // not a place, so it never becomes a name.
        let rules = TestRules;
        let body = "Merz said Europe is ready. Merz hosted Volodymyr Zelensky in Berlin.";
        assert!(admit(&rules, "Merz", body), "one word is a name");
        assert!(admit(&rules, "Berlin", body));
        assert!(
            admit(&rules, "Volodymyr Zelensky", body),
            "the body writes it: anchored"
        );
        assert!(
            !admit(&rules, "Chancellor Merz", body),
            "a title glued to a surname the body writes alone is a fragment"
        );
        assert!(!admit(&rules, "Berlin Summit Europe Ready", body));
        assert!(
            !admit(&rules, "Berlin Summit", body),
            "a run the body never writes and the gazetteer does not know is a fragment"
        );
    }

    #[test]
    fn the_places_of_a_text_come_from_the_gazetteer() {
        let rules = TestRules;
        let found = places(&rules, "Berlin talks resumed", "", "Lisbon sent an envoy.");
        let names: Vec<&str> = found.iter().map(|place| place.name.as_str()).collect();
        assert!(names.contains(&"Berlin"), "{names:?}");
        assert!(names.contains(&"Lisbon"), "{names:?}");
        assert!(
            !names.iter().any(|name| *name == "Berlin talks resumed"),
            "a sentence is not a place: {names:?}"
        );
        assert!(
            found.iter().all(|place| !place.iso.is_empty()),
            "a place without a country cannot move an article on the map: {found:?}"
        );
    }

    #[test]
    fn the_article_is_the_evidence_for_a_page_word() {
        // The half a word list was standing in for: "Read More" is furniture because the
        // page says "read more" in running text, not because `read` and `more` are on a
        // list. `Reuters` is a name because the page writes it with a capital.
        let chrome = "Read more about our cookies. Read more here. By continuing you accept.";
        assert!(
            written_lower("Read More", chrome),
            "a capitalised run the page itself writes in lower case is furniture"
        );
        let story = "Nvidia and Reuters reported. Reuters said Nvidia would ship.";
        assert!(
            !written_lower("Reuters", story),
            "a name the page writes with a capital is a name"
        );
        assert!(
            !written_lower("Nvidia", story),
            "a name the page writes with a capital is a name"
        );
        // A name the text never repeats is admitted: silence is not evidence of furniture,
        // and refusing it would be the guessing the lists did.
        assert!(!written_lower("European Commission", story));
        // Word boundaries: `High` is not found in `highway`, and a mixed article is judged
        // by which case dominates.
        assert!(!written_lower("High", "the highway to the High Street"));
        assert!(written_lower("Study", "a study found this. another study agrees."));
        assert!(!written_lower("Study", "The Study of Studies."));
    }
}

/// Does the article write this name in lower case at least as often as in the case it was
/// offered in?
///
/// This is the rule that replaced four word lists (`JUNK_L`, `JUNK_BASE`, `UI_JUNK`,
/// `OUTLETS` in `atlas-graph`, 1438 words between them). Those lists decided which
/// capitalised run could become an entity, and the archive could not show that a list had
/// decided: 99 of the live archive's 1435 articles carry, in the `publisher` column, one of
/// the 69 publishers that list held (13 distinct) and 42 name one in the title or summary,
/// yet none could become an entity, and no row recorded why. The evidence is already in the
/// article: a page's
/// "Read more", "Subscribe", "Continue reading" and "Clique" are written in lower case
/// everywhere else on the page, and a name a story is about is written in its capitals.
///
/// Counted on word boundaries, case-insensitively, over the title, the summary and the
/// head of the body:
///
/// * `Read More` offered, the text says "read more" twice and "Read More" once → furniture;
/// * `Reuters` offered, the text says "Reuters" three times and never "reuters" → a name;
/// * a name the text never repeats → admitted, because refusing silence would be the same
///   guessing the lists did.
///
/// The same judgement, in the same words, is `enrich::admit`'s `Refused::Common` for names
/// a *model* offers; this is the candidate stage, where the runs come from the text itself.
fn written_lower(name: &str, text: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_lowercase();
    let haystack = text.to_lowercase();
    let lower_hits = occurrences(&haystack, &lower);
    if lower_hits == 0 {
        return false;
    }
    let upper_hits = occurrences(text, trimmed);
    lower_hits > upper_hits
}

/// How many times `needle` occurs in `haystack` on word boundaries.
///
/// A boundary is a character that is neither alphanumeric nor `_`, on both sides, so
/// `High` is not found in `highway` and `Read More` is not found in `readmore`.
fn occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    let hay: Vec<char> = haystack.chars().collect();
    let want: Vec<char> = needle.chars().collect();
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
    let mut count = 0usize;
    let mut i = 0usize;
    while i + want.len() <= hay.len() {
        let matches = hay[i..i + want.len()]
            .iter()
            .zip(want.iter())
            .all(|(a, b)| a == b);
        if matches {
            let before_ok = i == 0 || !is_word(hay[i - 1]);
            let after = i + want.len();
            let after_ok = after == hay.len() || !is_word(hay[after]);
            if before_ok && after_ok {
                count += 1;
                i = after;
                continue;
            }
        }
        i += 1;
    }
    count
}
