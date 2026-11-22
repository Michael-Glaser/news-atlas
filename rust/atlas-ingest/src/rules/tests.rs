//! The rule engines' tests: the transport parity, the body caps, the reasons.
//!
//! Two kinds of test live here. The cheap ones (a missing engine is named, a bad
//! payload never starts a process) run on any machine. The expensive ones ask the
//! real engines and compare the two transports; they skip themselves when the
//! binaries are not built (`cargo build --release --offline`), so a checkout
//! without them still passes — but they are the reason a transport change is safe
//! to ship, so they run in every build that has the binaries.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::*;

// ---------------------------------------------------------------------------
// the cheap ones
// ---------------------------------------------------------------------------

#[test]
fn a_missing_engine_is_named_with_the_fix() {
    let error = RulesError::missing("atlas-text", std::path::Path::new("/nowhere/atlas-text"));
    assert!(error.message().contains("atlas-text"));
    assert!(error.message().contains("cargo build --release"));
}

#[test]
fn the_payload_ceiling_is_enforced_before_a_process_starts() {
    let rules = NativeRules::with_bin_dir(std::env::temp_dir(), Duration::from_millis(50));
    let huge = "x".repeat(answer::MAX_PAYLOAD + 1);
    let error = rules.clean_text(&huge).unwrap_err();
    // the binary is missing too, but the payload check comes first only when
    // the file exists; either way this is an Err, never a guess
    assert!(!error.message().is_empty());
}

#[test]
fn the_payload_ceiling_is_checked_on_the_in_process_path_too() {
    // The ceiling is part of the contract, not of the pipe: a question too large
    // for a process is refused for the library as well, so the two transports
    // cannot disagree about which questions are legal.
    let rules = NativeRules::with_bin_dir(std::env::temp_dir(), Duration::from_millis(50));
    let huge = ArticleText {
        title: "t".to_string(),
        content: "x".repeat(answer::MAX_PAYLOAD + 1),
        ..ArticleText::default()
    };
    let error = rules.locate(&huge).unwrap_err();
    assert!(
        error.message().contains("payload is"),
        "a placed question that is too big must be refused as too big: {}",
        error.message()
    );
}

#[test]
fn an_engine_refusal_keeps_the_engines_own_words() {
    // `atlas-dates window` refuses a negative window with a sentence that says
    // what is wrong. The caller must read that sentence: "could not answer"
    // would leave a model (or a reader) with nothing to change. This runs
    // in-process, so it needs no built binary.
    let rules = NativeRules::with_bin_dir(std::env::temp_dir(), Duration::from_millis(50));
    let error = rules
        .window("2026-11-05T10:00:00Z", -1, 1_793_872_800)
        .unwrap_err();
    assert!(
        error.message().contains("max_age_s"),
        "the engine's reason must survive: {}",
        error.message()
    );
    assert!(
        error.message().contains("atlas-dates window"),
        "the reason must name the engine and the op: {}",
        error.message()
    );
}

#[test]
fn the_error_envelope_is_the_one_a_binary_prints() {
    // `atlas_lib::cli::fail` prints exactly this on stdout and exits 1; the
    // in-process path builds the same bytes, which is what makes the parity
    // comparison of error messages meaningful.
    let refused = answer::Spoken::refused("unknown subcommand: nope");
    assert_eq!(refused.text, r#"{"error":"unknown subcommand: nope"}"#);
    assert!(!refused.ok);
    let error = answer::parse("atlas-places", "nope", &refused).unwrap_err();
    assert_eq!(
        error.message(),
        "atlas-places nope: unknown subcommand: nope"
    );
}

#[test]
fn an_answer_that_is_not_an_object_is_refused_as_such() {
    let rules = NativeRules::with_bin_dir(std::env::temp_dir(), Duration::from_millis(50));
    let spoken = answer::Spoken {
        text: "[1,2,3]".to_string(),
        ok: true,
    };
    let error = answer::parse("atlas-text", "publisher", &spoken).unwrap_err();
    assert!(error.message().contains("not a JSON object"), "{}", error.message());
    let empty = answer::Spoken {
        text: String::new(),
        ok: false,
    };
    let error = answer::parse("atlas-text", "publisher", &empty).unwrap_err();
    assert!(error.message().contains("could not answer"), "{}", error.message());
    let _ = rules;
}

#[test]
fn no_topics_is_no_question_at_all() {
    // A reader with no topics must not pay for a match they cannot have.
    let rules = NativeRules::with_bin_dir(std::env::temp_dir(), Duration::from_millis(50));
    let matched = rules
        .match_topics(&[], &ArticleText::default())
        .expect("no topics is an empty match, not a failure");
    assert!(matched.is_empty());
}

// ---------------------------------------------------------------------------
// the engines, for real
// ---------------------------------------------------------------------------

/// `rust/target/release`, the directory `Config::for_root` names in production.
fn engine_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("target")
        .join("release")
}

