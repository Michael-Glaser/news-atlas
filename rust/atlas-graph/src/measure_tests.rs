//! A measurement, not a test: what the name gate admits, over the live archive.
//!
//! This module is `#[ignore]`d — it reads files dumped from `data/atlas.db` into
//! `/tmp/atlas_measure` (read-only: `sqlite3 -readonly …`) and prints what the gate does
//! with them, so "removing the four word lists changed nothing in the archive" is a
//! command somebody can re-run instead of a claim:
//!
//! ```text
//! cargo test --offline -p atlas-graph --lib measure -- --ignored --nocapture
//! ```
//!
//! The numbers recorded before `JUNK_L`/`JUNK_BASE`/`UI_JUNK`/`OUTLETS` were deleted, from
//! the same inputs and the same harness (the old gate against the new one):
//!
//! ```text
//! stored entity names: 4539
//!   admitted by the old gate (with the four lists): 4532
//!   admitted by the structural gate:                4532
//!   old-only (silently hidden after the change):    0
//!   new-only (silently visible after the change):   0
//! articles: 1435
//!   distinct runs admitted by the old gate: 4031
//!   distinct runs admitted by the structural gate: 4240
//!   articles naming a word from the old OUTLETS list: 60
//!     publisher runs the old gate admitted: 0
//!     publisher runs the structural gate admits: 45
//! ```
//!
//! The publisher vocabulary is dumped *out of the old source* into `outlets.txt`, so this
//! file still works now that the list is gone: the words became measurement data, which is
//! what they always were.

use std::collections::BTreeMap;

use crate::Lex;

const DIR: &str = "/tmp/atlas_measure";

fn read_lines(path: &str) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => text.lines().map(|line| line.to_string()).collect(),
        Err(error) => panic!("measurement input {path}: {error}"),
    }
}

/// Capitalised runs in a headline and its summary — the candidates the ingest side feeds
/// the graph, as far as a measurement needs to imitate it: one to five words, each starting
/// with a capital, punctuation trimmed. `Lex::ok_name` is the judge, not this: this device
/// is deliberately naive so that what it admits is an upper bound on the real extractor.
fn runs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut run: Vec<String> = Vec::new();
    let flush = |run: &mut Vec<String>, out: &mut Vec<String>| {
        if !run.is_empty() {
            out.push(run.join(" "));
            run.clear();
        }
    };
    for raw in text.split_whitespace() {
        let token: String = raw
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '\'' && c != '’')
            .to_string();
        let capital = token.chars().next().map(|c| c.is_uppercase()).unwrap_or(false);
        if capital && token.chars().count() >= 3 {
            run.push(token);
            if run.len() == 5 {
                flush(&mut run, &mut out);
            }
        } else {
            flush(&mut run, &mut out);
        }
    }
    flush(&mut run, &mut out);
    out
}

#[test]
#[ignore = "measurement against a dump of data/atlas.db; run with --ignored --nocapture"]
fn measure_the_name_gate_against_the_archive() {
    let lex = Lex::new();
    let names = read_lines(&format!("{DIR}/names.tsv"));
    let articles = read_lines(&format!("{DIR}/articles.tsv"));
    let outlets: Vec<String> = read_lines(&format!("{DIR}/outlets.txt"));

    // 1. The names the archive already holds: how many would the gate still admit?
    let mut stored_total = 0usize;
    let mut stored_admitted = 0usize;
    let mut refused: Vec<String> = Vec::new();
    for line in &names {
        let name = line.split('\t').next().unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        stored_total += 1;
        if lex.ok_name(&name) {
            stored_admitted += 1;
        } else {
            refused.push(name);
        }
    }
    println!("stored entity names: {stored_total}");
    println!("  admitted by the structural gate: {stored_admitted}");
    println!("  refused  by the structural gate: {}", refused.len());
    for name in refused.iter().take(20) {
        println!("    - {name}");
    }

    // 2. What the extractor may now create from the text the archive holds.
    let mut candidate: BTreeMap<String, usize> = BTreeMap::new();
    let mut publisher_articles = 0usize;
    let mut publisher_runs = 0usize;
    let mut publisher_examples: Vec<String> = Vec::new();
    for line in &articles {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 4 {
            continue;
        }
        let text = format!("{} {}", fields[2], fields[3]);
        let mut saw_publisher = false;
        for run in runs(&text) {
            if !lex.ok_name(&run) {
                continue;
            }
            *candidate.entry(run.clone()).or_insert(0) += 1;
            let low = run.to_lowercase();
            if outlets.iter().any(|outlet| *outlet == low) {
                saw_publisher = true;
                publisher_runs += 1;
                if publisher_examples.len() < 20 && !publisher_examples.contains(&low) {
                    publisher_examples.push(low.clone());
                }
            }
        }
        if saw_publisher {
            publisher_articles += 1;
        }
    }
    println!();
    println!("articles: {}", articles.len());
    println!("  distinct runs admitted: {}", candidate.len());
    println!("  articles naming a word from the old OUTLETS list: {publisher_articles}");
    println!("    publisher runs admitted: {publisher_runs}");
    for example in &publisher_examples {
        println!("      + {example}");
    }
    let mut top: Vec<(&String, &usize)> = candidate.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    println!("  top runs by articles:");
    for (name, count) in top.iter().take(30) {
        println!("    {count:>4}  {name}");
    }
}

/// The type guess the ingest side used to make, measured the same way: how many stored
/// rows carry exactly the `two words ⇒ PERSON, anything else ⇒ ORG` answer.
#[test]
#[ignore = "measurement against a dump of data/atlas.db; run with --ignored --nocapture"]
fn measure_the_type_guess_against_the_archive() {
    let names = read_lines(&format!("{DIR}/names.tsv"));
    let mut total = 0usize;
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    let mut guessed = 0usize;
    let mut guessed_samples: Vec<String> = Vec::new();
    for line in &names {
        let fields: Vec<String> = line.split('\t').map(|field| field.to_string()).collect();
        if fields.len() < 2 {
            continue;
        }
        let (name, kind) = (fields[0].clone(), fields[1].clone());
        total += 1;
        *by_type.entry(kind.clone()).or_insert(0) += 1;
        let words = name.split_whitespace().count();
        let guess = if words == 2 { "PERSON" } else { "ORG" };
        if kind == guess {
            guessed += 1;
            if guessed_samples.len() < 15 {
                guessed_samples.push(format!("{name} ({kind})"));
            }
        }
    }
    println!("stored entities: {total}");
    for (kind, count) in &by_type {
        println!("  type {kind:<6} {count}");
    }
    println!("  rows carrying exactly the old two-word/ORG guess: {guessed}");
    for sample in &guessed_samples {
        println!("    · {sample}");
    }
}
