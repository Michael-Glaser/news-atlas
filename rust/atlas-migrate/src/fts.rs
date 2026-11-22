//! Is the full-text index healthy, and does the query every search path runs still
//! answer?
//!
//! Decisions: how "complete" is measured (`article_fts_docsize` — one row per
//! indexed document — against `articles`, never a scan of `article_fts`, which reads
//! the content table and always agrees with it), what proves `ORDER BY rank` works
//! (the statement executes, whether or not it matches), and how staleness is probed
//! (a term from the article's *current* text must be found in the index for that
//! row).
//!
//! It does not: rebuild anything (`atlas-store`'s `FTS_REBUILD` statement is run by
//! [`crate::normalize`] on the write connection), or guess a term from a word list —
//! every probe term comes from the archive's own text.

use atlas_store::SqlValue;

use crate::error::Result;
use crate::findings::{Finding, Kind};
use crate::open::Archive;
use crate::queries;

/// How many words of one row the staleness probe tests.
pub const PROBE_WORDS: usize = 3;

/// What the index looks like.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FtsHealth {
    /// Rows in `articles`.
    pub articles: i64,
    /// Documents the index holds.
    pub documents: i64,
    /// `ORDER BY rank` answered without an SQL error.
    pub rank_ok: bool,
    /// `snippet()` answered without an SQL error.
    pub snippet_ok: bool,
    /// Rows the staleness probe read.
    pub probed: usize,
    /// Rows whose current text the index does not know.
    pub stale: usize,
    /// The term the rank query used, when the archive had one.
    pub term: String,
}

impl FtsHealth {
    /// The index holds exactly the articles, and both queries answer.
    pub fn complete(&self) -> bool {
        self.articles == self.documents && self.rank_ok && self.snippet_ok && self.stale == 0
    }
}

/// Run every full-text check. Returns the health snapshot and the findings.
pub fn check(archive: &Archive, probe_rows: usize) -> Result<(FtsHealth, Vec<Finding>)> {
    let mut findings: Vec<Finding> = Vec::new();
    let mut health = FtsHealth {
        articles: archive.count(&queries::count("articles"), &[])?,
        documents: archive.count(queries::FTS_DOCUMENTS, &[])?,
        rank_ok: false,
        snippet_ok: false,
        ..FtsHealth::default()
    };

    // 1. Membership: an article with no index entry cannot be found by search.
    let missing = archive.query(queries::FTS_MISSING, &[])?;
    if !missing.is_empty() {
        findings.push(
            Finding::new(Kind::FtsMissing, "article_fts", missing.len())
                .with_ids(ids_of(&missing, 20))
                .with_detail(format!("{} of {} articles", missing.len(), health.articles)),
        );
    }
    let orphans = archive.query(queries::FTS_ORPHANS, &[])?;
    if !orphans.is_empty() {
        findings.push(
            Finding::new(Kind::FtsOrphan, "article_fts", orphans.len())
                .with_ids(ids_of(&orphans, 20))
                .with_detail(format!("{} documents", health.documents)),
        );
    }

    // 2. The two queries the rest of the app runs against the index.
    let terms = probe_terms(archive, probe_rows.max(1))?;
    health.term = terms
        .first()
        .and_then(|(_, words)| words.first().cloned())
        .unwrap_or_default();
    match rank_rows(archive, health.term.as_str(), 5) {
        Ok(_) => health.rank_ok = true,
        Err(failure) => findings.push(
            Finding::new(Kind::FtsRank, "article_fts", 1).with_detail(failure.message().to_string()),
        ),
    }
    match snippet_rows(archive, health.term.as_str(), 1) {
        Ok(_) => health.snippet_ok = true,
        Err(failure) => findings.push(
            Finding::new(Kind::FtsSnippet, "article_fts", 1)
                .with_detail(failure.message().to_string()),
        ),
    }

    // 3. Staleness: every word the article has *now* must be in the index for that row. One
    //    word missing is a stale document — probing only the longest word let a stale title
    //    hide behind an unchanged summary.
    let mut stale_ids: Vec<String> = Vec::new();
    for (id, words) in &terms {
        health.probed += 1;
        let mut missing = false;
        for word in words {
            let found = archive.query_one(
                queries::FTS_ROW_FOR_TERM,
                &[SqlValue::Text(quoted(word)), SqlValue::Integer(*id)],
            )?;
            if found.is_none() {
                missing = true;
                break;
            }
        }
        if missing {
            stale_ids.push(id.to_string());
        }
    }
    health.stale = stale_ids.len();
    if !stale_ids.is_empty() {
        findings.push(
            Finding::new(Kind::FtsStale, "article_fts", stale_ids.len())
                .with_ids(stale_ids)
                .with_detail(format!(
                    "{} of {} probed articles",
                    health.stale, health.probed
                )),
        );
    }
    Ok((health, findings))
}

/// `ORDER BY rank` over the index, with a bound term and limit.
pub fn rank_rows(archive: &Archive, term: &str, limit: i64) -> Result<Vec<atlas_store::Row>> {
    archive.query(queries::FTS_RANK, &[SqlValue::Text(quoted(term)), SqlValue::Integer(limit)])
}

/// `snippet()` over the index.
pub fn snippet_rows(archive: &Archive, term: &str, limit: i64) -> Result<Vec<atlas_store::Row>> {
    archive.query(
        queries::FTS_SNIPPET,
        &[SqlValue::Text(quoted(term)), SqlValue::Integer(limit)],
    )
}

