//! The `atlas-ai` binary's decisions: four questions, one JSON answer each.
//!
//! The Python layer can adopt these one at a time while `app.py` still exists, and
//! every answer is one JSON object on stdout so `server/aicore.py` can read it
//! without guessing:
//!
//! ```text
//! atlas-ai health
//! {"ok":true,"answering":true,"model":"…","context_size":6144,"base":"http://127.0.0.1:8080"}
//!
//! atlas-ai chat --message "What happened in the chip market?"
//! {"ok":true,"content":"…","steps":1,"tokens":42,"stopped":"answered"}
//!
//! atlas-ai verdict --title "…" --url "https://…" --text "…"
//! {"ok":true,"news":true,"reason":"…","source":"model"}
//!
//! atlas-ai analyze --id 42 --title "…" --summary "…" --text-file page.txt
//! {"ok":true,"entities":[{"name":"…","type":"ORG"}],"summary":"…","sentiment":"neutral"}
//! ```
//!
//! Exit 0 when the question was answered (including "the model is not running",
//! which is a fact, not a failure); 1 when it could not be answered; 2 for bad
//! usage. **Nothing is logged**: no prompt, no article text, no model output ever
//! goes to stderr. The base URL and model come from `atlas-config`, never from a
//! port compiled in here.
//!
//! It does not: call a tool, run the agent loop or parse a model answer — the
//! library does that. This file is argv, stdin, stdout and the exit code.

use std::io::Read;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::chat::{chat_simple, ChatMessage, ChatRequest, HttpLlamaClient, LlamaClient};
use crate::prompts;
use crate::health;

const USAGE: &str = "\
usage:
  atlas-ai health   [--base URL] [--timeout SECONDS]
  atlas-ai chat     (--message TEXT | --message-file PATH|-) [--system TEXT | --system-file PATH|-]
                    [--temperature 0.0-2.0] [--max-tokens 1-8192] [--base URL] [--timeout SECONDS]
  atlas-ai verdict  [--title T] [--text T | --text-file PATH|-] [--url U] [--publisher P]
                    [--base URL] [--timeout SECONDS]
  atlas-ai analyze  --title T [--summary S] [--text T | --text-file PATH|-] [--id N]
                    [--base URL]
  atlas-ai specs    print the tool schemas the model is offered
  atlas-ai version

--temperature and --max-tokens are clamped to the range above: a number outside it
is not a setting the local model can honour, so the request is bounded instead of
being sent as asked. --timeout is clamped to 0.001-3600 seconds.
";

/// The per-job sampling numbers. They live in [`crate::sampling::Job`] — one table,
/// tested, bounded — so this binary cannot drift from the library that serves the
/// same three questions.
use crate::sampling::Job;

pub fn serve() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match args.first() {
        Some(command) => command.clone(),
        None => {
            eprint!("{}", USAGE);
            return ExitCode::from(2);
        }
    };
    let flags = match Flags::parse(&args[1..]) {
        Ok(flags) => flags,
        Err(reason) => {
            eprintln!("{}", reason);
            eprint!("{}", USAGE);
            return ExitCode::from(2);
        }
    };

    let answer = match command.as_str() {
        "health" => health(&flags),
        "chat" => chat(&flags),
        "verdict" => verdict(&flags),
        "analyze" => analyze(&flags),
        "specs" => Ok(specs()),
        "version" | "--version" => Ok(json!({ "ok": true, "version": crate::VERSION })),
        "help" | "--help" | "-h" => {
            print!("{}", USAGE);
            return ExitCode::SUCCESS;
        }
        other => Err(format!("unknown command: {}", other)),
    };

    match answer {
        Ok(value) => {
            println!("{}", value);
            ExitCode::SUCCESS
        }
        Err(reason) => {
            // The reason is a configuration or transport sentence, never a prompt.
            println!("{}", json!({ "ok": false, "error": reason }));
            ExitCode::from(1)
        }
    }
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

