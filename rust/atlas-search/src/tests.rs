//! The tests: the fixtures, the two operations, and the behaviour a caller may
//! rely on.
//!
//! Every test here runs against the doubles in [`crate::doubles`], so the suite
//! needs no socket and no built binary: the parsing, the wrapper, the dedupe, the
//! ranking, the window and the cleaning are all exercised on pasted pages. The
//! fidelity tests — the same questions asked of the real `atlas-text` /
//! `atlas-dates` — live in [`crate::tests_engines`].

use std::sync::Arc;

use crate::cards;
use crate::clock::{Clock, Fixed};
use crate::doubles::*;
use crate::engine::{Engine, Surface};
use crate::extract;
use crate::fixtures;
use crate::html;
use crate::link;
use crate::rank;
use crate::rules::Rules;
use crate::search::Search;

// ---------------------------------------------------------------------------
// doubles: a fixed page, and the rules without their engines
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// the query the model wrote
// ---------------------------------------------------------------------------

#[test]
fn the_query_is_percent_encoded_once_and_never_rewritten() {
    let engine = Engine::default();
    let url = engine.url(Surface::Web, "\"chip export\" & ünïcode/规则");
    assert!(url.starts_with("https://html.duckduckgo.com/html/?q="), "{}", url);
    assert!(url.contains("%22chip%20export%22%20%26%20%C3%BCn%C3%AFcode%2F%E8%A7%84%E5%88%99"), "{}", url);
    // No `+`: a query value is not a form field.
    assert!(!url.contains('+'), "{}", url);
}

#[test]
fn a_unicode_query_reaches_the_engine_intact() {
    let (search, page, _) = tool(fixtures::DUCK_WEB);
    let answer = search.web_search("チップ 輸出 \"rules\"", 5);
    let asked = page.asked();
    assert_eq!(asked.len(), 1);
    assert!(
        asked[0].contains("%E3%83%81%E3%83%83%E3%83%97%20%E8%BC%B8%E5%87%BA%20%22rules%22"),
        "{}",
        asked[0]
    );
    // The evidence repeats the query as the model wrote it, not as the URL spells it.
    assert_eq!(pick(&answer, "query"), "チップ 輸出 \"rules\"");
}

#[test]
fn a_template_without_the_placeholder_is_refused() {
    let error = Engine::new("https://html.duckduckgo.com/html/", "").unwrap_err();
    assert!(error.contains("{query}"), "{}", error);
}

#[test]
fn a_template_on_a_loopback_address_is_refused_before_it_is_used() {
    let error = Engine::new("http://127.0.0.1:8092/?q={query}", "").unwrap_err();
    assert!(error.contains("may open"), "{}", error);
}

#[test]
fn an_empty_query_says_what_to_do_and_fetches_nothing() {
    let (search, page, _) = tool(fixtures::DUCK_WEB);
    let answer = search.web_search("   ", 5);
    assert!(pick(&answer, "error").contains("query required"));
    assert_eq!(rows(&answer).len(), 0);
    assert!(page.asked().is_empty());
}

// ---------------------------------------------------------------------------
// one engine, two surfaces
// ---------------------------------------------------------------------------

#[test]
fn web_search_reads_a_duckduckgo_page() {
    let (search, page, _) = tool(fixtures::DUCK_WEB);
    let answer = search.web_search("chip export rules", 6);
    assert_eq!(number(&answer, "count"), 3, "{:?}", answer);
    assert_eq!(pick(&answer, "engine"), "html.duckduckgo.com");
    assert_eq!(pick(&answer, "surface"), "web");
    let rows = rows(&answer);
    let northgate = find(&rows, "northgate-herald");
    // The wrapper is gone: the model gets the publisher's own URL.
    assert_eq!(
        northgate["url"].as_str().unwrap(),
        "https://www.northgate-herald.com/2026/11/chip-exports-rise.html"
    );
    assert_eq!(
        northgate["title"].as_str().unwrap(),
        "Chip exports rise to a record quarter"
    );
    assert_eq!(northgate["publisher"].as_str().unwrap(), "Northgate-herald");
    assert!(
        northgate["snippet"]
            .as_str()
            .unwrap()
            .contains("rose 12 percent in October"),
        "{:?}",
        northgate
    );
    // The three anchors that carried the same link produced one row, and the
    // tracking parameters are gone from the URL the model may open.
    assert!(!northgate["url"].as_str().unwrap().contains("utm_"), "{:?}", northgate);
    assert_eq!(number(&answer, "duplicates"), 4, "{:?}", answer);
    // The engine's own furniture never became a result.
    assert_eq!(number(&answer, "navigation"), 3, "{:?}", answer);
    assert!(!page.asked().is_empty());
}

