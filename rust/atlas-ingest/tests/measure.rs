//! The numbers behind the performance claims, measured rather than guessed.
//!
//! These are `#[ignore]`d: they print timings, they assert nothing about them, so
//! a slow laptop never turns the suite red. Run them with
//!
//! ```text
//! cargo test --release --offline -p atlas-ingest --test measure -- --ignored --nocapture
//! ```
//!
//! Nothing here touches the network, a model or a page: only the text cache (a
//! file per page) and `hash` — the parts whose cost is this crate's own.

use std::path::PathBuf;
use std::time::Instant;

use atlas_ingest::TextCache;

/// A body of the size the pipeline keeps: 20 000 characters of real prose shape.
fn body(seed: usize) -> String {
    let mut out = String::with_capacity(20_000);
    let mut n = seed;
    while out.len() < 20_000 {
        n = n.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        out.push_str("Der Ausschuss fuer Wirtschaft und Handel hat am Donnerstag ");
        out.push_str(&format!("den Bericht Nummer {} vorgelegt. ", n % 10_000));
        out.push_str("Die Zahlen zeigen, wie stark sich die Ausfuhren verschoben haben. ");
    }
    out.chars().take(20_000).collect()
}

#[test]
#[ignore]
fn the_text_cache_cost_per_page() {
    let dir: PathBuf = std::env::temp_dir().join("atlas-ingest-measure-cache");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("cache dir");
    let mut cache = TextCache::new(dir.clone(), 64, 500);
    let pages: Vec<String> = (0..200).map(body).collect();

    let started = Instant::now();
    for (i, text) in pages.iter().enumerate() {
        assert!(cache.put(&format!("https://example.com/{}", i), "", "Title", text, "2026-01-01T00:00:00Z"));
    }
    let put = started.elapsed();

    // 64 pages fit in memory: this is a warm hit, no file is touched
    let started = Instant::now();
    for i in 0..64 {
        assert!(cache.get(&format!("https://example.com/{}", i)).is_some());
    }
    let get_memory = started.elapsed();

    // a cold read: the memory copy is gone, the file is not
    let mut cold = TextCache::new(dir.clone(), 64, 500);
    let started = Instant::now();
    for i in 0..64 {
        assert!(cold.get(&format!("https://example.com/{}", i)).is_some());
    }
    let get_disk = cold_elapsed(started);

    eprintln!(
        "cache: 200 x 20 kB put = {:.2} ms ({:.3} ms/page), \
         warm get = {:.3} ms ({:.4} ms/page), cold get = {:.2} ms ({:.4} ms/page)",
        put.as_secs_f64() * 1000.0,
        put.as_secs_f64() * 1000.0 / 200.0,
        get_memory.as_secs_f64() * 1000.0,
        get_memory.as_secs_f64() * 1000.0 / 64.0,
        get_disk.as_secs_f64() * 1000.0,
        get_disk.as_secs_f64() * 1000.0 / 64.0,
    );
    assert_eq!(cold.stats().hits, 64);
    assert_eq!(cold.stats().misses, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

fn cold_elapsed(started: Instant) -> std::time::Duration {
    started.elapsed()
}

#[test]
#[ignore]
fn hashing_a_url_costs_nothing_worth_measuring() {
    let urls: Vec<String> = (0..10_000)
        .map(|i| format!("https://example.com/2026/01/artikel-{}?utm_source=x", i))
        .collect();
    let started = Instant::now();
    let mut total = 0usize;
    for url in &urls {
        total += atlas_ingest::hash::sha1_hex(url.as_bytes()).len();
    }
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "sha1: 10 000 URLs in {:.2} ms ({:.4} ms each)",
        elapsed,
        elapsed / 10_000.0
    );
    assert_eq!(total, 10_000 * 40);
}

#[test]
#[ignore]
fn what_a_twenty_kilobyte_clone_costs() {
    let page = body(7);
    let started = Instant::now();
    let mut total = 0usize;
    for _ in 0..200 {
        total += page.clone().len();
    }
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "clone: 200 x 20 kB = {:.2} ms ({:.3} ms each)",
        elapsed,
        elapsed / 200.0
    );
    assert!(total > 0);
}