fn health(flags: &Flags) -> Result<Value, String> {
    let timeout = flags.duration("timeout", crate::HEALTH_TIMEOUT);
    let client = open(flags, timeout)?;
    let (info, error) = health::check(&client);
    let mut value = info.to_json();
    if let Some(object) = value.as_object_mut() {
        object.insert("ok".to_string(), Value::Bool(true));
        object.insert("base".to_string(), Value::String(base_url(flags)?));
        if let Some(error) = error {
            object.insert("detail".to_string(), Value::String(error.human()));
        }
    }
    Ok(value)
}

fn chat(flags: &Flags) -> Result<Value, String> {
    // One completion, the way `ai.py::chat_simple` made one: the caller's prompt as
    // the user message, an optional system message, thinking off. The agent loop is a
    // library API (`crate::agent`): a tool loop needs handlers, and this binary
    // deliberately has none.
    let message = match flags.get("message-file") {
        Some(path) => read_text_file(&path)?,
        None => flags.required("message")?,
    };
    let system = match flags.get("system-file") {
        Some(path) => Some(read_text_file(&path)?),
        None => flags.get("system").map(|value| value.to_string()),
    };
    let timeout = flags.duration("timeout", crate::DEFAULT_TIMEOUT);
    let client = open(flags, timeout)?;
    let mut messages: Vec<ChatMessage> = Vec::new();
    if let Some(system) = system {
        messages.push(ChatMessage::system(system));
    }
    messages.push(ChatMessage::user(message));
    let mut request = ChatRequest::new(client.model().to_string(), messages);
    let (temperature, max_tokens) = Job::Chat.sampling();
    request.temperature = flags.float("temperature", temperature);
    request.max_tokens =
        flags.number("max-tokens", max_tokens as i64).clamp(1, crate::sampling::MAX_TOKENS as i64) as u32;
    // A stream is what the server uses; the CLI wants the whole answer in one object.
    let reply = client.complete(&request).map_err(|error| error.human())?;
    Ok(json!({
        "ok": true,
        "content": reply.content,
        "model": reply.model,
        "finish_reason": reply.finish_reason,
        "tokens": reply.usage_tokens,
    }))
}

fn verdict(flags: &Flags) -> Result<Value, String> {
    let timeout = flags.duration("timeout", crate::DEFAULT_TIMEOUT);
    let client = open(flags, timeout)?;
    // The page text arrives on stdin when it is long enough to matter: an article
    // body does not belong in argv, where any process could read it.
    let text = match flags.get("text-file") {
        Some(path) => read_text_file(&path)?,
        None => flags.text("text"),
    };
    let prompt = prompts::news_prompt(
        &flags.text("title"),
        &text,
        &flags.text("url"),
        &flags.text("publisher"),
    );
    // A page with no model to judge it is never filtered: that is the rule, not a
    // fallback. The answer says `source: "no-model"`.
    let (temperature, max_tokens) = Job::Verdict.sampling();
    match chat_simple(&client, &prompt, temperature, max_tokens) {
        Ok(raw) => {
            let mut value = prompts::verdict_from_answer(&raw).to_json();
            if let Some(object) = value.as_object_mut() {
                object.insert("ok".to_string(), Value::Bool(true));
            }
            Ok(value)
        }
        Err(error) => {
            let mut value = prompts::unchecked_verdict(&error).to_json();
            if let Some(object) = value.as_object_mut() {
                object.insert("ok".to_string(), Value::Bool(true));
                object.insert("detail".to_string(), Value::String(error.human()));
            }
            Ok(value)
        }
    }
}

fn analyze(flags: &Flags) -> Result<Value, String> {
    let title = flags.required("title")?;
    let summary = flags.text("summary");
    let content = match flags.get("text-file") {
        Some(path) => read_text_file(&path)?,
        None => flags.text("text"),
    };
    let timeout = flags.duration("timeout", crate::DEFAULT_TIMEOUT);
    let client = open(flags, timeout)?;
    let prompt = prompts::analysis_prompt(&title, &summary, &content);
    let (temperature, max_tokens) = Job::Analysis.sampling();
    let raw = chat_simple(&client, &prompt, temperature, max_tokens)
        .map_err(|error| error.human())?;
    Ok(coerce_analysis(&raw, flags.number("id", -1)))
}

