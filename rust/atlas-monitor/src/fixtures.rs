//! The fakes and the fixtures the check tests share.
//!
//! A fake research pass says exactly what it stored, a fixed judge says exactly
//! which score it gave, and the store is in memory: nothing here talks to a model,
//! opens a socket or reads a page. `mod fixtures` is compiled for tests only.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};

use atlas_ai::{AiError, ChatReply, ChatRequest, LlamaClient, ServerInfo};
use atlas_store::Store;

use crate::candidates::Article;
use crate::judge::{Judge, Judgement};
use crate::research::{Research, ResearchOutcome};
use crate::schedule::Clock;
use crate::service::MonitorService;
use crate::spec::Draft;
use crate::verdict::{Scorer, Verdict};
use crate::wiring::Settings;

pub(crate) fn store() -> Arc<Store> {
    let store = Store::open_memory().expect("memory store");
    store.init().expect("schema");
    store.init_monitors().expect("monitor schema");
    Arc::new(store)
}

pub(crate) fn insert_article(store: &Store, id: i64, fetched: &str, title: &str, relevant: bool) {
    store
        .exec(
            "INSERT INTO articles(id,url,title,summary,content,published,fetched,relevant,publisher) \
             VALUES(?,?,?,?,?,?,?,?,'Example')",
            &[
                atlas_store::SqlValue::Integer(id),
                atlas_store::SqlValue::Text(format!("https://example.com/{}", id)),
                atlas_store::SqlValue::Text(title.to_string()),
                atlas_store::SqlValue::Text("the summary of the story".to_string()),
                atlas_store::SqlValue::Text("the body of the story".to_string()),
                atlas_store::SqlValue::Text(fetched.to_string()),
                atlas_store::SqlValue::Text(fetched.to_string()),
                atlas_store::SqlValue::Integer(i64::from(relevant)),
            ],
        )
        .expect("insert article");
}

/// A research pass that reports exactly what the test says it stored.
pub(crate) struct FakeResearch {
    stored: Vec<i64>,
    failure: Option<String>,
    calls: Mutex<usize>,
}

impl FakeResearch {
    pub(crate) fn new(stored: Vec<i64>) -> Arc<FakeResearch> {
        Arc::new(FakeResearch {
            stored,
            failure: None,
            calls: Mutex::new(0),
        })
    }

    pub(crate) fn failing(reason: &str) -> Arc<FakeResearch> {
        Arc::new(FakeResearch {
            stored: Vec::new(),
            failure: Some(reason.to_string()),
            calls: Mutex::new(0),
        })
    }

    pub(crate) fn calls(&self) -> usize {
        *self.calls.lock().expect("calls")
    }
}

impl Research for FakeResearch {
    fn research(&self, _query: &str, _max_steps: usize) -> ResearchOutcome {
        *self.calls.lock().expect("calls") += 1;
        ResearchOutcome {
            ok: self.failure.is_none(),
            error: self.failure.clone(),
            stored: self.stored.clone(),
            answer: String::new(),
            steps: 3,
            tool_calls: 5,
            tools: if self.failure.is_none() {
                vec!["web_search".to_string(), "save_article".to_string()]
            } else {
                Vec::new()
            },
        }
    }
}

/// A research pass that blocks until the test lets it go — the "one check at a
/// time" case, without a sleep that guesses.
pub(crate) struct BlockingResearch {
    entered: Arc<(Mutex<bool>, Condvar)>,
    release: Arc<(Mutex<bool>, Condvar)>,
    stored: Vec<i64>,
}

impl BlockingResearch {
    /// A pass that says "I have started" on `entered` and then waits for `release`.
    pub(crate) fn new(
        entered: Arc<(Mutex<bool>, Condvar)>,
        release: Arc<(Mutex<bool>, Condvar)>,
        stored: Vec<i64>,
    ) -> Arc<BlockingResearch> {
        Arc::new(BlockingResearch {
            entered,
            release,
            stored,
        })
    }
}

