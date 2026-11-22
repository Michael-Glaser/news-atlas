//! The doubles a check needs: the five rule engines and the local model.
//!
//! Decisions: the answers are fixed strings a test can reason about, the pipeline is built
//! through `atlas-ingest`'s own `Pipeline::new` so nothing about the wiring is faked, and
//! the transport is never asked to fetch anything.
//!
//! It does not: decide anything the product decides — these are test doubles, and they are
//! only compiled in a test build.

use std::path::Path;
use std::sync::{Arc, Mutex};

use atlas_ingest::rules::{ArticleText, Locate, Parsed, Rules, RulesError, TopicMatch, TopicRow, Window};
use atlas_ingest::{Config, Model, NullBrowser, Pipeline};

/// The five rule engines, answered deterministically.
///
/// This is a test double, not a rule: it exists so a check can be exercised without a
/// process spawn, and it decides nothing the product decides.
pub struct FakeRules {
    pub place_in: Mutex<Vec<String>>,
    /// Answer every `locate` with an outlet anchor (the least evidence there is), so a
    /// test can prove a caller does not replace a place with a weaker one.
    pub outlet_only: bool,
    /// Refuse every `locate`, the way a missing or crashing `atlas-places` does: a check
    /// must report the row as unanswered, not as agreeing.
    pub refuse_locate: bool,
}

impl Default for FakeRules {
    fn default() -> Self {
        FakeRules::new()
    }
}

impl FakeRules {
    pub fn new() -> FakeRules {
        FakeRules {
            place_in: Mutex::new(Vec::new()),
            outlet_only: false,
            refuse_locate: false,
        }
    }

    /// Every place question answered with "where the newspaper sits".
    pub fn outlet_only() -> FakeRules {
        FakeRules {
            place_in: Mutex::new(Vec::new()),
            outlet_only: true,
            refuse_locate: false,
        }
    }

    /// Every place question refused, with the reason a missing engine gives.
    pub fn refusing_places() -> FakeRules {
        FakeRules {
            place_in: Mutex::new(Vec::new()),
            outlet_only: false,
            refuse_locate: true,
        }
    }

    fn located(place: &str, iso: &str, lat: f64, lon: f64, prec: &str, conf: f64) -> Locate {
        Locate {
            place: place.to_string(),
            iso: iso.to_string(),
            lat,
            lon,
            prec: prec.to_string(),
            conf,
        }
    }
}

impl Rules for FakeRules {
    fn clean_text(&self, text: &str) -> Result<String, RulesError> {
        Ok(text.to_string())
    }
    fn norm_url(&self, url: &str) -> Result<String, RulesError> {
        Ok(url.to_string())
    }
    fn publisher(&self, url: &str) -> Result<String, RulesError> {
        if url.trim().is_empty() {
            return Ok(String::new());
        }
        let host = crate::publisher::host_of(url);
        let label = host.split('.').next().unwrap_or("").to_string();
        Ok(label)
    }
    fn clean_title(&self, title: &str, _publisher: &str) -> Result<String, RulesError> {
        Ok(title.to_string())
    }
    fn tokenize(&self, text: &str, max_words: usize) -> Result<Vec<String>, RulesError> {
        Ok(text
            .split_whitespace()
            .take(max_words)
            .map(|word| word.to_lowercase())
            .collect())
    }
    fn extract_html(&self, _html: &str, _url: &str, _title: &str) -> Result<String, RulesError> {
        Ok(String::new())
    }
    fn entity_candidates(
        &self,
        _text: &str,
        _known: &[String],
        _min_count: usize,
        _max: usize,
    ) -> Result<Vec<(String, usize)>, RulesError> {
        Ok(Vec::new())
    }
    fn locate(&self, article: &ArticleText) -> Result<Locate, RulesError> {
        if self.refuse_locate {
            return Err(RulesError::new(
                "atlas-places unavailable: refused for a test",
            ));
        }
        let text = format!(
            "{} {} {}",
            article.title, article.summary, article.content
        )
        .to_lowercase();
        if let Ok(mut seen) = self.place_in.lock() {
            seen.push(article.title.clone());
        }
        if self.outlet_only {
            return Ok(FakeRules::located("Ireland", "IRL", 53.0, -8.0, "outlet", 0.2));
        }
        if text.contains("berlin") {
            return Ok(FakeRules::located("Berlin", "DEU", 52.52, 13.40, "city", 0.9));
        }
        if text.contains("shenzhen") {
            return Ok(FakeRules::located("Shenzhen", "CHN", 22.54, 114.06, "city", 1.0));
        }
        Ok(Locate::default())
    }
    fn match_topics(
        &self,
        topics: &[TopicRow],
        article: &ArticleText,
    ) -> Result<Vec<TopicMatch>, RulesError> {
        let text = format!(
            "{} {} {}",
            article.title, article.summary, article.content
        )
        .to_lowercase();
        let mut out: Vec<TopicMatch> = Vec::new();
        for topic in topics {
            if !topic.active {
                continue;
            }
            let hits: Vec<String> = topic
                .keywords
                .split(',')
                .map(|word| word.trim().to_lowercase())
                .filter(|word| !word.is_empty() && text.contains(word))
                .collect();
            if hits.len() >= topic.min_matches.max(1) as usize {
                out.push(TopicMatch {
                    id: topic.id,
                    name: topic.name.clone(),
                    score: 1.0,
                    matched: hits,
                });
            }
        }
        Ok(out)
    }
    fn parse_date(&self, ts: &str, _now: i64) -> Result<Parsed, RulesError> {
        Ok(Parsed {
            ok: !ts.trim().is_empty(),
            epoch: 0,
            iso: ts.to_string(),
            reason: String::new(),
        })
    }
    fn window(&self, _ts: &str, _max_age_s: i64, _now: i64) -> Result<Window, RulesError> {
        Ok(Window {
            fresh: true,
            reason: "fresh".to_string(),
            age_h: 0.0,
            iso: String::new(),
            note: String::new(),
        })
    }
}

