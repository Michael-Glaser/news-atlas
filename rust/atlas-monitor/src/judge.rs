//! `judge` — who answers "how relevant is this article to the monitor".
//!
//! Three implementations of one trait:
//!
//! * [`ModelJudge`] asks the local model twice: `atlas_ai::prompts::news_prompt`
//!   ("is this page reporting at all?") and this crate's [`crate::prompt`]
//!   ("how relevant, and which words matched?"). Both prompts are the tuned ones —
//!   the news rule stays in `atlas-ai`, it is never restated here.
//! * [`QueryTermsJudge`] is the fallback used only when the model cannot be asked.
//!   It scores with the terms of the **monitor's own query**, and it filters
//!   nothing: without a model, an article the reader can see beats a silent,
//!   wrong rejection (the same rule `ai.news_verdict` had).
//! * [`NoJudge`] answers "nobody could be asked" for a caller that has no model
//!   wired (a CLI listing alerts, a test of the alert path).
//!
//! Decisions: what a judgement may be ([`Judgement`]), how a model failure becomes
//! `Unasked` rather than a score of 5, when a failed question is asked once more
//! ([`worth_asking_again`]), and how term coverage turns into a number.
//!
//! It does not: decide the threshold, store an alert, or write a log line.
//!
//! # What the model's word is worth
//!
//! A score is the model's or it is nobody's. When the model answered prose, JSON
//! without a figure, or nothing at all, the judgement is [`Judgement::Unasked`] —
//! never a number this crate computed and labelled `model`. The fallback
//! ([`QueryTermsJudge`]) may still score the article, and it says so on the alert.
//! The same rule reads the other way for the terms: the words the model named are
//! kept only where the article actually contains them
//! ([`terms_in_article`]), so an alert never shows word evidence for a word the
//! story does not have.
//!
//! Two more ways a score can fail to be the model's, both answered at the answer's
//! boundary and not by a `clamp` lower down: a reply that is not one JSON object
//! (prose around it is refused, because the article is *inside* the prompt and a
//! model quoting the page would otherwise hand the page its own `{"relevance": 10}`),
//! and a figure outside 0–10 (both ends of the scale decide an alert, so `42` is
//! reported with the number in it rather than clamped into a verdict).

use std::sync::Arc;

use atlas_ai::judge::{Judged, Question};
use atlas_ai::prompts::{news_prompt, verdict_from_answer};
use atlas_ai::{AiError, LlamaClient};

use crate::candidates::Article;
use crate::prompt::{parse_answer, judge_prompt, JUDGE_REMINDER, NEWS_REMINDER};
use crate::spec::Monitor;
use crate::verdict::{query_terms, Scorer, Verdict};

/// What one judge said about one article.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Judgement {
    /// The model says this page is not reporting (an explainer, a guide, a shop
    /// page). It is remembered and never alerts.
    NotNews { reason: String },
    /// A score, with the terms behind it.
    Scored(Verdict),
    /// Nobody could be asked — the caller decides what that means.
    Unasked { reason: String },
}

impl Judgement {
    /// The score, when there is one.
    pub fn verdict(self) -> Option<Verdict> {
        match self {
            Judgement::Scored(verdict) => Some(verdict),
            _ => None,
        }
    }
}

/// Who scores an article for a monitor. `Send + Sync`: a check runs on a request
/// thread and the model client is shared.
pub trait Judge: Send + Sync {
    fn judge(&self, monitor: &Monitor, article: &Article) -> Judgement;
}

/// The model answers both questions. Budgets and temperatures are no longer here: the
/// thinking budget is the reader's `model_reasoning` and it is enforced by the bounded turn
/// in `atlas_ai::judge`, because a budget this crate sets locally cannot also guarantee room
/// to answer after thinking.
pub struct ModelJudge {
    client: Arc<dyn LlamaClient + Send + Sync>,
    budgets: atlas_ai::Budgets,
}