/// The article the parity tests ask about: a dateline the place engine can name,
/// a body long enough for the topic engine, and a date the date engine reads.
fn article() -> ArticleText {
    let mut body = String::from(
        "BERLIN (Outlets) - The council in Berlin published its quarterly figures on Tuesday. ",
    );
    while body.chars().count() < 2_000 {
        body.push_str("The budget proposal was approved after a debate about spending and trade. ");
    }
    ArticleText {
        title: "Council reviews the quarterly budget".to_string(),
        summary: "The council in Berlin approved the budget after a debate.".to_string(),
        content: body,
        url: "https://outlet1.example.invalid/story/1".to_string(),
        publisher: "Outlet 1".to_string(),
        entities: vec!["Berlin".to_string()],
        tags: vec!["budget".to_string()],
    }
}

fn topics() -> Vec<TopicRow> {
    vec![
        TopicRow {
            id: 1,
            name: "Budgets".to_string(),
            keywords: "budget, council, proposal".to_string(),
            exclude: String::new(),
            min_matches: 1,
            active: true,
        },
        TopicRow {
            id: 2,
            name: "Markets".to_string(),
            keywords: "market, trade, supply".to_string(),
            exclude: String::new(),
            min_matches: 1,
            active: true,
        },
        TopicRow {
            id: 3,
            name: "Excluding".to_string(),
            keywords: "budget".to_string(),
            exclude: "council".to_string(),
            min_matches: 1,
            active: true,
        },
    ]
}

/// The two transports for the same question, or `None` when the engines are not
/// built in this checkout.
fn both_ways() -> Option<(NativeRules, NativeRules)> {
    let dir = engine_dir();
    let in_process = NativeRules::with_bin_dir(&dir, Duration::from_secs(20));
    let spawned = NativeRules::spawn_only(&dir, Duration::from_secs(20));
    if !crate::rules::ENGINES
        .iter()
        .all(|engine| spawn::is_executable(&spawn::bin_path(&dir, engine)))
    {
        return None;
    }
    Some((in_process, spawned))
}

/// One HTML page `atlas-core extract` turns into a paragraph, and the text
/// `atlas-core entities` counts names in.
const PAGE: &str = r#"<html><head><title>Council reviews the budget</title>
<script>var tracker = 1;</script></head><body><nav>Home | World | Sport</nav>
<article><h1>Council reviews the quarterly budget</h1>
<p class="byline">By A. Reporter</p>
<p>BERLIN (Outlets) - The council in Berlin published its quarterly figures on
Tuesday, after a debate about spending that ran past midnight and into the
morning. The mayor said the budget proposal was approved with a clear majority,
and that the trade committee would review the spending plan again in March.</p>
<p>Berlin's finance senator said the trade figures were better than expected.</p>
</article><footer>(c) Outlet 1</footer></body></html>"#;

/// The text `atlas-core entities` is asked about: capitalised runs, a known name.
const NAMES: &str = "The council in Berlin published its figures on Tuesday. \
The mayor of Berlin said the budget proposal was approved. Angela Merkel met the \
mayor in Berlin, and the trade committee met again in March.";

/// What `atlas-text` is asked, op by op: the payloads a save sends.
fn text_questions() -> Vec<(&'static str, String)> {
    vec![
        (
            "clean_text",
            r#"{"text":"Council\u0000 reviews\tthe  budget \u200b"}"#.to_string(),
        ),
        (
            "clean_text",
            r#"{"text":"plain words, nothing to remove"}"#.to_string(),
        ),
        (
            "tokenize",
            r#"{"text":"Council reviews the quarterly budget","max_words":4}"#.to_string(),
        ),
        (
            "tokenize",
            r#"{"text":"the and of","max_words":4}"#.to_string(),
        ),
        (
            "clean_title",
            r#"{"title":"Council reviews budget | Outlet 1","publisher":"Outlet 1"}"#.to_string(),
        ),
        (
            "publisher",
            r#"{"url":"https://www.outlet1.example.invalid/story"}"#.to_string(),
        ),
        (
            "norm_url",
            r#"{"url":"https://Outlets.Example/Story/?utm_source=x&id=7#top"}"#.to_string(),
        ),
    ]
}