#[test]
fn the_engine_navigation_is_dropped_and_nothing_else() {
    let (search, _, _) = tool(fixtures::DUCK_WEB);
    let answer = search.web_search("chip export rules", 20);
    for row in rows(&answer) {
        let url = row["url"].as_str().unwrap();
        assert!(
            !url.contains("duckduckgo.com"),
            "engine navigation survived: {}",
            url
        );
    }
}

#[test]
fn a_second_engine_with_a_base64_wrapper_and_a_printed_date() {
    let (search, _, _) = tool_on(bing(), fixtures::BING_NEWS);
    let answer = search.web_search("chip export rules", 6);
    assert_eq!(pick(&answer, "engine"), "www.bing.com");
    let rows = rows(&answer);
    assert!(rows.len() >= 4, "{:?}", answer);
    let first = rows
        .iter()
        .find(|row| row["url"].as_str().unwrap().contains("kestreltimes"))
        .expect("the base64 wrapper was decoded");
    assert_eq!(first["published"].as_str().unwrap(), "2026-11-05T10:00:00Z");
    assert_eq!(first["printed"].as_str().unwrap(), "2 hours ago");
    // The German numeric date is read by the same rule.
    let german = rows
        .iter()
        .find(|row| row["url"].as_str().unwrap().contains("harbourview"))
        .expect("the second card is there");
    assert_eq!(german["published"].as_str().unwrap(), "2026-11-05T00:00:00Z");
}

#[test]
fn a_dated_web_result_carries_the_same_age_as_a_dated_news_result() {
    // `Hit::age_s` is documented as "seconds between that date and now, -1 when
    // there is no date". A dated web row used to report -1 — the field was filled
    // only on the news surface — so the model had to read two shapes for one date.
    let (search, _, _) = tool_on(bing(), fixtures::BING_NEWS);
    let web = search.web_search("chip export rules", 10);
    let web_rows = rows(&web);
    let dated = find(&web_rows, "kestreltimes");
    assert_eq!(dated["published"].as_str().unwrap(), "2026-11-05T10:00:00Z");
    assert_eq!(dated["printed"].as_str().unwrap(), "2 hours ago");
    assert_eq!(dated["age_s"].as_i64(), Some(2 * 3_600), "{:?}", dated);
    // The news surface reads the same card to the same number.
    let news = search.news_for("chip export rules", NOW - 48 * 3_600, 10);
    let news_rows = rows(&news);
    let same = find(&news_rows, "kestreltimes");
    assert_eq!(same["age_s"], dated["age_s"], "{:?}", same);
}

#[test]
fn a_link_to_this_machine_is_refused_and_counted() {    let (search, _, _) = tool_on(bing(), fixtures::BING_NEWS);
    let answer = search.web_search("chip export rules", 20);
    for row in rows(&answer) {
        assert!(
            !row["url"].as_str().unwrap().contains("127.0.0.1"),
            "a loopback link reached the model: {:?}",
            row
        );
    }
    assert_eq!(number(&answer, "refused_links"), 1, "{:?}", answer);
}

#[test]
fn a_wrapper_that_unwraps_to_a_loopback_address_is_refused() {
    let (search, _, _) = tool_on(bing(), fixtures::HOSTILE);
    let answer = search.web_search("anything", 20);
    for row in rows(&answer) {
        let url = row["url"].as_str().unwrap();
        assert!(!url.contains("127.0.0.1"), "{}", url);
        assert!(!url.starts_with("javascript:"), "{}", url);
    }
    assert_eq!(number(&answer, "refused_links"), 1, "{:?}", answer);
}

// ---------------------------------------------------------------------------
// the two failure shapes
// ---------------------------------------------------------------------------

#[test]
fn zero_results_is_a_plain_empty_answer() {
    let (search, _, _) = tool(fixtures::NO_RESULTS);
    let answer = search.web_search("nothing at all", 6);
    assert_eq!(number(&answer, "count"), 0, "{:?}", answer);
    assert_eq!(rows(&answer).len(), 0);
    assert!(pick(&answer, "error").is_empty(), "{:?}", answer);
    assert!(pick(&answer, "note").contains("nothing"), "{:?}", answer);
    assert_eq!(number(&answer, "hit_count"), 0);
}