impl ModelJudge {
    /// A judge on the shipped numbers (32 768 of window, 8 192 of thought, 8 192 of
    /// answer) — a test, a tool, a caller with no configuration.
    pub fn new(client: Arc<dyn LlamaClient + Send + Sync>) -> ModelJudge {
        ModelJudge::with_budgets(client, atlas_ai::Budgets::DEFAULT)
    }

    /// A judge on the reader's numbers (`data/config.json`'s `model_context`,
    /// `model_reasoning`, `model_answer`), so the monitor thinks for as long as the rest of
    /// the app is allowed to.
    pub fn with_budgets(
        client: Arc<dyn LlamaClient + Send + Sync>,
        budgets: atlas_ai::Budgets,
    ) -> ModelJudge {
        ModelJudge { client, budgets }
    }
}

impl Judge for ModelJudge {
    fn judge(&self, monitor: &Monitor, article: &Article) -> Judgement {
        let body = article.body();
        // Everything the two prompts show the model from the page, so the turn can tell the
        // page's own `{…}` from the model's answer (`atlas_ai::judge::Question::with_untrusted`).
        let page = format!(
            "{}\n{}\n{}\n{}",
            article.title, article.summary, body, article.url
        );
        // The one place "what counts as news" is decided: `atlas-ai`'s prompt, not
        // a word list in this crate. Both questions go through `atlas_ai::judge`, which is
        // the bounded turn: think with room to answer, rescue once without reasoning, and
        // read the object out of prose if that is what it wrote. A single 600-token call
        // with reasoning on was measured spending all of it thinking and returning an empty
        // answer — which read, correctly and uselessly, as "the model did not answer".
        let news = match self.asked(
            &news_prompt(&article.title, &body, &article.url, &article.publisher),
            NEWS_REMINDER,
            &["news"],
            &page,
        ) {
            Ok(judged) => verdict_from_answer(&judged.value.to_string()),
            Err(reason) => {
                return Judgement::Unasked {
                    reason: format!("the model did not answer the news question: {reason}"),
                }
            }
        };
        if !news.news {
            return Judgement::NotNews {
                reason: news.reason,
            };
        }
        let prompt = judge_prompt(&monitor.query, &article.title, &article.published, &body);
        let judged = match self.asked(&prompt, JUDGE_REMINDER, &["relevance"], &page) {
            Ok(judged) => judged,
            Err(reason) => {
                return Judgement::Unasked {
                    reason: format!("the model did not answer the relevance question: {reason}"),
                }
            }
        };
        let Some(answer) = parse_answer(&judged.value.to_string()) else {
            // The turn produced an object but not this question's shape: a `relevance` key
            // whose value is neither a number nor a numeric string. That is not a score, and
            // nothing is invented to replace it — the article stays unjudged.
            return Judgement::Unasked {
                reason: "the model's relevance answer carried no score this can read".to_string(),
            };
        };
        let Some(score) = answer.relevance else {
            // JSON, but no figure on the scale. `coverage()` would produce one — and it
            // would be stored as `Scorer::Model`, which is the one thing this crate may
            // not do: report the model's authority for a number it did not state. A
            // number it *did* state and that is not on the scale (42, -3) is refused
            // here too and named: `clamp` turned 42 into the top of the scale.
            let reason = match answer.refused_score {
                Some(stated) => format!(
                    "the model's relevance answer was {}, outside the 0-10 scale",
                    stated
                ),
                None => "the model's relevance answer carried no 0-10 score".to_string(),
            };
            return Judgement::Unasked { reason };
        };
        Judgement::Scored(Verdict::new(
            Scorer::Model,
            score,
            terms_in_article(answer.terms, article),
            &answer.why,
            if answer.summary.is_empty() {
                &article.summary
            } else {
                &answer.summary
            },
            answer.entities,
        ))
    }
}