/// What `atlas-core` is asked: one page to extract, one text to count names in.
fn core_questions() -> Vec<(&'static str, String)> {
    let known = ["Berlin", "Merkel"];
    vec![
        (
            "extract",
            serde_json::json!({ "html": PAGE, "url": "https://outlet1.example.invalid/s", "title": "Council reviews the budget" })
                .to_string(),
        ),
        (
            "entities",
            serde_json::json!({ "text": NAMES, "known": known, "min_count": 1, "max": 12 })
                .to_string(),
        ),
    ]
}

#[test]
fn the_in_process_answer_is_the_binary_answer() {
    let Some((in_process, spawned)) = both_ways() else {
        return; // engines not built in this checkout
    };
    let article = article();
    let rows = topics();

    // locate: the same place, the same coordinates, the same confidence.
    let here = in_process.locate(&article).expect("in-process locate");
    let there = spawned.locate(&article).expect("spawned locate");
    assert!(here.named(), "the fixture must have a place: {:?}", here);
    assert_eq!(here, there, "atlas-places answered two different places");

    // match_topics: every match, its score and its terms, in the same order.
    let here = in_process.match_topics(&rows, &article).expect("in-process match");
    let there = spawned.match_topics(&rows, &article).expect("spawned match");
    assert!(
        here.iter().any(|m| m.name == "Budgets"),
        "the fixture must match a topic: {:?}",
        here
    );
    assert_eq!(here, there, "atlas-topics matched two different topic sets");
    assert!(
        !here.iter().any(|m| m.name == "Excluding"),
        "the exclude term must still suppress the topic: {:?}",
        here
    );

    // parse: a timestamp, its epoch, the reason when there is none.
    for stamp in [
        "2026-11-05T10:00:00Z",
        "2026-11-05",
        "definitely not a timestamp",
        "",
    ] {
        let here = in_process.parse_date(stamp, 1_793_872_800).expect("in-process parse");
        let there = spawned.parse_date(stamp, 1_793_872_800).expect("spawned parse");
        assert_eq!(here, there, "atlas-dates parsed {:?} two ways", stamp);
    }

    // window: fresh, stale and future-dated, with the same reason.
    let epoch = in_process
        .parse_date("2026-11-05T10:00:00Z", 1_793_872_800)
        .expect("a readable stamp")
        .epoch;
    for now in [epoch + 3_600, epoch + 80 * 3_600, epoch - 7_200] {
        let here = in_process.window("2026-11-05T10:00:00Z", 72 * 3_600, now).expect("in-process window");
        let there = spawned.window("2026-11-05T10:00:00Z", 72 * 3_600, now).expect("spawned window");
        assert_eq!(here, there, "atlas-dates judged {} two ways", now);
    }

    // atlas-text: the control characters out, the words, the headline, the outlet
    // and the canonical URL — the four fields and one token list a save stores.
    for text in [
        "Council\u{0} reviews\tthe  budget \u{200b}",
        "plain words, nothing to remove",
        "  \n\n  spaced  \n ",
    ] {
        assert_eq!(
            in_process.clean_text(text).expect("in-process clean_text"),
            spawned.clean_text(text).expect("spawned clean_text"),
            "atlas-text cleaned {:?} two ways",
            text
        );
    }
    for words in ["Council reviews the quarterly budget", "the and of", "TSMC & AI-chips"] {
        for max in [1usize, 4, 32] {
            assert_eq!(
                in_process.tokenize(words, max).expect("in-process tokenize"),
                spawned.tokenize(words, max).expect("spawned tokenize"),
                "atlas-text tokenised {:?} at {} two ways",
                words,
                max
            );
        }
    }
    assert_eq!(
        in_process
            .clean_title("Council reviews budget | Outlet 1", "Outlet 1")
            .expect("in-process clean_title"),
        spawned
            .clean_title("Council reviews budget | Outlet 1", "Outlet 1")
            .expect("spawned clean_title")
    );
    assert_eq!(
        in_process.publisher("https://www.outlet1.example.invalid/story").expect("in-process publisher"),
        spawned.publisher("https://www.outlet1.example.invalid/story").expect("spawned publisher")
    );
    assert_eq!(
        in_process
            .norm_url("https://Outlets.Example/Story/?utm_source=x&id=7#top")
            .expect("in-process norm_url"),
        spawned
            .norm_url("https://Outlets.Example/Story/?utm_source=x&id=7#top")
            .expect("spawned norm_url")
    );

    // atlas-core: the article text out of a page, the names out of a text.
    let here = in_process
        .extract_html(PAGE, "https://outlet1.example.invalid/s", "Council reviews the budget")
        .expect("in-process extract");
    let there = spawned
        .extract_html(PAGE, "https://outlet1.example.invalid/s", "Council reviews the budget")
        .expect("spawned extract");
    assert!(
        here.contains("Berlin published its quarterly figures"),
        "the fixture must extract the article body: {:?}",
        here
    );
    assert!(!here.contains("tracker"), "the script must not survive: {:?}", here);
    assert_eq!(here, there, "atlas-core extracted two different texts");

    let known = ["Berlin".to_string(), "Merkel".to_string()];
    let here = in_process
        .entity_candidates(NAMES, &known, 1, 12)
        .expect("in-process entities");
    let there = spawned
        .entity_candidates(NAMES, &known, 1, 12)
        .expect("spawned entities");
    assert!(
        here.iter().any(|(name, _)| name.contains("Berlin")),
        "the fixture must name a place: {:?}",
        here
    );
    assert_eq!(here, there, "atlas-core counted two different candidate lists");
}