impl Research for BlockingResearch {
    fn research(&self, _query: &str, _max_steps: usize) -> ResearchOutcome {
        {
            let (lock, condvar) = &*self.entered;
            *lock.lock().expect("entered") = true;
            condvar.notify_all();
        }
        {
            let (lock, condvar) = &*self.release;
            let mut released = lock.lock().expect("release");
            while !*released {
                released = condvar.wait(released).expect("release wait");
            }
        }
        ResearchOutcome {
            ok: true,
            stored: self.stored.clone(),
            ..ResearchOutcome::default()
        }
    }
}

/// A judge with one fixed answer, so a test controls the score exactly.
pub(crate) struct FixedJudge {
    score: i64,
    not_news: bool,
    calls: Mutex<usize>,
}

impl FixedJudge {
    /// A judge that always says "reporting", with this score.
    pub(crate) fn scoring(score: i64) -> Arc<FixedJudge> {
        Arc::new(FixedJudge {
            score,
            not_news: false,
            calls: Mutex::new(0),
        })
    }

    /// A judge whose only answer is "this is not reporting" — whatever the score.
    pub(crate) fn not_news() -> Arc<FixedJudge> {
        Arc::new(FixedJudge {
            score: 10,
            not_news: true,
            calls: Mutex::new(0),
        })
    }

    pub(crate) fn calls(&self) -> usize {
        *self.calls.lock().expect("calls")
    }
}

impl Judge for FixedJudge {
    fn judge(&self, _monitor: &crate::spec::Monitor, _article: &Article) -> Judgement {
        *self.calls.lock().expect("calls") += 1;
        if self.not_news {
            return Judgement::NotNews {
                reason: "a guide, not a report".to_string(),
            };
        }
        Judgement::Scored(Verdict::new(
            Scorer::Model,
            self.score,
            vec!["export".to_string()],
            "the article reports the new rule",
            "",
            vec!["Ministry".to_string()],
        ))
    }
}

pub(crate) fn service(
    store: Arc<Store>,
    research: Arc<dyn Research>,
    judge: Arc<dyn Judge>,
    clock: Arc<dyn Clock>,
) -> Arc<MonitorService> {
    Arc::new(MonitorService::new(
        store,
        research,
        judge,
        clock,
        Settings::default(),
    ))
}

pub(crate) fn add_monitor(service: &MonitorService, query: &str, threshold: i64) -> i64 {
    service
        .add(&Draft::parse("Chips", query, Some(threshold), None, None).expect("draft"))
        .expect("add monitor")
}

// -- a scripted model -------------------------------------------------------

/// What the scripted model answers next: the model's text, or the failure the
/// real client would report.
pub(crate) enum Scripted {
    Says(&'static str),
    Fails(AiError),
}

/// A model whose every answer is written down in the test.
///
/// It records the prompt it was given, so a test can read what the judge actually
/// sent (`prompts()`), and it answers from a queue, so a test can script a
/// failure followed by an answer — the retry — or two failures in a row (an
/// outage). Nothing here opens a socket: it implements the same [`LlamaClient`]
/// the production `HttpLlamaClient` does.
pub(crate) struct ScriptedModel {
    script: Mutex<Vec<Scripted>>,
    prompts: Mutex<Vec<String>>,
}

impl ScriptedModel {
    pub(crate) fn new(script: Vec<Scripted>) -> Arc<ScriptedModel> {
        Arc::new(ScriptedModel {
            script: Mutex::new(script),
            prompts: Mutex::new(Vec::new()),
        })
    }

    /// One answer, then the same answer for ever: the shape most tests want.
    pub(crate) fn always(answer: &'static str) -> Arc<ScriptedModel> {
        ScriptedModel::new(vec![Scripted::Says(answer)])
    }

    /// Every prompt this model was asked, in order.
    pub(crate) fn prompts(&self) -> Vec<String> {
        self.prompts.lock().expect("prompts").clone()
    }

    /// How many questions it was asked.
    pub(crate) fn calls(&self) -> usize {
        self.prompts.lock().expect("prompts").len()
    }

