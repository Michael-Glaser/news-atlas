//! Does the token-shape redaction eat real content?
//!
//! `atlas_guard::redact::redact` started life on log lines, where a false positive costs
//! one unreadable field. It is now also asked *before* something is written, in three
//! places, and there the cost is the opposite: a false positive **refuses legitimate
//! data**.
//!
//! * `atlas_store::meta` (`pair_is_secret`) builds `name = value` and rejects the pair when
//!   `hits > 0` — a config value that looks token-shaped can no longer be saved.
//! * `atlas_store::requests` (`push`) rejects a queue payload when `hits > 0` — a request
//!   the model legitimately issued (a URL to fetch, a body to read) can no longer be
//!   queued.
//! * `atlas_store::error` rewrites the text, `atlas_config::warn` and the log writers
//!   *print* the marker, so there the cost is only readability.
//!
//! So the question this file answers, with the archive as the witness, is not "does the
//! rule work" (its own unit tests prove that) but "does the rule stay out of the way of
//! things that are not credentials". The corpora are read straight out of the user's own
//! archive, **read-only**, and are never filtered: a hit is reported with its corpus, its
//! row, the string and the span, and the test fails on it. Weakening the corpus would be
//! the one way to make this file lie.
//!
//! Read-only matters twice: `SQLITE_OPEN_READ_ONLY` keeps the connection from writing, and
//! `PRAGMA query_only` is the second lock in case a future edit drops the flag. The whole
//! test is offline — it opens one file and asks one pure function.
//!
//! What is here, and what each one measured on 2026-11-22 against the live archive:
//!
//! | test | corpus | result |
//! | --- | --- | --- |
//! | `real_article_titles_and_bodies_survive_redaction` | 200 titles + 200 bodies | 0 hits |
//! | `real_article_urls_survive_redaction` | the first 200 stored URLs | 0 hits |
//! | `model_authored_summaries_survive_redaction` | at least 50 model/`enrich` summaries (56 today) | 0 hits |
//! | `address_bearing_bodies_lose_the_address_and_nothing_else` | the 14 bodies quoting a contact address | 8 hits, every one a whole address |
//! | `every_stored_url_survives_redaction` | all 1439 stored URLs | **231 hits** |
//! | `every_article_body_loses_only_addresses` | all 683 bodies over 200 chars | **4 hits that are not addresses** |
//! | `real_meta_values_are_not_refused_as_secrets` | all 74 live `meta` rows, through `pair_is_secret`'s own probe | **1 row would be refused** |
//! | `a_real_credential_is_still_caught` | the two positive controls | both redacted |
//!
//! The three bold rows are the findings, and they are the reason the file is red: the
//! redaction is right about the corpora the brief named and wrong about the archive around
//! them. Each finding is written up for the crate that owns the fix — `redact.rs` is not
//! this file's to edit.

use std::path::{Path, PathBuf};

use atlas_guard::redact::redact;
use rusqlite::{Connection, OpenFlags};

/// The archive, resolved from the crate rather than the cwd so `cargo test` works from
/// anywhere. Crate dir is `rust/atlas-ingest`, so the repo root is two levels up.
fn archive_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("data")
        .join("atlas.db")
}