/// The strongest parity there is: the same bytes on the wire from both transports,
/// for an answer *and* for a refusal.
///
/// The typed comparison above proves the callers agree on the fields they read.
/// This one proves the envelope — the `{"…": …}` text one engine builds and
/// another function has to reproduce when the process is gone — is identical, so a
/// caller that ever reads a field nobody reads today cannot be the first to notice
/// a difference. `trim` removes the one thing a process has and a function does
/// not: the newline `println!` adds.
#[test]
fn the_text_and_core_envelopes_are_the_binaries() {
    // The binary has to exist to be asked, and only its presence is used here.
    if both_ways().is_none() {
        return;
    }
    let dir = engine_dir();
    let mut compared = 0usize;
    let mut refusals = 0usize;

    let questions = text_questions()
        .into_iter()
        .map(|(op, raw)| ("atlas-text", op, raw))
        .chain(
            core_questions()
                .into_iter()
                .map(|(op, raw)| ("atlas-core", op, raw)),
        );

    for (engine, op, raw) in questions {
        let here = inproc::ask(engine, op, &raw).expect("the library answers");
        let there = spawn::ask(
            &spawn::bin_path(&dir, engine),
            engine,
            op,
            &raw,
            Duration::from_secs(20),
        )
        .expect("the binary answers");
        assert!(here.ok, "{} {} refused in-process: {}", engine, op, here.text);
        assert_eq!(
            here.text,
            there.text.trim(),
            "{} {} answered two different objects in-process and as a process",
            engine,
            op
        );
        compared += 1;
    }

    // …and a refusal, which is an answer too: the engine's own words, in the same
    // `{"error": …}` envelope, with the same status. The last two carry the bytes
    // two escapers could disagree on — a quote, a backslash, a non-ASCII letter and
    // a C0 control (a NUL cannot travel in an argv, so a control above it does).
    for (engine, op, raw) in [
        ("atlas-text", "no_such_op", "{}"),
        ("atlas-text", "", "{}"),
        ("atlas-text", "op with \"quotes\" \\ and é", "{}"),
        ("atlas-text", "op\u{1}with\u{1}controls", "{}"),
        ("atlas-core", "no_such_op", "{}"),
        ("atlas-core", "extract", "not json at all"),
    ] {
        let here = inproc::ask(engine, op, raw).expect("the library refuses, it does not fail");
        let there = spawn::ask(
            &spawn::bin_path(&dir, engine),
            engine,
            op,
            raw,
            Duration::from_secs(20),
        )
        .expect("the binary refuses, it does not fail");
        assert!(!here.ok, "{} {} must be a refusal", engine, op);
        assert!(!there.ok, "{} {} must be a refusal", engine, op);
        assert!(
            here.text.starts_with("{\"error\":\""),
            "{} {} must refuse in the envelope the binary prints: {}",
            engine,
            op,
            here.text
        );
        assert_eq!(
            here.text,
            there.text.trim(),
            "{} {} refused with two different texts",
            engine,
            op
        );
        compared += 1;
        refusals += 1;
    }
    println!(
        "rules envelope parity: {} answers ({} refusals) identical byte for byte, in-process and as processes",
        compared, refusals
    );
    assert!(compared >= 13, "the parity set shrank to {} questions", compared);
}

