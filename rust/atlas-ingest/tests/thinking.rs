//! Thinking, priced and reported: the budgets, the truncation, and the endpoint.
//!
//! The user's demand is two sentences — "at least 8 000 tokens for thinking, at least a
//! 32 000-token context so it can read every article" — and the first one is only real if a
//! run can *see* what the thinking cost. Three things follow, and this file pins all three:
//!
//! 1. **The budget covers the thought and the answer.** `atlas-ai` asks for reasoning by
//!    default, and a reply's `max_tokens` is spent on the reasoning first; a 320-token
//!    ceiling on a 6 000-character article is 320 tokens of thought and an empty answer, and
//!    the pass then fell back to the heuristic. That is the whole of "die KI ist nicht so
//!    gut" as far as this crate goes, so the budgets are `model::ENRICH_MAX_TOKENS` /
//!    `VERDICT_MAX_TOKENS` (8 192) and a scripted model proves the reader reports a spent
//!    budget as a budget, not as a silent model.
//! 2. **The receipt tells the truth about thinking.** A receipt carries the characters of
//!    reasoning, the characters of the answer, the completion tokens the server billed, the
//!    ceiling and the attempts — so a run can say "it thought 1 503 characters and answered
//!    in 214", and an empty answer beside a huge thought is visible as truncation.
//! 3. **The endpoint is the one the rest of the app uses.** Port 8081 comes from
//!    `data/config.json`; `endpoint_string` answered 8080, so every enrichment in the CLI
//!    went to a closed port and the archive received the honest heuristic instead of an
//!    answer.
//!
//! No socket: the model is a scripted [`Model`] (for the pipeline) and a scripted
//! [`LlamaClient`] (for `AiModel`'s own reader), and the fetcher panics if it is asked.
//! The endpoint test manipulates process environment, so it runs under a lock and restores
//! every variable it touches.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use atlas_ai::prompts::{verdict_from_answer, NewsVerdict};
use atlas_ai::{AiError, ChatReply, ChatRequest, Control, LlamaClient, ServerInfo};
use atlas_ingest::fetcher::{Fetched, Fetcher};
use atlas_ingest::model::{as_object, AiModel, Answer, Model};
use atlas_ingest::rules::NativeRules;
use atlas_ingest::save::SaveInput;
use atlas_ingest::{Config, NullBrowser, Pipeline};
use atlas_store::{SqlValue, Store};

// ------------------------------------------------------------------------ fixtures

/// A fetcher that is never called: every article here arrives as text that was already
/// read. Panicking keeps a future change from quietly reaching the network from a test.
struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, url: &str, _max_bytes: usize) -> Fetched {
        panic!("enrichment must not fetch ({})", url);
    }
}

/// The model the pipeline asks: a scripted [`Answer`], which is where thinking is counted.
struct ThinkingModel {
    answer: Mutex<Answer>,
    calls: Mutex<usize>,
}