/// Open the archive read-only. `None` when this machine has no archive yet (a fresh
/// checkout, CI): the test then says so loudly and skips, which is the repo's idiom for
/// a corpus that lives outside the crate. It never creates the file.
fn archive() -> Option<Connection> {
    let path = archive_path();
    if !path.is_file() {
        eprintln!(
            "SKIP: no archive at {} — the corpus test needs the user's own data/atlas.db \
             (read-only, never written)",
            path.display()
        );
        return None;
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    let conn = Connection::open_with_flags(&path, flags)
        .unwrap_or_else(|err| panic!("open {} read-only: {err}", path.display()));
    // Second lock: even a read-only connection that somehow got a write statement would be
    // refused here rather than touch the live archive.
    conn.pragma_update(None, "query_only", true)
        .expect("pragma query_only");
    Some(conn)
}

/// One redaction hit, kept whole so a failure message can be acted on without a rerun.
struct Hit {
    corpus: &'static str,
    row: i64,
    field: &'static str,
    hits: usize,
    /// Byte span of the first replaced run, and the text that was replaced.
    span: (usize, usize),
    replaced: String,
    /// ±60 characters around the span: enough to judge whether the run was a credential
    /// or a word, without dumping a whole article body into a test log.
    context: String,
}

/// The markers `atlas_guard::redact` writes, longest first so a prefix match cannot pick
/// the shorter one.
const MARKERS: [&str; 2] = ["[redacted]", "[email]"];

/// Every span the redaction actually replaced, aligned against the markers it wrote.
///
/// A plain prefix/suffix diff is not enough: with two replacements in one body it reports
/// the *region between them*, which would make a body look as if more was eaten than was.
/// Here the walk is byte-exact — copy while the two strings agree, and where they part the
/// output has a marker, so the replaced run ends where the input and the output resynchronise
/// 24 bytes later (or at the end of both). The test never re-implements a rule: it only
/// locates what `redact` itself decided.
fn spans_of(input: &str, output: &str) -> Vec<(usize, usize)> {
    let a = input.as_bytes();
    let b = output.as_bytes();
    let (mut i, mut j) = (0usize, 0usize);
    let mut spans: Vec<(usize, usize)> = Vec::new();
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            i += 1;
            j += 1;
            continue;
        }
        let Some(marker) = MARKERS.iter().find(|m| b[j..].starts_with(m.as_bytes())) else {
            // Not a shape this file knows: stop rather than guess. The hit count from
            // `redact` still stands, so the caller's assertion is unaffected.
            break;
        };
        let after = j + marker.len();
        let mut length = None;
        let mut len = 1usize;
        while i + len <= a.len() {
            if input.is_char_boundary(i + len) {
                let tail_in = &a[i + len..];
                let tail_out = &b[after..];
                let both_empty = tail_in.is_empty() && tail_out.is_empty();
                let window = tail_in.len().min(tail_out.len()).min(24);
                let resync = window > 0 && tail_in[..window] == tail_out[..window];
                if both_empty || resync {
                    length = Some(len);
                    break;
                }
            }
            len += 1;
        }
        let Some(len) = length else { break };
        spans.push((i, i + len));
        i += len;
        j = after;
    }
    spans
}

/// A byte that can sit *inside* an address-like token. Used only to ask whether a replaced
/// span stopped in the middle of one, never to find the address itself — the redactor is the
/// only thing that decides where an address begins and ends.
fn word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '%' | '+' | '@' | '.')
}

/// ±60 characters of context around `span`, on character boundaries.
fn context_of(text: &str, span: (usize, usize)) -> String {
    let mut lo = span.0.saturating_sub(60);
    while lo > 0 && !text.is_char_boundary(lo) {
        lo -= 1;
    }
    let mut hi = (span.1 + 60).min(text.len());
    while hi < text.len() && !text.is_char_boundary(hi) {
        hi += 1;
    }
    format!("…{}…", &text[lo..hi])
}

/// Redact one field and record a hit if anything was replaced.
fn check(hits: &mut Vec<Hit>, corpus: &'static str, row: i64, field: &'static str, text: &str) {
    let outcome = redact(text);
    if outcome.hits == 0 {
        return;
    }
    let spans = spans_of(text, &outcome.text);
    let span = spans.first().copied().unwrap_or((0, 0));
    hits.push(Hit {
        corpus,
        row,
        field,
        hits: outcome.hits,
        span,
        replaced: text[span.0..span.1].to_string(),
        context: context_of(text, span),
    });
}