#[test]
fn the_two_transports_refuse_the_same_questions_the_same_way() {
    // An error is an answer too: the engine's words must not depend on who asked.
    let Some((in_process, spawned)) = both_ways() else {
        return;
    };
    // A negative window is a refusal both transports can be made to produce from
    // the outside: the request is well-formed, the value is not.
    let here = in_process
        .window("2026-11-05T10:00:00Z", -1, 1_793_872_800)
        .expect_err("a negative window must be refused");
    let there = spawned
        .window("2026-11-05T10:00:00Z", -1, 1_793_872_800)
        .expect_err("a negative window must be refused");
    assert_eq!(here, there, "the two transports phrased one refusal differently");
    assert!(here.message().contains("max_age_s"), "{}", here.message());
}

#[test]
fn the_body_caps_are_the_engines_own() {
    let Some((rules, spawned)) = both_ways() else {
        return;
    };
    let rows = vec![TopicRow {
        id: 1,
        name: "Late".to_string(),
        keywords: "zebraphrase".to_string(),
        exclude: String::new(),
        min_matches: 1,
        active: true,
    }];

    // The keyword sits at character 17 000 — past the engine's own 16 000-char
    // fold, so it cannot match however much of the body is sent.
    let mut late = "filler words about nothing in particular. ".repeat(400);
    late.push_str("zebraphrase ");
    let article = ArticleText {
        title: "A headline".to_string(),
        summary: "A summary".to_string(),
        content: late.clone(),
        ..ArticleText::default()
    };
    assert!(late.chars().count() > TOPIC_BODY_CHARS);
    for engine_rules in [&rules, &spawned] {
        let matched = engine_rules.match_topics(&rows, &article).expect("a match answer");
        assert!(
            matched.is_empty(),
            "a keyword past the fold cannot match, in-process or not: {:?}",
            matched
        );
    }
    // …and cutting the body at the cap changes nothing, which is what makes the
    // cap safe to apply before sending.
    let cut = ArticleText {
        content: truncate(&late, TOPIC_BODY_CHARS),
        ..article.clone()
    };
    assert_eq!(
        rules.match_topics(&rows, &article).unwrap(),
        rules.match_topics(&rows, &cut).unwrap()
    );
    let request = topics_request(&rows, &article);
    let sent = request["article"]["content"].as_str().unwrap_or("");
    assert_eq!(
        sent.chars().count(),
        TOPIC_BODY_CHARS,
        "the payload carries no more than the engine folds"
    );

    // The engine folds 16 000 characters but scans only the first 8 000 of the
    // folded text, so a keyword at 10 000 cannot match — the cap in `rules` is
    // not what loses it. This is the engine's own limit, measured:
    let mut deep = "filler words about nothing in particular. ".repeat(240);
    deep.push_str("zebraphrase ");
    assert!(deep.chars().count() > 8_000 && deep.chars().count() < TOPIC_BODY_CHARS);
    assert!(
        rules.match_topics(&rows, &ArticleText { title: "A headline".to_string(), summary: "A summary".to_string(), content: deep, ..ArticleText::default() }).unwrap().is_empty(),
        "past the 8 000-character scan window nothing matches, which is the engine's scan and not our cap"
    );

    // …and the cap keeps what the engine does read: a keyword at 6 000 matches,
    // so sending 16 000 characters is not what makes a match silent.
    let mut early = "filler words about nothing in particular. ".repeat(140);
    early.push_str("zebraphrase ");
    let matched = rules
        .match_topics(
            &rows,
            &ArticleText {
                title: "A headline".to_string(),
                summary: "A summary".to_string(),
                content: early,
                ..ArticleText::default()
            },
        )
        .expect("a match answer");
    assert_eq!(matched.len(), 1, "a keyword at 6 000 characters must match: {:?}", matched);

    // The place engine scans 20 000 characters. A city first named at 12 000 was
    // invisible to `save`, which sent the first 3 000 characters only. Two
    // mentions, because one passing mention is deliberately not enough for the
    // engine to name a place (`MIN_BODY_MENTIONS`):
    let mut body = "The council met and discussed the budget. ".repeat(280);
    body.push_str("The meeting moved to Lisbon for the second session. ");
    body.push_str("Lisbon will host the next round in March. ");
    let first_three_thousand = ArticleText {
        content: truncate(&body, 3_000),
        ..ArticleText::default()
    };
    let whole_body = ArticleText {
        content: truncate(&body, PLACE_BODY_CHARS),
        ..ArticleText::default()
    };
    let short = rules.locate(&first_three_thousand).expect("a locate answer");
    let full = rules.locate(&whole_body).expect("a locate answer");
    assert!(
        !short.named(),
        "the first 3 000 characters name no place, which is why sending them lost it: {:?}",
        short
    );
    assert_eq!(
        full.place, "Lisbon",
        "a city named at 12 000 characters must be found: {:?}",
        full
    );
    assert_eq!(
        full,
        spawned.locate(&whole_body).expect("a locate answer"),
        "the place engine answers one city two ways"
    );
}

