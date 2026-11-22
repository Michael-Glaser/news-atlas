//! Two spellings of one outlet.
//!
//! Decisions: when two stored publisher values are the *same outlet written
//! differently* — identical once case and punctuation are folded, or one's words are
//! a strict subset of the other's words **on the same delivery host** — and that the
//! spelling the archive itself uses most is the one to keep. A migration that
//! rewrote a publisher column leaves exactly this: the same newspaper under two
//! names, one of them on a handful of rows.
//!
//! It does not: keep a list of known outlets (that would be the forbidden
//! publisher-word rule), re-derive a publisher from a URL host (`atlas-text`'s
//! `publisher` is a host label — "Theguardian" for theguardian.com — and is worse
//! than what the archive already holds), or decide anything from article text.

use std::collections::BTreeMap;

use atlas_store::SqlValue;

use crate::error::Result;
use crate::findings::{Batch, Finding, Kind};
use crate::open::Archive;
use crate::queries;

/// One stored publisher value and the articles that carry it.
#[derive(Clone, Debug)]
pub struct Spelling {
    pub value: String,
    pub articles: Vec<i64>,
    /// Delivery hosts this spelling was stored under.
    pub hosts: Vec<String>,
}

impl Spelling {
    pub fn count(&self) -> usize {
        self.articles.len()
    }
}

/// Every stored publisher value, most used first.
pub fn spellings(archive: &Archive) -> Result<Vec<Spelling>> {
    let rows = archive.query(queries::ARTICLE_PUBLISHERS, &[])?;
    let mut by_value: BTreeMap<String, Spelling> = BTreeMap::new();
    for row in rows {
        let id = row.int("id").unwrap_or(0);
        let value = row.text("publisher").unwrap_or("").trim().to_string();
        if value.is_empty() {
            continue;
        }
        let host = host_of(row.text("url").unwrap_or(""));
        let entry = by_value.entry(value.clone()).or_insert_with(|| Spelling {
            value,
            articles: Vec::new(),
            hosts: Vec::new(),
        });
        entry.articles.push(id);
        if !host.is_empty() && !entry.hosts.contains(&host) {
            entry.hosts.push(host);
        }
    }
    let mut out: Vec<Spelling> = by_value.into_values().collect();
    out.sort_by(|a, b| b.count().cmp(&a.count()).then_with(|| a.value.cmp(&b.value)));
    Ok(out)
}

/// The authority of a URL, lowercased, with `www.` removed: grouping only.
pub fn host_of(url: &str) -> String {
    let trimmed = url.trim();
    let rest = match trimmed.find("://") {
        Some(at) => &trimmed[at + 3..],
        None => trimmed,
    };
    let authority = match rest.find(['/', '?', '#']) {
        Some(at) => &rest[..at],
        None => rest,
    };
    let authority = authority
        .rsplit('@')
        .next()
        .unwrap_or(authority)
        .to_ascii_lowercase();
    let authority = authority.split(':').next().unwrap_or("").to_string();
    authority.strip_prefix("www.").unwrap_or(&authority).to_string()
}

/// `folha de s.paulo` and `Folha` fold differently, but `NBC News` and `Nbcnews`
/// fold the same: lowercase letters and digits only.
pub fn folded(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

/// The words of a value: lowercase alphanumeric runs.
pub fn words(value: &str) -> Vec<String> {
    value
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| word.to_lowercase())
        .collect()
}

/// Is `short`'s word list a strict subset of `long`'s?
pub fn is_word_subset(short: &[String], long: &[String]) -> bool {
    if short.is_empty() || short.len() >= long.len() {
        return false;
    }
    short.iter().all(|word| long.contains(word))
}

/// Both spellings were stored under at least one shared host.
pub fn shares_host(a: &Spelling, b: &Spelling) -> bool {
    a.hosts.iter().any(|host| b.hosts.contains(host))
}

/// What one variant pair resolves to.
#[derive(Clone, Debug)]
pub struct Variant {
    /// The spelling to keep.
    pub keep: String,
    /// The spelling to replace.
    pub replace: String,
    /// Why they are one outlet — a rule name, never a value.
    pub rule: &'static str,
}

