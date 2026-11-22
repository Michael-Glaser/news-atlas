//! The extraction answer's own quality: what the prompt asks for, and what the parser
//! refuses before a word reaches `entities`.
//!
//! The archive's audit found `High`, `Suggests` and `Continue` typed `ORG`, a whole
//! headline typed as one organisation, and one relation stored as `Nvidia – High`. All
//! three came through this file: the prompt said "names exactly as written in the text",
//! which is an instruction to copy any capitalised word, and the parser accepted
//! whatever the answer called a name.
//!
//! The fix is two halves and both are pinned here: the instruction says what a named
//! thing is and what is not one, and the parser refuses the shapes that contradict it —
//! prose punctuation, an unbounded name, a lower-case name for a kind that is a name, a
//! word the article itself writes in lower case, and the same name in two casings. No
//! list of words is involved: a list would be this archive's vocabulary frozen.

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::enrich::{admit, clean_entities, Refused, CORRECTIVE, NAME_CHARS_MAX, NAME_WORDS_MAX, PROMPT};

/// The article the answer below was given: a headline whose words became entities, and a
/// body that uses two of them as ordinary prose.
const TEXT: &str = "Nvidia Stock Is Down 35% From Its High. History Suggests a $5,000 Investment \
Now Will Be Worth This Much by 2028.\nNvidia shares fell from their high after the review, and \
the report suggests the chip market will recover. Continue reading, or continue to the archive.";

fn answer() -> Value {
    json!({
        "entities": [
            {"name": "Nvidia", "type": "ORG", "alias_of": ""},
            {"name": "China", "type": "LOC", "alias_of": ""},
            {"name": "Jensen Huang", "type": "PERSON", "alias_of": ""},
            {"name": "High", "type": "ORG", "alias_of": ""},
            {"name": "Suggests", "type": "ORG", "alias_of": ""},
            {"name": "Continue", "type": "ORG", "alias_of": ""},
            {"name": "Nvidia Stock Is Down 35% From Its High", "type": "ORG", "alias_of": ""},
            {"name": "markets, chip supply and demand", "type": "TOPIC", "alias_of": ""},
            {"name": "nvidia", "type": "ORG", "alias_of": ""}
        ],
        "relations": [{"a": "Nvidia", "b": "China", "label": "supplies chips to"}],
        "categories": ["Technology"],
        "sentiment": "negative",
        "summary": "Nvidia fell. The market may recover."
    })
}

#[test]
fn a_junk_answer_is_refused_and_the_named_things_are_kept() {
    let mut refused: Vec<String> = Vec::new();
    let kept = clean_entities(answer().get("entities"), TEXT, &mut refused);

    let names: Vec<&str> = kept.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Nvidia", "China", "Jensen Huang"],
        "only the named things survive the answer"
    );
    assert_eq!(
        refused,
        vec![
            "High".to_string(),
            "Suggests".to_string(),
            "Continue".to_string(),
            "Nvidia Stock Is Down 35% From Its High".to_string(),
            "markets, chip supply and demand".to_string(),
            "nvidia".to_string(),
        ],
        "every refusal is counted, in the answer's own order"
    );

    // the kinds of the kept rows are the answer's, not repaired: an entity typed ORG
    // stays ORG, and an unknown kind is the one fold this file still does
    assert_eq!(kept[0].1, "ORG");
    assert_eq!(kept[2].1, "PERSON");
}

#[test]
fn the_reason_a_name_was_refused_is_the_rule_that_refused_it() {
    let seen: HashSet<String> = HashSet::new();
    // a common noun the body writes in lower case: `high` and `suggests` are prose there
    assert_eq!(admit("High", "ORG", TEXT, &seen), Err(Refused::Common));
    assert_eq!(admit("Suggests", "ORG", TEXT, &seen), Err(Refused::Common));
    // a name with no capital is a common word however it is typed
    assert_eq!(admit("chip", "ORG", TEXT, &seen), Err(Refused::Prose));
    // prose punctuation
    assert_eq!(
        admit("markets, chip supply and demand", "TOPIC", TEXT, &seen),
        Err(Refused::Fragment)
    );
    // a headline is longer than any plausible name
    assert_eq!(
        admit("Nvidia Stock Is Down 35% From Its High", "ORG", TEXT, &seen),
        Err(Refused::TooLong)
    );
    // the same name in another casing
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert("nvidia".to_string());
    assert_eq!(admit("NVIDIA", "ORG", TEXT, &seen), Err(Refused::Casing));
    assert_eq!(admit("   ", "ORG", TEXT, &seen), Err(Refused::Empty));

    // and the names that are names pass the same function, with nothing seen yet
    let clean: HashSet<String> = HashSet::new();
    assert_eq!(admit("Nvidia", "ORG", TEXT, &clean), Ok(()));
    assert_eq!(admit("Jensen Huang", "PERSON", TEXT, &clean), Ok(()));
    assert_eq!(
        admit("Securities and Exchange Commission", "ORG", TEXT, &clean),
        Ok(()),
        "a four-word institution is still a name"
    );
    // a name the article does not also write as prose passes even when it is one word
    assert_eq!(admit("Meloni", "PERSON", TEXT, &clean), Ok(()));
    // a topic is asked for as a subject, so it may be written in lower case
    assert_eq!(admit("semiconductors", "TOPIC", TEXT, &clean), Ok(()));
    assert_eq!(NAME_WORDS_MAX, 4);
    assert_eq!(NAME_CHARS_MAX, 48);
}

#[test]
fn a_capital_inside_a_name_is_not_prose() {
    let seen: HashSet<String> = HashSet::new();
    // the two shapes a lower-case first letter can have and still be a name
    assert_eq!(admit("iPhone", "ORG", "The iPhone shipped.", &seen), Ok(()));
    assert_eq!(admit("eBay", "ORG", "eBay reported.", &seen), Ok(()));
    assert_eq!(admit("McDonald", "ORG", "McDonald spoke.", &seen), Ok(()));
}

#[test]
fn the_prompt_asks_for_named_things_and_names_what_is_not_one() {
    // the instruction is the fix, so the instruction is the test: a prompt that slips
    // back to "names exactly as written in the text" is the defect returning
    for rule in [
        "named thing",
        "proper name",
        "common noun",
        "adjective",
        "verb form",
        "headline",
        "interface text",
        "quantifier",
    ] {
        assert!(
            PROMPT.contains(rule),
            "the prompt stopped saying what a name is not: {rule}"
        );
    }
    assert!(
        !PROMPT.contains("names exactly as written in the text"),
        "the loose instruction is back"
    );
    // the re-ask shows the model its own words rather than repeating the rule
    assert!(CORRECTIVE.contains("%(names)s"));
    assert!(CORRECTIVE.contains("not named things"));
}

#[test]
fn a_clean_answer_is_asked_nothing_again() {
    let mut refused: Vec<String> = Vec::new();
    let clean = json!([
        {"name": "Nvidia", "type": "ORG"},
        {"name": "China", "type": "LOC"}
    ]);
    let kept = clean_entities(Some(&clean), TEXT, &mut refused);
    assert_eq!(kept.len(), 2);
    assert!(
        refused.is_empty(),
        "a second model call is only paid for when the first answer was wrong"
    );
}
