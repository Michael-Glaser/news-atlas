//! What the payload means: the topic rows, the article, and their folded fields.
//!
//! Decides: which keys are required, which types are refused with which message,
//! how `min_matches` is coerced (`max(1, int(x or 1))`, numeric strings included),
//! how much of the body is folded (16 000 chars) and how much of that is scanned
//! (8 000), how the three scored fields are cut, the exclude blob that may span
//! the title and the summary, and the caps on the evidence-only name lists.
//!
//! It also decides what a *malformed* payload means, and the rule is: a shape
//! this crate cannot read is an error, never an empty article. An `article` that
//! is a string or an array used to be read as "every field empty", which answers
//! "this article matches no topic" — the one answer that cannot be told from a
//! correct one.
//!
//! Does not decide: relevance. Nothing here scores anything; `Entities` and tags
//! are carried for the evidence lists only, and `url`/`publisher` are validated
//! but never looked at by a decision.

use atlas_lib::json::Json;

use crate::chars;
use crate::terms;
use crate::fold;

/// How much of the body is folded, and how much of that is scanned
/// (`_CONTENT_LIMIT` in topics.py). Past it, text cannot match.
pub const CONTENT_FOLD: usize = 16_000;
pub const CONTENT_SCAN: usize = 8_000;
/// Extracted names kept from the payload, for the evidence lists.
pub const NAME_LIMIT: usize = 64;

pub struct Topic {
    /// Echoed back untouched: the id is whatever the store gave us.
    pub id: Json,
    pub name: String,
    pub min: usize,
    /// The keyword spelling the reader typed, and its compiled matcher.
    pub terms: Vec<(String, terms::TermPat)>,
    /// The exclude spellings too: an exclusion that fires has to be *named* in
    /// the answer, or nobody can tell which one to remove.
    pub exclude: Vec<(String, terms::TermPat)>,
}

pub struct Article {
    pub title: String,
    pub summary: String,
    pub content: String,
    /// Evidence only, never scored (Python's matcher ignores them too).
    pub entities: Vec<String>,
    pub tags: Vec<String>,
}

/// The article's text, folded, split into the three scored fields.
pub struct Fields {
    pub title: String,
    pub summary: String,
    pub content: String,
}

impl Fields {
    pub fn build(article: &Article) -> Fields {
        // Python folds the first 16 000 characters of the body and then
        // keeps the first 8 000 characters of the folded result.
        let head = chars::take_chars(&article.content, CONTENT_FOLD);
        let folded = fold::text(head);
        Fields {
            title: fold::text(&article.title),
            summary: fold::text(&article.summary),
            content: chars::take_chars(&folded, CONTENT_SCAN).to_string(),
        }
    }

    pub fn field(&self, index: usize) -> &str {
        match index {
            0 => &self.title,
            1 => &self.summary,
            _ => &self.content,
        }
    }

    /// Exclude terms are searched in one "\n"-joined blob, exactly like the
    /// Python `blob`: an exclude phrase can therefore span the title and the
    /// summary. Kept on purpose (see `exclude_spans_two_fields_like_python`).
    pub fn blob(&self) -> String {
        let mut out = String::with_capacity(
            self.title.len() + self.summary.len() + self.content.len() + 2,
        );
        out.push_str(&self.title);
        out.push('\n');
        out.push_str(&self.summary);
        out.push('\n');
        out.push_str(&self.content);
        out
    }
}

/// A string field: missing and `null` are "" (Python's `x or ""`), anything
/// else is a caller bug the bridge has to catch, not something to guess at.
fn string_field(obj: &Json, key: &str) -> Result<String, String> {
    match obj.get(key) {
        None | Some(Json::Null) => Ok(String::new()),
        Some(Json::Str(s)) => Ok(s.clone()),
        Some(_) => Err(format!("{} must be a string", key)),
    }
}