/// Every variant pair in the archive, deduplicated so no article is rewritten twice.
pub fn variants(all: &[Spelling]) -> Vec<Variant> {
    let mut out: Vec<Variant> = Vec::new();
    let mut claimed: Vec<String> = Vec::new();
    // 1. Folded equality: "NBC News" / "Nbcnews". Global, and safe on any host.
    for (index, spelling) in all.iter().enumerate() {
        if claimed.contains(&spelling.value) {
            continue;
        }
        let key = folded(&spelling.value);
        if key.is_empty() {
            continue;
        }
        let mut group: Vec<&Spelling> = vec![spelling];
        for other in all.iter().skip(index + 1) {
            if folded(&other.value) == key && !claimed.contains(&other.value) {
                group.push(other);
            }
        }
        if group.len() < 2 {
            continue;
        }
        // The most used spelling wins; a tie is broken by the value itself, so two
        // runs on the same archive write the same rows.
        group.sort_by(|a, b| b.count().cmp(&a.count()).then_with(|| a.value.cmp(&b.value)));
        let keep = group[0].value.clone();
        for other in group.iter().skip(1) {
            out.push(Variant {
                keep: keep.clone(),
                replace: other.value.clone(),
                rule: "folded",
            });
            claimed.push(other.value.clone());
        }
        claimed.push(keep);
    }
    // 2. Word subset on one host: "Folha" / "Folha de S.Paulo" on the same delivery
    //    host. Restricted to one host because two outlets legitimately share a
    //    prefix ("BBC" and "BBC World" are one, "Yahoo Finance" and "Yahoo Sports"
    //    are not — and the former share a host while the latter are two sites).
    for (index, spelling) in all.iter().enumerate() {
        if claimed.contains(&spelling.value) {
            continue;
        }
        let words_a = words(&spelling.value);
        for other in all.iter().skip(index + 1) {
            if claimed.contains(&other.value) || !shares_host(spelling, other) {
                continue;
            }
            let words_b = words(&other.value);
            if !(is_word_subset(&words_a, &words_b) || is_word_subset(&words_b, &words_a)) {
                continue;
            }
            let (keep, replace) = if spelling.count() >= other.count() {
                (spelling, other)
            } else {
                (other, spelling)
            };
            out.push(Variant {
                keep: keep.value.clone(),
                replace: replace.value.clone(),
                rule: "word-subset",
            });
            claimed.push(replace.value.clone());
        }
    }
    out
}

/// The finding for every variant, with one statement per article to rewrite.
///
/// The statement carries the spelling it replaces as its third bound value, so a plan
/// replayed after a third value was stored changes nothing instead of overwriting it.
pub fn check(archive: &Archive) -> Result<Vec<Finding>> {
    let all = spellings(archive)?;
    let pairs = variants(&all);
    let mut findings: Vec<Finding> = Vec::new();
    for pair in &pairs {
        let Some(spelling) = all.iter().find(|item| item.value == pair.replace) else {
            continue;
        };
        let mut batch = Batch::new(
            Kind::PublisherVariant,
            "articles",
            queries::SET_PUBLISHER,
        );
        for id in &spelling.articles {
            batch.push(vec![
                SqlValue::Text(pair.keep.clone()),
                SqlValue::Integer(*id),
                SqlValue::Text(pair.replace.clone()),
            ]);
        }
        let kept = all
            .iter()
            .find(|item| item.value == pair.keep)
            .map(Spelling::count)
            .unwrap_or(0);
        findings.push(
            Finding::new(Kind::PublisherVariant, "articles", spelling.count())
                .with_ids(spelling.articles.iter().take(20).map(|id| id.to_string()).collect())
                .with_detail(format!(
                    "keep {} (on {} row{}), replace {} (on {} row{}): {}",
                    crate::redact::cap(&pair.keep, crate::redact::CAP_CHARS),
                    kept,
                    if kept == 1 { "" } else { "s" },
                    crate::redact::cap(&pair.replace, crate::redact::CAP_CHARS),
                    spelling.count(),
                    if spelling.count() == 1 { "" } else { "s" },
                    pair.rule
                ))
                .with_batch(batch),
        );
    }
    Ok(findings)
}