#[test]
fn an_engine_that_refuses_the_request_is_a_reason_not_a_panic() {
    let engine = Engine::default();
    let rules = Arc::new(Doubling::default());
    let search = Search::new(engine, rules as Arc<dyn Rules>, Arc::new(Refused))
        .with_clock(Arc::new(Fixed(NOW)) as Arc<dyn Clock>);
    let answer = search.web_search("chip", 6);
    assert!(pick(&answer, "error").contains("did not answer"), "{:?}", answer);
    assert!(pick(&answer, "error").contains("gate refused"), "{:?}", answer);
    assert_eq!(rows(&answer).len(), 0);
}

// ---------------------------------------------------------------------------
// forty results
// ---------------------------------------------------------------------------

#[test]
fn forty_results_are_all_read_and_the_limit_is_the_limit() {
    let body: &'static str = Box::leak(many(40).into_boxed_str());
    let (search, _, _) = tool(body);
    let answer = search.web_search("chip exports", 10);
    assert_eq!(number(&answer, "hit_count"), 40, "{:?}", answer);
    assert_eq!(number(&answer, "count"), 10, "{:?}", answer);
    assert_eq!(rows(&answer).len(), 10);
    // Every row keeps the position the engine gave it.
    let ranks: Vec<i64> = rows(&answer)
        .iter()
        .map(|row| row["page_rank"].as_i64().unwrap())
        .collect();
    assert!(ranks.iter().all(|rank| (1..=40).contains(rank)), "{:?}", ranks);
    // And asking for more than the crate allows is clamped.
    let wide = search.web_search("chip exports", 500);
    assert_eq!(number(&wide, "count"), 40, "{:?}", wide);
}

#[test]
fn a_hundred_results_do_not_change_the_cost_of_the_page() {
    // The caps are what make a hostile page cost the same as a normal one; this
    // test is the assertion that the cap is in force, not a benchmark.
    let body: &'static str = Box::leak(many(100).into_boxed_str());
    let (search, _, _) = tool(body);
    let answer = search.web_search("chip exports", 5);
    assert_eq!(number(&answer, "count"), 5);
    assert!(number(&answer, "hit_count") <= html::MAX_ANCHORS as i64);
}

// ---------------------------------------------------------------------------
// duplicates, tracking parameters, and the rule behind them
// ---------------------------------------------------------------------------

#[test]
fn the_same_page_with_different_tracking_parameters_is_one_result() {
    let body = "<html><body><div class=\"card\"><a href=\"https://www.northgate-herald.com/x\">One and the same story</a>\
                <div class=\"snippet\">The first copy of the story, clean as the publisher wrote it.</div></div>\
                <div class=\"card\"><a href=\"https://www.northgate-herald.com/x?utm_source=newsletter&amp;utm_campaign=pm\">One and the same story</a>\
                <div class=\"snippet\">The second copy of the same story, dressed in tracking parameters.</div></div>\
                <div class=\"card\"><a href=\"https://www.northgate-herald.com/x?a=2\">A different page on the same path</a>\
                <div class=\"snippet\">A query parameter that decides which page it is, so it is not a duplicate.</div></div>\
                </body></html>";
    let leaked: &'static str = Box::leak(body.to_string().into_boxed_str());
    let (search, _, rules) = tool(leaked);
    let answer = search.web_search("story", 10);
    assert_eq!(number(&answer, "count"), 2, "{:?}", answer);
    assert_eq!(number(&answer, "duplicates"), 1, "{:?}", answer);
    // The rule was consulted for the collision — and only for the collision.
    let asked = rules.asked.lock().expect("no panic").clone();
    assert!(!asked.is_empty(), "norm_url was never asked about a collision");
}

#[test]
fn tracking_parameters_are_dropped_from_the_returned_url() {
    let body = "<html><body><div class=\"card\">\
                <a href=\"https://www.northgate-herald.com/y?utm_source=ddg&amp;utm_medium=organic\">A story with tracking</a>\
                <div class=\"snippet\">The link carries two tracking parameters and nothing else.</div></div>\
                <div class=\"card\"><a href=\"https://www.northgate-herald.com/z?id=7&amp;utm_source=ddg\">A story with one real parameter</a>\
                <div class=\"snippet\">The link carries a parameter that decides the page, so it stays.</div></div>\
                </body></html>";
    let leaked: &'static str = Box::leak(body.to_string().into_boxed_str());
    let (search, _, _) = tool(leaked);
    let answer = search.web_search("story", 10);
    let rows = rows(&answer);
    assert_eq!(rows.len(), 2, "{:?}", answer);
    assert_eq!(rows[0]["url"].as_str().unwrap(), "https://www.northgate-herald.com/y");
    assert_eq!(
        rows[1]["url"].as_str().unwrap(),
        "https://www.northgate-herald.com/z?id=7"
    );
}

