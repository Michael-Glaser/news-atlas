//! The cross-module cases: every check of `tools/topics_check.py`, plus the wire
//! and error paths.
//!
//! These drive `run()` from one end to the other — payload in, JSON out — so they
//! live at the crate root rather than inside one module. The unit tests of the
//! folding and of the keyword matcher sit next to the code they test
//! (`fold::tests`, `terms::tests`).

use atlas_lib::json::{self, Json};

use crate::run;


// -- helpers ------------------------------------------------------------

pub(super) fn out_of(cmd: &str, payload: &str) -> Json {
    let raw = run(cmd, payload).unwrap_or_else(|e| panic!("{} failed: {}", cmd, e));
    json::parse(&raw).unwrap_or_else(|e| panic!("{} emitted invalid json ({}): {}", cmd, e, raw))
}

pub(super) fn err_of(cmd: &str, payload: &str) -> String {
    match run(cmd, payload) {
        Ok(text) => panic!("{} should have failed, got {}", cmd, text),
        Err(message) => message,
    }
}

/// `{"id":…,"name":…,"keywords":…,"min_matches":…}`
pub(super) fn topic(id: i64, name: &str, keywords: &str, min: usize) -> String {
    format!(
        "{{\"id\":{},\"name\":{},\"keywords\":{},\"min_matches\":{}}}",
        id,
        json::to_string(&Json::Str(name.to_string())),
        json::to_string(&Json::Str(keywords.to_string())),
        min
    )
}

pub(super) fn match_payload(topics: &[String], title: &str, summary: &str, content: &str) -> String {
    format!(
        "{{\"topics\":[{}],\"article\":{{\"title\":{},\"summary\":{},\"content\":{}}}}}",
        topics.join(","),
        json::to_string(&Json::Str(title.to_string())),
        json::to_string(&Json::Str(summary.to_string())),
        json::to_string(&Json::Str(content.to_string()))
    )
}

pub(super) fn array_of<'a>(value: &'a Json, key: &str) -> Vec<Json> {
    value
        .get(key)
        .and_then(Json::as_arr)
        .cloned()
        .unwrap_or_default()
}

pub(super) fn ids(result: &Json) -> Vec<i64> {
    array_of(result, "topic_ids")
        .iter()
        .filter_map(Json::as_f64)
        .map(|n| n as i64)
        .collect()
}

pub(super) fn first_match(result: &Json) -> Json {
    array_of(result, "matches")
        .into_iter()
        .next()
        .expect("expected at least one match")
}