/// The model's answer, checked before anything could be stored — the same rules
/// `ai.py::analyze_article` applied, moved here so every caller gets them.
fn coerce_analysis(raw: &str, article_id: i64) -> Value {
    let parsed = crate::parse_json_loose(raw).unwrap_or(Value::Null);
    let object = parsed.as_object().cloned().unwrap_or_else(Map::new);
    let mut entities: Vec<Value> = Vec::new();
    let mut dropped = 0usize;
    if let Some(list) = object.get("entities").and_then(|e| e.as_array()) {
        for item in list.iter().take(40) {
            let name = item
                .as_object()
                .map(|e| prompts::entity_name(&scalar(e.get("name"))))
                .unwrap_or_default();
            if name.is_empty() {
                dropped += 1;
                continue;
            }
            let kind = prompts::entity_type(&scalar(
                item.as_object().and_then(|e| e.get("type")),
            ));
            entities.push(json!({ "name": name, "type": kind }));
        }
    }
    let sentiment = scalar(object.get("sentiment")).trim().to_ascii_lowercase();
    let sentiment = if matches!(sentiment.as_str(), "positive" | "neutral" | "negative") {
        sentiment
    } else {
        String::new()
    };
    let mut value = json!({
        "ok": true,
        "entities": entities,
        "summary": atlas_guard::cleantext::clean(&scalar(object.get("summary")), Some(600)).text,
        "sentiment": sentiment,
    });
    if let Some(id) = value.as_object_mut() {
        if article_id >= 0 {
            id.insert("article_id".to_string(), Value::Number(article_id.into()));
        }
        if dropped > 0 {
            id.insert(
                "rejected".to_string(),
                Value::String(format!(
                    "{} entity candidate(s) dropped (empty, too long or not a name)",
                    dropped
                )),
            );
        }
    }
    value
}

fn specs() -> Value {
    json!({
        "ok": true,
        "tools": crate::tool_specs()
            .iter()
            .map(|spec| spec.wire())
            .collect::<Vec<Value>>(),
    })
}

// ---------------------------------------------------------------------------
// plumbing
// ---------------------------------------------------------------------------

/// Build the client. Base URL from `--base` or from `atlas-config`; the model id
/// from the server itself when it will say, so a request is never addressed to a
/// model that is not loaded.
fn open(flags: &Flags, timeout: Duration) -> Result<HttpLlamaClient, String> {
    let base = base_url(flags)?;
    let client = HttpLlamaClient::new(&base, "local", timeout).map_err(|error| error.human())?;
    // One probe, purely to learn the id. A server that does not answer is not an
    // error here: the command's own call will report it with a better sentence.
    let model = client
        .probe()
        .map(|info| info.model)
        .unwrap_or_else(|_| "local".to_string());
    HttpLlamaClient::new(&base, &model, timeout).map_err(|error| error.human())
}

/// `--base`, or the port `atlas-config` resolved (`ATLAS_LLAMA_PORT` >
/// `data/config.json` > default). Never a port written into this file.
fn base_url(flags: &Flags) -> Result<String, String> {
    if let Some(base) = flags.get("base") {
        return Ok(base.to_string());
    }
    match atlas_config::Config::load() {
        Ok(config) => Ok(format!("http://127.0.0.1:{}", config.llama_port)),
        Err(error) => Err(format!(
            "cannot read the ATLAS configuration ({}); pass --base http://127.0.0.1:PORT",
            error
        )),
    }
}

fn read_text_file(path: &str) -> Result<String, String> {
    if path == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .take(4 * 1024 * 1024)
            .read_to_string(&mut buffer)
            .map_err(|error| format!("cannot read stdin: {}", error))?;
        return Ok(buffer);
    }
    let bytes = std::fs::read(path).map_err(|error| format!("cannot read {}: {}", path, error))?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

