//! What an answer says about itself: near misses, exclusions, refusals.
//!
//! Split out of `tests/mod.rs` (which was past the ~600 line mark in
//! `docs/RUST-STYLE.md`): every case here is about a reply that was
//! *internally* inconsistent or silently empty — the kind of answer that looks
//! like a correct one, which is worse than an error because nothing downstream
//! notices it. The helpers live in the parent module.

use atlas_lib::json::Json;

use super::{
    array_of, err_of, first_match, flag_of, ids, match_payload, out_of, score_of, strings, topic,
};

#[test]
fn an_article_that_is_not_an_object_is_refused() {
    // Reading a string as "an article with every field empty" answered
    // "matches no topic" — the one answer that cannot be told apart from a
    // correct one, so the caller never learned it had sent garbage.
    for bad in ["\"Nvidia ships\"", "[1, 2]", "5", "true"] {
        let payload = format!(
            "{{\"topics\":[{}],\"article\":{}}}",
            topic(1, "T", "nvidia", 1),
            bad
        );
        assert_eq!(err_of("match", &payload), "article must be a JSON object");
        let payload = format!("{{\"topic\":{},\"article\":{}}}", topic(1, "T", "nvidia", 1), bad);
        assert_eq!(err_of("score", &payload), "article must be a JSON object");
    }
    // A `null` article and an absent one still mean "the payload is flat".
    let flat = format!(
        "{{\"topics\":[{}],\"article\":null,\"title\":\"Nvidia ships\"}}",
        topic(1, "T", "nvidia", 1)
    );
    assert_eq!(ids(&out_of("match", &flat)), vec![1]);
}

#[test]
fn an_excluded_topic_names_the_term_that_blocked_it() {
    // "excluded" with no name is a dead end: the reader cannot tell which of
    // their exclude terms to remove to unblock the topic.
    let payload = match_payload(
        &["{\"id\":1,\"name\":\"Labour\",\"keywords\":\"strike\",\"exclude\":\"military strike, war\"}"
            .to_string()],
        "Military strike hits the port",
        "",
        "",
    );
    let row = array_of(&out_of("match", &payload), "rejected")
        .into_iter()
        .next()
        .expect("rejected row");
    assert_eq!(
        row.get("reason_code").and_then(Json::as_str),
        Some("excluded")
    );
    assert_eq!(strings(row.get("excluded_by")), vec!["military strike"]);

    let scored = format!(
        "{{\"topic\":{{\"id\":1,\"name\":\"Labour\",\"keywords\":\"strike\",\"exclude\":\"war\"}},\
         \"article\":{{\"title\":\"War and strike\"}}}}"
    );
    let result = out_of("score", &scored);
    assert_eq!(flag_of(&result, "excluded"), Some(true));
    assert_eq!(strings(result.get("excluded_by")), vec!["war"]);
}

#[test]
fn every_exclude_term_that_hits_is_named() {
    // Naming only the first would send the reader round the same loop: remove
    // it, rescan, still excluded, by the other one.
    let payload = match_payload(
        &["{\"id\":1,\"name\":\"Labour\",\"keywords\":\"strike\",\"exclude\":\"military strike, war on\"}"
            .to_string()],
        "Military strike in a war on prices",
        "",
        "",
    );
    let row = array_of(&out_of("match", &payload), "rejected")
        .into_iter()
        .next()
        .expect("rejected row");
    assert_eq!(strings(row.get("excluded_by")), vec!["military strike", "war on"]);
}

#[test]
fn a_topic_whose_only_terms_are_exclusions_never_matches() {
    // No keywords means nothing to score, so the topic can never be a hit; the
    // exclusion still decides *why* it was rejected, and says which term.
    let payload = format!(
        "{{\"topics\":[{{\"id\":1,\"name\":\"Quiet\",\"keywords\":\"\",\"exclude\":\"war\"}}],\
         \"article\":{{\"title\":\"War in the east\"}}}}"
    );
    let result = out_of("match", &payload);
    assert!(ids(&result).is_empty());
    let row = array_of(&result, "rejected").into_iter().next().unwrap();
    assert_eq!(
        row.get("reason_code").and_then(Json::as_str),
        Some("excluded")
    );
    assert_eq!(strings(row.get("excluded_by")), vec!["war"]);
    assert_eq!(score_of(&row, "terms_hit"), Some(0.0));

    // The same topic against text it does not exclude is a plain miss, not an
    // exclusion, and reports the zero it scored.
    let quiet = format!(
        "{{\"topics\":[{{\"id\":1,\"name\":\"Quiet\",\"keywords\":\"\",\"exclude\":\"war\"}}],\
         \"article\":{{\"title\":\"Weather turns cold\"}}}}"
    );
    let row = array_of(&out_of("match", &quiet), "rejected")
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(
        row.get("reason_code").and_then(Json::as_str),
        Some("below_threshold")
    );
    assert!(strings(row.get("excluded_by")).is_empty());
    assert_eq!(score_of(&row, "score"), Some(0.0));
}