#[test]
fn in_process_answers_are_cheaper_than_processes() {
    let Some((in_process, spawned)) = both_ways() else {
        return;
    };
    let article = article();
    let stamps = [
        "2026-11-05T10:00:00Z",
        "2026-11-05",
        "2026-11-05 10:00",
        "5 November 2026",
        "not a timestamp",
    ];
    let rounds = 5;

    // The process path: one spawn per question.
    let start = Instant::now();
    for _ in 0..rounds {
        for stamp in stamps {
            let _ = spawned.parse_date(stamp, 1_793_872_800).expect("a parse answer");
        }
        let _ = spawned
            .window("2026-11-05T10:00:00Z", 72 * 3_600, 1_793_872_800)
            .expect("a window answer");
        let _ = spawned.locate(&article).expect("a locate answer");
    }
    let spawned_total = start.elapsed();

    // The same questions, asked in this process.
    let start = Instant::now();
    for _ in 0..rounds {
        for stamp in stamps {
            let _ = in_process.parse_date(stamp, 1_793_872_800).expect("a parse answer");
        }
        let _ = in_process
            .window("2026-11-05T10:00:00Z", 72 * 3_600, 1_793_872_800)
            .expect("a window answer");
        let _ = in_process.locate(&article).expect("a locate answer");
    }
    let in_process_total = start.elapsed();

    let questions = rounds * (stamps.len() + 2);
    println!(
        "rules transport: {} questions — spawned {:?} ({:.3} ms each), in-process {:?} ({:.3} ms each)",
        questions,
        spawned_total,
        spawned_total.as_secs_f64() * 1_000.0 / questions as f64,
        in_process_total,
        in_process_total.as_secs_f64() * 1_000.0 / questions as f64,
    );
    assert!(
        in_process_total < spawned_total,
        "asking the library must cost less than starting a process: {:?} vs {:?}",
        in_process_total,
        spawned_total
    );
}

/// One save, as a list of questions: the ops (and how often) a stored article
/// asks its engines.
///
/// `tests/engines.rs` measures the real mix on a real `save_article`; this is the
/// same shape, written where the transport is decided, so a question that stops
/// being answerable without a process fails next to the code that moved it rather
/// than in a timing test.
fn save_profile() -> Vec<(&'static str, &'static str, Value)> {
    let mut questions: Vec<(&'static str, &'static str, Value)> = Vec::new();
    // 10 fields: title, summary, publisher, categories, tags, the note, …
    for field in 0..10 {
        questions.push((
            "atlas-text",
            "clean_text",
            json!({ "text": format!("field {}  of a stored article ", field) }),
        ));
    }
    // 3 tags, each rebuilt from tokens.
    for tag in ["chip export", "semiconductors", "trade policy"] {
        questions.push((
            "atlas-text",
            "tokenize",
            json!({ "text": tag, "max_words": 4 }),
        ));
    }
    // 2 name lists: the candidates the graph links, and the ones the model sees.
    let known = ["Berlin".to_string()];
    for _ in 0..2 {
        questions.push((
            "atlas-core",
            "entities",
            json!({ "text": NAMES, "known": known, "min_count": 1, "max": 12 }),
        ));
    }
    questions.push(("atlas-places", "locate", locate_request(&article())));
    questions.push((
        "atlas-topics",
        "match",
        topics_request(&topics(), &article()),
    ));
    questions.push((
        "atlas-dates",
        "window",
        window_request("2026-11-05T10:00:00Z", 72 * 3_600, 1_793_872_800),
    ));
    questions
}