/// The repair for an article with no publisher: the delivery host's label, which
/// `atlas-text` derives and `atlas-ingest` stores at ingest time.
///
/// This is a different disagreement from [`Kind::PublisherVariant`] — nothing is being
/// unified, a gap is being filled — so it carries its own id, its own statement (one that
/// only writes into an empty column) and its own bound.
pub fn fill_empty(
    archive: &Archive,
    rules: &dyn atlas_ingest::Rules,
    limit: usize,
) -> Result<Option<Finding>> {
    let rows = archive.query(
        queries::ARTICLES_WITHOUT_PUBLISHER,
        &[SqlValue::Integer(limit.max(1) as i64)],
    )?;
    if rows.is_empty() {
        return Ok(None);
    }
    let mut batch = Batch::new(Kind::PublisherEmpty, "articles", queries::SET_PUBLISHER_IF_EMPTY);
    let mut ids: Vec<String> = Vec::new();
    let mut unnamed = 0usize;
    for row in &rows {
        let id = row.int("id").unwrap_or(0);
        let url = row.text("url").unwrap_or("");
        match rules.publisher(url) {
            Ok(name) if !name.trim().is_empty() => {
                // The archive gets the engine's value byte for byte: `redact::cap` is a
                // display rule and would truncate a stored publisher.
                batch.push(vec![
                    SqlValue::Text(name.trim().to_string()),
                    SqlValue::Integer(id),
                ]);
                ids.push(id.to_string());
            }
            // A URL the engine cannot name a host for: no publisher is invented for it.
            _ => unnamed += 1,
        }
    }
    if batch.is_empty() {
        return Ok(None);
    }
    // A bound that is not stated reads like a complete pass: say how much of the gap the
    // run looked at, and that raising `--publisher-limit` widens it.
    let total = archive.count(queries::COUNT_WITHOUT_PUBLISHER, &[])? as usize;
    let mut detail = format!(
        "filled from the delivery host on {} row{}",
        batch.rows(),
        if batch.rows() == 1 { "" } else { "s" }
    );
    if total > rows.len() {
        detail.push_str(&format!(
            " of {} articles with no publisher ({} left for another run: raise \
             --publisher-limit)",
            total,
            total - batch.rows()
        ));
    }
    if unnamed > 0 {
        detail.push_str(&format!(", {} left empty (no host in the URL)", unnamed));
    }
    Ok(Some(
        Finding::new(Kind::PublisherEmpty, "articles", batch.rows())
            .with_ids(ids)
            .with_detail(detail)
            .with_batch(batch),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::FakeRules;

    #[test]
    fn hosts_and_folds_are_computed_without_a_parser_crate() {
        assert_eq!(host_of("https://www.TheGuardian.com/a/b?x=1"), "theguardian.com");
        assert_eq!(host_of("http://news.google.com/rss/articles/x"), "news.google.com");
        assert_eq!(host_of(""), "");
        assert_eq!(folded("NBC News"), "nbcnews");
        assert_eq!(folded("Folha de S.Paulo"), "folhadespaulo");
        assert_eq!(words("Folha de S.Paulo"), vec!["folha", "de", "s", "paulo"]);
    }

    #[test]
    fn a_word_subset_needs_fewer_words() {
        let short = words("Folha");
        let long = words("Folha de S.Paulo");
        assert!(is_word_subset(&short, &long));
        assert!(!is_word_subset(&long, &short));
        assert!(!is_word_subset(&long, &long));
    }

    #[test]
    fn two_genuinely_different_outlets_are_never_merged() {
        let short = words("Yahoo Finance");
        let long = words("Yahoo Sports");
        assert!(!is_word_subset(&short, &long));
    }

    #[test]
    fn a_folded_variant_is_one_outlet_and_a_different_name_is_not() {
        let path = crate::fixture::database_with_inconsistencies("publisher");
        let archive = Archive::open_read_only(&path).expect("open");
        let all = spellings(&archive).expect("spellings");
        let pairs = variants(&all);
        let names: Vec<(&str, &str, &str)> = pairs
            .iter()
            .map(|pair| (pair.keep.as_str(), pair.replace.as_str(), pair.rule))
            .collect();
        assert!(
            names.contains(&("NBC News", "Nbcnews", "folded")),
            "folded variant missing: {:?}",
            names
        );
        assert!(
            names.contains(&("Folha", "Folha de S.Paulo", "word-subset")),
            "host-restricted word variant missing: {:?}",
            names
        );
        // Two genuinely different outlets are never merged.
        assert!(!names.iter().any(|(keep, replace, _)| keep == &"Yahoo Finance"
            && replace == &"Yahoo Sports"));
    }

    #[test]
    fn a_stored_spelling_reaches_the_report_on_one_line() {
        // The detail carries archive values, so it goes through `redact`: a publisher a
        // page grew a newline or a control character into must not forge a finding line of
        // its own, and the rewrite it names must still be planned.
        let path = crate::fixture::database_with_inconsistencies("publisher-cap");
        {
            let store = crate::open::open_write(&path).expect("open");
            store
                .exec(
                    "UPDATE articles SET publisher=? WHERE id=5",
                    &[atlas_store::SqlValue::Text("Nbc\nnews\u{7}".to_string())],
                )
                .expect("hostile publisher");
        }
        let archive = Archive::open_read_only(&path).expect("open");
        let findings = check(&archive).expect("check");
        assert!(!findings.is_empty(), "the folded variant is still found");
        for finding in &findings {
            let line = finding.line();
            assert!(
                !line.contains('\n') && !line.contains('\u{7}'),
                "a stored value reached the report raw: {:?}",
                line
            );
        }
        // And the plan still binds the stored value byte for byte, because the guard has
        // to match the row it measured.
        let raw = findings
            .iter()
            .filter_map(|finding| finding.batch.as_ref())
            .flat_map(|batch| batch.params.iter())
            .filter_map(|params| match params.get(2) {
                Some(atlas_store::SqlValue::Text(text)) if text.contains('\u{7}') => {
                    Some(text.clone())
                }
                _ => None,
            })
            .count();
        assert_eq!(raw, 1, "the guard must bind the stored spelling unchanged");
    }

    #[test]
    fn an_empty_publisher_is_reported_as_a_fill_and_writes_only_into_the_gap() {
        let path = crate::fixture::database_with_inconsistencies("publisher-empty");
        {
            let store = crate::open::open_write(&path).expect("open");
            store
                .exec("UPDATE articles SET publisher='' WHERE id=1", &[])
                .expect("empty publisher");
        }
        let archive = Archive::open_read_only(&path).expect("open");
        let finding = fill_empty(&archive, &FakeRules::new(), 50)
            .expect("fill")
            .expect("a finding");
        assert_eq!(finding.kind, Kind::PublisherEmpty);
        assert!(!finding.line().contains("two spellings"), "{}", finding.line());
        let batch = finding.batch.as_ref().expect("a statement");
        assert_eq!(batch.sql, queries::SET_PUBLISHER_IF_EMPTY);
        // The statement cannot overwrite a value that arrived after the plan was built.
        {
            let store = crate::open::open_write(&path).expect("open");
            store
                .exec(
                    "UPDATE articles SET publisher='Reuters' WHERE id=1",
                    &[],
                )
                .expect("late publisher");
            let applied = crate::normalize::apply(
                &path,
                &crate::normalize::Plan {
                    batches: vec![batch.clone()],
                    unfixable: Vec::new(),
                },
            )
            .expect("apply");
            assert_eq!(applied.rows, 0, "a filled gap was overwritten");
        }
        let archive = Archive::open_read_only(&path).expect("open");
        let stored = archive
            .query_one("SELECT publisher FROM articles WHERE id=1", &[])
            .expect("row")
            .and_then(|row| row.text("publisher").map(|text| text.to_string()))
            .unwrap_or_default();
        assert_eq!(stored, "Reuters");
    }

    #[test]
    fn the_fill_is_bounded_so_verify_cannot_spawn_a_process_per_row() {
        let path = crate::fixture::database_with_inconsistencies("publisher-bound");
        {
            let store = crate::open::open_write(&path).expect("open");
            store
                .exec("UPDATE articles SET publisher=''", &[])
                .expect("empty publishers");
        }
        let archive = Archive::open_read_only(&path).expect("open");
        let finding = fill_empty(&archive, &FakeRules::new(), 2)
            .expect("fill")
            .expect("a finding");
        assert_eq!(
            finding.batch.as_ref().map(Batch::rows),
            Some(2),
            "the bound is what keeps verify cheap: {}",
            finding.line()
        );
        // A silent bound reads like a complete pass, so the detail states the remainder
        // and the flag that widens it.
        assert!(
            finding.detail.contains("--publisher-limit"),
            "the bound is not stated: {}",
            finding.detail
        );
    }
}
