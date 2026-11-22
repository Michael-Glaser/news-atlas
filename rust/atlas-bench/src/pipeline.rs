//! The per-article pipeline: what one story costs before it is stored.
//!
//! Three numbers, because the app has three shapes of the same work:
//!
//! * `pipeline.engines` — the shipped path: `atlas-ingest`'s [`NativeRules`]
//!   answers three of the five questions in-process (`atlas-places`,
//!   `atlas-topics`, `atlas-dates`) and spawns two binaries for the rest
//!   (`atlas-text`, `atlas-core`). This is what a collected article costs.
//! * `pipeline.spawned` — the same questions asked only as subprocesses
//!   (`NativeRules::spawn_only`). Not a shipped path: it is the witness that
//!   says what a process spawn costs on top of the work, so a regression in the
//!   in-process route shows up as a gap between these two rows.
//! * `pipeline.inprocess` — the three stages that carry a library call, run
//!   directly with the payloads the binaries would receive: the work itself, with
//!   no `atlas-ingest` layer and no transport at all.
//!
//! It does not: decide anything about the article. Every stage is the engine's
//! own function, called with the payload `atlas-ingest` would send.
//!
//! The article is synthetic and 20 kB, the size a real page's text reaches.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::stats::{measure, Samples};
use atlas_ingest::rules::{
    ArticleText, NativeRules, Rules, TopicRow,
};
use atlas_lib::json;
use serde_json::json as sjson;

use crate::fixture::body;

/// The body of the synthetic article, in bytes of plain text.
pub const ARTICLE_BYTES: usize = 20_000;

/// How long one engine call may take before the harness refuses to keep timing.
const ENGINE_TIMEOUT: Duration = Duration::from_secs(30);

/// The HTML page an article arrives as: a title, the body in paragraphs.
pub fn page() -> String {
    let mut rng = crate::fixture::rng(0x5EED);
    let text = body(&mut rng, ARTICLE_BYTES);
    let mut html = String::with_capacity(text.len() + 512);
    // A dateline, so the place engine has a city to find: an article with no
    // geography would measure a lookup that answers "nowhere", which is cheaper
    // than the lookup a real story pays for.
    html.push_str(
        "<!doctype html><html><head><title>Council reviews the quarterly budget</title></head>\
         <body><article><p>BERLIN (Outlets) - The council in Berlin published its quarterly \
         figures.</p>",
    );
    for paragraph in text.split("\n\n") {
        html.push_str("<p>");
        html.push_str(paragraph.trim());
        html.push_str("</p>\n");
    }
    html.push_str("</article></body></html>");
    html
}

/// The two topic rows a reader's workspace carries into a match.
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
    ]
}

/// The directory the engine binaries live in: `rust/target/release`, found from
/// this binary's own location or from `CARGO_MANIFEST_DIR`.
pub fn engine_dir() -> PathBuf {
    let mut candidates: Vec<PathBuf> = Vec::new();
    // Next to this binary (target/release) when the harness runs from there.
    if let Ok(current) = std::env::current_exe() {
        for ancestor in current.ancestors().skip(1).take(6) {
            candidates.push(ancestor.to_path_buf());
        }
    }
    // The workspace's own release directory, whether or not it is where we run.
    candidates.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("target")
            .join("release"),
    );
    for candidate in candidates {
        if atlas_platform::host::app_binary(&candidate, "atlas-core").is_file() {
            return candidate;
        }
    }
    PathBuf::from("")
}

/// The pipeline numbers.
pub struct PipelinePaths {
    pub engines: Samples,
    pub spawned: Samples,
    pub inprocess: Samples,
    /// Where the engines were found, so a report can say what was run.
    pub engine_dir: PathBuf,
    /// What the engines said, so the harness can prove they answered.
    pub located: String,
    pub topics_matched: usize,
}

