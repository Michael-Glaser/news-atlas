//! The `match` answer: every active topic one article belongs to.
//!
//! Decides: that every topic is evaluated once against one folded article, that
//! the hits are ordered by score with ties kept in arrival order (Python's stable
//! `sort(key=-score)`), and the shape of the reply — `matches`, `topic_ids`,
//! `count`, `relevant`, and one `rejected` row per topic that did not make it,
//! naming the exclude term that blocked it (`excluded_by`) or the keywords it
//! did match and how many short of `min_matches` it fell.
//!
//! Does not decide: the score itself (`crate::score`) or what a keyword is
//! (`crate::terms`). Evidence is attached last and can never change a match.

use std::cmp::Ordering;

use atlas_lib::json::{self, Json};

use crate::fold;
use crate::model::{article_of, blob_for, topics_of, Fields, Topic};
use crate::score::{evaluate, Kind, Verdict, EVIDENCE_BUDGET, EVIDENCE_MAX};
use crate::terms;
use crate::wire::{write_reason, write_string_list};

pub fn match_article(root: &Json) -> Result<String, String> {
    let topics = topics_of(root)?;
    let article = article_of(root)?;
    let fields = Fields::build(&article);
    let blob = blob_for(&topics, &fields);

    let mut hits: Vec<(usize, Verdict)> = Vec::new();
    let mut rejected: Vec<(usize, &'static str, Verdict)> = Vec::new();
    for (index, topic) in topics.iter().enumerate() {
        let verdict = evaluate(topic, &fields, blob.as_deref());
        match verdict.kind {
            Kind::Hit => hits.push((index, verdict)),
            Kind::Excluded => rejected.push((index, "excluded", verdict)),
            Kind::Below => rejected.push((index, "below_threshold", verdict)),
        }
    }
    // Python: out.sort(key=lambda x: -x["score"]) — stable, so topics with
    // the same score keep the order they arrived in.
    hits.sort_by(|a, b| b.1.score.partial_cmp(&a.1.score).unwrap_or(Ordering::Equal));

    let mut out = String::with_capacity(256 + hits.len() * 192);
    out.push_str("{\"matches\":[");
    for (i, (index, hit)) in hits.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_hit(&mut out, &topics[*index], hit);
    }
    out.push_str("],\"topic_ids\":[");
    for (i, (index, _)) in hits.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        json::write(&mut out, &topics[*index].id);
    }
    out.push_str("],\"count\":");
    out.push_str(&json::num_str(hits.len() as f64));
    out.push_str(",\"relevant\":");
    out.push_str(if hits.is_empty() { "false" } else { "true" });
    out.push_str(",\"rejected\":[");
    for (i, (index, why, verdict)) in rejected.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let topic = &topics[*index];
        out.push_str("{\"id\":");
        json::write(&mut out, &topic.id);
        out.push_str(",\"name\":");
        json::escape_str(&mut out, &topic.name);
        out.push_str(",\"reason_code\":");
        json::escape_str(&mut out, why);
        // `score` on a rejected row is what the keywords were worth, so the
        // row reads "3.25 from 1 term, needed 2". The `score` answer calls the
        // same number `raw_score` because it also reports the hit score.
        out.push_str(",\"score\":");
        out.push_str(&json::fmt_score(verdict.raw));
        out.push_str(",\"terms_hit\":");
        out.push_str(&json::num_str(verdict.terms_hit as f64));
        out.push_str(",\"min_matches\":");
        out.push_str(&json::num_str(topic.min as f64));
        write_string_list(&mut out, "matched", &verdict.matched);
        write_string_list(&mut out, "terms", &verdict.terms);
        write_string_list(&mut out, "excluded_by", &verdict.excluded_by);
        out.push_str(",\"reason\":");
        write_reason(&mut out, &verdict.matched);
        out.push('}');
    }
    out.push_str("],\"entity_hits\":[");
    write_evidence(&mut out, &topics, &article.entities, "entity");
    out.push_str("],\"tag_hits\":[");
    write_evidence(&mut out, &topics, &article.tags, "tag");
    out.push_str("]}");
    Ok(out)
}

fn write_hit(out: &mut String, topic: &Topic, hit: &Verdict) {
    out.push_str("{\"id\":");
    json::write(out, &topic.id);
    out.push_str(",\"name\":");
    json::escape_str(out, &topic.name);
    out.push_str(",\"score\":");
    out.push_str(&json::fmt_score(hit.score));
    write_string_list(out, "matched", &hit.matched);
    write_string_list(out, "terms", &hit.terms);
    out.push_str(",\"terms_hit\":");
    out.push_str(&json::num_str(hit.terms_hit as f64));
    out.push_str(",\"min_matches\":");
    out.push_str(&json::num_str(topic.min as f64));
    out.push_str(",\"reason\":");
    write_reason(out, &hit.matched);
    out.push('}');
}

/// `,"key":["a","b"]`

fn write_evidence(out: &mut String, topics: &[Topic], names: &[String], key: &str) {
    if names.is_empty() {
        return;
    }
    let mut budget = EVIDENCE_BUDGET;
    let mut written = 0usize;
    let folded: Vec<String> = names.iter().map(|name| fold::text(name)).collect();
    'topics: for topic in topics {
        for (term, compiled) in &topic.terms {
            for (index, name) in folded.iter().enumerate() {
                if budget == 0 || written >= EVIDENCE_MAX {
                    break 'topics;
                }
                budget -= 1;
                if terms::search(name, compiled) {
                    if written > 0 {
                        out.push(',');
                    }
                    written += 1;
                    out.push_str("{\"term\":");
                    json::escape_str(out, term);
                    out.push(',');
                    json::escape_str(out, key);
                    out.push(':');
                    json::escape_str(out, &names[index]);
                    out.push_str(",\"topic\":");
                    json::write(out, &topic.id);
                    out.push('}');
                }
            }
        }
    }
}