#[test]
fn the_shadow_index_never_merges_what_the_canonical_form_keeps_apart() {
    let a = url::Url::parse("https://www.northgate-herald.com/a?x=1").unwrap();
    let b = url::Url::parse("https://www.northgate-herald.com/a?x=2").unwrap();
    let c = url::Url::parse("https://WWW.northgate-herald.com/a/").unwrap();
    assert_eq!(link::shadow_key(&a), link::shadow_key(&b));
    assert_eq!(link::shadow_key(&a), link::shadow_key(&c));
    // …and the coarse key is only an index: the canonical form still separates them.
    let rules = Doubling::default();
    assert_ne!(rules.norm_url(a.as_str()), rules.norm_url(b.as_str()));
    assert_eq!(
        rules.norm_url(c.as_str()),
        rules.norm_url("https://www.northgate-herald.com/a")
    );
}

// ---------------------------------------------------------------------------
// a result with no date
// ---------------------------------------------------------------------------

#[test]
fn a_result_with_no_date_is_still_a_result_and_says_so() {
    let (search, _, _) = tool_on(bing(), fixtures::BING_NEWS);
    let answer = search.web_search("chip export rules", 10);
    let undated = rows(&answer)
        .into_iter()
        .find(|row| row["url"].as_str().unwrap().contains("straitswire"))
        .expect("the undated card survives a web search");
    assert_eq!(undated["published"].as_str().unwrap(), "");
    assert!(undated.get("age_s").is_none(), "{:?}", undated);
    assert!(undated.get("printed").is_none(), "{:?}", undated);
    assert!(number(&answer, "undated") >= 1, "{:?}", answer);
}

// ---------------------------------------------------------------------------
// news_for: only what is dated inside the window the caller passed
// ---------------------------------------------------------------------------

#[test]
fn news_for_keeps_only_what_is_inside_the_window() {
    let (search, page, _) = tool_on(bing(), fixtures::BING_NEWS);
    let answer = search.news_for("chip export rules", NOW - 48 * 3_600, 10);
    assert_eq!(pick(&answer, "surface"), "news", "{:?}", answer);
    assert_eq!(number(&answer, "since"), NOW - 48 * 3_600);
    assert_eq!(number(&answer, "window_s"), 48 * 3_600);
    // The news surface was asked, not the web one.
    assert!(page.asked()[0].contains("/news/search"), "{:?}", page.asked());
    let urls: Vec<String> = rows(&answer)
        .iter()
        .map(|row| row["url"].as_str().unwrap().to_string())
        .collect();
    assert!(urls.iter().any(|url| url.contains("kestreltimes")), "{:?}", urls);
    assert!(urls.iter().any(|url| url.contains("harbourview")), "{:?}", urls);
    // Three days old is outside a 48 h window; the undated card cannot be shown
    // to be inside it either; the loopback link was never a candidate.
    assert!(!urls.iter().any(|url| url.contains("lindmark")), "{:?}", urls);
    assert!(!urls.iter().any(|url| url.contains("straitswire")), "{:?}", urls);
    assert_eq!(number(&answer, "out_of_window"), 1, "{:?}", answer);
    assert_eq!(number(&answer, "undated"), 1, "{:?}", answer);
    assert_eq!(number(&answer, "refused_links"), 1, "{:?}", answer);
    for row in rows(&answer) {
        assert!(!row["published"].as_str().unwrap().is_empty(), "{:?}", row);
        assert!(row["age_s"].as_i64().unwrap() >= 0, "{:?}", row);
    }
}

#[test]
fn a_window_of_one_hour_drops_what_a_window_of_three_days_keeps() {
    let (search, _, _) = tool_on(bing(), fixtures::BING_NEWS);
    let tight = search.news_for("chip", NOW - 3_600, 10);
    let wide = search.news_for("chip", NOW - 3 * 86_400, 10);
    assert!(
        number(&wide, "count") > number(&tight, "count"),
        "wide={:?} tight={:?}",
        wide["count"],
        tight["count"]
    );
    // The one dated exactly three days back is on the boundary: atlas-dates says
    // whether that counts, and the wide window must include it.
    let urls: Vec<String> = rows(&wide)
        .iter()
        .map(|row| row["url"].as_str().unwrap().to_string())
        .collect();
    assert!(urls.iter().any(|url| url.contains("lindmark")), "{:?}", urls);
}

