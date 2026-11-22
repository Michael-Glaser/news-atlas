//! The `rank` answer: one topic scored against a batch of articles.
//!
//! Decides: that the needle set is compiled once and each article costs one fold
//! and one pass over its three fields, that only hits are reported, the
//! score-descending order with arrivals kept on ties, and how `limit` truncates
//! (`0` and absent both mean "no limit"; a `limit` this crate cannot read is
//! refused instead of silently ignored). A batch entry it cannot read stops the
//! batch with the *position* of that entry in the message.
//!
//! Does not decide: the score itself (`crate::score`). Ranking a batch and
//! matching one article share the evaluator, so they cannot disagree.

use std::cmp::Ordering;

use atlas_lib::json::{self, Json};

use crate::model::{blob_for, one_topic, Article, Fields};
use crate::score::{evaluate, Kind, Verdict};
use crate::wire::{write_reason, write_string_list};

pub fn rank(root: &Json) -> Result<String, String> {
    let topic = one_topic(root)?;
    let articles = match root.get("articles") {
        Some(Json::Arr(items)) => items,
        Some(_) => return Err("articles must be an array".to_string()),
        None => return Err("missing articles".to_string()),
    };
    let limit = limit_of(root)?;
    // The needle set is built once (above); each article costs one fold and
    // one pass over each of its three fields.
    let mut rows: Vec<(usize, Json, Verdict)> = Vec::new();
    for (index, item) in articles.iter().enumerate() {
        // One unreadable article used to end the whole batch with a message
        // that did not say which one, so the caller had to guess. It names the
        // position in `articles` now.
        let article = Article::parse(item)
            .map_err(|reason| format!("articles[{}]: {}", index, reason))?;
        let fields = Fields::build(&article);
        let blob = blob_for(std::slice::from_ref(&topic), &fields);
        let verdict = evaluate(&topic, &fields, blob.as_deref());
        if verdict.kind == Kind::Hit {
            rows.push((index, item.get("id").cloned().unwrap_or(Json::Null), verdict));
        }
    }
    // Same ordering as the Python list: score descending, ties in arrival
    // order (Rust's sort_by is stable, like list.sort).
    rows.sort_by(|a, b| b.2.score.partial_cmp(&a.2.score).unwrap_or(Ordering::Equal));
    rows.truncate(limit);

    let mut out = String::with_capacity(128 + rows.len() * 160);
    out.push_str("{\"topic_id\":");
    json::write(&mut out, &topic.id);
    out.push_str(",\"count\":");
    out.push_str(&json::num_str(rows.len() as f64));
    out.push_str(",\"min_matches\":");
    out.push_str(&json::num_str(topic.min as f64));
    out.push_str(",\"matches\":[");
    for (i, (index, id, hit)) in rows.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"index\":");
        out.push_str(&json::num_str(*index as f64));
        out.push_str(",\"id\":");
        json::write(&mut out, id);
        out.push_str(",\"score\":");
        out.push_str(&json::fmt_score(hit.score));
        out.push_str(",\"terms_hit\":");
        out.push_str(&json::num_str(hit.terms_hit as f64));
        write_string_list(&mut out, "matched", &hit.matched);
        write_string_list(&mut out, "terms", &hit.terms);
        out.push_str(",\"reason\":");
        write_reason(&mut out, &hit.matched);
        out.push('}');
    }
    out.push_str("]}");
    Ok(out)
}

/// `limit`: absent, `null` and `0` all mean "every hit".
///
/// A `limit` that is not a number (`"5"` is a number to this reader, as it is
/// for `min_matches`) is refused rather than ignored: silently ranking the
/// whole archive because a caller sent `limit: "ten"` hides the mistake.
fn limit_of(root: &Json) -> Result<usize, String> {
    let raw = match root.get("limit") {
        None | Some(Json::Null) => return Ok(usize::MAX),
        Some(value) => value,
    };
    let number = match raw {
        Json::Num(n) => *n,
        Json::Str(s) => match s.trim().parse::<f64>() {
            Ok(v) => v,
            Err(_) => return Err("limit must be a number".to_string()),
        },
        _ => return Err("limit must be a number".to_string()),
    };
    if !number.is_finite() {
        return Err("limit must be a number".to_string());
    }
    let truncated = number.trunc();
    if truncated < 0.0 {
        return Err("limit must be zero or more".to_string());
    }
    if truncated == 0.0 {
        return Ok(usize::MAX);
    }
    // Anything past the number of articles is the same as no limit at all.
    Ok(truncated.min(usize::MAX as f64) as usize)
}