fn scalar(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// `--key value` pairs. No combined `--key=value`, no positional arguments: one
/// shape, refused loudly when it does not fit.
struct Flags {
    values: Vec<(String, String)>,
}

impl Flags {
    fn parse(args: &[String]) -> Result<Flags, String> {
        let mut values: Vec<(String, String)> = Vec::new();
        let mut index = 0usize;
        while index < args.len() {
            let arg = &args[index];
            let key = arg
                .strip_prefix("--")
                .ok_or_else(|| format!("unexpected argument {:?}", arg))?;
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("--{} needs a value", key))?;
            values.push((key.to_string(), value.clone()));
            index += 2;
        }
        Ok(Flags { values })
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.values
            .iter()
            .rev()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    fn text(&self, key: &str) -> String {
        self.get(key).unwrap_or("").to_string()
    }

    fn required(&self, key: &str) -> Result<String, String> {
        match self.get(key) {
            Some(value) if !value.trim().is_empty() => Ok(value.to_string()),
            _ => Err(format!("--{} is required", key)),
        }
    }

    fn number(&self, key: &str, default: i64) -> i64 {
        self.get(key)
            .and_then(|value| value.trim().parse::<i64>().ok())
            .unwrap_or(default)
    }

    fn float(&self, key: &str, default: f32) -> f32 {
        self.get(key)
            .and_then(|value| value.trim().parse::<f32>().ok())
            .unwrap_or(default)
    }

    fn duration(&self, key: &str, default: Duration) -> Duration {
        match self.get(key).and_then(|value| value.trim().parse::<f64>().ok()) {
            Some(seconds) if seconds > 0.0 => Duration::from_secs_f64(seconds.min(3600.0)),
            _ => default,
        }
    }
}

#[cfg(test)]
mod tests {
use super::*;

fn flags(list: &[&str]) -> Flags {
    let owned: Vec<String> = list.iter().map(|item| item.to_string()).collect();
    Flags::parse(&owned).unwrap()
}

#[test]
fn flags_are_pairs_and_mistakes_are_loud() {
    let parsed = flags(&["--title", "T", "--summary", "S"]);
    assert_eq!(parsed.text("title"), "T");
    assert_eq!(parsed.text("summary"), "S");
    assert_eq!(parsed.text("missing"), "");
    assert!(Flags::parse(&["--title".to_string()]).is_err(), "a flag with no value");
    assert!(Flags::parse(&["title".to_string()]).is_err(), "a bare word");
}

#[test]
fn defaults_hold_when_a_number_is_nonsense() {
    let parsed = flags(&["--max-tokens", "many"]);
    assert_eq!(parsed.number("max-tokens", 7), 7);
    assert_eq!(parsed.float("temperature", 0.3), 0.3);
    assert_eq!(parsed.duration("timeout", Duration::from_secs(5)), Duration::from_secs(5));
}

#[test]
fn an_analysis_answer_is_checked_before_it_could_be_stored() {
    let raw = r#"{
        "entities": [
            {"name": "Angela Merkel", "type": "person"},
            {"name": "<b>x</b>", "type": "ORG"},
            {"name": "ab", "type": "LOC"},
            {"name": "Nvidia", "type": "GEOGRAPHY"},
            "not an object"
        ],
        "summary": "  Two   sentences.  ",
        "sentiment": "POSITIVE"
    }"#;
    let out = coerce_analysis(raw, 42);
    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["article_id"], json!(42));
    let entities = out["entities"].as_array().unwrap();
    assert_eq!(entities.len(), 2);
    assert_eq!(entities[0], json!({ "name": "Angela Merkel", "type": "PERSON" }));
    assert_eq!(entities[1], json!({ "name": "Nvidia", "type": "ORG" }));
    assert_eq!(out["sentiment"], json!("positive"));
    assert_eq!(out["summary"], json!("Two sentences."));
    assert!(out["rejected"].as_str().unwrap().contains("3 entity"));
}

#[test]
fn a_model_answer_that_is_not_json_stores_nothing() {
    let out = coerce_analysis("I cannot help with that.", 7);
    assert_eq!(out["entities"], json!([]));
    assert_eq!(out["summary"], json!(""));
    assert_eq!(out["sentiment"], json!(""));
}
}