    fn next(&self, request: &ChatRequest) -> Result<ChatReply, AiError> {
        let prompt = request
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<&str>>()
            .join("\n");
        self.prompts.lock().expect("prompts").push(prompt);
        let answer = {
            let mut script = self.script.lock().expect("script");
            if script.is_empty() {
                // A test that asks more questions than it scripted gets the last
                // answer again: the monitor's loop is bounded by MAX_JUDGED, and a
                // test should not have to write eight identical answers.
                None
            } else if script.len() == 1 {
                Some(match &script[0] {
                    Scripted::Says(text) => Ok((*text).to_string()),
                    Scripted::Fails(error) => Err(error.clone()),
                })
            } else {
                Some(match script.remove(0) {
                    Scripted::Says(text) => Ok(text.to_string()),
                    Scripted::Fails(error) => Err(error),
                })
            }
        };
        match answer {
            Some(Ok(content)) => Ok(ChatReply {
                model: "scripted".to_string(),
                content,
                // The fixture's model does not think out loud: `thinking` is the model's
                // own working, and a scripted answer has none.
                thinking: String::new(),
                calls: Vec::new(),
                finish_reason: "stop".to_string(),
                usage_tokens: None,
                truncated: false,
            }),
            Some(Err(error)) => Err(error),
            None => Err(AiError::Unreachable(
                "the test scripted no further answer".to_string(),
            )),
        }
    }
}

impl LlamaClient for ScriptedModel {
    fn model(&self) -> &str {
        "scripted"
    }

    fn complete(&self, request: &ChatRequest) -> Result<ChatReply, AiError> {
        self.next(request)
    }

    fn complete_stream(
        &self,
        request: &ChatRequest,
        _on_delta: &mut dyn FnMut(&str) -> atlas_ai::Control,
    ) -> Result<ChatReply, AiError> {
        self.next(request)
    }

