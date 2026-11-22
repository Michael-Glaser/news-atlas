//! How much evidence a place needs before it is the answer.
//!
//! The threshold, the zone weights, the repetition caps, the outlet as the last
//! resort, and the scripts that are matched as substrings rather than as words.

use super::*;

// --- how much evidence a place needs -----------------------------------

#[test]
fn one_body_mention_is_not_enough() {
    check(
        "one body mention is not enough",
        "Markets rally",
        "",
        "Traders in Chicago were upbeat.",
        "",
        "",
        Want {
            iso: Some(""),
            prec: Some(""),
            ..Want::default()
        },
    );
}

#[test]
fn two_body_mentions_are_enough() {
    check(
        "two body mentions are enough",
        "Markets rally",
        "",
        "Traders in Chicago were upbeat. Chicago desks added risk.",
        "",
        "",
        Want {
            iso: Some("USA"),
            prec: Some("city"),
            place: Some("Chicago"),
            ..Want::default()
        },
    );
}

#[test]
fn a_country_adjective_once_is_not_a_place() {
    case(
        "a country adjective once is not a place",
        "French and German ministers meet",
        "Officials spoke afterwards.",
        Want {
            iso: Some(""),
            prec: Some(""),
            ..Want::default()
        },
    );
}

#[test]
fn city_word_vs_plain_english_word() {
    case(
        "city word vs plain English word",
        "Tyre imports fall after UK tariffs",
        "Tyre sellers in the Midlands complained.",
        Want {
            iso: Some("GBR"),
            ..Want::default()
        },
    );
}

#[test]
fn a_bird_is_not_a_place() {
    case(
        "a bird is not a place",
        "Turkey prices climb before Thanksgiving",
        "Shoppers compared bird prices.",
        Want {
            iso: Some(""),
            prec: Some(""),
            ..Want::default()
        },
    );
}

// --- structural checks -------------------------------------------------

#[test]
fn alternatives_payload() {
    let got = locate(
        "Gaza ceasefire holds in Rafah",
        "Aid trucks entered Khan Younis. Talks in Cairo continued.",
        "Cairo hosts the talks again.",
        "",
        "",
    );
    let list = alts(&got);
    assert!(!list.is_empty() && list.len() <= 3, "{} runner-ups", list.len());
    for a in list {
        for key in ["iso", "place", "prec", "score", "conf"] {
            assert!(a.get(key).is_some(), "alternative without {}", key);
        }
        assert!(a.get("score").and_then(Json::as_f64).is_some(), "score is a number");
        assert!(a.get("conf").and_then(Json::as_f64).is_some(), "conf is a number");
    }
    // The three runner-ups of the Python check, in order.
    let have: Vec<(&str, f64)> = list
        .iter()
        .map(|a| (text_of(a, "place"), num_of(a, "score")))
        .collect();
    assert_eq!(have, vec![("Rafah", 6.0), ("Cairo", 4.0), ("Khan Younis", 3.0)]);
}

#[test]
fn explain_scoring_table() {
    let table = call(
        "explain",
        "Gaza ceasefire holds in Rafah",
        "Aid trucks entered Khan Younis.",
        "Talks in Cairo continued. Cairo hosts the talks again.",
        "",
        "",
    );
    let rows = table.get("rows").and_then(Json::as_arr).expect("rows");
    assert!(!rows.is_empty());
    let ranks: Vec<f64> = rows.iter().map(|r| num_of(r, "rank")).collect();
    let mut sorted = ranks.clone();
    sorted.sort_by(|a, b| b.partial_cmp(a).unwrap());
    assert_eq!(ranks, sorted, "rows are ranked highest first");
    assert_eq!(rows.iter().filter(|r| r.get("chosen") == Some(&Json::Bool(true))).count(), 1);
    for r in rows {
        assert!(!text_of(r, "note").is_empty(), "every row says what happened to it");
        assert!(r.get("kind").and_then(Json::as_str).is_some());
        assert!(r.get("title").and_then(Json::as_usize).is_some());
        assert!(r.get("summary").and_then(Json::as_usize).is_some());
        assert!(r.get("body").and_then(Json::as_usize).is_some());
    }
    // Python: "4 rows, top Gaza City 7.8".
    assert_eq!(rows.len(), 4);
    assert_eq!(text_of(&rows[0], "place"), "Gaza City");
    assert_eq!(num_of(&rows[0], "rank"), 7.8);
    let scanned = table.get("scanned").expect("scanned");
    assert_eq!(scanned.get("title").and_then(Json::as_usize), Some(29));
    assert_eq!(scanned.get("body").and_then(Json::as_usize), Some(54));
}

#[test]
fn twenty_kilobyte_article_stays_fast() {
    let mut body = String::new();
    for _ in 0..200 {
        body.push_str(
            "The committee met in Brussels and later in Washington to discuss tariffs. \
                 Officials from Tokyo, Frankfurt and Seoul joined the call, while analysts in \
                 New York warned of spillovers. ",
        );
    }
    assert!(body.chars().count() > BODY_CHARS);
    // Build the index before timing, like the Python check does.
    locate("warm up", "", &body, "", "");
    let built = index();
    let mut best = f64::MAX;
    for _ in 0..3 {
        let start = std::time::Instant::now();
        for _ in 0..5 {
            locate("Tariffs rise", "Officials met in Brussels.", &body, "", "");
        }
        best = best.min(start.elapsed().as_secs_f64() * 1000.0 / 5.0);
    }
    assert!(std::ptr::eq(built, index()), "the index is built once");
    // A debug build is 10-30x slower than the release build this ships as;
    // the 5 ms budget is the release-build contract.
    let budget = if cfg!(debug_assertions) { 150.0 } else { 5.0 };
    println!("20k chars: {:.2} ms/call (budget {:.0} ms)", best, budget);
    assert!(best < budget, "{:.2} ms/call over the {:.0} ms budget", best, budget);
}