fn report(corpus: &'static str, checked: usize, hits: &[Hit]) {
    println!("corpus {corpus}: {checked} strings checked, {} hit(s)", hits.len());
    for hit in hits.iter().filter(|h| h.corpus == corpus) {
        println!(
            "  row {} field {}: {} replacement(s); span {}..{} replaced {:?}\n    context: {}",
            hit.row, hit.field, hit.hits, hit.span.0, hit.span.1, hit.replaced, hit.context
        );
    }
}

fn message(corpus: &'static str, hits: &[Hit]) -> String {
    let mut out = format!(
        "{corpus}: the redaction ate {} real string(s). A hit here is a false positive, \
         and the same string is refused by atlas_store's `hits > 0` gates (meta pair / \
         queue payload) or mangled in an error message and a log line.\n",
        hits.iter().filter(|h| h.corpus == corpus).count()
    );
    for hit in hits.iter().filter(|h| h.corpus == corpus).take(10) {
        out.push_str(&format!(
            "  row {} {} .. span {}..{} replaced {:?} in {:?}\n",
            hit.row, hit.field, hit.span.0, hit.span.1, hit.replaced, hit.context
        ));
    }
    out
}

/// (a) Real article text: 200 titles and their bodies.
///
/// The filter (`length(title) >= 10`, `length(content) >= 200`) exists so the corpus is
/// actual prose rather than a stub row, not to dodge anything; `ORDER BY id` keeps the
/// selection deterministic so a failure names the same rows next time.
#[test]
fn real_article_titles_and_bodies_survive_redaction() {
    let Some(conn) = archive() else { return };
    let mut stmt = conn
        .prepare(
            "SELECT id, title, content FROM articles \
             WHERE length(title) >= 10 AND length(content) >= 200 ORDER BY id LIMIT 200",
        )
        .expect("prepare articles");
    let rows: Vec<(i64, String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query articles")
        .collect::<Result<_, _>>()
        .expect("read articles");
    assert!(!rows.is_empty(), "the archive has no article text to check");

    let mut hits = Vec::new();
    let mut bodies = 0usize;
    for (id, title, content) in &rows {
        check(&mut hits, "(a) article", *id, "title", title);
        check(&mut hits, "(a) article", *id, "content", content);
        bodies += 2;
    }
    report("(a) article", bodies, &hits);
    assert_eq!(
        hits.len(),
        0,
        "{}",
        message("(a) article", &hits)
    );
}

/// (b) Real article URLs: 200 of them, exactly as they are stored.
///
/// A URL is the shape most likely to trip the token rule (every path segment and query
/// value counts as a value position inside one whitespace-free run), and the shape most
/// likely to be *refused* by `atlas_store::requests::push` when the model asks the app to
/// fetch it — so this corpus protects a live feature, not just a log line.
#[test]
fn real_article_urls_survive_redaction() {
    let Some(conn) = archive() else { return };
    let mut stmt = conn
        .prepare("SELECT id, url FROM articles WHERE url <> '' ORDER BY id LIMIT 200")
        .expect("prepare urls");
    let rows: Vec<(i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query urls")
        .collect::<Result<Vec<_>, _>>()
        .expect("read urls");
    assert!(!rows.is_empty(), "the archive has no urls to check");

    let mut hits = Vec::new();
    for (id, url) in &rows {
        check(&mut hits, "(b) url", *id, "url", url);
    }
    report("(b) url", rows.len(), &hits);
    assert_eq!(hits.len(), 0, "{}", message("(b) url", &hits));
}

/// (c) at least 50 model-authored summaries.
///
/// The archive is honest about how few of these it holds: `alerts.summary` (17 rows) and
/// `alerts.analysis` (17 rows) are model prose by construction — two of those 34 fields are
/// empty in this archive, so 32 strings — and `articles.summary` is written by
/// `atlas_ingest::enrich` (`enrich.rs:1268`), of which 18 from enriched rows completes the
/// 50. The table does not record whether a given summary came from the model or from the
/// feed, so this split is the honest maximum, not a guess at provenance the schema lost.
///
/// The total is a **floor**, not an equality: the archive grows while the monitor runs (it
/// read 56 on 2026-11-23 after three more alerts had landed), and an exact count on live
/// data goes red the next time the model answers anything — a failure that says nothing
/// about redaction. What the test must prove is that the corpus is populated and that every
/// string in it survives, so `checked` is only required to hold the 50 measured above.
#[test]
fn model_authored_summaries_survive_redaction() {
    let Some(conn) = archive() else { return };
    let mut hits = Vec::new();
    let mut checked = 0usize;

    let mut stmt = conn
        .prepare("SELECT id, summary, analysis FROM alerts ORDER BY id")
        .expect("prepare alerts");
    let rows: Vec<(i64, Option<String>, Option<String>)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query alerts")
        .collect::<Result<Vec<_>, _>>()
        .expect("read alerts");
    for (id, summary, analysis) in &rows {
        for (field, value) in [("alerts.summary", summary), ("alerts.analysis", analysis)] {
            if let Some(text) = value.as_deref().filter(|t| !t.trim().is_empty()) {
                check(&mut hits, "(c) model prose", *id, field, text);
                checked += 1;
            }
        }
    }

    let mut stmt = conn
        .prepare(
            "SELECT id, summary FROM articles \
             WHERE length(summary) > 0 ORDER BY id LIMIT 18",
        )
        .expect("prepare summaries");
    let rows: Vec<(i64, Option<String>)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query summaries")
        .collect::<Result<Vec<_>, _>>()
        .expect("read summaries");
    for (id, summary) in &rows {
        if let Some(text) = summary.as_deref().filter(|t| !t.trim().is_empty()) {
            check(&mut hits, "(c) model prose", *id, "articles.summary", text);
            checked += 1;
        }
    }

    report("(c) model prose", checked, &hits);
    assert!(
        checked >= 50,
        "the corpus is meant to hold the 50 model-authored strings in the table above, got {checked}"
    );
    assert_eq!(hits.len(), 0, "{}", message("(c) model prose", &hits));
}

/// Redact every body `sql` returns and require every replaced span to be a whole address
/// token.
///
/// An address in a log line is exactly what the e-mail rule is for, so `hits > 0` is the
/// *intended* answer for a body that quotes a press-release contact; the question here is
/// **how much** the rule takes. A span that swallowed the prose around an address would be
/// real content eaten, and the same string is refused outright by `atlas_store`'s `hits > 0`
/// gates (`meta::pair_is_secret`, `requests::push`).
fn sweep_bodies(corpus: &'static str, sql: &str) {
    let Some(conn) = archive() else { return };
    let mut stmt = conn.prepare(sql).expect("prepare bodies");
    let rows: Vec<(i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query bodies")
        .collect::<Result<Vec<_>, _>>()
        .expect("read bodies");
    println!("{corpus}: {} bodies", rows.len());

    let mut offenders = Vec::new();
    let mut rows_with_hits = 0usize;
    let mut total_spans = 0usize;
    for (id, content) in &rows {
        let outcome = redact(content);
        if outcome.hits == 0 {
            continue;
        }
        rows_with_hits += 1;
        let spans = spans_of(content, &outcome.text);
        total_spans += spans.len();
        for span in &spans {
            let replaced = &content[span.0..span.1];
            // Precise test: the replaced run must be a *whole* address token, not a piece cut
            // out of a longer word. A span that begins or ends in the middle of a word would
            // mean the reader loses characters the redactor never decided about.
            let before_ok = span.0 == 0
                || !content[..span.0]
                    .chars()
                    .next_back()
                    .is_some_and(word_char);
            let after_ok = span.1 == content.len()
                || !content[span.1..]
                    .chars()
                    .next()
                    .is_some_and(|c| word_char(c) && c != '.');
            if !replaced.contains('@') || !before_ok || !after_ok {
                offenders.push((
                    *id,
                    *span,
                    replaced.to_string(),
                    context_of(content, *span),
                ));
            }
        }
        if let Some(first) = spans.first() {
            println!(
                "  row {id}: {} replacement(s); first span {}..{} replaced {:?}\n    context: {}",
                outcome.hits,
                first.0,
                first.1,
                &content[first.0..first.1],
                context_of(content, *first)
            );
        }
    }
    println!(
        "{corpus}: {rows_with_hits} of {} bodies produced a hit, {} span(s) total, {} of them \
         not a whole address token",
        rows.len(),
        total_spans,
        offenders.len()
    );

    assert!(
        offenders.is_empty(),
        "{corpus}: the redaction ate text that is not an address in {} place(s): {:?}",
        offenders.len(),
        offenders
            .iter()
            .map(|(id, span, replaced, context)| format!(
                "row {id} span {}..{} replaced {replaced:?} in {context:?}",
                span.0, span.1
            ))
            .collect::<Vec<_>>()
    );
}

/// Every body that carries an address-shaped run (`@`), redacted and inspected span by span.
#[test]
fn address_bearing_bodies_lose_the_address_and_nothing_else() {
    sweep_bodies(
        "(e) address bodies",
        "SELECT id, content FROM articles \
         WHERE content LIKE '%@%' AND length(content) > 200 ORDER BY id",
    );
}

/// Every stored body, so the 200-row sample in corpus (a) cannot hide a second rule that
/// eats prose. Addresses are allowed and must be whole; anything else fails.
#[test]
fn every_article_body_loses_only_addresses() {
    sweep_bodies(
        "(a) every body",
        "SELECT id, content FROM articles WHERE length(content) > 200 ORDER BY id",
    );
}

/// The positive controls. Without these the three corpora above prove only that the
/// function returns zero, which a stub would also do: a 40-byte mixed-case run carrying a
/// digit, in a value position, and a named credential must both be replaced.
#[test]
fn a_real_credential_is_still_caught() {
    // 40 bytes: 4 groups of 10, mixed case, digits, no separators — the shape of an opaque
    // API key. Built by repetition so the length is visibly 40.
    let token: String = ["Ab3dEf6hIj", "9Kl2Mn5Op8", "Qr1St4Uv7W", "x0Yz3A2bC1"]
        .concat()
        .chars()
        .take(40)
        .collect();
    assert_eq!(token.len(), 40, "the control token must be exactly 40 bytes");
    let probe = format!("search_engine = {token}");
    let outcome = redact(&probe);
    assert!(
        outcome.hits >= 1,
        "a 40-byte mixed-case token in a value position must be redacted; \
         hits={} text={:?}",
        outcome.hits,
        outcome.text
    );
    assert!(
        !outcome.text.contains(&token),
        "the token survived redaction: {}",
        outcome.text
    );

    let named = "password = hunter2";
    let outcome = redact(named);
    assert!(
        outcome.hits >= 1,
        "a named password must be redacted; hits={} text={:?}",
        outcome.hits,
        outcome.text
    );
    assert!(
        !outcome.text.contains("hunter2"),
        "the password survived redaction: {}",
        outcome.text
    );
}

/// The measurement has teeth.
///
/// Zero hits on 650 strings is only meaningful if this file would notice one: a corpus that
/// is somehow never fed to the redactor would also report zero. So the same `check` path the
/// three corpora use is handed a real archive body with a named credential appended, and it
/// must record exactly one hit — which is what `assert_eq!(hits.len(), 0, message(..))` in
/// each corpus test turns into a failure naming the row, the span and the surrounding text.
#[test]
fn the_harness_records_a_credential_hidden_in_a_real_body() {
    let Some(conn) = archive() else { return };
    let body: String = conn
        .query_row(
            "SELECT content FROM articles WHERE length(content) > 500 ORDER BY id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("one real body");
    assert_eq!(
        redact(&body).hits,
        0,
        "the chosen real body must be clean before the credential is seeded into it"
    );

    let seeded = format!("{body}\n\npassword = hunter2\n");
    let mut hits = Vec::new();
    check(&mut hits, "(d) seeded", 1, "content", &seeded);
    assert_eq!(
        hits.len(),
        1,
        "the measurement path must record a credential seeded into a real body"
    );
    assert!(hits[0].replaced.contains("hunter2"), "{:?}", hits[0].replaced);
    assert!(
        !redact(&seeded).text.contains("hunter2"),
        "the seeded credential survived redaction"
    );
}

/// The gate itself, fed the app's own live meta table.
///
/// `atlas_store::meta::pair_is_secret` (`meta.rs:107`) asks exactly this question — it builds
/// `name = value` and refuses the write when the redaction reports a hit — so this test
/// replays the probe over all 74 rows of the real `meta` table and asserts the answer is "not
/// a secret" for every one. A hit here is a setting the user could never save, among them the
/// 8 KB `research_result` and the `enrich:` / `reason:` model prose.
///
/// Values are never printed: a meta value can be private by nature, so a failure names the
/// key and the length of the span, which is what a person needs to go and look at it.
#[test]
fn real_meta_values_are_not_refused_as_secrets() {
    let Some(conn) = archive() else { return };
    let mut stmt = conn
        .prepare("SELECT k, v FROM meta ORDER BY k")
        .expect("prepare meta");
    let rows: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query meta")
        .collect::<Result<Vec<_>, _>>()
        .expect("read meta");
    assert!(!rows.is_empty(), "the archive has no meta rows to check");

    let mut refused = Vec::new();
    for (key, value) in &rows {
        let probe = format!("{key} = {value}");
        let outcome = redact(&probe);
        if outcome.hits > 0 {
            let spans = spans_of(&probe, &outcome.text);
            let widths: Vec<usize> = spans.iter().map(|(a, b)| b - a).collect();
            refused.push((key.clone(), outcome.hits, widths));
        }
    }
    println!(
        "meta gate: {} rows probed as `name = value`, {} would be refused",
        rows.len(),
        refused.len()
    );
    for (key, hits, widths) in &refused {
        println!("  key {key}: {hits} hit(s), span widths {widths:?}");
    }
    assert!(
        refused.is_empty(),
        "atlas_store::meta would refuse to save {} real setting(s): {refused:?}",
        refused.len()
    );
}

/// The whole `url` column, not just the first 200 rows.
///
/// Corpus (b) above follows the brief's 200 rows, and that sample is a hole: `ORDER BY id
/// LIMIT 200` happens to contain no Google News link, while the archive holds 231 of them
/// (`https://news.google.com/rss/articles/CBMi…` — a base64url article id: mixed case, digits,
/// `-` and `_`, well past 40 bytes). So this test asks the redactor about **every** stored
/// URL. A hit here is not a cosmetic marker: `articles.url` is where the app says a story
/// lives, the same string goes into `read_article` requests and into error messages.
#[test]
fn every_stored_url_survives_redaction() {
    let Some(conn) = archive() else { return };
    let mut stmt = conn
        .prepare("SELECT id, url FROM articles WHERE url <> '' ORDER BY id")
        .expect("prepare all urls");
    let rows: Vec<(i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query all urls")
        .collect::<Result<Vec<_>, _>>()
        .expect("read all urls");
    assert!(!rows.is_empty(), "the archive has no urls to check");

    let mut hits = Vec::new();
    for (id, url) in &rows {
        check(&mut hits, "(b) every url", *id, "url", url);
    }
    report("(b) every url", rows.len(), &hits);
    assert_eq!(
        hits.len(),
        0,
        "{}",
        message("(b) every url", &hits)
    );
}
