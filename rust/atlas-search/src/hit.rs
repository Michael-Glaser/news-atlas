//! One answer row: what the engine said about one link, and its JSON.
//!
//! The shape is the one `atlas-ingest`'s `web_search` / `find_news` already
//! return (`title`, `url`, `publisher`, `snippet`, `published`), plus what this
//! crate adds: the date as the engine printed it, the age in seconds, the
//! relevance score behind the order, and the position the engine gave it. The
//! model can see both orders and decide for itself.
//!
//! Nothing here is trusted: every field is already cleaned and capped by its
//! producer ([`crate::extract`]), and this module only renders it.

use serde_json::{json, Value};

/// Cap a string at `max_chars` characters, never inside a character.
pub fn cap(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.trim().to_string();
    }
    let mut out = String::with_capacity(max_chars * 4);
    for (index, ch) in text.chars().enumerate() {
        if index >= max_chars {
            break;
        }
        out.push(ch);
    }
    out.trim_end().to_string()
}

/// One result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Hit {
    pub title: String,
    /// The link the model may open: wrapper unwrapped, gate-checked, fragment
    /// dropped, tracking parameters the index ignores removed.
    pub url: String,
    /// A presentable outlet name (`atlas-text`'s answer for the URL).
    pub publisher: String,
    pub snippet: String,
    /// The date, ISO-8601 UTC, when the page showed one `atlas-dates` could read.
    pub published: String,
    /// The date exactly as the engine printed it (`"2 hours ago"`, `"05.11.2026"`).
    pub printed: String,
    /// Seconds between that date and now; -1 when there is no date.
    pub age_s: i64,
    /// Relevance to the query, 0.0 … 1.0, from `atlas-core`'s similarity.
    pub score: f64,
    /// 1-based position on the result page. Relevance ties keep it, so the
    /// engine's own order survives where this crate has no reason to disagree.
    pub page_rank: usize,
}

impl Hit {
    pub fn dated(&self) -> bool {
        !self.published.is_empty()
    }

    /// The JSON row.
    pub fn to_json(&self) -> Value {
        let mut row = json!({
            "title": self.title,
            "url": self.url,
            "publisher": self.publisher,
            "snippet": self.snippet,
            "published": self.published,
            "score": round(self.score),
            "page_rank": self.page_rank,
        });
        if !self.printed.is_empty() && self.printed != self.published {
            row["printed"] = json!(self.printed);
        }
        if self.age_s >= 0 {
            row["age_s"] = json!(self.age_s);
        }
        row
    }
}

/// Three decimals is more precision than a relevance order needs, and it keeps
/// the tool result readable.
fn round(score: f64) -> f64 {
    if !score.is_finite() {
        return 0.0;
    }
    (score * 1000.0).round() / 1000.0
}