    fn probe(&self) -> Result<ServerInfo, AiError> {
        Ok(ServerInfo {
            answering: true,
            model: "scripted".to_string(),
            context_size: Some(4_096),
            slots: Some(1),
        })
    }
}

/// An enrichment pass that says what it analysed, and remembers the limit it was given.
pub(crate) struct FakeEnrich {
    analysed: usize,
    articles: usize,
    failure: Option<String>,
    limits: Mutex<Vec<usize>>,
}

impl FakeEnrich {
    pub(crate) fn new(analysed: usize) -> Arc<FakeEnrich> {
        Arc::new(FakeEnrich {
            analysed,
            articles: analysed,
            failure: None,
            limits: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn failing(reason: &str) -> Arc<FakeEnrich> {
        Arc::new(FakeEnrich {
            analysed: 0,
            articles: 2,
            failure: Some(reason.to_string()),
            limits: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn limits(&self) -> Vec<usize> {
        self.limits.lock().expect("limits").clone()
    }
}

impl crate::enrich::Enrich for FakeEnrich {
    fn backfill(&self, limit: usize) -> crate::enrich::EnrichOutcome {
        self.limits.lock().expect("limits").push(limit);
        match &self.failure {
            Some(reason) => crate::enrich::EnrichOutcome::refused(reason),
            None => crate::enrich::EnrichOutcome {
                ok: true,
                error: None,
                articles: self.articles,
                analysed: self.analysed,
            },
        }
    }
}

/// A briefing seam that records the calls it was given, so a test can read the order the
/// refresh chose instead of guessing it from a log line.
pub(crate) struct FakeBriefs {
    ready: bool,
    /// `(scope, key, hours) -> watermark`, the cached row's newest article id.
    watermarks: Mutex<BTreeMap<(String, String, i64), i64>>,
    calls: Mutex<Vec<(String, String)>>,
    /// Scopes whose briefing answers `ok: false`.
    failing: Mutex<Vec<String>>,
    /// What one briefing reports it covered.
    articles: i64,
}

impl FakeBriefs {
    pub(crate) fn recording() -> Arc<FakeBriefs> {
        Arc::new(FakeBriefs {
            ready: true,
            watermarks: Mutex::new(BTreeMap::new()),
            calls: Mutex::new(Vec::new()),
            failing: Mutex::new(Vec::new()),
            articles: 4,
        })
    }

    /// A seam that is present but answers every request with a sentence.
    pub(crate) fn refusing(reason: &str) -> Arc<FakeBriefs> {
        let fake = FakeBriefs::recording();
        fake.failing.lock().expect("failing").push(reason.to_string());
        fake
    }

    pub(crate) fn calls(&self) -> Vec<(String, String)> {
        self.calls.lock().expect("calls").clone()
    }

    pub(crate) fn set_watermark(&self, scope: &str, key: &str, hours: i64, watermark: i64) {
        self.watermarks.lock().expect("watermarks").insert(
            (scope.to_string(), key.to_string(), hours),
            watermark,
        );
    }

    pub(crate) fn fail(&self, scope: &str) {
        self.failing.lock().expect("failing").push(scope.to_string());
    }
}

impl crate::brief::Briefs for FakeBriefs {
    fn ready(&self) -> bool {
        self.ready
    }

    fn watermark(&self, scope: &str, key: &str, hours: i64) -> Option<i64> {
        self.watermarks
            .lock()
            .expect("watermarks")
            .get(&(scope.to_string(), key.to_string(), hours))
            .copied()
    }

    fn brief(&self, scope: &str, key: &str, hours: i64) -> serde_json::Value {
        self.calls
            .lock()
            .expect("calls")
            .push((scope.to_string(), key.to_string()));
        let failing = self.failing.lock().expect("failing");
        if failing.iter().any(|failed| failed == scope) {
            return serde_json::json!({
                "ok": false,
                "error": "the model did not answer",
                "stored": 0,
                "scope": scope,
                "key": key,
                "hours": hours,
            });
        }
        serde_json::json!({
            "ok": true,
            "stored": 1,
            "articles": self.articles,
            "scope": scope,
            "key": key,
            "hours": hours,
        })
    }
}

/// Give one article a country, so a test can watch the briefing refresh pick the busiest.
pub(crate) fn insert_country(store: &Store, article_id: i64, iso: &str) {
    store
        .exec(
            "INSERT OR REPLACE INTO article_countries(article_id, iso, is_primary) VALUES(?,?,0)",
            &[
                atlas_store::SqlValue::Integer(article_id),
                atlas_store::SqlValue::Text(iso.to_string()),
            ],
        )
        .expect("insert country");
}

/// A research pass that really writes rows, with the countries its stories carry.
///
/// The briefing refresh reads what the archive gained, not what a seam reported, so a fake
/// that only returns ids would leave it nothing to see.
pub(crate) struct StoringResearch {
    store: Arc<Store>,
    rows: Vec<(i64, Option<&'static str>)>,
    calls: Mutex<usize>,
}

impl StoringResearch {
    pub(crate) fn new(store: Arc<Store>, rows: Vec<(i64, Option<&'static str>)>) -> Arc<StoringResearch> {
        Arc::new(StoringResearch {
            store,
            rows,
            calls: Mutex::new(0),
        })
    }

    pub(crate) fn calls(&self) -> usize {
        *self.calls.lock().expect("calls")
    }
}

impl Research for StoringResearch {
    fn research(&self, _query: &str, _max_steps: usize) -> ResearchOutcome {
        *self.calls.lock().expect("calls") += 1;
        let mut stored = Vec::new();
        for (id, iso) in &self.rows {
            insert_article(&self.store, *id, "2026-11-22T09:00:00Z", "a story the pass stored", true);
            if let Some(iso) = iso {
                insert_country(&self.store, *id, iso);
            }
            stored.push(*id);
        }
        ResearchOutcome {
            ok: true,
            error: None,
            stored,
            answer: String::new(),
            steps: 1,
            tool_calls: 1,
            tools: vec!["save_article".to_string()],
        }
    }
}