/// `[{"name": …}]` or `["…"]`, capped: this is evidence only.
///
/// The cap counts the *names kept*, not the items looked at: a list that opens
/// with a hundred shapes this crate has no name for still yields the names
/// behind them, instead of silently reporting no evidence at all.
fn name_list(obj: &Json, key: &str) -> Result<Vec<String>, String> {
    let items = match obj.get(key) {
        None | Some(Json::Null) => return Ok(Vec::new()),
        Some(Json::Arr(items)) => items,
        Some(_) => return Err(format!("{} must be an array", key)),
    };
    let mut out = Vec::new();
    for item in items.iter() {
        if out.len() == NAME_LIMIT {
            break;
        }
        match item {
            Json::Str(s) => out.push(s.clone()),
            Json::Obj(_) => {
                if let Some(Json::Str(s)) = item.get("name") {
                    out.push(s.clone());
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// `max(1, int(t["min_matches"] or 1))`, including Python's tolerance for a
/// numeric string and `int()`'s truncation toward zero.
fn min_matches(obj: &Json) -> Result<usize, String> {
    let raw = match obj.get("min_matches") {
        None | Some(Json::Null) => return Ok(1),
        Some(value) => value,
    };
    let number = match raw {
        Json::Num(n) => *n,
        Json::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Json::Str(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Ok(1);
            }
            match trimmed.parse::<f64>() {
                Ok(v) => v,
                Err(_) => return Err("min_matches must be a number".to_string()),
            }
        }
        _ => return Err("min_matches must be a number".to_string()),
    };
    if !number.is_finite() {
        return Ok(1);
    }
    let truncated = number.trunc();
    if truncated < 1.0 {
        Ok(1)
    } else if truncated > 50_000.0 {
        // Far beyond any real threshold; keeps the usize conversion sane.
        Ok(50_000)
    } else {
        Ok(truncated as usize)
    }
}

impl Topic {
    pub fn parse(obj: &Json, index: usize) -> Result<Topic, String> {
        if obj.get("id").is_none() {
            return Err(format!("topic {} is missing id", index));
        }
        let name = match obj.get("name") {
            Some(Json::Str(s)) => s.clone(),
            _ => return Err(format!("topic {} is missing name", index)),
        };
        let raw_keywords = string_field(obj, "keywords")?;
        let raw_exclude = string_field(obj, "exclude")?;
        let min = min_matches(obj)?;

        let mut seen = std::collections::HashSet::new();
        let mut terms = Vec::new();
        for term in terms::terms(&raw_keywords) {
            // "war" + "wars" in one topic must count once, so the family
            // that is kept is the one the user typed first.
            if !seen.insert(terms::stem_key(&term)) {
                continue;
            }
            terms.push((term.clone(), terms::compile(&term)));
        }
        let exclude = terms::terms(&raw_exclude)
            .into_iter()
            .map(|term| {
                let compiled = terms::compile(&term);
                (term, compiled)
            })
            .collect();
        Ok(Topic {
            id: obj.get("id").cloned().unwrap_or(Json::Null),
            name,
            min,
            terms,
            exclude,
        })
    }
}

impl Article {
    pub fn parse(obj: &Json) -> Result<Article, String> {
        // The Python signature calls the body `content`; the collector calls
        // it `text`. Both names are accepted.
        let content = match obj.get("content") {
            Some(Json::Null) | None => string_field(obj, "text")?,
            Some(_) => string_field(obj, "content")?,
        };
        // `url` and `publisher` are part of the payload and are validated,
        // but no topic decision is allowed to depend on where an article was
        // published, so neither is carried any further.
        let _ = string_field(obj, "url")?;
        let _ = string_field(obj, "publisher")?;
        Ok(Article {
            title: string_field(obj, "title")?,
            summary: string_field(obj, "summary")?,
            content,
            entities: name_list(obj, "entities")?,
            tags: name_list(obj, "tags")?,
        })
    }
}

// ---------------------------------------------------------------------------
// reading one payload: which topics, which article
// ---------------------------------------------------------------------------

pub(crate) fn topics_of(root: &Json) -> Result<Vec<Topic>, String> {
    let list = match root.get("topics") {
        Some(Json::Arr(items)) => items,
        Some(_) => return Err("topics must be an array".to_string()),
        None => return Err("missing topics".to_string()),
    };
    let mut out = Vec::with_capacity(list.len());
    for (index, item) in list.iter().enumerate() {
        out.push(Topic::parse(item, index)?);
    }
    Ok(out)
}

pub(crate) fn one_topic(root: &Json) -> Result<Topic, String> {
    match root.get("topic") {
        Some(value) => Topic::parse(value, 0),
        None => Err("missing topic".to_string()),
    }
}

/// The article lives under "article"; a flat payload is accepted too, so a
/// caller can hand over `{title, summary, text}` directly.
///
/// An `article` that is present but is not an object is a caller bug and is
/// refused. Reading it as "an article with every field empty" would answer
/// "this article matches no topic" — the one answer that is indistinguishable
/// from a correct one, and the caller would never learn it had sent garbage.
pub(crate) fn article_of(root: &Json) -> Result<Article, String> {
    match root.get("article") {
        None | Some(Json::Null) => Article::parse(root),
        Some(article @ Json::Obj(_)) => Article::parse(article),
        Some(_) => Err("article must be a JSON object".to_string()),
    }
}

/// The exclude blob is only worth building when some topic has exclusions.
pub(crate) fn blob_for(topics: &[Topic], fields: &Fields) -> Option<String> {
    if topics.iter().any(|topic| !topic.exclude.is_empty()) {
        Some(fields.blob())
    } else {
        None
    }
}
