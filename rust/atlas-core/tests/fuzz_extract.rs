//! Adversarial input for the HTML extractor: 10 000 mutations, no panic.
//!
//! `atlas-core` is what turns an untrusted page into the text a model reads, so
//! the input here is the worst case: unbalanced tags, half-open comments, entity
//! bombs, invalid UTF-8 that a socket delivered, and a page that is one long run
//! with no whitespace. The claim: `extract` always answers (a body, or the empty
//! string), never panics, and the text it returns is bounded by the page it was
//! given — a page cannot make it allocate megabytes per kilobyte.

use atlas_core::extract;
use atlas_core::json::Json;

/// A normal page, a JSON-LD page, a paragraph-only page and two broken ones: the
/// mutations need a valid skeleton as much as they need garbage.
const SEEDS: &[&str] = &[
    "<html><head><title>Harbour strike ends</title></head><body><article><h1>Harbour strike ends</h1><p>Dockworkers approved a new contract on Tuesday, ending a nine-day strike.</p><p>The union said the agreement covers shift length.</p></article></body></html>",
    "<script type=\"application/ld+json\">{\"@type\":\"NewsArticle\",\"articleBody\":\"One sentence. Two sentences. Three.\"}</script>",
    "<div><p>first</p><p>second</p><p>third</p></div>",
    "<p>&amp;&#x1F600;&notanentity; unclosed",
    "<!-- never closed <article><p>hidden",
    "<article><p>a</p><aside><p>nav</p></aside></article>",
];

/// One extraction on an untrusted page.
fn extract_one(html: &str) -> String {
    let root = Json::Obj(vec![
        ("html".to_string(), Json::Str(html.to_string())),
        ("title".to_string(), Json::Str("hint".to_string())),
    ]);
    extract::run(&root).unwrap_or_default()
}

#[test]
fn ten_thousand_mutated_pages_never_panic_and_stay_linear() {
    let mut longest_out = 0usize;
    let mut worst_ratio = 0.0f64;
    let mut empty = 0usize;
    let started = std::time::Instant::now();

    let stats = atlas_guard::mutate::run(SEEDS, 10_000, 0x_A71A5_0002, |page| {
        let out = extract_one(page);
        if out.is_empty() {
            empty += 1;
            return;
        }
        longest_out = longest_out.max(out.len());
        // The answer is a small JSON object around the chosen text: the escapes
        // for control characters are at most 6 bytes per byte of input, and the
        // title/framing is capped. Anything super-linear would break this.
        assert!(
            out.len() <= page.len() * 8 + 4096,
            "{} bytes of page produced {} bytes of text",
            page.len(),
            out.len()
        );
        worst_ratio = worst_ratio.max(out.len() as f64 / (page.len().max(1)) as f64);
        // It must be a JSON object the caller can read — checked with this
        // crate's own reader, so no dependency is added for a test.
        let value = atlas_core::json::parse(&out).expect("extract produced text that is not JSON");
        assert!(value.get("text").is_some(), "no text field: {:?}", out);
        assert!(value.get("chars").is_some(), "no chars field: {:?}", out);
    });

    let elapsed = started.elapsed();
    println!(
        "{} mutated pages in {:.0} ms (longest output {} bytes, worst ratio {:.1}x, {} empty)",
        stats.runs,
        elapsed.as_secs_f64() * 1e3,
        longest_out,
        worst_ratio,
        empty
    );
    assert_eq!(stats.runs, 10_000);
    assert!(
        elapsed.as_secs_f64() < 20.0,
        "10 000 pages took {:.1} s",
        elapsed.as_secs_f64()
    );
}

#[test]
fn a_megabyte_of_nested_markup_costs_a_linear_extraction() {
    // The shape that makes a naive tree builder quadratic: 60 000 sibling blocks.
    let page = format!(
        "<html><body>{}</body></html>",
        "<div><p>paragraph text here</p></div>".repeat(30_000)
    );
    let started = std::time::Instant::now();
    let out = extract_one(&page);
    let elapsed = started.elapsed();
    println!(
        "{} byte page -> {} bytes of JSON in {:.1} ms",
        page.len(),
        out.len(),
        elapsed.as_secs_f64() * 1e3
    );
    assert!(!out.is_empty());
    assert!(
        elapsed.as_secs_f64() < 5.0,
        "a 1 MB page took {:.1} s",
        elapsed.as_secs_f64()
    );
}

#[test]
fn a_giant_single_token_does_not_become_quadratic() {
    // No whitespace at all: the sentence ungluer and the length filter both see
    // one enormous token.
    let page = format!("<html><body><p>{}</p></body></html>", "a".repeat(500_000));
    let started = std::time::Instant::now();
    let out = extract_one(&page);
    let elapsed = started.elapsed();
    println!("500 kB single token: {:.1} ms", elapsed.as_secs_f64() * 1e3);
    assert!(out.len() < page.len() + 4096);
    assert!(elapsed.as_secs_f64() < 3.0, "{:.1} s", elapsed.as_secs_f64());
}