impl ModelJudge {
    /// One judgement question through `atlas-ai`'s bounded turn.
    ///
    /// `required` is the key the answer must carry: every step of the turn refuses an object
    /// without it, so a page's own quoted `{…}` cannot stand in for the model's verdict.
    /// `page` is the untrusted text the prompt shows, so an object the page spells itself is
    /// refused as a quote even when it has the right shape.
    fn asked(
        &self,
        prompt: &str,
        reminder: &str,
        required: &[&str],
        page: &str,
    ) -> Result<Judged, String> {
        atlas_ai::judge::ask_with(
            self.client.as_ref(),
            &Question::new(prompt, reminder, required).with_untrusted(page),
            &self.budgets,
        )
    }
}

/// Exactly one more attempt, for the failures a second identical question can fix:
/// nothing was listening (still loading), the listener dropped, the deadline
/// passed, or the server answered 5xx. A bad request, a prompt that does not fit
/// the context and a body that is not the answer are not retried — asking again
/// produces the same refusal.
///
/// The rule itself lives in `atlas_ai::judge` — the bounded turn there is what
/// actually retries now, so the decision must be made where the retrying happens
/// or the two would drift apart.
pub fn worth_asking_again(error: &AiError) -> bool {
    atlas_ai::judge::worth_retrying(error)
}

/// The model's terms, kept only where the article actually contains them.
///
/// The prompt asks for "the words of the topic that actually appear in it", and a
/// model that answered from the topic line alone would otherwise put word
/// evidence into an alert for words the story does not contain. This is a check
/// on the answer, not a vocabulary: the words are the model's, and the only
/// question asked of them is whether the text holds them at a word start. When
/// none survives, the alert keeps the model's reason and shows no terms — it never
/// invents one.
pub fn terms_in_article(terms: Vec<String>, article: &Article) -> Vec<String> {
    if terms.is_empty() {
        return Vec::new();
    }
    let lowered: Vec<String> = terms
        .into_iter()
        .map(|term| term.trim().to_lowercase())
        // A single character is not a word to match on; `clean_terms` drops it
        // later anyway, and this keeps the two rules saying the same thing.
        .filter(|term| term.chars().count() > 1)
        .collect();
    // `matched_terms` lower-cases the text once; building the haystack without
    // lowering it here keeps that to one copy of the article per article.
    matched_terms(&lowered, &article_text(article))
}

/// Everything the model was shown about the article, plus the publisher, so a term
/// is checked against the whole stored page and not only the capped excerpt.
fn article_text(article: &Article) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        article.title, article.summary, article.content, article.publisher
    )
}

/// Scores with the monitor's own query terms and nothing else.
///
/// `monitor.py::_keyword_score`: `2 + 6 * (hits / terms)`, plus one when a term
/// appears in the title, at least 1 and at most 10. A term matches at a word
/// start, so a two-letter term does not score every article that happens to
/// contain those letters.
///
/// One pass over the text: the hit list the score is computed from is the same
/// list the alert shows as its evidence, so the words in the "why" are exactly
/// the words behind the number. (The port used to scan title and body twice —
/// once for the hits, once inside [`coverage`] for the score.)
pub struct QueryTermsJudge;

impl Judge for QueryTermsJudge {
    fn judge(&self, monitor: &Monitor, article: &Article) -> Judgement {
        let terms = monitor.terms();
        let title = article.title.to_lowercase();
        let body = article.body().to_lowercase();
        let mut hits: Vec<String> = Vec::with_capacity(terms.len());
        let mut in_title = false;
        for term in &terms {
            let in_head = starts_a_word(&title, term);
            if in_head || starts_a_word(&body, term) {
                in_title = in_title || in_head;
                hits.push(term.clone());
            }
        }
        let score = score_of(hits.len(), terms.len(), in_title);
        // The matched words are the verdict's own `terms` and the alert lists them
        // once; repeating the list here made the stored "why it fired" line say
        // every word twice.
        let why = if hits.is_empty() {
            "no word of the query appears in the article".to_string()
        } else {
            "the query's own words are in the article".to_string()
        };
        Judgement::Scored(Verdict::new(
            Scorer::QueryTerms,
            score,
            if hits.is_empty() { terms } else { hits },
            &why,
            &article.summary,
            Vec::new(),
        ))
    }
}