#[test]
fn find_news_is_news_for_in_hours_and_says_which_window_it_used() {
    let (search, _, _) = tool_on(bing(), fixtures::BING_NEWS);
    let answer = search.find_news("chip export rules", 48, 8);
    assert_eq!(number(&answer, "hours"), 48);
    assert_eq!(number(&answer, "since"), NOW - 48 * 3_600);
    assert_eq!(number(&answer, "count"), 2, "{:?}", answer);
    // The hour count is clamped to the app's reporting window, not to a week.
    let clamped = search.find_news("chip", 10_000, 8);
    assert_eq!(number(&clamped, "hours"), crate::search::MAX_HOURS);
}

#[test]
fn news_for_on_the_web_surface_of_an_engineless_page_finds_nothing_and_says_why() {
    let (search, _, _) = tool(fixtures::DUCK_WEB);
    let answer = search.news_for("chip export rules", NOW - 48 * 3_600, 10);
    assert_eq!(number(&answer, "count"), 0, "{:?}", answer);
    assert_eq!(number(&answer, "undated"), 3, "{:?}", answer);
    assert!(pick(&answer, "note").contains("undated"), "{}\n", pick(&answer, "note"));
}

#[test]
fn the_date_budget_says_which_links_it_never_read() {
    // Forty-five cards, every one of them dated nine days ago: the first
    // `MAX_DATE_LOOKUPS` are read and dropped as stale, and the rest are never
    // looked at. Those are not "undated" — the page is perfectly dated — so the
    // answer must not tell the model that it is.
    let mut body = String::from("<html><body>");
    for index in 0..45 {
        body.push_str(&format!(
            "<div class=\"result\"><h2><a href=\"https://www.paper{index}.net/story/{index}\">Chip story number {index} about exports</a></h2>\
             <span class=\"when\">9 days ago</span></div>"
        ));
    }
    body.push_str("</body></html>");
    let leaked: &'static str = Box::leak(body.into_boxed_str());
    let (search, _, _) = tool(leaked);
    let answer = search.news_for("chip exports", NOW - 48 * 3_600, 10);
    assert_eq!(number(&answer, "count"), 0, "{:?}", answer);
    assert_eq!(
        number(&answer, "out_of_window") as usize,
        extract::MAX_DATE_LOOKUPS,
        "{:?}",
        answer
    );
    assert_eq!(number(&answer, "undated"), 0, "{:?}", answer);
    let not_checked = number(&answer, "not_checked");
    assert_eq!(not_checked, number(&answer, "hit_count") - extract::MAX_DATE_LOOKUPS as i64, "{:?}", answer);
    assert!(not_checked > 0, "{:?}", answer);
    let note = pick(&answer, "note");
    assert!(note.contains("not read"), "{}", note);
    assert!(!note.contains("carried no readable date"), "{}", note);
}

// ---------------------------------------------------------------------------
// ranking
// ---------------------------------------------------------------------------

#[test]
fn relevance_beats_the_engines_order_without_dropping_anything() {
    let body = "<html><body>\
        <div class=\"card\"><a href=\"https://www.northgate-herald.com/one\">A story about the weather</a>\
        <div class=\"snippet\">Rain is expected over the weekend in three ports.</div></div>\
        <div class=\"card\"><a href=\"https://www.northgate-herald.com/two\">Chip export rules tightened</a>\
        <div class=\"snippet\">Chip export rules were tightened on Thursday, the ministry said.</div></div>\
        </body></html>";
    let leaked: &'static str = Box::leak(body.to_string().into_boxed_str());
    let (search, _, _) = tool(leaked);
    let answer = search.web_search("chip export rules", 10);
    let rows = rows(&answer);
    assert_eq!(rows.len(), 2, "ranking never drops a row");
    assert_eq!(rows[0]["url"].as_str().unwrap(), "https://www.northgate-herald.com/two");
    assert!(rows[0]["score"].as_f64().unwrap() > 0.0, "{:?}", rows[0]);
    assert_eq!(rows[1]["score"].as_f64().unwrap(), 0.0);
    assert_eq!(answer["ranked"].as_bool(), Some(true), "{:?}", answer);
    // The engine's own position is still visible.
    assert_eq!(rows[0]["page_rank"].as_i64().unwrap(), 2);
}

