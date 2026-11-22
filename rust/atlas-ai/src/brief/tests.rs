//! The briefing instruction and its parser, measured: what a model writes, and what this crate
//! is willing to read out of it.

use super::*;

/// The answer a good turn writes: one sentence, three facts, each with its citation.
const CLEAN: &str = r#"{"line": "Chip export rules tightened across the region this week.", "bullets": ["The commerce department added three processor families to the licence list [Article #3418].", "TSMC paused shipments to Chinese customers within a day of the rule [Article #3441].", "Analysts expect the curbs to slow data-centre build-outs into next year [Article #3443]."]}"#;

#[test]
fn a_clean_answer_parses() {
    let read = parse(CLEAN).expect("a briefing");
    assert_eq!(
        read.line,
        "Chip export rules tightened across the region this week."
    );
    assert_eq!(read.bullets.len(), 3);
    assert!(read.bullets[1].ends_with("[Article #3441]."));
}

/// Models fence their json. The fence is layout, not content: it is unwrapped before the object
/// is looked for, with or without a language tag.
#[test]
fn a_fenced_answer_parses() {
    for fenced in [
        format!("```json\n{CLEAN}\n```"),
        format!("```\n{CLEAN}\n```"),
        format!("  ```json {CLEAN} ```  "),
    ] {
        let read = parse(&fenced).expect("a fenced briefing is still a briefing");
        assert_eq!(read.line, "Chip export rules tightened across the region this week.");
        assert_eq!(read.bullets.len(), 3);
    }
}

/// A bullet may hold a brace (a quoted json fragment from a story, a `{DEU}` marker). The scan
/// is string- and escape-aware, so the object ends where the *object* ends, not at the first
/// closing brace.
#[test]
fn a_brace_inside_a_bullet_does_not_end_the_object() {
    let answer = r#"{"line": "The registry closed the case.", "bullets": ["The filing names {\"where\": \"Berlin }\"} as the seat, and the case closed [Article #77].", "A second fact sits after the quoted braces [Article #78]."]}"#;
    let read = parse(answer).expect("the object survives its own content");
    assert_eq!(read.bullets.len(), 2, "both bullets were read: {read:?}");
    assert!(read.bullets[0].contains("Berlin"));
    assert!(read.bullets[1].ends_with("[Article #78]."));
}

/// Prose before the object is ignored — the instruction asks for none, and a model that explains
/// itself first still gets its briefing read. Prose *inside* the object is not repaired: a
/// missing `line` is no briefing, because the panel has nowhere to put the answer.
#[test]
fn prose_before_the_object_is_ignored_but_a_missing_line_is_none() {
    let with_preamble = format!("Here is the briefing you asked for:\n{CLEAN}");
    assert!(parse(&with_preamble).is_some());

    let no_line = r#"{"bullets": ["A fact [Article #1]."]}"#;
    assert_eq!(parse(no_line), None, "no line, no briefing");
    let empty_line = r#"{"line": "   ", "bullets": ["A fact [Article #1]."]}"#;
    assert_eq!(parse(empty_line), None, "a blank line is no line");
    let number_line = r#"{"line": 7, "bullets": ["A fact [Article #1]."]}"#;
    assert_eq!(parse(number_line), None, "a line is a sentence, not a number");
}

