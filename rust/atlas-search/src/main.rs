//! `atlas-search <operation>` — one JSON object in, one JSON object out.
//!
//! ```text
//! atlas-search web_search <<< '{"query":"chip export rules","max_results":6}'
//! atlas-search news_for   <<< '{"query":"chip export rules","since":1795251600,"limit":8}'
//! atlas-search find_news  <<< '{"query":"chip export rules","hours":48,"limit":8}'
//! ```
//!
//! The wiring only: argv, stdin, stdout and the exit code
//! ([`atlas_lib::cli::serve`]). The order is chosen so a mistake is reported
//! where it was made: the operation, then the engine templates, then whether the
//! rule engines are built, then the request.
//!
//! The engine comes from the payload's `web` / `news` templates, else
//! `ATLAS_SEARCH_WEB` / `ATLAS_SEARCH_NEWS`, else the plain HTML default. The rule
//! engines (`atlas-text`, `atlas-dates`) are found next to this executable;
//! `ATLAS_BIN_DIR` overrides. Every decision is in [`atlas_search`].

use std::sync::Arc;

use serde_json::Value;

use atlas_search::fetch::Http;
use atlas_search::rules::Engines;
use atlas_search::{Engine, Search};

fn main() {
    atlas_lib::cli::serve(&run);
}

/// One request, already checked against the argument contract.
enum Plan {
    Web(String, usize),
    News(String, i64, usize),
    Find(String, i64, usize),
}

fn run(command: &str, raw: &str) -> Result<String, String> {
    let input: Value = serde_json::from_str(raw.trim())
        .map_err(|_| "input must be one JSON object".to_string())?;
    if !input.is_object() {
        return Err("input must be one JSON object".to_string());
    }
    let default_limit = atlas_search::DEFAULT_RESULTS as i64;
    let plan = match command {
        "web_search" => Plan::Web(
            field(&input, "query").to_string(),
            int(&input, "max_results", default_limit) as usize,
        ),
        "news_for" => Plan::News(
            field(&input, "query").to_string(),
            int(&input, "since", 0),
            int(&input, "limit", default_limit) as usize,
        ),
        "find_news" => Plan::Find(
            field(&input, "query").to_string(),
            int(&input, "hours", 48),
            int(&input, "limit", default_limit) as usize,
        ),
        other => {
            return Err(format!(
                "unknown subcommand: {} (expected one of {})",
                other,
                atlas_search::TOOLS.join(", ")
            ))
        }
    };
    let engine = engine_from(&input)?;
    let engines = Engines::discover()?;
    let search = Search::new(engine, Arc::new(engines), Arc::new(Http::new()));
    let answer = match plan {
        Plan::Web(query, limit) => search.web_search(&query, limit),
        Plan::News(query, since, limit) => search.news_for(&query, since, limit),
        Plan::Find(query, hours, limit) => search.find_news(&query, hours, limit),
    };
    serde_json::to_string(&answer).map_err(|_| "the answer could not be written".to_string())
}

/// The engine from one request: the payload's templates, then the environment,
/// then the defaults.
fn engine_from(input: &Value) -> Result<Engine, String> {
    let web = text(input, "web");
    let news = text(input, "news");
    if web.is_empty() && news.is_empty() {
        Engine::from_env()
    } else {
        Engine::new(&web, &news)
    }
}

fn text(input: &Value, key: &str) -> String {
    input
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn field<'a>(input: &'a Value, key: &str) -> &'a str {
    input.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn int(input: &Value, key: &str, fallback: i64) -> i64 {
    match input.get(key) {
        Some(Value::Number(number)) => number.as_i64().unwrap_or(fallback),
        Some(Value::String(text)) => text.trim().parse::<i64>().unwrap_or(fallback),
        _ => fallback,
    }
}