#[test]
fn a_query_that_shares_no_word_keeps_the_engines_order() {
    let scores = rank::scores("zzz", &["alpha beta".to_string(), "beta gamma".to_string()]);
    assert_eq!(scores, vec![0.0, 0.0]);
    let order = rank::order(&scores, &[1, 2]);
    assert_eq!(order, vec![0, 1]);
}

#[test]
fn ranking_is_the_similarity_primitive_and_nothing_else() {
    let texts = vec![
        "chip export rules tightened on Thursday".to_string(),
        "the weather over the weekend".to_string(),
    ];
    let scores = rank::scores("chip export rules", &texts);
    assert!(scores[0] > 0.5, "{:?}", scores);
    assert_eq!(scores[1], 0.0);
    // Both vectors are unit length, which is what the primitive documents.
    let tokens = rank::tokens("Chip, CHIP and chips");
    assert_eq!(tokens, vec!["chip", "chip", "and", "chips"]);
}

// ---------------------------------------------------------------------------
// markup that is broken, hostile, or enormous
// ---------------------------------------------------------------------------

#[test]
fn malformed_markup_returns_what_it_can_and_never_panics() {
    let (search, _, _) = tool(fixtures::MALFORMED);
    let answer = search.web_search("anything", 20);
    assert!(number(&answer, "count") >= 1, "{:?}", answer);
    for row in rows(&answer) {
        assert!(!row["url"].as_str().unwrap().is_empty());
        assert!(row["title"].as_str().unwrap().chars().count() <= 300);
    }
    // A `script` body is not a result, however much it looks like one.
    assert!(
        !rows(&answer)
            .iter()
            .any(|row| row["url"].as_str().unwrap().contains("scripted")),
        "{:?}",
        answer
    );
}

#[test]
fn an_empty_page_and_a_page_of_noise_are_both_empty_answers() {
    let (empty, _, _) = tool("");
    assert_eq!(number(&empty.web_search("chip", 5), "count"), 0);

    let noise: &'static str = Box::leak("<div>".repeat(20_000).into_boxed_str());
    let (search, _, _) = tool(noise);
    let answer = search.web_search("chip", 5);
    assert_eq!(number(&answer, "count"), 0, "{:?}", answer);
}

#[test]
fn a_hostile_title_is_cleaned_and_capped() {
    let (search, _, _) = tool_on(bing(), fixtures::HOSTILE);
    let answer = search.web_search("chip", 20);
    let rows = rows(&answer);
    let long = rows
        .iter()
        .find(|row| row["url"].as_str().unwrap().contains("harbourview.net/long"))
        .expect("the oversized headline is still a result");
    assert!(
        long["title"].as_str().unwrap().chars().count() <= 300,
        "{} chars",
        long["title"].as_str().unwrap().chars().count()
    );
    for row in rows {
        let title = row["title"].as_str().unwrap();
        // Bidi overrides and control characters never reach the model.
        assert!(!title.contains('\u{202E}'), "{:?}", title);
        assert!(!title.chars().any(|ch| ch.is_control()), "{:?}", title);
        let snippet = row["snippet"].as_str().unwrap();
        assert!(!snippet.chars().any(|ch| ch.is_control()), "{:?}", snippet);
        assert!(snippet.chars().count() <= 400);
    }
}

#[test]
fn the_snippet_is_the_longest_run_counted_in_characters() {
    // Two runs in one card, both long enough to be a snippet: sixty Latin
    // characters and thirty that each cost three bytes. "The longest run" is a
    // character count — the unit `MIN_SNIPPET_CHARS` is written in — so the Latin
    // one is the snippet. Comparing byte lengths picked the shorter summary.
    let latin = "Chip exports rose in October, the ministry said on Thursday this week.";
    let wide = "半導体の輸出は十月に過去最高を記録したと当局が木曜日に発表した。";
    assert!(wide.chars().count() < latin.chars().count());
    assert!(wide.len() > latin.len(), "the bug this test pins needs the bytes to differ");
    let body = format!(
        "<html><body><div class=\"result\"><h2><a href=\"https://www.northgate-herald.com/one\">A story about chips</a></h2>\
         <div class=\"snippet\">{latin}</div><div class=\"also\">{wide}</div></div></body></html>"
    );
    let leaked: &'static str = Box::leak(body.into_boxed_str());
    let markup = html::scan(leaked);
    let page = cards::cards(&markup);
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].snippet, latin);
    // …and the other run is still a date candidate, not lost.
    assert!(page[0].runs.iter().any(|run| run == wide), "{:?}", page[0].runs);
}