pub(super) fn strings(value: Option<&Json>) -> Vec<String> {
    value
        .and_then(Json::as_arr)
        .map(|items| {
            items
                .iter()
                .filter_map(Json::as_str)
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn score_of(value: &Json, key: &str) -> Option<f64> {
    value.get(key).and_then(Json::as_f64)
}

pub(super) fn flag_of(value: &Json, key: &str) -> Option<bool> {
    match value.get(key) {
        Some(Json::Bool(b)) => Some(*b),
        _ => None,
    }
}

fn does_match(keywords: &str, min: usize, title: &str) -> bool {
    let payload = match_payload(&[topic(1, "T", keywords, min)], title, "", "");
    !ids(&out_of("match", &payload)).is_empty()
}


// -- match: the scoring branches ---------------------------------------

#[test]
fn exact_term_in_the_title_scores_title_weight() {
    let payload = match_payload(&[topic(1, "Nvidia watch", "nvidia", 1)], "Nvidia ships", "", "");
    let result = out_of("match", &payload);
    assert_eq!(ids(&result), vec![1]);
    assert_eq!(flag_of(&result, "relevant"), Some(true));
    assert_eq!(score_of(&result, "count"), Some(1.0));
    let hit = first_match(&result);
    assert_eq!(score_of(&hit, "score"), Some(3.25));
    assert_eq!(strings(hit.get("matched")), vec!["nvidia (title)"]);
    assert_eq!(strings(hit.get("terms")), vec!["nvidia"]);
    assert_eq!(
        hit.get("reason").and_then(Json::as_str),
        Some("nvidia (title)")
    );
    assert_eq!(score_of(&hit, "terms_hit"), Some(1.0));
    assert_eq!(score_of(&hit, "min_matches"), Some(1.0));
    assert_eq!(hit.get("name").and_then(Json::as_str), Some("Nvidia watch"));
}

#[test]
fn partial_token_hits_the_summary_and_the_body() {
    let summary = match_payload(
        &[topic(4, "Oil", "refinery", 1)],
        "Oil slips",
        "A refinery outage",
        "The plant restarted.",
    );
    let hit = first_match(&out_of("match", &summary));
    assert_eq!(score_of(&hit, "score"), Some(1.75)); // 1.5 + 0.25
    assert_eq!(strings(hit.get("matched")), vec!["refinery (summary)"]);

    let body = match_payload(
        &[topic(4, "Oil", "refinery", 1)],
        "Oil slips",
        "",
        "The refinery restarted.",
    );
    let hit = first_match(&out_of("match", &body));
    assert_eq!(score_of(&hit, "score"), Some(0.75)); // 0.5 + 0.25
    assert_eq!(strings(hit.get("matched")), vec!["refinery (content)"]);
}

#[test]
fn the_topics_check_reason_string_is_reproduced() {
    let payload = match_payload(
        &[topic(7, "Oil watch", "oil price, opec, refinery", 2)],
        "Oil price jumps",
        "OPEC meets in Vienna",
        "A refinery outage tightened supply.",
    );
    let hit = first_match(&out_of("match", &payload));
    assert_eq!(score_of(&hit, "score"), Some(5.75)); // 3 + 1.5 + 0.5 + 0.75
    assert_eq!(
        strings(hit.get("matched")),
        vec![
            "oil price (title)",
            "opec (summary)",
            "refinery (content)"
        ]
    );
    assert_eq!(
        hit.get("reason").and_then(Json::as_str),
        Some("oil price (title), opec (summary), refinery (content)")
    );
}

#[test]
fn a_keyword_counts_once_at_its_best_field() {
    let payload = match_payload(
        &[topic(1, "T", "nvidia", 1)],
        "Nvidia beats estimates",
        "Nvidia again",
        "Nvidia nvidia nvidia",
    );
    let hit = first_match(&out_of("match", &payload));
    assert_eq!(score_of(&hit, "score"), Some(3.25));
    assert_eq!(strings(hit.get("matched")), vec!["nvidia (title)"]);
}

#[test]
fn the_threshold_decides_like_python() {
    let payload = match_payload(
        &[topic(7, "Oil watch", "oil price, opec, refinery", 2)],
        "Oil price slips",
        "",
        "",
    );
    let result = out_of("match", &payload);
    assert!(ids(&result).is_empty());
    assert_eq!(flag_of(&result, "relevant"), Some(false));
    let rejected = array_of(&result, "rejected")
        .into_iter()
        .next()
        .expect("rejected entry");
    assert_eq!(
        rejected.get("reason_code").and_then(Json::as_str),
        Some("below_threshold")
    );
    assert_eq!(score_of(&rejected, "terms_hit"), Some(1.0));
    assert_eq!(score_of(&rejected, "min_matches"), Some(2.0));
    assert_eq!(score_of(&rejected, "score"), Some(3.25));
    assert_eq!(strings(rejected.get("matched")), vec!["oil price (title)"]);
}

#[test]
fn one_keyword_family_counts_once() {
    // "oil" and "oils" are one family: a threshold of 2 must not be met.
    let payload = match_payload(&[topic(9, "Dup", "oil, oils", 2)], "Oil slips", "", "");
    assert!(ids(&out_of("match", &payload)).is_empty());
}

#[test]
fn an_empty_topic_never_matches_anything() {
    let payload = match_payload(&[topic(3, "Nothing", "", 1)], "War on drugs", "", "");
    let result = out_of("match", &payload);
    assert!(ids(&result).is_empty());
    let rejected = array_of(&result, "rejected")
        .into_iter()
        .next()
        .expect("rejected entry");
    assert_eq!(
        rejected.get("reason_code").and_then(Json::as_str),
        Some("below_threshold")
    );
    assert_eq!(score_of(&rejected, "terms_hit"), Some(0.0));
}

#[test]
fn the_topic_name_is_identity_not_evidence() {
    // Python matches keywords only: the display name must not create a hit.
    let payload = match_payload(
        &[topic(2, "Semiconductors", "wafer", 1)],
        "Semiconductors rally on demand",
        "",
        "",
    );
    assert!(ids(&out_of("match", &payload)).is_empty());
}

#[test]
fn an_entity_only_hit_is_evidence_but_never_a_match() {
    // The article never says the keyword; only the extracted entity does.
    // Python decides on text alone, so the topic stays out — the entity is
    // reported as evidence so the UI can explain the near miss.
    let payload = format!(
        "{{\"topics\":[{}],\"article\":{{\"title\":\"Chipmaker beats estimates\",\
             \"entities\":[\"Nvidia\"],\"tags\":[\"semiconductors\"],\"publisher\":\"Reuters\"}}}}",
        topic(1, "Nvidia watch", "nvidia", 1)
    );
    let result = out_of("match", &payload);
    assert!(ids(&result).is_empty());
    assert_eq!(score_of(&result, "count"), Some(0.0));
    assert_eq!(flag_of(&result, "relevant"), Some(false));
    let entity_hits = array_of(&result, "entity_hits");
    assert_eq!(entity_hits.len(), 1);
    assert_eq!(
        entity_hits[0].get("term").and_then(Json::as_str),
        Some("nvidia")
    );
    assert_eq!(
        entity_hits[0].get("entity").and_then(Json::as_str),
        Some("Nvidia")
    );
    assert!(array_of(&result, "tag_hits").is_empty());
    // No score anywhere: evidence cannot lift an article over the line.
    let rejected = array_of(&result, "rejected").into_iter().next().unwrap();
    assert_eq!(score_of(&rejected, "score"), Some(0.0));
    assert_eq!(score_of(&rejected, "terms_hit"), Some(0.0));
}

#[test]
fn entities_are_reported_as_evidence_for_a_topic_term() {
    let payload = format!(
        "{{\"topics\":[{}],\"article\":{{\"title\":\"Nvidia beats estimates\",\
             \"entities\":[\"Nvidia\",{{\"name\":\"TSMC\"}}],\"tags\":[\"nvidia results\"]}}}}",
        topic(1, "Nvidia watch", "nvidia", 1)
    );
    let result = out_of("match", &payload);
    assert_eq!(ids(&result), vec![1]);
    let entity_hits = array_of(&result, "entity_hits");
    assert_eq!(entity_hits.len(), 1);
    assert_eq!(
        entity_hits[0].get("term").and_then(Json::as_str),
        Some("nvidia")
    );
    assert_eq!(
        entity_hits[0].get("entity").and_then(Json::as_str),
        Some("Nvidia")
    );
    assert_eq!(score_of(&entity_hits[0], "topic"), Some(1.0));
    let tag_hits = array_of(&result, "tag_hits");
    assert_eq!(tag_hits.len(), 1);
    assert_eq!(
        tag_hits[0].get("tag").and_then(Json::as_str),
        Some("nvidia results")
    );
    // Evidence never touched the score.
    assert_eq!(score_of(&first_match(&result), "score"), Some(3.25));
}

// -- exclude ------------------------------------------------------------

#[test]
fn exclude_terms_block_a_topic() {
    let blocked = format!(
        "{{\"topics\":[{{\"id\":1,\"name\":\"Labour\",\"keywords\":\"labour strike, walkout\",\
             \"exclude\":\"military strike\",\"min_matches\":1}}],\
             \"article\":{{\"title\":\"Military strike hits the port\"}}}}"
    );
    let result = out_of("match", &blocked);
    assert!(ids(&result).is_empty());
    let rejected = array_of(&result, "rejected")
        .into_iter()
        .next()
        .expect("rejected entry");
    assert_eq!(
        rejected.get("reason_code").and_then(Json::as_str),
        Some("excluded")
    );

    let allowed = match_payload(
        &["{\"id\":1,\"name\":\"Labour\",\"keywords\":\"labour strike, walkout\",\
               \"exclude\":\"military strike\"}"
            .to_string()],
        "Labour strike enters a second week",
        "",
        "",
    );
    assert_eq!(ids(&out_of("match", &allowed)), vec![1]);
}

#[test]
fn exclude_spans_two_fields_like_the_python_blob() {
    // The exclude terms are searched in "title\nsummary\ncontent", so a
    // two-word phrase can straddle the join. Faithful to topics.py.
    let payload = match_payload(
        &["{\"id\":1,\"name\":\"T\",\"keywords\":\"war\",\"exclude\":\"war peace\"}".to_string()],
        "war",
        "peace talks resume",
        "",
    );
    assert!(ids(&out_of("match", &payload)).is_empty());
}

// -- ordering, determinism, no false positives -------------------------

#[test]
fn matches_are_ordered_by_score_and_stable_on_ties() {
    let payload = match_payload(
        &[
            topic(1, "A", "nvidia", 1),
            topic(2, "B", "nvidia, tsmc", 1),
            topic(3, "C", "tsmc", 1),
        ],
        "Nvidia and TSMC expand capacity",
        "",
        "",
    );
    let result = out_of("match", &payload);
    // B: 3 + 3 + 0.5 = 6.5; A: 3.25; C: 3.25 (stable, arrival order kept).
    assert_eq!(ids(&result), vec![2, 1, 3]);
    let scores: Vec<f64> = array_of(&result, "matches")
        .iter()
        .filter_map(|m| score_of(m, "score"))
        .collect();
    assert_eq!(scores, vec![6.5, 3.25, 3.25]);
}

#[test]
fn the_same_payload_twice_gives_the_same_bytes() {
    let payload = match_payload(
        &[
            topic(2, "Oil", "oil price, opec", 1),
            topic(1, "Chips", "semiconductor, wafer", 1),
            topic(3, "War", "ceasefire, troops", 2),
        ],
        "Oil price climbs as OPEC meets",
        "Semiconductor demand",
        "Wafer fabs run flat out.",
    );
    assert_eq!(
        run("match", &payload).expect("first"),
        run("match", &payload).expect("second")
    );
    let single = format!(
        "{{\"topic\":{},\"article\":{{\"title\":\"Oil price\"}}}}",
        topic(2, "Oil", "oil price", 1)
    );
    assert_eq!(
        run("score", &single).expect("first"),
        run("score", &single).expect("second")
    );
}

#[test]
fn no_false_positive_on_an_unrelated_article() {
    // The delivered keyword sets from BUILTIN_SUGGESTIONS, against an
    // article about something else entirely.
    let topics = vec![
        topic(
            1,
            "Semiconductors",
            "semiconductor, chip export, TSMC, chipmaker, wafer fab, Nvidia",
            1,
        ),
        topic(
            2,
            "Climate",
            "climate summit, emissions, heatwave, wildfire, carbon",
            1,
        ),
        topic(
            3,
            "Wars & conflict",
            "ceasefire, airstrike, offensive, troops, peace talks",
            1,
        ),
        topic(
            4,
            "Labour disputes",
            "labour strike, walkout, union talks, pay dispute",
            1,
        ),
    ];
    let payload = match_payload(
        &topics,
        "Bakery reopens after renovation",
        "The family bakery on the corner is baking bread again.",
        "Flour, butter and patience. The ovens were relit on Tuesday morning \
             and the first loaves sold out within an hour.",
    );
    let result = out_of("match", &payload);
    assert!(ids(&result).is_empty(), "unexpected match: {:?}", ids(&result));
    assert_eq!(array_of(&result, "rejected").len(), 4);
    assert_eq!(score_of(&result, "count"), Some(0.0));
}

// -- article size and truncation ---------------------------------------

#[test]
fn a_20_kb_body_is_scanned_in_one_pass() {
    let filler = "The committee met again to discuss the budget and the timetable. ";
    let mut body = String::with_capacity(20 * 1024);
    body.push_str("A refinery outage tightened supply. ");
    while body.len() < 20_000 {
        body.push_str(filler);
    }
    let payload = match_payload(&[topic(1, "Oil", "refinery", 1)], "Markets", "", &body);
    assert!(body.len() >= 20_000);
    let started = std::time::Instant::now();
    let result = out_of("match", &payload);
    let elapsed = started.elapsed();
    assert_eq!(ids(&result), vec![1]);
    assert_eq!(score_of(&first_match(&result), "score"), Some(0.75));
    // The budget is 5 ms for a 20 kB article in a release build; the test
    // harness build is much slower, so allow it more room.
    println!("20 kB article decided in {:?}", elapsed);
    let budget = if cfg!(debug_assertions) { 250 } else { 20 };
    assert!(
        elapsed.as_millis() < budget,
        "20 kB article took {:?}",
        elapsed
    );
}

#[test]
fn body_text_beyond_the_scan_window_cannot_match() {
    let mut early = String::from("refinery ");
    while early.len() < 16_000 {
        early.push('x');
    }
    let mut late = String::new();
    while late.len() < 16_400 {
        late.push('x');
    }
    late.push_str(" refinery");
    let inside = match_payload(&[topic(1, "Oil", "refinery", 1)], "Markets", "", &early);
    assert_eq!(ids(&out_of("match", &inside)), vec![1]);
    // Past the 16 000-character fold window the body cannot match anymore.
    let outside = match_payload(&[topic(1, "Oil", "refinery", 1)], "Markets", "", &late);
    assert!(ids(&out_of("match", &outside)).is_empty());
}

// -- rank ---------------------------------------------------------------

#[test]
fn rank_orders_by_score_and_keeps_arrivals_on_ties() {
    let payload = format!(
        "{{\"topic\":{},\"articles\":[\
             {{\"id\":11,\"title\":\"Nvidia ships\"}},\
             {{\"id\":12,\"title\":\"Nvidia and TSMC expand capacity\"}},\
             {{\"id\":13,\"title\":\"Nvidia results\"}},\
             {{\"id\":14,\"title\":\"Weather turns cold\"}}]}}",
        topic(1, "Chips", "nvidia, tsmc", 1)
    );
    let result = out_of("rank", &payload);
    assert_eq!(score_of(&result, "count"), Some(3.0));
    assert_eq!(score_of(&result, "topic_id"), Some(1.0));
    let rows = array_of(&result, "matches");
    let order: Vec<f64> = rows.iter().filter_map(|r| score_of(r, "index")).collect();
    assert_eq!(order, vec![1.0, 0.0, 2.0]);
    let scores: Vec<f64> = rows.iter().filter_map(|r| score_of(r, "score")).collect();
    assert_eq!(scores, vec![6.5, 3.25, 3.25]);
    assert_eq!(score_of(&rows[0], "id"), Some(12.0));
    assert_eq!(
        strings(rows[0].get("matched")),
        vec!["nvidia (title)", "tsmc (title)"]
    );
    assert_eq!(
        rows[0].get("reason").and_then(Json::as_str),
        Some("nvidia (title), tsmc (title)")
    );
}

#[test]
fn rank_honours_a_limit_and_a_threshold() {
    let payload = format!(
        "{{\"topic\":{},\"limit\":1,\"articles\":[\
             {{\"id\":1,\"title\":\"Nvidia ships\"}},{{\"id\":2,\"title\":\"Nvidia and TSMC\"}}]}}",
        topic(1, "Chips", "nvidia, tsmc", 2)
    );
    let rows = array_of(&out_of("rank", &payload), "matches");
    assert_eq!(rows.len(), 1);
    assert_eq!(score_of(&rows[0], "id"), Some(2.0));
}

// -- score --------------------------------------------------------------

#[test]
fn score_is_the_bare_relevance_of_one_pair() {
    let hit = format!(
        "{{\"topic\":{},\"article\":{{\"title\":\"Oil price jumps\"}}}}",
        topic(7, "Oil", "oil price", 1)
    );
    let result = out_of("score", &hit);
    assert_eq!(flag_of(&result, "hit"), Some(true));
    assert_eq!(score_of(&result, "score"), Some(3.25));
    assert_eq!(score_of(&result, "raw_score"), Some(3.25));
    assert_eq!(score_of(&result, "terms_hit"), Some(1.0));
    assert_eq!(flag_of(&result, "excluded"), Some(false));

    let below = format!(
        "{{\"topic\":{},\"article\":{{\"title\":\"Oil price jumps\"}}}}",
        topic(7, "Oil", "oil price, opec", 2)
    );
    let result = out_of("score", &below);
    assert_eq!(flag_of(&result, "hit"), Some(false));
    assert_eq!(score_of(&result, "score"), Some(0.0));
    assert_eq!(score_of(&result, "raw_score"), Some(3.25));

    let excluded = "{\"topic\":{\"id\":1,\"name\":\"T\",\"keywords\":\"war\",\
                        \"exclude\":\"peace\"},\"article\":{\"title\":\"War and peace\"}}";
    let result = out_of("score", excluded);
    assert_eq!(flag_of(&result, "excluded"), Some(true));
    assert_eq!(score_of(&result, "score"), Some(0.0));
    assert_eq!(score_of(&result, "raw_score"), Some(0.0));
}

#[test]
fn score_accepts_a_flat_payload() {
    let flat = format!(
        "{{\"topic\":{},\"title\":\"Nvidia ships\"}}",
        topic(1, "T", "nvidia", 1)
    );
    assert_eq!(score_of(&out_of("score", &flat), "score"), Some(3.25));
}

// -- nasty input --------------------------------------------------------

#[test]
fn quotes_newlines_and_emoji_stay_valid_json() {
    let payload = match_payload(
        &[topic(1, "T", "nvidia", 1)],
        "He said \"Nvidia\"\nand left 🙂",
        "emoji only 🙂🙂",
        "Nvidia \\ backslash",
    );
    let result = out_of("match", &payload);
    assert_eq!(ids(&result), vec![1]);
    assert_eq!(
        first_match(&result).get("reason").and_then(Json::as_str),
        Some("nvidia (title)")
    );
}

#[test]
fn a_ten_kb_single_line_is_handled() {
    let mut body = String::from("Nvidia ");
    while body.len() < 10_000 {
        body.push('y');
    }
    let payload = match_payload(&[topic(1, "T", "nvidia", 1)], "", "", &body);
    assert_eq!(ids(&out_of("match", &payload)), vec![1]);
}

#[test]
fn an_article_with_every_field_empty_matches_nothing() {
    let payload = match_payload(&[topic(1, "T", "nvidia", 1)], "", "", "");
    let result = out_of("match", &payload);
    assert!(ids(&result).is_empty());
    assert_eq!(score_of(&result, "count"), Some(0.0));
}

#[test]
fn no_topics_is_a_valid_question() {
    let payload = "{\"topics\":[],\"article\":{\"title\":\"Nvidia ships\"}}";
    let result = out_of("match", payload);
    assert_eq!(score_of(&result, "count"), Some(0.0));
    assert!(ids(&result).is_empty());
    assert_eq!(flag_of(&result, "relevant"), Some(false));
}

#[test]
fn missing_keys_are_reported_per_subcommand() {
    assert_eq!(err_of("match", "{\"article\":{}}"), "missing topics");
    assert_eq!(err_of("match", "{\"topics\":{}}"), "topics must be an array");
    assert_eq!(err_of("rank", "{}"), "missing topic");
    assert_eq!(
        err_of("rank", &format!("{{\"topic\":{}}}", topic(1, "T", "x y", 1))),
        "missing articles"
    );
    assert_eq!(err_of("score", "{}"), "missing topic");
    assert_eq!(
        err_of("match", "{\"topics\":[{\"name\":\"T\"}]}"),
        "topic 0 is missing id"
    );
    assert_eq!(
        err_of("match", "{\"topics\":[{\"id\":1}]}"),
        "topic 0 is missing name"
    );
    assert_eq!(
        err_of(
            "match",
            "{\"topics\":[{\"id\":1,\"name\":\"T\",\"min_matches\":\"lots\"}]}"
        ),
        "min_matches must be a number"
    );
    assert!(err_of(
        "score",
        "{\"topic\":{\"id\":1,\"name\":\"T\"},\"article\":{\"title\":5}}"
    )
    .contains("must be a string"));
    assert!(err_of(
        "score",
        "{\"topic\":{\"id\":1,\"name\":\"T\"},\"article\":{\"entities\":7}}"
    )
    .contains("must be an array"));
}

#[test]
fn unknown_subcommands_and_non_objects_are_refused() {
    assert!(err_of("nope", "{}").starts_with("unknown subcommand: nope"));
    assert_eq!(err_of("match", "[]"), "input must be a JSON object");
    assert!(err_of("match", "{oops}").contains("expected object key"));
}

#[test]
fn min_matches_is_coerced_like_int_or_one() {
    let cases = [("0", 1), ("1", 1), ("2", 2), ("-3", 1), ("\"2\"", 2), ("\"\"", 1)];
    for (raw, want) in cases {
        let payload = format!(
            "{{\"topics\":[{{\"id\":1,\"name\":\"T\",\"keywords\":\"a b, c d\",\
                 \"min_matches\":{}}}],\"article\":{{\"title\":\"a b\"}}}}",
            raw
        );
        let result = out_of("match", &payload);
        let hit = array_of(&result, "matches").into_iter().next();
        if want <= 1 {
            assert_eq!(
                hit.and_then(|h| score_of(&h, "min_matches")),
                Some(1.0),
                "min_matches {} should behave as 1",
                raw
            );
        } else {
            assert!(hit.is_none(), "min_matches {} should need two terms", raw);
        }
    }
}

#[test]
fn a_topic_id_of_any_shape_is_echoed_unchanged() {
    let payload = "{\"topics\":[{\"id\":\"demo\",\"name\":\"T\",\"keywords\":\"nvidia\"}],\
                       \"article\":{\"title\":\"Nvidia ships\"}}";
    let result = out_of("match", payload);
    let id = array_of(&result, "topic_ids").into_iter().next();
    assert_eq!(id.and_then(|v| v.as_str().map(|s| s.to_string())), Some("demo".to_string()));
}

#[test]
fn the_first_spelling_of_a_family_is_the_one_reported() {
    // Faithful to _build: "rates" and "rate" share a stem, so the second is
    // dropped and the spelling the user typed first is the one reported.
    let payload = match_payload(&[topic(1, "T", "Rates, rate", 1)], "Rates are rising", "", "");
    let hit = first_match(&out_of("match", &payload));
    assert_eq!(strings(hit.get("terms")), vec!["rates"]);

    // Two spellings that *do not* share a stem both count: "wars" is only
    // four characters, so _stem_key leaves it alone (kept from Python).
    let payload = match_payload(&[topic(1, "T", "Wars, war", 1)], "Wars end", "", "");
    let hit = first_match(&out_of("match", &payload));
    assert_eq!(strings(hit.get("terms")), vec!["wars", "war"]);
    assert_eq!(score_of(&hit, "score"), Some(6.5));
}

#[test]
fn a_long_keyword_list_stays_fast_and_correct() {
    let mut keywords = String::new();
    for i in 0..40 {
        keywords.push_str(&format!("filler{} term, ", i));
    }
    keywords.push_str("nvidia");
    let payload = match_payload(&[topic(1, "Big", &keywords, 1)], "Nvidia ships", "", "");
    let result = out_of("match", &payload);
    assert_eq!(ids(&result), vec![1]);
    assert_eq!(strings(first_match(&result).get("terms")), vec!["nvidia"]);
}

#[test]
fn does_match_helper_is_sane() {
    assert!(does_match("nvidia", 1, "Nvidia ships"));
    assert!(!does_match("nvidia", 1, "Weather turns cold"));
}

mod answers;