/// A model a test drives: it answers with one fixed JSON object, or not at all.
pub struct FakeModel {
    pub online: bool,
    pub answer: Option<serde_json::Value>,
    pub prompts: Mutex<usize>,
}

impl FakeModel {
    pub fn answering(answer: serde_json::Value) -> Arc<FakeModel> {
        Arc::new(FakeModel {
            online: true,
            answer: Some(answer),
            prompts: Mutex::new(0),
        })
    }

    /// An online model that returns nothing usable — the case where
    /// `atlas-ingest` answers with its heuristic receipt.
    pub fn silent() -> Arc<FakeModel> {
        Arc::new(FakeModel {
            online: true,
            answer: None,
            prompts: Mutex::new(0),
        })
    }

    pub fn offline() -> Arc<FakeModel> {
        Arc::new(FakeModel {
            online: false,
            answer: None,
            prompts: Mutex::new(0),
        })
    }

    pub fn questions(&self) -> usize {
        self.prompts.lock().map(|count| *count).unwrap_or(0)
    }
}

impl Model for FakeModel {
    fn model_id(&self) -> String {
        "fake".to_string()
    }
    fn online(&self) -> bool {
        self.online
    }
    fn verdict(
        &self,
        _title: &str,
        _text: &str,
        _url: &str,
        _publisher: &str,
    ) -> atlas_ai::prompts::NewsVerdict {
        atlas_ai::prompts::NewsVerdict::unchecked("fake model")
    }
    fn chat(&self, _prompt: &str, _temperature: f32, _max_tokens: u32) -> Result<String, String> {
        Err("the fake model does not chat".to_string())
    }
    fn json(&self, _prompt: &str, _max_tokens: u32) -> Option<serde_json::Value> {
        if let Ok(mut count) = self.prompts.lock() {
            *count += 1;
        }
        self.answer.clone()
    }
}

/// A pipeline over one fixture: the real store, the fake rules, the given model, and a
/// transport that is never asked to fetch anything.
pub fn pipeline(path: &Path, model: Arc<FakeModel>) -> Pipeline {
    let store = Arc::new(crate::fixture::open(path));
    let root = path.parent().unwrap_or(Path::new("."));
    let config = Config::for_root(root);
    Pipeline::new(
        store,
        config,
        Arc::new(FakeRules::new()),
        model,
        Arc::new(atlas_ingest::NetFetcher::new()),
        Arc::new(NullBrowser),
        None,
    )
}

/// The model answer a carry test expects: two names, one category.
pub fn model_answer() -> serde_json::Value {
    serde_json::json!({
        "entities": [
            { "name": "Aurora Systems", "type": "ORG", "alias_of": "" },
            { "name": "Shenzhen", "type": "LOC", "alias_of": "" }
        ],
        "relations": [ { "a": "Aurora Systems", "b": "Shenzhen", "label": "located in" } ],
        "categories": ["Technology"],
        "sentiment": "neutral",
        "summary": "Aurora Systems opened an office in Shenzhen."
    })
}