#[test]
fn the_parser_reads_one_page_of_known_shape_the_same_way_twice() {
    let first = html::scan(fixtures::BING_NEWS);
    let second = html::scan(fixtures::BING_NEWS);
    assert_eq!(first.anchors.len(), second.anchors.len());
    assert_eq!(first.blocks.len(), second.blocks.len());
    assert_eq!(first.segments.len(), second.segments.len());
    assert_eq!(cards::cards(&first).len(), cards::cards(&second).len());
    assert_eq!(first.anchors.len(), 6, "the nav element is furniture");
}

// ---------------------------------------------------------------------------
// the seams
// ---------------------------------------------------------------------------

#[test]
fn the_anchor_attribute_reader_handles_quotes_and_entities() {
    assert_eq!(html::attr(" class=\"a\" href=\"x&amp;y\"", "href"), "x&y");
    assert_eq!(html::attr("href='single' rel=nofollow", "href"), "single");
    assert_eq!(html::attr("data-href=\"nope\" href=\"yes\"", "href"), "yes");
    assert_eq!(html::attr("href=nofollow rel=nofollow", "href"), "nofollow");
    assert_eq!(html::attr("class=\"x\"", "href"), "");
}

#[test]
fn a_wrapper_is_only_unwrapped_when_a_value_is_a_link() {
    let wrapper = url::Url::parse(
        "https://www.bing.com/ck/a?p=1&u=a1aHR0cHM6Ly93d3cubm9ydGhnYXRlLWhlcmFsZC5jb20vc3Rvcnk",
    )
    .unwrap();
    let real = link::unwrap(&wrapper).expect("the base64 payload is the link");
    assert_eq!(real.as_str(), "https://www.northgate-herald.com/story");
    assert!(link::unwrap(&url::Url::parse("https://example.com/story?id=7").unwrap()).is_none());
    assert!(link::decode_embedded("not-base64-at-all!!!").is_none());
    assert_eq!(
        link::base64url_decode("aHR0cHM6Ly9hLmI").as_deref(),
        Some("https://a.b")
    );
}

#[test]
fn a_relative_href_resolves_against_the_engine() {
    let base = "https://html.duckduckgo.com/html/?q=x";
    assert_eq!(
        link::resolve("/story/1", base).unwrap().as_str(),
        "https://html.duckduckgo.com/story/1"
    );
    assert_eq!(
        link::resolve("//news.site.org/a", base).unwrap().as_str(),
        "https://news.site.org/a"
    );
    assert!(link::resolve("#fragment", base).is_none());
    assert!(link::resolve("mailto:a@b.c", base).is_none());
    assert!(link::resolve("javascript:alert(1)", base).is_none());
    assert!(link::resolve("", base).is_none());
}

#[test]
fn the_engine_owns_its_own_domain_and_nothing_else() {
    assert!(link::engine_owns("html.duckduckgo.com", "html.duckduckgo.com"));
    assert!(link::engine_owns("duckduckgo.com", "html.duckduckgo.com"));
    // A parent or a child of the engine's host is the engine's too; a sibling is
    // not, because "the same registrable domain" needs a public-suffix list and
    // this crate carries no list it cannot justify.
    assert!(link::engine_owns("www.bing.com", "bing.com"));
    assert!(link::engine_owns("bing.com", "www.bing.com"));
    assert!(!link::engine_owns("news.bing.com", "www.bing.com"));
    assert!(!link::engine_owns("northgate-herald.com", "html.duckduckgo.com"));
    assert!(!link::engine_owns("notduckduckgo.com", "duckduckgo.com"));
    assert!(!link::engine_owns("", "duckduckgo.com"));
}

#[test]
fn the_date_candidates_are_short_runs_first() {
    let segments = vec![
        "A long line of prose that runs on for a while about the exports and the inventory".to_string(),
        "Kestrel Times".to_string(),
        "2 hours ago".to_string(),
        "published 05 November 2026".to_string(),
    ];
    let found = crate::dates::candidates(&segments);
    assert!(found.len() <= crate::dates::MAX_CANDIDATES);
    assert!(found.contains(&"2 hours ago".to_string()), "{:?}", found);
    assert!(found.contains(&"Kestrel Times".to_string()), "{:?}", found);
    assert!(
        found.iter().any(|candidate| candidate.contains("2026")),
        "{:?}",
        found
    );
    assert_eq!(crate::dates::trim_edges("· 05.11.2026 ·"), "05.11.2026");
}