/// A judge that never answers: for a caller with no model wired.
pub struct NoJudge {
    reason: String,
}

impl NoJudge {
    pub fn new(reason: impl Into<String>) -> NoJudge {
        NoJudge {
            reason: reason.into(),
        }
    }
}

impl Judge for NoJudge {
    fn judge(&self, _monitor: &Monitor, _article: &Article) -> Judgement {
        Judgement::Unasked {
            reason: self.reason.clone(),
        }
    }
}

/// The word-matching rules live in [`crate::terms`]; these names are re-exported
/// so `atlas_monitor::judge::coverage` and its neighbours keep working.
pub use crate::terms::{coverage, matched_terms, score_of, starts_a_word};

/// The terms a monitor's query would produce — a convenience for callers that
/// only have the query string.
pub fn terms_of(query: &str) -> Vec<String> {
    query_terms(query)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{Scripted, ScriptedModel};

    fn article(title: &str, summary: &str) -> Article {
        Article {
            id: 1,
            title: title.to_string(),
            summary: summary.to_string(),
            content: String::new(),
            url: "https://example.com/a".to_string(),
            published: "2026-11-05T09:00:00Z".to_string(),
            fetched: "2026-11-05T09:01:00Z".to_string(),
            publisher: String::new(),
        }
    }

    fn monitor(query: &str) -> Monitor {
        Monitor {
            id: 1,
            name: "Chips".to_string(),
            query: query.to_string(),
            threshold: 6,
            ..Monitor::default()
        }
    }

    #[test]
    fn a_term_matches_only_at_a_word_start() {
        // The rule itself is tested in `crate::terms`; this pins that the judge is
        // asking that rule and not a second copy of it.
        assert!(starts_a_word("the chip export rule", "chip"));
        assert!(!starts_a_word("microchip exports", "chip"));
    }

    #[test]
    fn the_fallback_scores_by_coverage_and_says_which_terms_matched() {
        let judge = QueryTermsJudge;
        let verdict = judge
            .judge(&monitor("chip export rules"), &article("New chip rules", "the export ban"))
            .verdict()
            .expect("score");
        assert_eq!(verdict.scorer, Scorer::QueryTerms);
        assert_eq!(verdict.score, 9);
        assert_eq!(
            verdict.terms,
            vec!["chip".to_string(), "export".to_string(), "rules".to_string()]
        );
    }

    #[test]
    fn an_article_with_none_of_the_terms_scores_zero_and_alerts_on_nothing() {
        let verdict = QueryTermsJudge
            .judge(&monitor("chip export"), &article("Football result", "a match report"))
            .verdict()
            .expect("score");
        assert_eq!(verdict.score, 0);
        assert!(!verdict.fired(1));
    }

    #[test]
    fn a_judge_without_a_model_says_so_instead_of_scoring() {
        let judgement = NoJudge::new("no model here").judge(&monitor("chips"), &article("t", "s"));
        assert!(matches!(judgement, Judgement::Unasked { .. }));
    }

    fn model_judge(script: Vec<Scripted>) -> (ModelJudge, Arc<ScriptedModel>) {
        let client = ScriptedModel::new(script);
        let judge = ModelJudge::new(Arc::clone(&client) as Arc<dyn LlamaClient + Send + Sync>);
        (judge, client)
    }

    #[test]
    fn the_models_reason_and_terms_reach_the_verdict() {
        let (judge, client) = model_judge(vec![
            Scripted::Says("{\"news\": true, \"reason\": \"a report\"}"),
            Scripted::Says(
                "{\"relevance\": 8, \"terms\": [\"export\"], \"why\": \"a new export rule\", \
                 \"summary\": \"the ministry ruled\", \"entities\": [\"Ministry\"]}",
            ),
        ]);
        let verdict = judge
            .judge(&monitor("chip export"), &article("New chip export rules", "the export ban"))
            .verdict()
            .expect("score");
        assert_eq!(verdict.scorer, Scorer::Model);
        assert_eq!(verdict.score, 8);
        assert_eq!(verdict.why, "a new export rule");
        assert_eq!(verdict.summary, "the ministry ruled");
        assert_eq!(verdict.entities, vec!["Ministry".to_string()]);
        assert_eq!(verdict.terms, vec!["export".to_string()]);
        assert_eq!(client.calls(), 2, "the news question, then the relevance one");
    }

    #[test]
    fn a_page_the_model_calls_not_reporting_is_never_scored() {
        // An index page of chip articles: every term of the query is in the title,
        // so the fallback would score it 10 — the model's answer is what stops it.
        let (judge, client) = model_judge(vec![Scripted::Says(
            "{\"news\": false, \"reason\": \"an index page of chip articles\"}",
        )]);
        let judgement = judge.judge(
            &monitor("chip export rules"),
            &article("Chip export rules — all articles", "chip export rules"),
        );
        assert_eq!(
            judgement,
            Judgement::NotNews {
                reason: "an index page of chip articles".to_string()
            }
        );
        assert_eq!(client.calls(), 1, "a page that is not reporting is not scored");
    }

    #[test]
    fn a_model_that_cannot_be_reached_is_asked_once_more() {
        let (judge, client) = model_judge(vec![
            Scripted::Fails(AiError::Unreachable("connection refused".to_string())),
            Scripted::Says("{\"news\": true, \"reason\": \"a report\"}"),
            Scripted::Says("{\"relevance\": 7, \"terms\": [], \"why\": \"it is the story\"}"),
        ]);
        let verdict = judge
            .judge(&monitor("chip export"), &article("New chip export rules", "the export ban"))
            .verdict()
            .expect("the second attempt answered");
        assert_eq!(verdict.score, 7);
        assert_eq!(client.calls(), 3, "one retry, then the relevance question");
        let prompts = client.prompts();
        assert_eq!(
            prompts[0], prompts[1],
            "the retry asks the same question, word for word"
        );
    }

    #[test]
    fn a_model_that_is_loading_is_asked_once_more_and_then_reported() {
        let (judge, client) = model_judge(vec![Scripted::Fails(AiError::Http {
            status: 503,
            detail: "loading model".to_string(),
        })]);
        let judgement = judge.judge(&monitor("chip export"), &article("t", "s"));
        match &judgement {
            Judgement::Unasked { reason } => {
                assert!(reason.contains("did not answer"), "{reason}");
                assert!(reason.contains("503") || reason.contains("loading"), "{reason}");
            }
            other => panic!("an outage is not a score: {other:?}"),
        }
        assert_eq!(client.calls(), 2, "one attempt and exactly one retry");
    }

    #[test]
    fn a_refusal_that_a_second_ask_cannot_fix_is_not_asked_again() {
        for error in [
            AiError::Config("no endpoint".to_string()),
            AiError::BadResponse("not json".to_string()),
            AiError::Http {
                status: 400,
                detail: "bad request".to_string(),
            },
        ] {
            assert!(!worth_asking_again(&error), "{error:?} is not transient");
        }
        assert!(worth_asking_again(&AiError::Unreachable(String::new())));
        assert!(worth_asking_again(&AiError::Timeout(String::new())));
        assert!(worth_asking_again(&AiError::Http {
            status: 502,
            detail: String::new()
        }));
    }

    #[test]
    fn a_number_the_model_did_not_give_is_not_reported_as_the_models() {
        // The article matches every term, so `coverage` would say 10 — and the alert would
        // then name the model as the scorer. The object has the wrong shape (no `relevance`
        // key), so `atlas_ai::judge` refuses it at every step and the refusal is the answer.
        let (judge, _client) = model_judge(vec![
            Scripted::Says("{\"news\": true, \"reason\": \"a report\"}"),
            Scripted::Says("{\"terms\": [\"chip\"], \"why\": \"it matches\"}"),
        ]);
        let judgement = judge.judge(
            &monitor("chip export"),
            &article("New chip export rules", "the export ban"),
        );
        match judgement {
            Judgement::Unasked { reason } => {
                assert!(
                    reason.contains("did not answer the relevance question"),
                    "the reason names the question: {reason}"
                );
                assert!(reason.contains("relevance"), "and the key it wanted: {reason}");
            }
            other => panic!("a missing figure is not a score: {other:?}"),
        }
    }

    #[test]
    fn prose_instead_of_json_is_not_a_judgement() {
        let (judge, _client) = model_judge(vec![
            Scripted::Says("{\"news\": true, \"reason\": \"a report\"}"),
            Scripted::Says("I would say this is quite relevant, maybe an eight."),
        ]);
        assert!(matches!(
            judge.judge(&monitor("chip export"), &article("t", "s")),
            Judgement::Unasked { .. }
        ));
    }

    #[test]
    fn the_models_terms_are_kept_only_where_the_article_has_them() {
        let mut story = article("New chip export rules", "the ministry said");
        story.content = "quota raised".to_string();
        assert_eq!(
            terms_in_article(
                vec!["Quota".to_string(), "tariff".to_string(), "x".to_string()],
                &story
            ),
            vec!["quota".to_string()],
            "tariff is not in the story, and a one-letter term is not a word"
        );
        assert!(terms_in_article(vec![], &story).is_empty());
        // A phrase the model named is kept when the phrase is there.
        assert_eq!(
            terms_in_article(vec!["export rules".to_string()], &story),
            vec!["export rules".to_string()]
        );
    }

    #[test]
    fn the_fallback_scores_the_same_number_whether_it_is_asked_once_or_per_term() {
        // The single-pass judge and the public `coverage` helper are the same rule.
        let story = article("New chip rules", "the export ban");
        let terms = monitor("chip export rules").terms();
        assert_eq!(
            coverage(&terms, &story),
            QueryTermsJudge
                .judge(&monitor("chip export rules"), &story)
                .verdict()
                .expect("score")
                .score
        );
    }

    /// Measured, not claimed: one fallback pass over eight full-length articles.
    /// `cargo test --offline -p atlas-monitor -- --nocapture` prints the number.
    #[test]
    fn the_fallback_pass_is_measured() {
        use crate::MAX_JUDGED;
        let mut story = article("New chip export rules", "the ministry said");
        story.content = "export licence rules for chip makers. ".repeat(60);
        let terms = monitor("chip export rules quota tariff licence ministry makers")
            .terms();
        let started = std::time::Instant::now();
        let mut scored = 0i64;
        for _ in 0..MAX_JUDGED {
            scored += QueryTermsJudge
                .judge(&monitor("chip export rules quota tariff licence ministry makers"), &story)
                .verdict()
                .expect("score")
                .score;
        }
        let elapsed = started.elapsed();
        // For scale: the same eight articles scored by `coverage` alone, which is
        // one scan of the text and no verdict. The judge used to run this on top of
        // its own pass; it now runs the pass once and the score comes out of it.
        let started = std::time::Instant::now();
        let mut single = 0i64;
        for _ in 0..MAX_JUDGED {
            single += coverage(&terms, &story);
        }
        let coverage_elapsed = started.elapsed();
        println!(
            "atlas-monitor: QueryTermsJudge, {} articles of {} chars, {} terms: {:?} \
             (one coverage() pass each: {:?})",
            MAX_JUDGED,
            story.body().chars().count(),
            terms.len(),
            elapsed,
            coverage_elapsed
        );
        // One model call is ~1 s; the fallback for a whole check has to be noise
        // beside it.
        assert!(
            elapsed.as_millis() < 100,
            "eight fallback scores must stay far below one model call: {:?}",
            elapsed
        );
        assert_eq!(scored, single, "the two paths agree on the number");
    }
}