/// No bullets is no briefing either: the panel's body would be empty, and an empty panel reads
/// as a finished briefing that says nothing.
#[test]
fn no_bullets_is_none() {
    assert_eq!(parse(r#"{"line": "A quiet week."}"#), None);
    assert_eq!(parse(r#"{"line": "A quiet week.", "bullets": []}"#), None);
    assert_eq!(
        parse(r#"{"line": "A quiet week.", "bullets": ["  ", ""]}"#),
        None,
        "blank bullets are dropped, and nothing is left"
    );
    assert_eq!(
        parse(r#"{"line": "A quiet week.", "bullets": "one fact"}"#),
        None,
        "bullets are a list of strings"
    );
}

/// Nine bullets are cut to eight — the documented choice: the facts are true whether or not the
/// ninth fits the panel, and a refusal would throw away the line as well.
#[test]
fn nine_bullets_are_cut_to_eight() {
    let bullets: Vec<String> = (1..=9)
        .map(|n| format!("\"fact {n} [Article #{n}]\"", n = n))
        .collect();
    let answer = format!(
        "{{\"line\": \"Nine things happened.\", \"bullets\": [{}]}}",
        bullets.join(", ")
    );
    let read = parse(&answer).expect("a briefing");
    assert_eq!(read.bullets.len(), MAX_BULLETS);
    assert_eq!(read.bullets[0], "fact 1 [Article #1]");
    assert_eq!(
        read.bullets[MAX_BULLETS - 1],
        "fact 8 [Article #8]",
        "the *first* eight survive, in the model's order"
    );
}

/// The caps are characters, not bytes: a bullet of 300 characters is cut to 240 and still reads,
/// and an umlaut is one character rather than two.
#[test]
fn a_long_bullet_is_cut_to_240_and_the_line_to_400() {
    let long = "ä".repeat(300);
    let answer = format!(
        "{{\"line\": \"{}\", \"bullets\": [\"{}\"]}}",
        "a".repeat(500),
        long
    );
    let read = parse(&answer).expect("a briefing");
    assert_eq!(read.line.chars().count(), LINE_CHARS);
    assert_eq!(read.bullets[0].chars().count(), BULLET_CHARS);
}

/// A bullet is drawn on one line: newlines and tabs inside it are whitespace, not layout.
#[test]
fn a_multiline_bullet_becomes_one_line() {
    let answer =
        r#"{"line": "One fact,\n split.", "bullets": ["First half\tsecond half\n[Article #5]."]}"#;
    let read = parse(answer).expect("a briefing");
    assert_eq!(read.line, "One fact, split.");
    assert_eq!(read.bullets[0], "First half second half [Article #5].");
    assert!(!read.bullets[0].contains('\n'));
}

/// Braces that are not json are not a briefing: the parser takes the first balanced object and
/// refuses it rather than scanning on for something that happens to parse.
#[test]
fn braces_that_are_not_json_are_refused() {
    assert_eq!(parse("no object here at all"), None);
    assert_eq!(parse("{not json at all}"), None);
    assert_eq!(parse(r#"{"line": "unclosed", "bullets": ["a"]"#), None);
    assert_eq!(
        parse(r#"{"line": "stray", "bullets": ["a"]} {"line": "second", "bullets": ["b"]}"#)
            .map(|read| read.line),
        Some("stray".to_string()),
        "the first object is the answer; a later one is a stray, not a fallback"
    );
}

/// The bullet count the caller asks for is clamped: a briefing of one bullet or of twenty is not
/// a briefing.
#[test]
fn digest_instruction_clamps() {
    assert_eq!(digest_instruction(0), "Write 3 bullets.");
    assert_eq!(digest_instruction(9), "Write 6 bullets.");
    assert_eq!(digest_instruction(4), "Write 4 bullets.");
    assert_eq!(digest_instruction(3), "Write 3 bullets.");
    assert_eq!(digest_instruction(6), "Write 6 bullets.");
}

/// The instruction names the citation format exactly as the digest writes it, so the model copies
/// a shape it has seen rather than inventing one.
#[test]
fn the_prompt_names_the_citation_format() {
    assert!(BRIEF_PROMPT.contains("[Article #1234]"), "{BRIEF_PROMPT}");
    assert!(
        BRIEF_PROMPT.contains(r#""line""#) && BRIEF_PROMPT.contains(r#""bullets""#),
        "the object's keys are spelled out: {BRIEF_PROMPT}"
    );
    assert!(
        BRIEF_PROMPT.contains("one json object and nothing else"),
        "one object, and the model is told so: {BRIEF_PROMPT}"
    );
    assert!(
        BRIEF_PROMPT.contains("untrusted"),
        "the story list is named as data: {BRIEF_PROMPT}"
    );
    assert!(
        !BRIEF_PROMPT.contains('!'),
        "no exclamation marks: {BRIEF_PROMPT}"
    );
}

/// The design rule, measured the way the window's disclosure scan measures it: no word of two or
/// more letters is written entirely in capitals. A shout is decoration, and the two words this
/// prompt invites — the format and the citation marker — are written `json` and with a real
/// number instead.
#[test]
fn the_prompt_does_not_shout() {
    for text in [BRIEF_PROMPT, &digest_instruction(4)] {
        for word in text.split_whitespace() {
            let letters: Vec<char> = word.chars().filter(|ch| ch.is_alphabetic()).collect();
            if letters.len() < 2 {
                continue;
            }
            assert!(
                !letters.iter().all(|ch| ch.is_uppercase()),
                "a word shouts: {:?} in {:?}",
                word,
                text
            );
        }
    }
}

/// And the scan has teeth: a prompt that shouts is caught, so the test above cannot pass by
/// looking at nothing.
#[test]
fn the_shout_scan_finds_a_shout() {
    let shouted = format!("{BRIEF_PROMPT}\n\nANSWER WITH JSON ONLY.");
    let caught = shouted.split_whitespace().any(|word| {
        let letters: Vec<char> = word.chars().filter(|ch| ch.is_alphabetic()).collect();
        letters.len() >= 2 && letters.iter().all(|ch| ch.is_uppercase())
    });
    assert!(caught, "the scan must see a shout when there is one");
}