#[test]
fn a_save_shaped_profile_answers_with_no_engine_binary_at_all() {
    // The proof that the count of processes is zero, without timing anything: the
    // same 18 questions through rules wired to a directory that holds no engine
    // binary. A question that still needed a child would fail with
    // "… is not built (/nowhere/atlas-bin/…)" — succeeding is the measurement.
    let questions = save_profile();
    assert_eq!(questions.len(), 18, "one save asks 18 engine questions");
    let rules = NativeRules::with_bin_dir("/nowhere/atlas-bin", Duration::from_millis(50));
    let mut asked: Vec<(&str, usize)> = Vec::new();
    for (engine, op, request) in &questions {
        assert!(
            inproc::answers_here(engine),
            "{} must answer in this process",
            engine
        );
        assert!(!SPAWNED.contains(engine), "{} must not need a file", engine);
        rules
            .ask(engine, op, request)
            .unwrap_or_else(|error| panic!("{} {} needs a process: {}", engine, op, error.message()));
        match asked.iter_mut().find(|(name, _)| name == engine) {
            Some((_, count)) => *count += 1,
            None => asked.push((engine, 1)),
        }
    }
    let per_engine: Vec<String> = asked
        .iter()
        .map(|(engine, count)| format!("{} {}", engine, count))
        .collect();
    println!(
        "save profile: {} engine questions, 0 child processes ({}); asked with an empty bin dir",
        questions.len(),
        per_engine.join(", ")
    );
    // The two engines that were children are the majority of the questions, which
    // is why they were the ones worth moving.
    let moved: usize = asked
        .iter()
        .filter(|(engine, _)| *engine == "atlas-text" || *engine == "atlas-core")
        .map(|(_, count)| count)
        .sum();
    println!(
        "  {} of {} questions were child processes before this change ({} text, {} core)",
        moved,
        questions.len(),
        asked
            .iter()
            .find(|(engine, _)| *engine == "atlas-text")
            .map(|(_, count)| *count)
            .unwrap_or(0),
        asked
            .iter()
            .find(|(engine, _)| *engine == "atlas-core")
            .map(|(_, count)| *count)
            .unwrap_or(0),
    );
    assert_eq!(moved, 15, "15 of the 18 questions were subprocesses");
}

#[test]
fn one_saves_questions_cost_no_process_so_the_difference_is_the_spawn() {
    let Some((in_process, spawned)) = both_ways() else {
        return; // engines not built in this checkout
    };
    let questions = save_profile();
    let rounds = 5;
    let total = questions.len() * rounds;

    // The same 18 questions, asked the way the app asks them now…
    let start = Instant::now();
    for _ in 0..rounds {
        for (engine, op, request) in &questions {
            let _ = in_process
                .ask(engine, op, request)
                .unwrap_or_else(|error| panic!("{} {}: {}", engine, op, error.message()));
        }
    }
    let here = start.elapsed();

    // …and the way they were asked before: one child process each.
    let start = Instant::now();
    for _ in 0..rounds {
        for (engine, op, request) in &questions {
            let _ = spawned
                .ask(engine, op, request)
                .unwrap_or_else(|error| panic!("{} {}: {}", engine, op, error.message()));
        }
    }
    let there = start.elapsed();

    println!(
        "save profile cost: {} questions — in-process {:?} ({:.3} ms each), one process each {:?} ({:.3} ms each)",
        total,
        here,
        here.as_secs_f64() * 1_000.0 / total as f64,
        there,
        there.as_secs_f64() * 1_000.0 / total as f64,
    );
    println!(
        "  one save of 18 questions: ~{:.1} ms in-process vs ~{:.1} ms as processes",
        here.as_secs_f64() * 1_000.0 / rounds as f64,
        there.as_secs_f64() * 1_000.0 / rounds as f64
    );
    assert!(
        here < there,
        "asking the libraries must cost less than starting 18 processes: {:?} vs {:?}",
        here,
        there
    );
}