/// A probe term, quoted so FTS5 reads it as one literal token and not as syntax.
pub fn quoted(term: &str) -> String {
    let clean: String = term
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect();
    format!("\"{}\"", clean)
}

/// Terms taken from the rows themselves: the whole ASCII words of the headline, and of the
/// summary only when the headline offers none, longest first, up to [`PROBE_WORDS`].
///
/// A token is only usable when it is ASCII letters all the way through and 4–20 characters
/// long. That is what keeps the probe honest: `nchen`, split out of `München`, is not a
/// token FTS5 ever stored, so probing it would report a healthy index as stale — and the
/// headline comes first because the headline is what an edit changes, while a summary can
/// keep an indexed word that masks a stale title.
pub fn probe_terms(archive: &Archive, rows: usize) -> Result<Vec<(i64, Vec<String>)>> {
    let sample = archive.query(
        queries::PROBE_SAMPLE,
        &[SqlValue::Integer(rows as i64)],
    )?;
    let mut out: Vec<(i64, Vec<String>)> = Vec::new();
    for row in sample {
        let id = row.int("id").unwrap_or(0);
        let mut words = words_of(row.text("title").unwrap_or(""));
        if words.is_empty() {
            // A headline with no usable word: the summary is the only text there is.
            words = words_of(row.text("summary").unwrap_or(""));
        }
        words.sort_by_key(|word| std::cmp::Reverse(word.chars().count()));
        words.truncate(PROBE_WORDS);
        if words.is_empty() {
            // A row that offers no usable term simply does not contribute a probe.
            continue;
        }
        out.push((id, words));
    }
    Ok(out)
}

/// The usable probe words of one piece of text, de-duplicated, in the order they appear.
fn words_of(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for token in text.split_whitespace() {
        let token = token.trim_matches(|ch: char| !ch.is_alphanumeric());
        if token.chars().count() < 4 || token.chars().count() > 20 {
            continue;
        }
        // The *whole* token has to be ASCII letters: a fragment of `München` is not a word
        // any tokenizer stores.
        if !token.chars().all(|ch| ch.is_ascii_alphabetic()) {
            continue;
        }
        let word = token.to_ascii_lowercase();
        if !out.contains(&word) {
            out.push(word);
        }
    }
    out
}

fn ids_of(rows: &[atlas_store::Row], limit: usize) -> Vec<String> {
    rows.iter()
        .take(limit)
        .map(|row| {
            row.get("id")
                .map(|value| crate::redact::cell("id", value))
                .unwrap_or_else(|| "?".to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_term_is_quoted_syntax_free() {
        assert_eq!(quoted("AND"), "\"AND\"");
        assert_eq!(quoted("chip-export"), "\"chipexport\"");
        assert_eq!(quoted("(x OR"), "\"xOR\"");
    }

    #[test]
    fn terms_come_from_the_archive_and_never_from_a_list() {
        let path = crate::fixture::database_with_inconsistencies("fts-terms");
        let archive = Archive::open_read_only(&path).expect("open");
        let terms = probe_terms(&archive, 10).expect("terms");
        assert!(!terms.is_empty());
        // Every term is a word of a stored headline, lowercased, 4+ chars.
        for (_, words) in &terms {
            assert!(!words.is_empty());
            assert!(words.len() <= PROBE_WORDS);
            for term in words {
                assert!(term.chars().count() >= 4);
                assert!(term.chars().all(|ch| ch.is_ascii_lowercase()));
            }
        }
    }

    #[test]
    fn a_stale_title_is_found_even_when_the_summary_is_still_indexed() {
        // The probe used to test only the longest word of title+summary. A title edited
        // after indexing left the summary's longer word in place, so the index answered
        // that one word and the staleness went unreported.
        let path = crate::fixture::clean_database("fts-stale-title");
        {
            let store = crate::open::open_write(&path).expect("open");
            store
                .exec(
                    "UPDATE articles SET title='Brief note' WHERE id=1",
                    &[],
                )
                .expect("a newer headline");
        }
        let archive = Archive::open_read_only(&path).expect("open");
        let (health, findings) = check(&archive, 20).expect("check");
        assert!(health.probed > 0);
        assert!(health.stale >= 1, "the stale title was not reported: {:?}", health);
        let stale = findings
            .iter()
            .find(|finding| finding.kind == Kind::FtsStale)
            .expect("a stale finding");
        assert!(stale.ids.contains(&"1".to_string()), "ids {:?}", stale.ids);
    }

    #[test]
    fn a_word_split_out_of_a_non_ascii_token_is_not_probed() {
        // `München` splits into `M` and `nchen`; `nchen` is not a token FTS5 ever stored, so
        // probing it would report a healthy index as stale.
        let path = crate::fixture::clean_database("fts-unicode");
        {
            let store = crate::open::open_write(&path).expect("open");
            store
                .exec(
                    "UPDATE articles SET title='München report', summary='München report' \
                     WHERE id=1",
                    &[],
                )
                .expect("a non-ASCII headline");
            // The index is rebuilt through FTS5's own command, so it is fresh by construction.
            store
                .exec("INSERT INTO article_fts(article_fts) VALUES('rebuild')", &[])
                .expect("rebuild");
        }
        let archive = Archive::open_read_only(&path).expect("open");
        let (health, findings) = check(&archive, 20).expect("check");
        assert_eq!(
            health.stale, 0,
            "a fragment of a non-ASCII word was probed as if it were a token: {:?}",
            findings.iter().map(|f| f.line()).collect::<Vec<String>>()
        );
    }
}
