//! `atlas-ingest` — the pipeline on the workspace's stdin/stdout contract.
//!
//! ```text
//! atlas-ingest read      <<< '{"url":"https://example.com/a","title":"…"}'
//! atlas-ingest save      <<< '{"url":"…","title":"…","summary":"…","topic":"Chips"}'
//! atlas-ingest tag       <<< '{"id":12,"tags":["chip export","semiconductors"]}'
//! atlas-ingest research  <<< '{"topic":"chip export rules","max_steps":14}'
//! atlas-ingest brief     <<< '{"scope":"country","key":"DEU","hours":72}'
//! atlas-ingest enrich    <<< '{"ids":[12,13],"force":false}'   (or {"repair":3} / {"limit":5})
//! atlas-ingest status    <<< '{}'
//! ```
//!
//! Success prints one JSON object on stdout and exits 0; failure prints
//! `{"error": "…"}` and exits 1 — never a panic, never a partial answer, exactly
//! like every other ATLAS binary (`atlas_lib::cli`).
//!
//! `read`, `save` and `tag` go through the **same handlers** the agent loop calls
//! (`crate::handlers`), so the bridge and the model cannot get different answers
//! for the same arguments — including the argument validation.
//!
//! Nothing here loops, polls or starts a background job: `research` runs one pass
//! and returns.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use atlas_ingest::{Config, Pipeline};
use atlas_store::Store;

fn main() {
    atlas_lib::cli::serve(&run);
}

fn run(cmd: &str, raw: &str) -> Result<String, String> {
    let input: Value = if raw.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(raw).map_err(|_| "input must be a JSON object".to_string())?
    };
    let object = match input {
        Value::Object(map) => map,
        _ => return Err("input must be a JSON object".to_string()),
    };
    let root = std::env::var("ATLAS_ROOT").unwrap_or_else(|_| ".".to_string());
    let config = Config::for_root(&root);
    match cmd {
        // `read` / `save` / `tag` are the short spellings; `read_article`,
        // `save_article`, `tag_article` and every other tool name in
        // `atlas_ai::tool_specs()` are accepted as subcommands too, so the bridge
        // can call any of the 23 tools without a second dispatch table here.
        "read" | "save" | "tag" => {
            let tool = match cmd {
                "read" => "read_article",
                "save" => "save_article",
                _ => "tag_article",
            };
            call_tool(&config, tool, &object)
        }
        "research" => {
            let pipeline = build(&config)?;
            let topic = object
                .get("topic")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let steps = object
                .get("max_steps")
                .and_then(Value::as_i64)
                .map(|value| value.clamp(1, 40) as usize);
            Ok(pipeline.research(&topic, steps).to_string())
        }
        "enrich" => {
            let pipeline = build(&config)?;
            let answer = if object.get("repair").is_some() {
                let limit = object
                    .get("repair")
                    .and_then(Value::as_u64)
                    .unwrap_or(3)
                    .clamp(1, 50) as usize;
                pipeline.repair(limit)
            } else if let Some(ids) = object.get("ids").and_then(Value::as_array) {
                let ids: Vec<i64> = ids.iter().filter_map(Value::as_i64).collect();
                let force = object.get("force").and_then(Value::as_bool).unwrap_or(false);
                pipeline.analyze_batch(&ids, force)
            } else {
                let limit = object
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(5)
                    .clamp(1, 50) as usize;
                pipeline.backfill(limit)
            };
            Ok(answer.to_string())
        }
        // One briefing: the digest of the scope's stories, one model call, one stored row.
        // The args are the queue payload (`scope`, `key`, `hours`), read the same way
        // `research` reads its topic, and the answer is already the JSON object the caller
        // speaks — `{"ok":true,…,"stored":1}` or `{"ok":false,"error":"…"}`, exit 0 either
        // way, because a refusal is an answer and not a crash.
        "brief" => {
            let pipeline = build(&config)?;
            let scope = object
                .get("scope")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let key = object
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            // The archive's own window when the caller names none: a briefing of "everything"
            // must mean the app's window, not seven days of stale rows.
            let hours = object
                .get("hours")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| pipeline.window_hours());
            Ok(pipeline.brief(&scope, &key, hours).to_string())
        }
        "status" => Ok(status(&config)),
        // Every other tool name the registry knows, e.g. `find_news`, `web_search`,
        // `search_news`, `top_entities`, `list_topics`, `graph_neighbors`. The name
        // is checked against the specs *before* the pipeline is built, so a typo
        // costs no database open and no engine lookup.
        other => {
            let mut known: Vec<String> = atlas_ai::tool_specs()
                .iter()
                .map(|spec| spec.name.clone())
                .collect();
            known.sort();
            if !known.iter().any(|name| name == other) {
                return Err(format!(
                    "unknown subcommand: {} (expected read | save | tag | research | \
                     brief | enrich | status, or one of: {})",
                    other,
                    known.join(", ")
                ));
            }
            call_tool(&config, other, &object)
        }
    }
}

/// Run one tool by name through the same registry the agent loop uses.
fn call_tool(config: &Config, tool: &str, args: &Map<String, Value>) -> Result<String, String> {
    let pipeline = build(config)?;
    let registry = atlas_ingest::handlers::registry(pipeline);
    let answer = registry
        .call(tool, args)
        .map_err(|error| format!("{} failed: {}", tool, error))?;
    Ok(answer.to_string())
}

fn build(config: &Config) -> Result<Arc<Pipeline>, String> {
    let path = config.root.join("data").join("atlas.db");
    let store = Store::open(&path).map_err(|error| error.message().to_string())?;
    store.init().map_err(|error| error.message().to_string())?;
    let pipeline =
        Pipeline::native(Arc::new(store), config.clone()).map_err(|error| error.message().to_string())?;
    Ok(Arc::new(pipeline))
}

/// What this binary can and cannot do on this machine — the answer the boot gate
/// and `/api/status.native` read. Never a page's text, only paths and verdicts.
///
/// `required` is the engines that must exist **as a file**
/// (`rules::SPAWNED`, empty: all five answer from this process), so a machine with
/// no engine binary is not a machine that cannot run the pipeline. The list is
/// still reported, and `in_process` names the engines a missing file cannot take
/// away.
fn status(config: &Config) -> String {
    let mut missing: Vec<String> = Vec::new();
    let mut present: Vec<String> = Vec::new();
    for engine in atlas_ingest::rules::SPAWNED {
        let path = config.bin(engine);
        if path.is_file() {
            present.push((*engine).to_string());
        } else {
            missing.push((*engine).to_string());
        }
    }
    // Which engine will answer, in the same two words the tool result carries. A
    // reader who configured none is not left with "none": the built-in default runs,
    // and the line says so instead of implying search is unavailable.
    let engine = atlas_ingest::search::resolve(
        config.search_engine.as_deref(),
        None,
        std::env::var(atlas_ingest::search::ENV_KEY).ok().as_deref(),
    );
    let engine_source = engine
        .as_ref()
        .map(|engine| engine.source().label())
        .unwrap_or("none");
    let engine_host = engine.map(|engine| engine.host()).unwrap_or_default();
    json!({
        "required": atlas_ingest::rules::SPAWNED,
        "in_process": atlas_ingest::rules::inproc::ENGINES,
        "present": present,
        "missing": missing,
        "native": missing.is_empty(),
        "cache_dir": config.cache_dir.display().to_string(),
        "search_engine": engine_source,
        "search_engine_host": engine_host,
        "loopback_only": true,
    })
    .to_string()
}
