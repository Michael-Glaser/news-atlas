//! The fidelity tests, and the numbers.
//!
//! These call the real `atlas-text` / `atlas-dates` binaries when the workspace
//! is built in `rust/target/release`, and print a clear skip line when it is not.
//! They exist for the two things a double cannot prove: that this crate's rules
//! stand-in answers what the engine answers, and what one page costs.

use std::sync::Arc;

use crate::cards;
use crate::clock::{Clock, Fixed};
use crate::extract;
use crate::doubles::*;
use crate::engine::Engine;
use crate::fetch::Fetcher;
use crate::html;
use crate::rank;
use crate::rules::Rules;
use crate::search::Search;
use crate::fixtures;

// ---------------------------------------------------------------------------
// fidelity: the same questions, asked of the real engines
// ---------------------------------------------------------------------------

#[test]
fn the_real_atlas_text_agrees_with_the_double_about_duplicates() {
    let Some(engines) = real() else {
        println!("skipped: atlas-text / atlas-dates are not built in rust/target/release");
        return;
    };
    let href = "https://www.northgate-herald.com/x?utm_source=newsletter&utm_campaign=pm";
    let bare = "https://www.northgate-herald.com/x";
    assert_eq!(engines.norm_url(href), engines.norm_url(bare));
    assert_eq!(engines.norm_url(href), Doubling::default().norm_url(href));
    assert_eq!(engines.publisher(bare), "Northgate-herald");
    assert_eq!(
        engines.clean_title("Chip exports rise | Northgate Herald", "Northgate-herald"),
        "Chip exports rise"
    );
}

#[test]
fn the_real_atlas_dates_reads_the_dates_the_fixtures_print() {
    let Some(engines) = real() else {
        println!("skipped: atlas-text / atlas-dates are not built in rust/target/release");
        return;
    };
    for (printed, expected) in [
        ("2 hours ago", "2026-11-05T10:00:00Z"),
        ("3 days ago", "2026-11-02T12:00:00Z"),
        ("05.11.2026", "2026-11-05T00:00:00Z"),
    ] {
        assert_eq!(
            engines.date_iso(printed, NOW).as_deref(),
            Some(expected),
            "{}",
            printed
        );
        assert_eq!(
            Doubling::default().date_iso(printed, NOW).as_deref(),
            Some(expected),
            "the double must agree about {}",
            printed
        );
    }
    let window = engines.window("3 days ago", NOW, 48 * 3_600).expect("an answer");
    assert!(!window.fresh);
    assert_eq!(window.reason, "stale");
    let fresh = engines.window("2 hours ago", NOW, 48 * 3_600).expect("an answer");
    assert!(fresh.fresh);
    assert_eq!(fresh.reason, "fresh");
    // A date in the future is not current, whatever the engine printed.
    let skew = engines
        .window("2027-06-01", NOW, 48 * 3_600)
        .expect("an answer");
    assert!(!skew.fresh);
    assert_eq!(skew.reason, "skew");
}

#[test]
fn the_real_engines_read_the_pasted_pages_end_to_end() {
    let Some(engines) = real() else {
        println!("skipped: atlas-text / atlas-dates are not built in rust/target/release");
        return;
    };
    let page = Page::new(fixtures::BING_NEWS);
    let search = Search::new(
        Engine::default(),
        Arc::new(engines) as Arc<dyn Rules>,
        page as Arc<dyn Fetcher>,
    )
    .with_clock(Arc::new(Fixed(NOW)) as Arc<dyn Clock>);
    let answer = search.news_for("chip export rules", NOW - 48 * 3_600, 10);
    assert_eq!(number(&answer, "count"), 2, "{:?}", answer);
    for row in rows(&answer) {
        assert!(!row["published"].as_str().unwrap().is_empty(), "{:?}", row);
        assert!(!row["publisher"].as_str().unwrap().is_empty(), "{:?}", row);
    }
}

// ---------------------------------------------------------------------------
// the numbers, printed: one page, each stage, measured
// ---------------------------------------------------------------------------

#[test]
fn measured_cost_of_one_forty_result_page() {
        let body: &'static str = Box::leak(many(40).into_boxed_str());
    let rules = Doubling::default();
    let started = std::time::Instant::now();
    let markup = html::scan(body);
    let scan = started.elapsed();
    let started = std::time::Instant::now();
    let page = cards::cards(&markup);
    let cards_us = started.elapsed();
    let started = std::time::Instant::now();
    let outcome = extract::extract(&page, "https://e.example.org/?q=x", "e.example.org", &rules);
    let extract_us = started.elapsed();
    let started = std::time::Instant::now();
    let texts: Vec<String> = outcome
        .candidates
        .iter()
        .map(|candidate| format!("{} {}", candidate.hit.title, candidate.hit.snippet))
        .collect();
    let scores = rank::scores("chip exports", &texts);
    let order = rank::order(
        &scores,
        &outcome
            .candidates
            .iter()
            .map(|candidate| candidate.hit.page_rank)
            .collect::<Vec<usize>>(),
    );
    let rank_us = started.elapsed();
    println!(
        "40-result page ({} bytes): scan {:?}, cards {:?}, extract {:?}, rank {:?} ({} ranked)",
        body.len(),
        scan,
        cards_us,
        extract_us,
        rank_us,
        order.len()
    );
    assert_eq!(outcome.candidates.len(), 40);
    // The whole page is parsed and ordered in well under a network round trip.
    assert!(
        scan + cards_us + extract_us + rank_us < std::time::Duration::from_millis(50),
        "the parser is the cheap part"
    );
}