impl PipelinePaths {
    /// Time both shapes. `engine_dir` must hold the five binaries.
    pub fn run(engine_dir: &Path) -> PipelinePaths {
        let html = page();
        let rules = NativeRules::with_bin_dir(engine_dir, ENGINE_TIMEOUT);
        let article = ArticleText {
            title: "Council reviews the quarterly budget".to_string(),
            summary: "A short summary of the session.".to_string(),
            content: String::new(),
            url: "https://outlet1.example.invalid/story/1".to_string(),
            publisher: "Outlet 1".to_string(),
            entities: vec!["Entity 1".to_string(), "Entity 2".to_string()],
            tags: vec!["tag1".to_string()],
        };

        // Warm-up happens inside `measure`; the text is extracted once per pass so
        // the per-article cost is measureable as one unit.
        let (engines, last) = measure("pipeline.engines", || {
            (ask(&rules, &html, &article), 0)
        });
        assert!(
            last.2 > 5_000,
            "the extract engine must return article text, got {} chars",
            last.2
        );
        assert!(last.3, "the date engine must read an ISO timestamp");

        // The witness: the very same five questions, every one of them a
        // subprocess. A row that no user waits for, kept so the cost of a spawn is
        // a number rather than a belief.
        let spawned_rules = NativeRules::spawn_only(engine_dir, ENGINE_TIMEOUT);
        let (spawned, spawned_last) = measure("pipeline.spawned", || {
            (ask(&spawned_rules, &html, &article), 0)
        });
        assert_eq!(
            (last.0.clone(), last.1, last.3),
            (spawned_last.0.clone(), spawned_last.1, spawned_last.3),
            "the two transports must answer the same article the same way"
        );

        // The in-process shape: three of the five stages, no spawn.
        let (inprocess, _extracted) = measure_with_payload(&html);

        PipelinePaths {
            engines,
            spawned,
            inprocess,
            engine_dir: engine_dir.to_path_buf(),
            located: last.0,
            topics_matched: last.1,
        }
    }

    pub fn all(&self) -> Vec<&Samples> {
        vec![&self.engines, &self.spawned, &self.inprocess]
    }
}

/// One article through the rules: extract, entities, place, topics, dates.
///
/// The same function for both transports, so `pipeline.engines` and
/// `pipeline.spawned` differ in nothing but who answers — which is what makes
/// the gap between the two rows a spawn cost and not a different question.
///
/// Returns what the engines said: the place, the topic matches, the extracted
/// text length, whether the date parsed, and whether the window was fresh.
fn ask(rules: &NativeRules, html: &str, article: &ArticleText) -> (String, usize, usize, bool, bool) {
    let text = rules
        .extract_html(html, &article.url, &article.title)
        .expect("atlas-core extract");
    let entities = rules
        .entity_candidates(&text, &[], 2, 40)
        .expect("atlas-core entities");
    let named: Vec<String> = entities.iter().map(|(name, _)| name.clone()).collect();
    let with_entities = ArticleText {
        content: text,
        entities: named,
        ..article.clone()
    };
    let place = rules.locate(&with_entities).expect("atlas-places locate");
    let matched = rules
        .match_topics(&topics(), &with_entities)
        .expect("atlas-topics match");
    let parsed = rules
        .parse_date("2026-11-20T09:30:00Z", 1_764_000_000)
        .expect("atlas-dates parse");
    let window = rules
        .window("2026-11-20T09:30:00Z", 72 * 3_600, 1_764_000_000)
        .expect("atlas-dates window");
    (
        place.place,
        matched.len(),
        with_entities.content.len(),
        parsed.ok,
        window.fresh,
    )
}

/// The three library-target stages, timed over one extracted article.
///
/// `atlas-core` carries its own JSON type and `atlas-places` uses `atlas-lib`'s,
/// so each payload is built for the crate it is handed to — the same separation
/// the two binaries have.
fn measure_with_payload(html: &str) -> (Samples, String) {
    let (samples, text) = measure("pipeline.inprocess", || {
        let extract_payload = atlas_core::json::parse(
            &sjson!({
                "html": html,
                "url": "https://outlet1.example.invalid/story/1",
                "title": "Council reviews the quarterly budget",
            })
            .to_string(),
        )
        .expect("an extract payload");
        let extracted = atlas_core::extract::run(&extract_payload).expect("extract");
        let answer: serde_json::Value =
            serde_json::from_str(&extracted).expect("extract answers JSON");
        let body = answer
            .get("text")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let entities_payload = atlas_core::json::parse(
            &sjson!({
                "text": body,
                "known": Vec::<String>::new(),
                "min_count": 2,
                "max": 40,
            })
            .to_string(),
        )
        .expect("an entities payload");
        let found = atlas_core::entities::run(&entities_payload).expect("entities");
        let locate_payload = json::parse(
            &sjson!({
                "title": "Council reviews the quarterly budget",
                "summary": "A short summary of the session.",
                "text": body,
                "url": "https://outlet1.example.invalid/story/1",
                "publisher": "Outlet 1",
            })
            .to_string(),
        )
        .expect("a locate payload");
        let place = atlas_places::locate(&locate_payload).expect("locate");
        ((body, found, place), 0)
    });
    (samples, text.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_is_html_with_about_twenty_kilobytes_of_text() {
        let html = page();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<p>"));
        assert!(html.len() > ARTICLE_BYTES, "{} bytes", html.len());
    }

    #[test]
    fn the_engines_are_found_next_to_the_workspace() {
        let dir = engine_dir();
        assert!(
            atlas_platform::host::app_binary(&dir, "atlas-core").is_file(),
            "engine dir was {} — run cargo build --release --offline first",
            dir.display()
        );
    }
}
