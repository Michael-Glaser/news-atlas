//! Which rule engines this run has, and what a check reports when it has none.
//!
//! Decisions: the engines are `atlas-ingest`'s (`atlas-text`, `atlas-core`,
//! `atlas-places`, `atlas-topics`, `atlas-dates` in `rust/target/release`), a missing
//! one is a *skip* with the reason and the fix — never a fallback and never a guess —
//! and the same trait object serves a real run and a test.
//!
//! It does not: build anything, spawn anything by itself (`NativeRules` does), or
//! re-implement a single rule those engines own.

use std::path::Path;

use atlas_ingest::rules::{ArticleText, Locate, Parsed, Rules, RulesError, TopicMatch, TopicRow, Window};

/// The engines when they are not built: every question fails with the reason, so a
/// check reports a skip instead of an answer nobody measured.
pub struct MissingRules {
    reason: String,
}

impl MissingRules {
    pub fn new(reason: impl Into<String>) -> MissingRules {
        MissingRules {
            reason: crate::redact::reason(&reason.into()),
        }
    }

    fn refuse<T>(&self, what: &str) -> Result<T, RulesError> {
        Err(RulesError::new(format!(
            "{} unavailable: {} (cargo build --release --offline)",
            what, self.reason
        )))
    }
}

impl Rules for MissingRules {
    fn clean_text(&self, _text: &str) -> Result<String, RulesError> {
        self.refuse("atlas-text")
    }
    fn norm_url(&self, _url: &str) -> Result<String, RulesError> {
        self.refuse("atlas-text")
    }
    fn publisher(&self, _url: &str) -> Result<String, RulesError> {
        self.refuse("atlas-text")
    }
    fn clean_title(&self, _title: &str, _publisher: &str) -> Result<String, RulesError> {
        self.refuse("atlas-text")
    }
    fn tokenize(&self, _text: &str, _max_words: usize) -> Result<Vec<String>, RulesError> {
        self.refuse("atlas-text")
    }
    fn extract_html(&self, _html: &str, _url: &str, _title: &str) -> Result<String, RulesError> {
        self.refuse("atlas-core")
    }
    fn entity_candidates(
        &self,
        _text: &str,
        _known: &[String],
        _min_count: usize,
        _max: usize,
    ) -> Result<Vec<(String, usize)>, RulesError> {
        self.refuse("atlas-core")
    }
    fn locate(&self, _article: &ArticleText) -> Result<Locate, RulesError> {
        self.refuse("atlas-places")
    }
    fn match_topics(
        &self,
        _topics: &[TopicRow],
        _article: &ArticleText,
    ) -> Result<Vec<TopicMatch>, RulesError> {
        self.refuse("atlas-topics")
    }
    fn parse_date(&self, _ts: &str, _now: i64) -> Result<Parsed, RulesError> {
        self.refuse("atlas-dates")
    }
    fn window(&self, _ts: &str, _max_age_s: i64, _now: i64) -> Result<Window, RulesError> {
        self.refuse("atlas-dates")
    }
}

/// The engines for `root`, or the reason there are none.
///
/// `root` is the application root (`<root>/rust/target/release` holds the binaries),
/// the same value `atlas-ingest` uses in production.
pub fn for_root(root: &Path) -> (Box<dyn Rules>, Option<String>) {
    let config = atlas_ingest::Config::for_root(root);
    match atlas_ingest::NativeRules::discover(&config) {
        Ok(rules) => (Box::new(rules), None),
        Err(error) => {
            let reason = error.message().to_string();
            (Box::new(MissingRules::new(reason.clone())), Some(reason))
        }
    }
}

/// Is this rule set answering? One cheap question, asked once, so a run states
/// "skipped" instead of printing an empty result that looks like agreement.
pub fn answering(rules: &dyn Rules) -> Option<String> {
    match rules.publisher("https://example.com/a") {
        Ok(_) => None,
        Err(error) => Some(error.message().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_engine_says_what_is_missing_and_how_to_fix_it() {
        let rules = MissingRules::new("nothing built");
        let error = rules.publisher("https://example.com/a").expect_err("must refuse");
        assert!(error.message().contains("atlas-text"));
        assert!(error.message().contains("cargo build --release"));
        let error = rules.locate(&ArticleText::default()).expect_err("must refuse");
        assert!(error.message().contains("atlas-places"));
        assert!(answering(&MissingRules::new("x")).is_some());
    }
}