impl ThinkingModel {
    fn with(answer: Answer) -> Arc<ThinkingModel> {
        Arc::new(ThinkingModel {
            answer: Mutex::new(answer),
            calls: Mutex::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.lock().map(|held| *held).unwrap_or(0)
    }
}

impl Model for ThinkingModel {
    fn model_id(&self) -> String {
        "scripted".to_string()
    }

    fn online(&self) -> bool {
        true
    }

    /// The save gate's question, answered plainly so the article reaches the archive; the
    /// gate under test here is not the model's opinion.
    fn verdict(&self, _title: &str, _text: &str, _url: &str, _publisher: &str) -> NewsVerdict {
        verdict_from_answer("{\"news\": true, \"reason\": \"scripted\"}")
    }

    fn chat(&self, _prompt: &str, _temperature: f32, _max_tokens: u32) -> Result<String, String> {
        Ok(String::new())
    }

    fn json(&self, _prompt: &str, _max_tokens: u32) -> Option<Value> {
        self.answer.lock().ok().and_then(|held| held.value.clone())
    }

    /// The reader under test: the scripted answer, with the ceiling the pipeline passed and
    /// the attempt count a run reports.
    fn json_answer(&self, _prompt: &str, max_tokens: u32) -> Answer {
        if let Ok(mut held) = self.calls.lock() {
            *held += 1;
        }
        let mut answer = self
            .answer
            .lock()
            .map(|held| held.clone())
            .unwrap_or_default();
        answer.budget = max_tokens;
        answer.attempts = 1;
        answer
    }
}

/// One temporary archive and pipeline, removed on drop. The article is stored through the
/// real save path, so what the model is asked is a real article of this archive.
struct Fixture {
    pipeline: Pipeline,
    root: PathBuf,
    article: i64,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A directory no other test uses: the process id plus a counter, because two fixtures alive
/// at once must not share a database.
fn temp_root(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "atlas-ingest-thinking-{}-{}-{}",
        label,
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("data")).expect("data dir");
    root
}

/// The article the model is asked about: two sentences about chips, repeated until the body
/// clears `MIN_CHARS` (700) — a teaser is refused by the save gate, and the point of this
/// fixture is a row that the enrichment pass would really be asked about.
fn body() -> String {
    const ONE: &str = "Nvidia and Microsoft said on Tuesday that the chip export rules \
         published by the United States Commerce Department this week will change which \
         processors reach Chinese data centres. The two companies told customers in Singapore \
         and Malaysia that orders already in transit would continue under the previous terms.";
    let mut text = String::new();
    while text.chars().count() < 900 {
        text.push_str(ONE);
        text.push('\n');
    }
    text
}

fn fixture(label: &str, model: Arc<ThinkingModel>) -> Fixture {
    let root = temp_root(label);
    let store = Arc::new(Store::open(root.join("data").join("atlas.db")).expect("open"));
    store.init().expect("schema");
    store.init_monitors().expect("monitor tables, as the app makes them");
    let config = Config::for_root(&root);
    let rules = NativeRules::discover(&config).expect("the engines answer in this process");
    // One active topic, the way `add_topic` writes it: the save gate refuses an article that
    // matches no topic, and this test is not about that gate.
    store
        .exec(
            "INSERT INTO topics(name,keywords,exclude,min_matches,active,created,hits) \
             VALUES('Chips','chip, export','',1,1,?,0)",
            &[SqlValue::Text(atlas_store::now_iso())],
        )
        .expect("topic");
    let pipeline = Pipeline::new(
        store.clone(),
        config,
        Arc::new(rules),
        model,
        Arc::new(NoFetch),
        Arc::new(NullBrowser),
        None,
    );
    let saved = pipeline.save_article(&SaveInput {
        url: "https://example.com/chips".to_string(),
        title: "Chip export rules tighten".to_string(),
        text: body(),
        summary: "New licences for advanced processors.".to_string(),
        tags: Vec::new(),
        categories: Vec::new(),
        topic: String::new(),
        published: atlas_store::now_iso(),
        publisher: String::new(),
        place: String::new(),
        entities: Vec::new(),
    });
    assert_eq!(
        saved.status(),
        "stored",
        "the fixture needs a stored article: {:?}",
        saved.to_json()
    );
    let article = saved.id().expect("a stored article has an id");
    Fixture {
        pipeline,
        root,
        article,
    }
}

/// The one answer an enrichment call is supposed to produce.
fn good_answer() -> Value {
    json!({
        "entities": [
            {"name": "Nvidia", "type": "ORG"},
            {"name": "Microsoft", "type": "ORG"}
        ],
        "categories": ["Chips"],
        "summary": "Nvidia and Microsoft said the new export licences will change which chips reach Chinese data centres.",
        "sentiment": "neutral"
    })
}

// ------------------------------------------------------------------------ the budget

/// A thought that spends the whole ceiling is reported as a budget, in one sentence naming
/// the number and the fix — never written over by the heuristic.
#[test]
fn a_thought_that_spends_the_budget_is_reported_not_hidden() {
    let model = ThinkingModel::with(Answer {
        value: None,
        // The shape the live server produces when the ceiling is too small: it thought at
        // length, hit `max_tokens`, and the answer never started.
        thinking: 7_842,
        answered: 0,
        tokens: Some(8_192),
        budget: 0, // filled by the pipeline with ENRICH_MAX_TOKENS
        finish_reason: "length".to_string(),
        error: None,
        attempts: 1,
    });
    let fixture = fixture("truncated", model.clone());

    let result = fixture.pipeline.analyze_article(fixture.article, false);
    let sentence = result["error"]
        .as_str()
        .unwrap_or_else(|| panic!("a spent budget is an error, not a heuristic: {result}"));

    assert_eq!(result["truncated"], json!(true), "{result}");
    assert_eq!(result["model"], json!(false), "no model answer was stored: {result}");
    assert_eq!(result["thinking"], json!(7_842));
    assert!(
        sentence.contains("8192") || sentence.contains("8 192"),
        "the sentence must name the budget it hit: {sentence}"
    );
    assert!(
        sentence.contains("ENRICH_MAX_TOKENS"),
        "the sentence must name the constant to raise: {sentence}"
    );
    assert!(
        !sentence.contains('\n') && sentence.chars().count() < 400,
        "one sentence, so it reads as a diagnosis and not as a log: {sentence}"
    );
    assert_eq!(
        model.calls(),
        1,
        "a spent budget is not retried: a second call costs another thought"
    );

    // Nothing was stored as a receipt, so the row is still unanalysed and a re-run after the
    // budget is raised tries again instead of skipping it forever.
    assert!(
        !fixture.pipeline.analyzed(fixture.article),
        "a truncated call must not mark the article analysed"
    );
    assert!(fixture.pipeline.enrichment_record(fixture.article).is_none());
}

/// The receipt carries what the thinking cost: characters of reasoning, characters of
/// answer, the tokens the server billed, the ceiling and the attempts.
#[test]
fn the_receipt_carries_the_thinking_it_paid_for() {
    let model = ThinkingModel::with(Answer {
        value: Some(good_answer()),
        thinking: 1_503,
        answered: 214,
        tokens: Some(641),
        budget: 0,
        finish_reason: "stop".to_string(),
        error: None,
        attempts: 1,
    });
    let fixture = fixture("receipt", model);

    let result = fixture.pipeline.analyze_article(fixture.article, false);
    assert_eq!(result["model"], json!(true), "{result}");
    assert_eq!(result["source"], json!("model"));
    assert_eq!(result["thinking"], json!(1_503));
    assert_eq!(result["answered"], json!(214));
    assert_eq!(result["tokens"], json!(641));
    assert_eq!(
        result["budget"],
        json!(atlas_ingest::model::ENRICH_MAX_TOKENS),
        "the receipt names the ceiling the answer was produced under"
    );
    assert_eq!(result["refused"], json!(0), "the scripted answer named only real names");
    assert_eq!(result["retried"], json!(false));
    assert_eq!(result["attempts"], json!(1));
    assert_eq!(
        result["note"],
        json!(""),
        "the model answered, so there is nothing to explain"
    );
    // The answer reached the graph. Which counter moves depends on what the save pass had
    // already linked from the body — the ingest heuristic names and types a capitalised word
    // the moment the article is stored, so a model answer usually *retypes* (measured here:
    // 2) rather than adds a fresh name. Either way the model's JSON is what the archive now
    // holds, and a receipt with neither counter at zero would be an answer that went nowhere.
    let entities = result["entities"].as_u64().unwrap_or(0);
    let retyped = result["retyped"].as_u64().unwrap_or(0);
    assert!(
        entities + retyped >= 1,
        "a model answer stores the names it named: {result}"
    );

    // And the stored receipt — what the window and a later run read — says the same thing.
    let stored = fixture
        .pipeline
        .enrichment_record(fixture.article)
        .expect("the receipt is stored with the analysis");
    assert_eq!(stored["thinking"], json!(1_503), "{stored}");
    assert_eq!(stored["tokens"], json!(641));
    assert_eq!(
        stored["budget"],
        json!(atlas_ingest::model::ENRICH_MAX_TOKENS)
    );
}

/// Reasoning is not an answer, in either place it can arrive: the reader refuses it as JSON,
/// and a reply whose only text is its working is a truncation.
#[test]
fn an_answer_that_is_only_reasoning_is_still_refused() {
    let reasoning = "Let me think. The article names Nvidia and Microsoft, so the answer is \
                     {\"entities\": [{\"name\": \"Nvidia\"}], \"categories\": [\"Chips\"]}";
    assert!(
        as_object(reasoning).is_none(),
        "the model's working must never be read as its answer"
    );

    let model = ThinkingModel::with(Answer {
        value: as_object(reasoning),
        thinking: reasoning.chars().count(),
        answered: 0,
        tokens: Some(512),
        budget: 0,
        finish_reason: "length".to_string(),
        error: None,
        attempts: 1,
    });
    let fixture = fixture("reasoning-only", model);
    let result = fixture.pipeline.analyze_article(fixture.article, false);
    assert_eq!(result["truncated"], json!(true), "{result}");
    assert_eq!(result["model"], json!(false));
    assert!(!fixture.pipeline.analyzed(fixture.article));
}

/// A server that is not there still falls back to the heuristic — with the reason in the
/// receipt, so "absent" and "thought too long" are different sentences in a run's output.
#[test]
fn an_absent_model_falls_back_and_says_why() {
    let model = ThinkingModel::with(Answer::absent("connection refused on 127.0.0.1:8081"));
    let fixture = fixture("absent", model);

    let result = fixture.pipeline.analyze_article(fixture.article, false);
    assert_eq!(result["source"], json!("heuristic"), "{result}");
    assert_eq!(result["model"], json!(false));
    assert_eq!(result["thinking"], json!(0));
    assert_eq!(result["truncated"], Value::Null, "an absent server is not a truncation");
    let note = result["note"].as_str().unwrap_or_default();
    assert!(
        note.contains("connection refused"),
        "the heuristic receipt says why the model was not asked: {note}"
    );
    // The fallback still stores a receipt: the article was analysed, just not by the model.
    assert!(fixture.pipeline.analyzed(fixture.article));
}

// ------------------------------------------------------------------------ the endpoint

/// The endpoint is resolved in the order the window uses, and `data/config.json` is part of
/// it. It used not to be: `data/config.json` said 8081, this function answered 8080, and
/// every enrichment in the CLI went to a closed port.
///
/// One test, one lock, every variable restored: `std::env` is process-wide and these tests
/// share a process.
#[test]
fn the_endpoint_follows_the_environment_then_the_config_then_the_default() {
    static ENVIRONMENT: Mutex<()> = Mutex::new(());
    let _guard = ENVIRONMENT
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());

    let names = ["ATLAS_MODEL_URL", "ATLAS_LLAMA_PORT", "ATLAS_ROOT"];
    let saved: Vec<Option<String>> = names.iter().map(|name| std::env::var(name).ok()).collect();
    let root = temp_root("endpoint");
    let config = root.join("data").join("config.json");
    std::env::set_var("ATLAS_ROOT", &root);
    std::env::remove_var("ATLAS_MODEL_URL");
    std::env::remove_var("ATLAS_LLAMA_PORT");
    let default = format!("http://127.0.0.1:{}", atlas_ingest::DEFAULT_LLAMA_PORT);

    // 4. No environment, no config: the crate's default, which is `atlas-config`'s.
    assert_eq!(atlas_ingest::endpoint_string(), default);

    // 1. The full URL wins over everything.
    std::env::set_var("ATLAS_MODEL_URL", "http://127.0.0.1:9099");
    std::env::set_var("ATLAS_LLAMA_PORT", "8082");
    std::fs::write(&config, "{\"llama_port\": 8083}").expect("config");
    assert_eq!(atlas_ingest::endpoint_string(), "http://127.0.0.1:9099");

    // 2. Then the port variable, over the file.
    std::env::remove_var("ATLAS_MODEL_URL");
    assert_eq!(atlas_ingest::endpoint_string(), "http://127.0.0.1:8082");

    // 3. Then the file — the case that was broken: the pipeline asked 8080 while the server
    //    that the launcher and the window use answers on 8081.
    std::env::remove_var("ATLAS_LLAMA_PORT");
    assert_eq!(atlas_ingest::endpoint_string(), "http://127.0.0.1:8083");

    // A port of zero is not a port: a zero and a bad number both fall through to the file
    // instead of becoming `127.0.0.1:0`.
    std::env::set_var("ATLAS_LLAMA_PORT", "0");
    assert_eq!(atlas_ingest::endpoint_string(), "http://127.0.0.1:8083");
    std::env::set_var("ATLAS_LLAMA_PORT", "not a port");
    assert_eq!(atlas_ingest::endpoint_string(), "http://127.0.0.1:8083");
    std::env::remove_var("ATLAS_LLAMA_PORT");

    // A config that is not JSON, or has no port, is simply absent.
    std::fs::write(&config, "not json").expect("config");
    assert_eq!(atlas_ingest::endpoint_string(), default);
    std::fs::write(&config, "{\"llama_port\": 70000}").expect("config");
    assert_eq!(atlas_ingest::endpoint_string(), default);
    std::fs::write(&config, "{}").expect("config");
    assert_eq!(atlas_ingest::endpoint_string(), default);

    for (name, value) in names.iter().zip(saved.iter()) {
        match value {
            Some(text) => std::env::set_var(name, text),
            None => std::env::remove_var(name),
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

// ------------------------------------------------------------------------ the reader

/// `AiModel`'s own reader: a reply that thought and stopped is a spent budget, and one that
/// thought and answered carries both counts. Driven by a scripted client, so no server and
/// no 45 seconds are involved.
#[test]
fn the_reader_counts_the_thought_and_the_answer_separately() {
    struct Scripted {
        reply: Mutex<Vec<ChatReply>>,
        /// What each request asked for: the ceiling and whether the model may think.
        asked: Mutex<Vec<(u32, bool)>>,
    }

    impl LlamaClient for Scripted {
        fn model(&self) -> &str {
            "scripted"
        }

        fn complete(&self, request: &ChatRequest) -> Result<ChatReply, AiError> {
            if let Ok(mut asked) = self.asked.lock() {
                asked.push((request.max_tokens, request.enable_thinking));
            }
            self.reply
                .lock()
                .ok()
                .and_then(|mut held| {
                    if held.is_empty() {
                        None
                    } else {
                        Some(held.remove(0))
                    }
                })
                .ok_or_else(|| AiError::Unreachable("no scripted reply left".to_string()))
        }

        fn complete_stream(
            &self,
            _request: &ChatRequest,
            _on_delta: &mut dyn FnMut(&str) -> Control,
        ) -> Result<ChatReply, AiError> {
            Err(AiError::Protocol("the batch does not stream".to_string()))
        }

        fn probe(&self) -> Result<ServerInfo, AiError> {
            Ok(ServerInfo::offline("scripted"))
        }
    }

    /// A reply with the fields llama.cpp sends.
    fn reply(content: &str, thinking: &str, finish: &str, tokens: u64) -> ChatReply {
        ChatReply {
            model: "scripted".to_string(),
            content: content.to_string(),
            thinking: thinking.to_string(),
            calls: Vec::new(),
            finish_reason: finish.to_string(),
            usage_tokens: Some(tokens),
            truncated: false,
        }
    }

    let thought = "thought ".repeat(300);
    let client = Arc::new(Scripted {
        reply: Mutex::new(vec![reply("", &thought, "length", 8_192)]),
        asked: Mutex::new(Vec::new()),
    });
    let model = AiModel::with_client(client.clone());
    let answer = model.json_answer("ask", atlas_ingest::model::ENRICH_MAX_TOKENS);

    assert!(answer.value.is_none(), "an empty reply is not an answer");
    assert_eq!(answer.thinking, thought.chars().count());
    assert_eq!(answer.answered, 0);
    assert!(answer.thought_then_ran_out());
    assert_eq!(answer.tokens, Some(8_192));
    assert_eq!(answer.attempts, 1, "a spent budget is not retried");
    let sentence = answer.budget_sentence("ENRICH_MAX_TOKENS");
    assert!(sentence.contains("ENRICH_MAX_TOKENS"), "{sentence}");
    assert!(
        sentence.contains(&atlas_ingest::model::ENRICH_MAX_TOKENS.to_string()),
        "the sentence names the ceiling: {sentence}"
    );

    // The request really carried the ceiling and asked for thinking.
    let asked = client.asked.lock().expect("the scripted client");
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].0, atlas_ingest::model::ENRICH_MAX_TOKENS);
    assert!(
        asked[0].1,
        "the batch lets the model think; the budget is there for it"
    );
    drop(asked);

    // And a reply that thought *and* answered carries both numbers.
    let answered = "{\"entities\": [{\"name\": \"Nvidia\"}]}";
    let client = Arc::new(Scripted {
        reply: Mutex::new(vec![reply(answered, &"thought ".repeat(40), "stop", 262)]),
        asked: Mutex::new(Vec::new()),
    });
    let model = AiModel::with_client(client);
    let answer = model.json_answer("ask", atlas_ingest::model::ENRICH_MAX_TOKENS);
    assert!(answer.value.is_some(), "one JSON object is the answer");
    assert_eq!(answer.thinking, "thought ".repeat(40).chars().count());
    assert_eq!(answer.answered, answered.chars().count());
    assert_eq!(answer.tokens, Some(262));
    assert!(!answer.thought_then_ran_out());
    assert_eq!(answer.attempts, 1);
}

/// The floor the user asked for, asserted where a future edit would lower it by accident.
#[test]
fn the_budgets_are_at_least_the_users_floor_and_leave_room_for_the_answer() {
    assert!(
        atlas_ingest::model::ENRICH_MAX_TOKENS >= 8_000,
        "the enrichment ceiling must cover thinking, not just the JSON answer"
    );
    assert!(
        atlas_ingest::model::VERDICT_MAX_TOKENS >= 8_000,
        "the verdict ceiling must cover thinking too: the gate asks the same model"
    );
    // A thinking answer on a long article takes minutes, not a 15-second rules timeout.
    assert!(
        atlas_ingest::model::ENRICH_TIMEOUT >= std::time::Duration::from_secs(120),
        "the model's deadline has to outlast a thought: it was 40 s and never read"
    );
}
