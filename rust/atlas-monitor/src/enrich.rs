//! `enrich` — the second seam the agent drives: the enrichment pass over the archive.
//!
//! A check asks "what is new for this query"; enrichment asks "what has been stored but
//! not analysed yet" and runs `atlas-ingest`'s `backfill` over it — entities, places and
//! tags for articles the pipeline stored before the model had time to look at them. It
//! is the work the agent does when nobody clicked anything.
//!
//! Decisions: what a pass reports back ([`EnrichOutcome`]: did it work, how many articles
//! were looked at, how many were analysed), and that the production implementation is
//! `atlas-ingest`'s own pipeline ([`IngestEnrich`]). Nothing here decides *which*
//! articles are worth analysing — `backfill` selects them in SQL, and the model decides
//! what they are about.
//!
//! It does not: open the archive, choose a query, or interpret an analysis. The limit is
//! the caller's (the request's, clamped by the pipeline), and an article's text never
//! reaches this file: the answer is counts and a sentence.

use std::sync::Arc;

use serde_json::Value;

use atlas_ingest::Pipeline;

/// What one enrichment pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnrichOutcome {
    pub ok: bool,
    /// Why it did not work, when it did not. A reason, never an article's text.
    pub error: Option<String>,
    /// Articles the pass looked at.
    pub articles: usize,
    /// Articles it managed to analyse (the model answered for them).
    pub analysed: usize,
}

impl EnrichOutcome {
    /// A pass that did not run.
    pub fn refused(reason: &str) -> EnrichOutcome {
        EnrichOutcome {
            ok: false,
            error: Some(crate::error::scrub(reason)),
            ..EnrichOutcome::default()
        }
    }

    /// Read the shape `Pipeline::backfill` returns.
    ///
    /// Public because a caller that already holds the pipeline's answer (the bridge) must
    /// read it the same way: one place decides which field means "analysed".
    pub fn from_answer(answer: &Value) -> EnrichOutcome {
        let number = |key: &str| {
            answer
                .get(key)
                .and_then(Value::as_u64)
                .unwrap_or_default() as usize
        };
        let articles = number("articles");
        let analysed = number("analysed");
        let failed = number("failed");
        // A pass that looked at nothing is not a failure: an archive with every article
        // analysed is the steady state of a running agent.
        let error = if articles > 0 && analysed == 0 && failed > 0 {
            Some(format!(
                "the enrichment pass looked at {} article(s) and analysed none",
                articles
            ))
        } else {
            None
        };
        EnrichOutcome {
            ok: error.is_none(),
            error,
            articles,
            analysed,
        }
    }
}

/// One enrichment pass. `Send + Sync`, like [`crate::Research`]: the loop may run it on
/// any thread.
pub trait Enrich: Send + Sync {
    /// Analyse up to `limit` stored-but-unanalysed articles. Never panics.
    fn backfill(&self, limit: usize) -> EnrichOutcome;
}

/// The real thing: `atlas-ingest`'s `backfill`, which selects the rows and drives the
/// model through its own handlers.
pub struct IngestEnrich {
    pipeline: Arc<Pipeline>,
}

impl IngestEnrich {
    pub fn new(pipeline: Arc<Pipeline>) -> IngestEnrich {
        IngestEnrich { pipeline }
    }
}

impl Enrich for IngestEnrich {
    fn backfill(&self, limit: usize) -> EnrichOutcome {
        EnrichOutcome::from_answer(&self.pipeline.backfill(limit))
    }
}

/// No pass at all: for a caller that only reads, or a test.
pub struct NoEnrich {
    reason: String,
}

impl NoEnrich {
    pub fn new(reason: impl Into<String>) -> NoEnrich {
        NoEnrich {
            reason: reason.into(),
        }
    }
}

impl Default for NoEnrich {
    fn default() -> NoEnrich {
        NoEnrich::new("this caller does not enrich")
    }
}

impl Enrich for NoEnrich {
    fn backfill(&self, _limit: usize) -> EnrichOutcome {
        EnrichOutcome::refused(&self.reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pass_that_analysed_articles_reports_counts_and_no_error() {
        let outcome = EnrichOutcome::from_answer(&serde_json::json!({
            "articles": 5, "analysed": 4, "skipped": 1, "failed": 0, "model": true,
        }));
        assert!(outcome.ok);
        assert_eq!(outcome.articles, 5);
        assert_eq!(outcome.analysed, 4);
        assert_eq!(outcome.error, None);
    }

    #[test]
    fn an_archive_with_nothing_left_to_analyse_is_not_a_failure() {
        // The steady state of a running agent: this is the case that must not print a
        // warning every twenty seconds.
        let outcome = EnrichOutcome::from_answer(&serde_json::json!({
            "articles": 0, "analysed": 0, "skipped": 0, "failed": 0,
        }));
        assert!(outcome.ok, "{:?}", outcome);
    }

    #[test]
    fn a_pass_that_looked_at_articles_and_analysed_none_says_so() {
        let outcome = EnrichOutcome::from_answer(&serde_json::json!({
            "articles": 3, "analysed": 0, "failed": 3,
        }));
        assert!(!outcome.ok);
        assert!(outcome
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("analysed none"));
    }

    #[test]
    fn nothing_wired_is_reported_as_nothing_ran() {
        let outcome = NoEnrich::default().backfill(5);
        assert!(!outcome.ok);
        assert!(outcome.error.is_some());
        assert_eq!(outcome.analysed, 0);
    }
}