#[test]
fn a_near_miss_reports_its_matched_terms_like_a_hit_does() {
    // `matched` and `terms` describe the same keywords, so one may not be
    // filled while the other is empty: a caller that renders `terms` showed
    // nothing under an answer that listed a matched keyword.
    let below = format!(
        "{{\"topic\":{},\"article\":{{\"title\":\"Oil price jumps\"}}}}",
        topic(7, "Oil", "oil price, opec", 2)
    );
    let result = out_of("score", &below);
    assert_eq!(flag_of(&result, "hit"), Some(false));
    assert_eq!(strings(result.get("matched")), vec!["oil price (title)"]);
    assert_eq!(strings(result.get("terms")), vec!["oil price"]);
    assert!(strings(result.get("excluded_by")).is_empty());

    // The rejected row of `match` carries the same pair.
    let payload = match_payload(
        &[topic(7, "Oil", "oil price, opec", 2)],
        "Oil price jumps",
        "",
        "",
    );
    let row = array_of(&out_of("match", &payload), "rejected")
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(strings(row.get("matched")), vec!["oil price (title)"]);
    assert_eq!(strings(row.get("terms")), vec!["oil price"]);
    assert_eq!(
        row.get("reason").and_then(Json::as_str),
        Some("oil price (title)")
    );
}

#[test]
fn rank_refuses_a_limit_it_cannot_read_instead_of_ranking_everything() {
    let batch = format!(
        "{{\"topic\":{},\"articles\":[{{\"id\":1,\"title\":\"Nvidia ships\"}},\
         {{\"id\":2,\"title\":\"Nvidia and TSMC\"}}]}}",
        topic(1, "Chips", "nvidia, tsmc", 1)
    );
    // A number written as a string is read, like `min_matches`.
    let one = batch.replace("\"articles\"", "\"limit\":\"1\",\"articles\"");
    assert_eq!(array_of(&out_of("rank", &one), "matches").len(), 1);
    // A limit no reader can interpret is the caller's bug, and it is named.
    let junk = batch.replace("\"articles\"", "\"limit\":\"ten\",\"articles\"");
    assert_eq!(err_of("rank", &junk), "limit must be a number");
    let negative = batch.replace("\"articles\"", "\"limit\":-2,\"articles\"");
    assert_eq!(err_of("rank", &negative), "limit must be zero or more");
}

#[test]
fn a_rank_batch_names_the_article_it_cannot_read() {
    // Before this, one unreadable article ended the batch with "title must be
    // a string" and no way to tell which of a hundred articles it was.
    let payload = format!(
        "{{\"topic\":{},\"articles\":[{{\"id\":1,\"title\":\"Nvidia ships\"}},\
         {{\"id\":2,\"title\":7}}]}}",
        topic(1, "Chips", "nvidia", 1)
    );
    assert_eq!(
        err_of("rank", &payload),
        "articles[1]: title must be a string"
    );
}

#[test]
fn the_score_is_bounded_by_the_keywords_the_topic_has() {
    // 3.0 for a keyword in the title and 0.25 for each keyword found, so no
    // pair can score above 3.25 x its keyword count, below zero, or differently
    // on a second run: every reachable sum is a multiple of 0.25, which
    // `round3` keeps exact.
    let keywords = "alpha, bravo, charlie, delta, echo, foxtrot";
    let words = "alpha bravo charlie delta echo foxtrot";
    let in_title = match_payload(&[topic(1, "T", keywords, 1)], words, "", "");
    let hit = first_match(&out_of("match", &in_title));
    assert_eq!(strings(hit.get("terms")).len(), 6);
    assert_eq!(score_of(&hit, "score"), Some(6.0 * 3.25));

    let in_body = match_payload(&[topic(1, "T", keywords, 1)], "Nothing", "", words);
    let hit = first_match(&out_of("match", &in_body));
    assert_eq!(score_of(&hit, "score"), Some(6.0 * 0.5 + 6.0 * 0.25));

    // Score does not depend on which of the two orders the keywords were typed
    // in: the same set has to give the same number.
    let shuffled = match_payload(
        &[topic(1, "T", "foxtrot, alpha, echo, bravo, delta, charlie", 1)],
        words,
        "",
        "",
    );
    assert_eq!(
        score_of(&first_match(&out_of("match", &shuffled)), "score"),
        Some(6.0 * 3.25)
    );
}

#[test]
fn one_article_can_hit_one_topic_and_be_excluded_from_another() {
    // The three verdicts of one article at once: a hit, an exclusion and a
    // near miss. Each has to be reported in its own list, and the hit must not
    // be dragged down by the topic that was blocked.
    let payload = match_payload(
        &[
            "{\"id\":1,\"name\":\"Energy\",\"keywords\":\"refinery\",\"min_matches\":1}".to_string(),
            "{\"id\":2,\"name\":\"Labour\",\"keywords\":\"refinery\",\"exclude\":\"military\",\
             \"min_matches\":1}"
                .to_string(),
            "{\"id\":3,\"name\":\"Two terms\",\"keywords\":\"refinery, opec\",\"min_matches\":2}"
                .to_string(),
        ],
        "A military strike closed the refinery",
        "",
        "",
    );
    let result = out_of("match", &payload);
    assert_eq!(ids(&result), vec![1]);
    assert_eq!(score_of(&result, "count"), Some(1.0));
    let rejected = array_of(&result, "rejected");
    assert_eq!(rejected.len(), 2);
    assert_eq!(
        rejected[0].get("reason_code").and_then(Json::as_str),
        Some("excluded")
    );
    assert_eq!(strings(rejected[0].get("excluded_by")), vec!["military"]);
    assert_eq!(
        rejected[1].get("reason_code").and_then(Json::as_str),
        Some("below_threshold")
    );
    assert_eq!(strings(rejected[1].get("terms")), vec!["refinery"]);
    assert_eq!(score_of(&first_match(&result), "score"), Some(3.25));
}