#[test]
fn the_clock_is_the_only_way_time_enters() {
    let fixed = Fixed(NOW);
    assert_eq!(fixed.now(), NOW);
    assert!(crate::clock::System.now() > 1_700_000_000);
}

#[test]
fn extraction_over_one_page_reports_every_drop() {
    let markup = html::scan(fixtures::BING_NEWS);
    let page = cards::cards(&markup);
    let rules = Doubling::default();
    let outcome = extract::extract(
        &page,
        "https://www.bing.com/news/search?q=x",
        "www.bing.com",
        &rules,
    );
    assert_eq!(outcome.candidates.len(), 4, "{:?}", outcome.parsed);
    assert_eq!(outcome.refused, 1);
    assert_eq!(outcome.navigation, 1);
    // Extraction does not read dates: it hands the candidates on.
    assert!(!outcome.candidates[0].dates.is_empty());
    assert_eq!(outcome.duplicates, 0);
    // Every parsed link ends in exactly one bucket.
    assert_eq!(
        outcome.parsed,
        outcome.candidates.len()
            + outcome.navigation
            + outcome.unusable
            + outcome.refused
            + outcome.duplicates
    );
}

#[test]
fn the_text_runs_inside_a_range_are_looked_up_not_scanned() {
    // 400 links is `html::MAX_ANCHORS`: the largest page this crate will read.
    // `cards` asks for the runs inside a range once per candidate block per link, so
    // the question is asked thousands of times on one page — it is a range lookup
    // now, and this test is what makes that safe: the lookup answers exactly what a
    // scan of the whole page answers, same runs, same order.
    let body: &'static str = Box::leak(many(400).into_boxed_str());
    let markup = html::scan(body);
    assert_eq!(markup.anchors.len(), 400, "the page is at the anchor cap");
    assert!(
        markup.segments_are_ordered(),
        "the lookup needs the segments in page order"
    );
    for anchor in markup.anchors.iter().take(50) {
        let from = anchor.start.saturating_sub(1);
        let to = anchor.end + 400;
        let looked_up: Vec<&str> = markup
            .segments_within(from, to)
            .iter()
            .map(|segment| segment.text.as_str())
            .collect();
        let scanned: Vec<&str> = markup
            .segments
            .iter()
            .filter(|segment| from <= segment.start && segment.end <= to)
            .map(|segment| segment.text.as_str())
            .collect();
        assert_eq!(looked_up, scanned, "the lookup and the scan disagree");
    }
    let page = cards::cards(&markup);
    assert_eq!(page.len(), 400, "every link on the page is a card");
}

#[test]
fn reading_a_full_page_and_cutting_it_into_cards_stay_in_the_same_order_of_cost() {
    // Not a benchmark: the assertion is the order of magnitude, so a lookup that
    // went back to a full scan per question (2.1x the cost of reading the page,
    // measured) fails while the noise of a loaded machine does not. Run with
    // `--nocapture` for the numbers.
    use std::time::Instant;
    let body: &'static str = Box::leak(many(400).into_boxed_str());
    let markup = html::scan(body);
    let mut scan_ms: Vec<f64> = Vec::new();
    let mut cards_ms: Vec<f64> = Vec::new();
    for _ in 0..5 {
        let started = Instant::now();
        let markup = html::scan(body);
        scan_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
        let started = Instant::now();
        let page = cards::cards(&markup);
        cards_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
        assert_eq!(page.len(), 400);
    }
    assert_eq!(markup.anchors.len(), 400);
    // The minimum of five, not the median: a machine compiling something else
    // inflates any single run, and the fastest run of each is the one that measured
    // the work rather than the neighbours.
    let fastest = |values: &mut Vec<f64>| {
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        values.first().copied().unwrap_or(0.0)
    };
    let (scan, cut) = (fastest(&mut scan_ms), fastest(&mut cards_ms));
    println!("400-link page: read {scan:.2} ms, cut into cards {cut:.2} ms");
    assert!(
        cut < scan * 3.0,
        "cutting the page into cards cost {cut:.2} ms against {scan:.2} ms to read it"
    );
}
