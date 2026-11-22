//! Full-text search over `article_fts`: the query builder, the ranking order, the
//! snippet, the fallback scan that keeps it answering, and the rebuild that puts
//! the index back in sync.
//!
//! # One builder, and what it refuses to pass on
//!
//! A query arrives from a person typing or from a model writing JSON; neither
//! writes FTS5. So a query never reaches `MATCH` as written — it is reduced to the
//! alphabet the index actually holds ([`match_terms`]):
//!
//! * every character that is not alphanumeric is a separator, so `"`, `(`, `)`,
//!   `*`, `^`, `:`, `-`, `NEAR`, `AND`, `OR` and `NOT` are all just text — and
//!   `title:chip` is the two words `title` and `chip`, not a column filter;
//! * each run of alphanumerics becomes one term, quoted and marked as a prefix
//!   (`"chip"*`), so `chip` finds `chips` without a stemmer and without a
//!   vocabulary list — and the list's `q` (a `LIKE` substring) and this one find
//!   the same stories, which is the whole point of two routes sharing one meaning
//!   of `q`;
//! * a one-character term is dropped when a longer one is in the query (it carries
//!   almost no information), and kept when it is all the query has — `db.py`'s
//!   rule, kept because a model does search `Q3` or `x`;
//! * the terms are **`AND`ed**, deduplicated and capped at [`MAX_TERMS`]: the same
//!   meaning of `q` the article list uses (every word must be present), so
//!   `/api/articles?q=…`, `/api/search?q=…` and the model's `search_news` can agree
//!   instead of answering three different questions. Relevance then orders the rows
//!   that do match, which is what the ranking section below is about.
//!
//! The output therefore contains only `"`, `*`, space, `A`, `N`, `D` and the
//! query's own letters and digits — there is no way for a query to be FTS5 syntax,
//! which is the whole point (a bare `(` is a syntax error; a `"` is an unterminated
//! string).
//!
//! # Ranking, in this order (and why it is the same every run)
//!
//! 1. `bm25(article_fts)` — FTS5's relevance, best first, so of the articles that
//!    contain every word, the one that is most *about* them comes first.
//! 2. `published DESC` — of two equally relevant articles, the newer one first.
//! 3. `id DESC` — the tie that is left, so two rows with the same relevance and
//!    the same timestamp never swap places between two runs.
//!
//! The fallback scan has no relevance of its own to offer, so it is recency then
//! id. Both orders are total orders over the same rows, so the same archive and
//! the same query always answer the same way — [`Matched`] says which of the two
//! answered.
//!
//! That order is also why the index statement has the shape it has: the page is cut
//! by `bm25()` *inside* the `MATCH` (a co-routine that returns `LIMIT` row ids), and
//! only those rows are read. Sorting the matched articles in one join made SQLite
//! carry every match's whole row through a temp b-tree — measured p50 7.5–8.0 ms
//! against 3.3–3.9 ms for the same 25 rows on a file archive. [`index_sql`] keeps
//! the measurement, including the two `CROSS JOIN`s that stop the planner from
//! turning the filter into a probe of the index per article.
//!
//! # Where the two paths differ, and why that is said out loud
//!
//! The index matches a **word**, and only from its start (`"chip"*` finds `chips`,
//! not `microchip`). The fallback matches a **substring** (`LIKE '%chip%'` finds
//! both). So for a query that lands inside a word the scan can answer with rows the
//! index would not, and a fallback answer is therefore *approximately* the index's
//! answer — which is exactly why which path answered is part of the result rather
//! than an implementation detail (`the_scan_is_a_substring_where_the_index_is_a_word`
//! pins it).
//!
//! # A snippet, from where the words were
//!
//! Every row carries a `snippet` column: the text around the match with the
//! matched word in `[brackets]`, drawn from whichever of `title`, `summary`,
//! `content` carried it ([`snippet_around`]). One function serves both paths, so a
//! query does not change what its snippet looks like when the index stops
//! answering.
//!
//! It is *not* FTS5's `snippet()` function, and the reason is measured: FTS5
//! computes a snippet for every row the `MATCH` produced, before `LIMIT` applies.
//! On 1 000 articles with 2 kB bodies and a word in all of them, one statement with
//! `snippet()` in the select list measured 32.4 ms for 50 rows on this machine; the
//! same statement without it, selecting the same order and limit, measured 2.2 ms.
//! The function cost fifteen times what the search did, and the snippet is a
//! property of the rows that are returned — so it is computed for those.
//!
//! # What the two paths cost (measured, not claimed)
//!
//! 1 000 indexed articles, 2 kB bodies, 50 rows, warm, median of five runs, on the
//! machine this was written on — `the_index_path_is_the_faster_path_for_a_selective_query`
//! prints the numbers for the run in front of you:
//!
//! | query | index | scan |
//! | --- | --- | --- |
//! | a word one article contains | 0.2–0.4 ms | 1.6–1.8 ms |
//! | a word every article contains | 4.7–8.7 ms | 3.9–9.0 ms |
//!
//! So the index wins where selectivity exists, and a query whose word is in every
//! article is the scan's best case — the index still has to score every match with
//! bm25. The ranges are the spread across runs on a busy machine; the mechanism,
//! not the millisecond, is what the numbers are here to show.
//!
//! # The index and the articles table are two things, and they drift
//!
//! `article_fts` is an external-content table (`content='articles'`), so it is
//! only as good as the writes into it: a row stored without an index row is
//! invisible to search, and the live archive has had that gap. [`Store::fts_sync`]
//! reports the two counts, [`Store::fts_rebuild`] re-reads every article into the
//! index, and [`Store::fts_integrity_ok`] answers the narrower question FTS5 can
//! answer itself. Deliberately absent: any trigger on `articles`. The ingest path
//! indexes in the same transaction as it stores (see `atlas-ingest`), and a
//! trigger would double-write the body of every article.
//!
//! # What this module still does not do
//!
//! It does not decide what "relevant" means — it reads the stored `relevant` flag.
//! It does not write articles. It does not log a query: an error carries a reason
//! and a count.

use crate::clock::iso_ago;
use crate::error::Result;
use crate::params::{Row, SqlValue};
use crate::Store;

/// The most terms one `MATCH` may carry.
///
/// Eight, the same ceiling the article list's `q` filter uses: `q` is capped at
/// 200 characters, which can hold forty words, and every extra word costs an index
/// probe here and a three-column scan in the fallback. Past eight the caller is
/// pasting a paragraph, not searching.
pub const MAX_TERMS: usize = 8;

/// The longest one term may be, in characters. A 2 000-character "word" is not a
/// word, and it must not become a 2 000-character index probe.
pub const MAX_TERM_CHARS: usize = 48;

/// The most rows one call may ask for, whatever the caller passes: a search
/// result is a page, not the archive.
pub const MAX_LIMIT: i64 = 500;

/// How many characters of a column a snippet shows.
const SNIPPET_CHARS: usize = 240;

/// How far into a column the snippet looks for the matched word before it gives up
/// on that column. Bounded, because a 200 kB page is not a scan target: an article
/// body in this archive is a few thousand characters, and a match deeper than this
/// still puts its article in the answer — it just shows the next column's snippet.
const SNIPPET_SCAN_CHARS: usize = 40_000;

/// The columns a snippet may be drawn from, in the order they are tried: the
/// headline says what the story is, the summary says how it starts, the body is
/// the last resort.
const SNIPPET_COLUMNS: [&str; 3] = ["title", "summary", "content"];

/// Where a set of rows came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matched {
    /// The index answered: relevance order (`"index"`).
    Index,
    /// The index had nothing to say (no rows, or an unreadable index) and the rows
    /// come from the `LIKE` scan over the articles table, in recency order
    /// (`"scan"`).
    Scan,
    /// The query had characters but no word to search for, so nothing was asked
    /// and nothing came back (`"nothing"`): `%`, `(`, `—`. An *empty* query is not
    /// this — it is `db.py`'s "everything in the window", and it is a scan.
    Nothing,
}

impl Matched {
    /// The word a payload prints, so a caller can tell the three apart.
    pub fn name(&self) -> &'static str {
        match self {
            Matched::Index => "index",
            Matched::Scan => "scan",
            Matched::Nothing => "nothing",
        }
    }
}

/// How many documents the index holds, and how many articles exist to be indexed.
///
/// Two numbers rather than one because they answer two questions: `stored` is what
/// the archive is, `indexed` is what search can see. Equal is in sync; `indexed <
/// stored` is the gap [`Store::fts_rebuild`] closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexSync {
    /// Rows in the index (`article_fts_docsize`: one row per indexed document —
    /// `COUNT(*) FROM article_fts` reads the *content* table, not the index).
    pub indexed: i64,
    /// Rows in `articles`.
    pub stored: i64,
}

impl IndexSync {
    /// Every stored article can be found by search.
    pub fn in_sync(&self) -> bool {
        self.indexed == self.stored
    }
}

/// The terms a query is made of, in query order, deduplicated.
///
/// A term is a run of alphanumeric characters: everything else in the query is a
/// separator. Callers use these for two things — [`match_query`] quotes them into
/// FTS5 syntax, and [`snippet_around`] looks for them in the rows — so the
/// highlights always mark words the query actually asked for.
pub fn match_terms(q: &str) -> Vec<String> {
    let mut runs: Vec<String> = Vec::new();
    let mut current = String::new();
    for ch in q.chars() {
        if ch.is_alphanumeric() {
            if current.chars().count() < MAX_TERM_CHARS {
                current.push(ch);
            }
        } else if !current.is_empty() {
            runs.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        runs.push(current);
    }
    // `db.py`'s rule: one letter carries almost no information, and under `OR` it
    // matches nearly everything. It is kept when it is all the query has.
    let long: Vec<String> = runs
        .iter()
        .filter(|run| run.chars().count() > 1)
        .cloned()
        .collect();
    let chosen = if long.is_empty() { runs } else { long };
    let mut terms: Vec<String> = Vec::with_capacity(chosen.len().min(MAX_TERMS));
    for term in chosen {
        if terms.len() >= MAX_TERMS {
            break;
        }
        if !terms.contains(&term) {
            terms.push(term);
        }
    }
    terms
}

/// The `MATCH` string for a query — `""` when the query has no usable term, which
/// callers read as "do not ask the index".
///
/// Every term is a quoted prefix query, joined with `AND`: the query's words must
/// all be present, and each may be the start of a longer word. Only `"`, `*`,
/// space, `AND` and the query's own letters and digits can appear in the result, so
/// no query can become FTS5 syntax (see the module docs).
pub fn match_query(q: &str) -> String {
    match_terms(q)
        .iter()
        .map(|term| format!("\"{}\"*", term))
        .collect::<Vec<String>>()
        .join(" AND ")
}

/// The `LIKE` pattern for a query: `%query%` with the query's own `%`, `_` and `\`
/// escaped, so a search for `100%` finds `100%` and a search for `_` finds an
/// underscore instead of every row.
///
/// Public because the two statements here are not the only place a caller wants a
/// substring match on the same columns; whatever binds it **must** add
/// `ESCAPE '\'`, which is what makes the escaping real:
///
/// ```text
/// title LIKE ? ESCAPE '\'
/// ```
pub fn like_pattern(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    out.push('%');
    for ch in text.chars() {
        if ch == '\\' || ch == '%' || ch == '_' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('%');
    out
}

/// The snippet for a row: the first term the query used that the row contains, in
/// `[brackets]`, with the text around it — drawn from `title`, then `summary`, then
/// `content`, and from the column that matched.
///
/// The bracket covers the whole word the term matched, so a query for `chip` marks
/// `[chips]` in a story that says `chips` — the reader sees the word that was found,
/// not the prefix that was asked for.
///
/// Case is folded one character at a time (`char::to_lowercase` can expand to two
/// characters, which would shift every offset after it), and the search stops at
/// [`SNIPPET_SCAN_CHARS`] so one enormous body cannot decide how long a search takes.
fn snippet_around(row: &Row, terms: &[String]) -> String {
    for column in SNIPPET_COLUMNS {
        let Some(text) = row.text(column) else {
            continue;
        };
        let chars: Vec<char> = text.chars().take(SNIPPET_SCAN_CHARS).collect();
        let folded: Vec<char> = chars
            .iter()
            .map(|ch| ch.to_lowercase().next().unwrap_or(*ch))
            .collect();
        for term in terms {
            let needle: Vec<char> = term
                .chars()
                .map(|ch| ch.to_lowercase().next().unwrap_or(ch))
                .collect();
            if needle.is_empty() || needle.len() > folded.len() {
                continue;
            }
            let found = (0..=folded.len() - needle.len())
                .find(|start| folded[*start..*start + needle.len()] == needle[..]);
            let Some(mut start) = found else {
                continue;
            };
            // The whole word: a prefix query (`chip`) marks `[chips]`, and a
            // substring match in the fallback (`or` in "record") marks the word it
            // sits in. Both directions are widened, so the bracket never starts or
            // ends inside a word.
            let mut end = start + needle.len();
            while end < folded.len() && folded[end].is_alphanumeric() {
                end += 1;
            }
            while start > 0 && folded[start - 1].is_alphanumeric() {
                start -= 1;
            }
            let lead = SNIPPET_CHARS / 3;
            let from = start.saturating_sub(lead);
            let to = (end + (SNIPPET_CHARS - lead)).min(chars.len());
            let mut out = String::with_capacity(SNIPPET_CHARS + 8);
            if from > 0 {
                out.push('…');
            }
            for (index, ch) in chars[from..to].iter().enumerate() {
                let at = from + index;
                if at == start {
                    out.push('[');
                }
                out.push(*ch);
                if at == end - 1 {
                    out.push(']');
                }
            }
            if to < chars.len() {
                out.push('…');
            }
            return collapse_ws(&out);
        }
    }
    String::new()
}

/// One line: runs of whitespace become one space, ends trimmed.
///
/// `atlas-store` has no dependency on `atlas-core`'s text helpers, and a snippet is
/// the only text this crate ever shapes, so the rule is written here once.
fn collapse_ws(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(ch);
    }
    out
}

/// A row with the computed snippet appended as its last column.
fn with_snippet(row: &Row, snippet: String) -> Row {
    let mut columns = row.columns().to_vec();
    let mut values = row.values().to_vec();
    columns.push("snippet".to_string());
    values.push(SqlValue::Text(snippet));
    Row::new(columns, values)
}

/// The index statement: the page is chosen by the index, then those rows are read.
///
/// The shape is a co-routine rather than one join, and it is measured, not
/// stylistic. Written as a single `FROM article_fts JOIN articles a … ORDER BY
/// rank, a.published DESC, a.id DESC LIMIT ?`, SQLite cannot use FTS5's own
/// ordering through the join: it walks every match, fetches **each article's whole
/// row** into a temp b-tree (`USE TEMP B-TREE FOR ORDER BY` in the plan), and keeps
/// 25. On the archive `docs/PERFORMANCE.md` describes — 5 000 articles, 2.4 kB
/// bodies, one term in every article — that is 12 MB of text sorted per search:
/// p50 7.5–8.0 ms, p95 7.7–8.6 ms on a file archive (6.0 and 6.4–6.9 ms on an
/// in-memory one, which is what made it easy to miss). The same question written as
/// "score the matches, keep the top `LIMIT` ids, then read those rows" is p50
/// 3.3–3.9 ms, p95 3.7–4.0 ms on file (1.96 and 2.10 ms in memory). Both numbers
/// are reproducible without the harness:
/// `tests::a_search_over_the_whole_archive_is_what_store_fts_budgets`.
///
/// Two `CROSS JOIN`s are load-bearing: SQLite may reorder an inner join, and with
/// the article table offered as a filter the planner drives *from* `articles`
/// (`SEARCH f USING COVERING INDEX idx_articles_relevant`, then a probe of the
/// index per row) — measured at 1 024 ms for the same 25 rows. A `CROSS JOIN` fixes
/// the loop order, which is what keeps the costing on the index side.
///
/// No `snippet()`: see the module docs for the measurement that keeps it out of the
/// statement.
fn index_sql(relevant_only: bool, hours: Option<i64>, iso: Option<&str>) -> String {
    let mut inner = String::from(
        "SELECT article_fts.rowid AS rid, bm25(article_fts) AS score \
         FROM article_fts CROSS JOIN articles f ON f.id=article_fts.rowid \
         WHERE article_fts MATCH ?",
    );
    if relevant_only {
        inner.push_str(" AND f.relevant=1");
    }
    if hours.is_some() {
        // The cutoff is the archive's own timestamp format (`2026-11-05T10:00:00Z`,
        // `clock::iso_ago`), bound as a value. `datetime('now', ?)` would hand back
        // `2026-11-05 10:00:00` — a *different shape* — and `published >= ?` on two
        // different shapes is a string comparison that gets the cutoff's own
        // calendar day wrong: `T` sorts above the space, so every article published
        // that day passed no matter what the hour was. See
        // `tests::a_search_window_compares_timestamps_in_the_archive_format`.
        inner.push_str(" AND f.published >= ?");
    }
    if iso.is_some() {
        inner.push_str(" AND f.iso=?");
    }
    // The ranking contract in the module docs, in the order it documents. `bm25()`
    // is the value FTS5's `rank` column holds by default, named explicitly so the
    // outer statement can order by the same number the inner one cut the page with.
    inner.push_str(" ORDER BY score, f.published DESC, f.id DESC LIMIT ?");
    format!(
        "SELECT a.* FROM ({}) m CROSS JOIN articles a ON a.id=m.rid \
         ORDER BY m.score, a.published DESC, a.id DESC",
        inner
    )
}

/// The fallback statement: the same rows, found by substring, newest first.
///
/// One `LIKE` group per term, `AND`ed, because the fallback has to answer the
/// question the index was asked: every word present, in any column. The whole
/// query as one substring (`LIKE '%chip export%'`) is a different and much
/// narrower question, and it made the fallback answer nothing whenever a story
/// said the words apart.
fn scan_sql(relevant_only: bool, hours: Option<i64>, iso: Option<&str>, terms: usize) -> String {
    let group = "(title LIKE ? ESCAPE '\\' OR summary LIKE ? ESCAPE '\\' \
                 OR content LIKE ? ESCAPE '\\')";
    let mut clauses: Vec<String> = Vec::new();
    for _ in 0..terms {
        clauses.push(group.to_string());
    }
    if relevant_only {
        clauses.push("relevant=1".to_string());
    }
    if hours.is_some() {
        // Same rule as the index statement: the bound cutoff is an archive
        // timestamp, not SQLite's own `datetime()` shape.
        clauses.push("published >= ?".to_string());
    }
    if iso.is_some() {
        clauses.push("iso=?".to_string());
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    format!(
        "SELECT * FROM articles{} ORDER BY published DESC, id DESC LIMIT ?",
        where_sql
    )
}

impl Store {
    /// Full-text search over `article_fts`, with the `LIKE` scan as the fallback
    /// (`db.py: fts_query`).
    ///
    /// A broken index never breaks search: an error drains to the scan, exactly
    /// like the `except sqlite3.Error` there. [`Store::fts_query_traced`] is the
    /// same call when the caller wants to know which path answered.
    pub fn fts_query(
        &self,
        q: &str,
        limit: i64,
        hours: Option<i64>,
        iso: Option<&str>,
        relevant_only: bool,
    ) -> Result<Vec<Row>> {
        self.fts_query_traced(q, limit, hours, iso, relevant_only)
            .map(|(rows, _)| rows)
    }

    /// [`Store::fts_query`], plus where the rows came from and, for a caller that
    /// shows them, a `snippet` column drawn from the matched text.
    pub fn fts_query_traced(
        &self,
        q: &str,
        limit: i64,
        hours: Option<i64>,
        iso: Option<&str>,
        relevant_only: bool,
    ) -> Result<(Vec<Row>, Matched)> {
        let limit = limit.clamp(1, MAX_LIMIT);
        let terms = match_terms(q);
        let matched = match_query(q);
        if !matched.is_empty() {
            let mut params: Vec<SqlValue> = vec![SqlValue::Text(matched)];
            if let Some(hours) = hours {
                // The window, in the archive's timestamp format (see `index_sql`).
                params.push(SqlValue::Text(iso_ago(hours.max(0) as f64)));
            }
            if let Some(iso) = iso {
                params.push(SqlValue::Text(iso.to_string()));
            }
            params.push(SqlValue::Integer(limit));
            match self.query(&index_sql(relevant_only, hours, iso), &params) {
                // The index answered: its rows are already in relevance order. An
                // index that answered *nothing* is not an answer (it may be
                // half-built), so the scan decides.
                Ok(found) if !found.is_empty() => {
                    let rows = found
                        .iter()
                        .map(|row| with_snippet(row, snippet_around(row, &terms)))
                        .collect();
                    return Ok((rows, Matched::Index));
                }
                _ => {}
            }
        } else if !q.trim().is_empty() {
            // A query with characters but no word (`%`, `(`, `—`). There is nothing
            // to look for, and scanning for it with `LIKE` would either match the
            // whole archive (`%` as a wildcard) or nothing at all. The honest
            // answer is nothing, and the caller is told which case this is.
            return Ok((Vec::new(), Matched::Nothing));
        }
        let mut params: Vec<SqlValue> = Vec::with_capacity(terms.len() * 3 + 3);
        for term in &terms {
            let like = SqlValue::Text(like_pattern(term));
            params.push(like.clone());
            params.push(like.clone());
            params.push(like);
        }
        if let Some(hours) = hours {
            // Same window, same format, on the fallback statement too.
            params.push(SqlValue::Text(iso_ago(hours.max(0) as f64)));
        }
        if let Some(iso) = iso {
            params.push(SqlValue::Text(iso.to_string()));
        }
        params.push(SqlValue::Integer(limit));
        let scanned = self.query(&scan_sql(relevant_only, hours, iso, terms.len()), &params)?;
        let rows = scanned
            .iter()
            .map(|row| with_snippet(row, snippet_around(row, &terms)))
            .collect();
        Ok((rows, Matched::Scan))
    }

    /// The plain search every caller starts with: the index, relevant only, no
    /// window.
    pub fn search(&self, q: &str, limit: i64) -> Result<Vec<Row>> {
        self.fts_query(q, limit, None, None, true)
    }

    /// Documents in the index, and articles to be indexed.
    pub fn fts_sync(&self) -> Result<IndexSync> {
        let row = self.query_one(
            "SELECT (SELECT COUNT(*) FROM article_fts_docsize) AS indexed, \
             (SELECT COUNT(*) FROM articles) AS stored",
            &[],
        )?;
        Ok(match row {
            Some(row) => IndexSync {
                indexed: row.int("indexed").unwrap_or(0),
                stored: row.int("stored").unwrap_or(0),
            },
            None => IndexSync {
                indexed: 0,
                stored: 0,
            },
        })
    }

    /// Re-read every article into `article_fts` (`'rebuild'`): the one statement
    /// that closes a gap left by a write that stored a row without indexing it.
    ///
    /// The index is an external-content table, so this reads `articles` and rewrites
    /// the index from it — it cannot invent an article and it cannot lose one. It is
    /// not a repair for a corrupt database (that is `backup` + `sqlite3`), and it is
    /// not cheap on a large archive, so it is an ops call, not a per-search one.
    pub fn fts_rebuild(&self) -> Result<IndexSync> {
        self.exec("INSERT INTO article_fts(article_fts) VALUES('rebuild')", &[])?;
        self.fts_sync()
    }

    /// Does FTS5 agree that its own index structures are consistent?
    ///
    /// This is deliberately *not* "is everything indexed": a probe of this bundled
    /// SQLite shows `integrity-check` passing on an index that is missing a stored
    /// article. [`Store::fts_sync`] answers that question; this one answers "is the
    /// index readable at all".
    pub fn fts_integrity_ok(&self) -> bool {
        self.exec(
            "INSERT INTO article_fts(article_fts) VALUES('integrity-check')",
            &[],
        )
        .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::now_iso;
    use crate::tests::*;

    /// Index one article by hand, the way `collector.py` does: no triggers.
    fn index_article(store: &Store, id: i64, title: &str, summary: &str, content: &str) {
        store
            .exec(
                "INSERT INTO article_fts(rowid,title,summary,content) VALUES(?,?,?,?)",
                &[
                    int(id),
                    text(title),
                    text(summary),
                    text(content),
                ],
            )
            .expect("fts insert");
    }

    /// An article with a body, indexed.
    fn stored(
        store: &Store,
        title: &str,
        summary: &str,
        content: &str,
        published: &str,
    ) -> i64 {
        let id = store
            .insert(
                "INSERT INTO articles(title,summary,content,published,publisher,relevant) \
                 VALUES(?,?,?,?,?,1)",
                &[
                    text(title),
                    text(summary),
                    text(content),
                    text(published),
                    text("Reuters"),
                ],
            )
            .expect("insert article");
        index_article(store, id, title, summary, content);
        id
    }

    fn titles(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| row.text("title").unwrap_or("").to_string())
            .collect()
    }

    // ----------------------------------------------------------- the builder

    #[test]
    fn a_query_cannot_be_fts5_syntax() {
        // Every one of these is, or contains, FTS5 syntax. None of it survives:
        // the builder's alphabet is letters, digits, `"`, `*`, space, `A`, `N`, `D`.
        for query in [
            "chip:\"export rules\"",
            "NEAR(chip export)",
            "chip AND NOT (export OR *)",
            "^title: harvest",
            "\"\"",
            "((((",
            "a-b*c^d",
            "don't",
        ] {
            let match_query = match_query(query);
            for ch in match_query.chars() {
                assert!(
                    ch.is_alphanumeric() || ch == '"' || ch == '*' || ch == ' ',
                    "{query:?} leaked {ch:?} into {match_query:?}"
                );
            }
            // …and the statement that would have failed on the raw query works.
            let store = store();
            stored(&store, "Chip exports", "chips", "body", "2026-11-05T10:00:00Z");
            let rows = store
                .search(query, 10)
                .unwrap_or_else(|error| panic!("{query:?} broke the statement: {}", error.message()));
            assert!(rows.len() <= 1, "{query:?} invented rows");
        }
    }

    #[test]
    fn the_match_terms_are_words_quoted_prefixed_and_all_required() {
        // Every word must be present (the list's meaning of `q`), and each is asked
        // for as a prefix of the indexed word.
        assert_eq!(match_query("chip export"), "\"chip\"* AND \"export\"*");
        // quotes, parentheses and the colon are separators, not syntax — and
        // `title:chip` is not a column filter, it is the two words
        assert_eq!(
            match_query("title:chip \"export\""),
            "\"title\"* AND \"chip\"* AND \"export\"*"
        );
        assert_eq!(match_query("chip (export)*"), "\"chip\"* AND \"export\"*");
        // a repeated word is one term
        assert_eq!(match_query("chip chip CHIP"), "\"chip\"* AND \"CHIP\"*");
        // punctuation only: no term, so no MATCH at all
        assert_eq!(match_query(""), "");
        assert_eq!(match_query("()*:\"^"), "");
        // a lone one-character word is kept — it is all the query has
        assert_eq!(match_query("x"), "\"x\"*");
        // …and dropped when a longer word is there to search for
        assert_eq!(match_query("a chip"), "\"chip\"*");
        // two characters is a word: `us` is a country, not noise
        assert_eq!(match_query("us chip"), "\"us\"* AND \"chip\"*");
        // a script without spaces is one term, and its mark stays a prefix query
        assert_eq!(match_query("半導体 chip"), "\"半導体\"* AND \"chip\"*");
        // eight words, the same ceiling the list's filter uses
        let long: String = (0..20).map(|index| format!("word{index} ")).collect();
        assert_eq!(match_terms(&long).len(), MAX_TERMS);
    }

    #[test]
    fn the_like_pattern_escapes_what_like_would_read_as_a_wildcard() {
        assert_eq!(like_pattern("harvest"), "%harvest%");
        assert_eq!(like_pattern("100%"), "%100\\%%");
        assert_eq!(like_pattern("a_b"), "%a\\_b%");
        assert_eq!(like_pattern("c:\\x"), "%c:\\\\x%");
        assert_eq!(like_pattern(""), "%%");
        // Nothing a caller passes can end the pattern early or add a clause: the
        // only characters that mean something to LIKE are backslash-escaped.
        for hostile in ["%", "_", "\\", "'; DROP TABLE articles; --"] {
            let pattern = like_pattern(hostile);
            assert!(pattern.starts_with('%') && pattern.ends_with('%'));
        }
    }

    #[test]
    fn a_query_of_two_thousand_characters_becomes_eight_terms() {
        let long: String = (0..400)
            .map(|index| format!("word{index} "))
            .collect::<String>();
        let terms = match_terms(&long);
        assert_eq!(terms.len(), MAX_TERMS);
        assert_eq!(terms[0], "word0");
        // a single "word" longer than the cap is cut, not carried
        let enormous = "z".repeat(2_000);
        assert_eq!(match_terms(&enormous)[0].chars().count(), MAX_TERM_CHARS);
    }

    // ------------------------------------------------------------ the search

    #[test]
    fn fts_round_trip_uses_the_manual_index_like_db_py() {
        let store = store();
        // Neither db.py nor this crate installs triggers on article_fts: the index is
        // written by hand, which is what collector.py does.
        let id = insert_article(
            &store,
            "Chip exports hit a record",
            "Reuters",
            "2026-11-05T10:00:00Z",
        );
        index_article(
            &store,
            id,
            "Chip exports hit a record",
            "chips",
            "body",
        );
        let hits = store.search("exports", 10).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text("title"), Some("Chip exports hit a record"));
        // snippet() and bm25() work in the bundled build.
        let ranked = store
            .query(
                "SELECT snippet(article_fts, 0, '[', ']', '…', 8) s, bm25(article_fts) r \
                 FROM article_fts WHERE article_fts MATCH ? ORDER BY rank",
                &[text("\"exports\"")],
            )
            .expect("snippet + bm25");
        assert_eq!(ranked.len(), 1);
        assert!(ranked[0].text("s").unwrap_or("").contains('['));
        assert!(ranked[0].real("r").unwrap_or(0.0) < 0.0);
        // A query the index cannot match falls back to LIKE, still relevant only.
        insert_article(&store, "Unrelated", "AP", "2026-11-05T10:00:00Z");
        assert_eq!(store.search("unrelated", 10).expect("like fallback").len(), 1);
        // Nothing matches: empty, never an error.
        assert!(store.search("zzzzzz", 10).expect("no hits").is_empty());
        // An empty query never reaches MATCH; it goes straight to the LIKE fallback
        // with "%%", which is `db.py`'s behaviour: every relevant article comes back.
        assert_eq!(store.search("", 10).expect("empty query").len(), 2);
        // FTS syntax the matcher cannot parse falls back instead of failing.
        let syntax = store.search("AND OR (", 10).expect("syntax");
        assert!(syntax.len() <= 2);
    }

    #[test]
    fn quotes_parentheses_and_an_empty_phrase_still_answer() {
        let store = store();
        stored(
            &store,
            "Chip exports hit a record",
            "the rules changed",
            "seoul and taipei both noticed",
            "2026-11-05T10:00:00Z",
        );
        // The three shapes: a quoted phrase, parentheses, and nothing at all. A
        // quoted phrase is not a phrase — the quotes are separators and the words
        // are `AND`ed, which is why every one of these still finds the story.
        for query in ["\"chip exports\"", "(chip) (exports)", "chip)", "((((chip))))"] {
            let rows = store.search(query, 10).expect(query);
            assert_eq!(rows.len(), 1, "{query:?} should still find the story");
        }
        for query in ["\"\"", "()", "(   )", "-", "*", "^", "?!", "%_"] {
            let rows = store.search(query, 10).expect(query);
            assert!(rows.is_empty(), "{query:?} should match nothing: {rows:?}", rows = titles(&rows));
        }
        // A boolean operator a reader types is a word to look for, not an operator:
        // `AND`/`OR`/`NOT` are escaped into terms like any other word (see
        // `the_match_terms_are_words_quoted_prefixed_and_all_required`), which is
        // why the escaping has to be exhaustive — a query is never a program.
        // A query that matches nothing is an empty answer, not an error, on both
        // paths (this one has terms, so it went through the index first).
        let empty = store.search("zimbabwe lithium", 10).expect("no hits");
        assert!(empty.is_empty());
    }

    #[test]
    fn a_term_finds_the_longer_word_it_starts() {
        // No stemmer and no word list: `chip` must find `chips` because the term is
        // asked for as a prefix of the indexed token.
        let store = store();
        stored(
            &store,
            "Semiconductor supply",
            "chips are short again",
            "body",
            "2026-11-05T10:00:00Z",
        );
        assert_eq!(store.search("chip", 10).expect("prefix").len(), 1);
        assert_eq!(store.search("semiconductors", 10).expect("infix").len(), 0);
    }

    #[test]
    fn a_wildcard_in_a_query_is_a_character_and_never_a_wildcard() {
        let store = store();
        stored(
            &store,
            "Tariff bulletin",
            "the rate rose to 100% on steel",
            "body",
            "2026-11-05T10:00:00Z",
        );
        stored(
            &store,
            "Another story",
            "nothing in common",
            "body",
            "2026-11-04T10:00:00Z",
        );
        // Before the fix `LIKE '%%%'` matched every article: a query of one percent
        // sign returned the whole archive. Now a query with no word in it is
        // answered as such ([`Matched::Nothing`]) and asks no statement at all.
        let (rows, matched) = store
            .fts_query_traced("%", 10, None, None, true)
            .expect("percent");
        assert!(rows.is_empty(), "{:?}", titles(&rows));
        assert_eq!(matched, Matched::Nothing);
        // `_` is a literal underscore, and no article has one.
        for query in ["_", "___", "\\", "%_%"] {
            let rows = store.search(query, 10).expect(query);
            assert!(
                rows.is_empty(),
                "{query:?} matched {} rows instead of none",
                rows.len()
            );
        }
        // The literal characters are still searchable: `100%` finds `100%`.
        let percent = store.search("100%", 10).expect("literal percent");
        assert_eq!(percent.len(), 1, "{:?}", titles(&percent));
    }

    #[test]
    fn the_order_is_rank_then_recency_then_id() {
        let store = store();
        stored(
            &store,
            "Chip export rules tightened",
            "chip export",
            "chip export licences were tightened again this month",
            "2026-11-01T10:00:00Z",
        );
        stored(&store, "Chip export", "chip export", "", "2026-11-04T10:00:00Z");
        stored(&store, "Chip exports rose", "chip export", "", "2026-11-05T10:00:00Z");
        let rows = store.search("chip export", 10).expect("ranked");
        assert_eq!(rows.len(), 3);
        // The documented order, checked against the three keys read out of the
        // database rather than against the statement that produced the answer: sort
        // the matches by (relevance, newest, highest id) in the test, and the store's
        // answer must be exactly that order.
        let scored = store
            .query(
                "SELECT a.id, a.published, bm25(article_fts) AS r FROM article_fts \
                 JOIN articles a ON a.id=article_fts.rowid WHERE article_fts MATCH ?",
                &[text(&match_query("chip export"))],
            )
            .expect("scored");
        let mut expected: Vec<(i64, f64, String)> = scored
            .iter()
            .map(|row| {
                (
                    row.int("id").unwrap_or(0),
                    row.real("r").unwrap_or(0.0),
                    row.text("published").unwrap_or("").to_string(),
                )
            })
            .collect();
        expected.sort_by(|left, right| {
            left.1
                .partial_cmp(&right.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right.2.cmp(&left.2))
                .then_with(|| right.0.cmp(&left.0))
        });
        let got: Vec<i64> = rows.iter().filter_map(|row| row.int("id")).collect();
        let want: Vec<i64> = expected.iter().map(|(id, _, _)| *id).collect();
        assert_eq!(
            got, want,
            "the documented order is rank, then published, then id"
        );
        // …and asking twice answers the same way.
        assert_eq!(
            titles(&rows),
            titles(&store.search("chip export", 10).expect("twice"))
        );
    }

    #[test]
    fn rows_with_one_relevance_and_one_date_never_swap_places() {
        // Three articles with the same words and the same timestamp: bm25 ties,
        // `published` ties, so `id DESC` is the only key left and it is total.
        let store = store();
        for _ in 0..3 {
            stored(&store, "Harvest report", "grain", "grain", "2026-11-05T10:00:00Z");
        }
        let first = store.search("harvest", 10).expect("ties");
        let second = store.search("harvest", 10).expect("ties again");
        assert_eq!(first.len(), 3);
        let ids = |rows: &[Row]| rows.iter().filter_map(|row| row.int("id")).collect::<Vec<i64>>();
        assert_eq!(ids(&first), vec![3, 2, 1], "id DESC breaks the last tie");
        assert_eq!(ids(&first), ids(&second));
    }

    // ----------------------------------------------------------- the snippet

    #[test]
    fn the_scan_is_a_substring_where_the_index_is_a_word() {
        // The one honest difference between the two paths, pinned so it is a
        // documented property and not a surprise: the index matches a *word* (here
        // a prefix of one), the fallback matches a *substring*. For the query `or`,
        // "Orders fell" is a word match and "Tariff record" is only a substring
        // match inside "record" — so the index answers one row and the fallback two.
        // That is why `Matched` is part of the answer.
        let store = store();
        stored(&store, "Orders fell", "grain", "body", "2026-11-05T09:00:00Z");
        stored(&store, "Tariff record", "grain", "body", "2026-11-05T10:00:00Z");
        let (indexed, matched) = store
            .fts_query_traced("or", 10, None, None, true)
            .expect("index");
        assert_eq!(matched, Matched::Index);
        assert_eq!(titles(&indexed), vec!["Orders fell".to_string()]);
        // Take the index away: the same question, answered by substring.
        store
            .exec("DELETE FROM article_fts", &[])
            .expect("empty the index");
        let (scanned, matched) = store
            .fts_query_traced("or", 10, None, None, true)
            .expect("scan");
        assert_eq!(matched, Matched::Scan);
        assert_eq!(
            titles(&scanned),
            vec!["Tariff record".to_string(), "Orders fell".to_string()],
            "the scan finds `or` inside `record` as well"
        );
        assert_eq!(
            scanned[0].text("snippet"),
            Some("Tariff [record]"),
            "the bracket covers the word the substring sits in"
        );
    }

    #[test]
    fn the_snippet_is_drawn_from_where_the_words_were() {
        let store = store();
        stored(
            &store,
            "Semiconductor supply",
            "the ministry said nothing",
            "in the body: a harvest shortfall was reported by two traders",
            "2026-11-05T10:00:00Z",
        );
        let rows = store.search("harvest", 10).expect("content match");
        let snippet = rows[0].text("snippet").unwrap_or("").to_string();
        assert!(snippet.contains("[harvest]"), "{snippet:?}");
        assert!(
            !snippet.contains("Semiconductor"),
            "the snippet must come from the column that matched: {snippet:?}"
        );
        // A headline match draws from the headline.
        let rows = store.search("semiconductor", 10).expect("title match");
        let snippet = rows[0].text("snippet").unwrap_or("").to_string();
        assert!(snippet.contains("[Semiconductor]"), "{snippet:?}");
    }

    #[test]
    fn the_scan_path_marks_the_same_words() {
        // The same article, the same query, the index taken away: the row is found
        // by the scan and its snippet marks the same word in the same column.
        let store = store();
        stored(
            &store,
            "Semiconductor supply",
            "quiet",
            "a harvest shortfall was reported",
            "2026-11-05T10:00:00Z",
        );
        let (indexed, matched) = store
            .fts_query_traced("harvest", 10, None, None, true)
            .expect("index");
        assert_eq!(matched, Matched::Index);
        assert_eq!(
            indexed[0].text("snippet"),
            Some("a [harvest] shortfall was reported")
        );
        store
            .exec("DELETE FROM article_fts", &[])
            .expect("empty the index");
        let (scanned, matched) = store
            .fts_query_traced("harvest", 10, None, None, true)
            .expect("scan");
        assert_eq!(matched, Matched::Scan);
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].text("snippet"), indexed[0].text("snippet"));
        // …and both name their path, because a caller has to be able to tell.
        assert_eq!(Matched::Index.name(), "index");
        assert_eq!(Matched::Scan.name(), "scan");
    }

    #[test]
    fn the_bracket_covers_the_whole_word_that_was_found() {
        // The term is asked for as a prefix, so it can be shorter than the word in
        // the text: the snippet must mark the word, `[chips]`, not `[chip]s`.
        let store = store();
        stored(
            &store,
            "Semiconductor supply",
            "quiet",
            "chips are short again",
            "2026-11-05T10:00:00Z",
        );
        let rows = store.search("chip", 10).expect("prefix");
        assert_eq!(rows[0].text("snippet"), Some("[chips] are short again"));
    }

    #[test]
    fn a_long_body_becomes_one_line_of_snippet() {
        let store = store();
        let body = format!(
            "{}\n\n\n  harvest   shortfall  {}",
            "filler ".repeat(200),
            "tail ".repeat(200)
        );
        stored(&store, "Report", "nothing", &body, "2026-11-05T10:00:00Z");
        let rows = store.search("harvest", 10).expect("content match");
        let snippet = rows[0].text("snippet").unwrap_or("").to_string();
        assert!(snippet.contains("[harvest]"), "{snippet:?}");
        assert!(snippet.chars().count() <= SNIPPET_CHARS + 16, "{snippet:?}");
        assert!(!snippet.contains('\n'), "{snippet:?}");
        assert!(!snippet.contains("  "), "{snippet:?}");
        assert!(snippet.starts_with('…') && snippet.ends_with('…'), "{snippet:?}");
    }

    #[test]
    fn a_row_with_no_readable_match_still_gets_a_snippet_column() {
        // `db.py`'s empty query returns everything and matches nothing; the column
        // is still there, empty, so a caller never has to guess whether it exists.
        let store = store();
        stored(&store, "Harvest report", "grain", "grain", "2026-11-05T10:00:00Z");
        let rows = store.search("", 10).expect("empty query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text("snippet"), Some(""));
    }

    // ------------------------------------------------------- window/filters

    #[test]
    fn fts_query_honours_hours_iso_and_relevant_only() {
        let store = store();
        let id = store
            .insert(
                "INSERT INTO articles(title,summary,published,iso,relevant) VALUES(?,?,?,?,?)",
                &[
                    text("Harvest report"),
                    text("grain"),
                    text(&now_iso()),
                    text("FRA"),
                    int(1),
                ],
            )
            .expect("insert");
        index_article(&store, id, "Harvest report", "grain", "");
        // The limit is a page, not the archive: a caller cannot ask for everything.
        assert_eq!(store.search("harvest", 10_000).expect("clamped").len(), 1);
        assert_eq!(
            store
                .fts_query("harvest", 10, Some(48), Some("FRA"), true)
                .expect("windowed")
                .len(),
            1
        );
        assert_eq!(
            store
                .fts_query("harvest", 10, Some(48), Some("USA"), true)
                .expect("other iso")
                .len(),
            0
        );
        store
            .exec("UPDATE articles SET relevant=0 WHERE id=?", &[int(id)])
            .expect("flag");
        assert_eq!(
            store
                .fts_query("harvest", 10, None, None, true)
                .expect("relevant only")
                .len(),
            0,
            "relevant_only must exclude the row"
        );
        assert_eq!(
            store
                .fts_query("harvest", 10, None, None, false)
                .expect("any relevance")
                .len(),
            1
        );
    }

    // ------------------------------------------------- the index and its sync

    #[test]
    fn fts_index_membership_is_what_docsize_counts() {
        // The measurement behind the two comments in the module docs: a delete from
        // an external-content FTS5 table empties the index (`article_fts_docsize`)
        // while `SELECT COUNT(*) FROM article_fts` keeps reading the content table.
        let store = store();
        let id = insert_article(&store, "Old story", "AP", "2020-01-01T00:00:00Z");
        index_article(&store, id, "Old story", "", "");
        assert_eq!(fts_docs(&store), 1);
        assert_eq!(
            store
                .exec("DELETE FROM article_fts WHERE rowid IN (?)", &[int(id)])
                .expect("delete"),
            1
        );
        assert_eq!(fts_docs(&store), 0, "the index entry is gone");
        assert_eq!(
            store
                .query("SELECT COUNT(*) c FROM article_fts", &[])
                .expect("content scan")[0]
                .int("c"),
            Some(1),
            "a scan of article_fts counts the content table, not the index"
        );
    }

    #[test]
    fn a_deleted_article_leaves_the_index_and_the_search() {
        let store = store();
        let keep = stored(&store, "Harvest report", "grain", "grain", "2026-11-05T10:00:00Z");
        let gone = stored(&store, "Harvest forecast", "grain", "grain", "2026-11-05T11:00:00Z");
        assert_eq!(store.search("harvest", 10).expect("both").len(), 2);
        // The delete order `prune` uses: the index row first, then the article.
        store
            .exec("DELETE FROM article_fts WHERE rowid IN (?)", &[int(gone)])
            .expect("fts delete");
        store
            .exec("DELETE FROM articles WHERE id=?", &[int(gone)])
            .expect("article delete");
        let rows = store.search("harvest", 10).expect("after delete");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].int("id"), Some(keep));
        // A rebuild after the delete does not resurrect it: rebuild reads `articles`.
        let sync = store.fts_rebuild().expect("rebuild");
        assert!(sync.in_sync(), "{sync:?}");
        assert_eq!(store.search("harvest", 10).expect("after rebuild").len(), 1);
        assert_eq!(store.search("forecast", 10).expect("gone forever").len(), 0);
    }

    #[test]
    fn a_rebuild_indexes_every_stored_article_and_only_those() {
        let store = store();
        // Two articles stored without an index row — the live archive's gap.
        insert_article(&store, "Unindexed harvest story", "AP", "2026-11-05T10:00:00Z");
        insert_article(&store, "Second unindexed story", "AP", "2026-11-05T11:00:00Z");
        let before = store.fts_sync().expect("sync");
        assert_eq!(before, IndexSync { indexed: 0, stored: 2 });
        assert!(!before.in_sync());
        assert!(store.fts_integrity_ok(), "the index is readable, just empty");
        // The index has nothing, so the scan answers and says so.
        let (rows, matched) = store
            .fts_query_traced("harvest", 10, None, None, true)
            .expect("scan");
        assert_eq!(matched, Matched::Scan);
        assert_eq!(rows.len(), 1);
        // One call, and the index is the archive again.
        let after = store.fts_rebuild().expect("rebuild");
        assert_eq!(after, IndexSync { indexed: 2, stored: 2 });
        assert!(after.in_sync());
        assert!(store.fts_integrity_ok());
        let (rows, matched) = store
            .fts_query_traced("harvest", 10, None, None, true)
            .expect("index");
        assert_eq!(matched, Matched::Index, "the rebuild made the index answer");
        assert_eq!(rows.len(), 1);
        // Same rows either way: a path change is not a result change.
        assert_eq!(store.search("harvest", 10).expect("index").len(), 1);
    }

    #[test]
    fn the_index_and_the_scan_answer_with_the_same_rows() {
        // The two paths are one contract: for a query both can answer, the set of
        // articles (and the snippet's subject) must not depend on which one ran.
        let store = store();
        stored(
            &store,
            "Chip export rules",
            "chips and export licences",
            "body",
            "2026-11-05T10:00:00Z",
        );
        stored(
            &store,
            "Unrelated story",
            "nothing",
            "body",
            "2026-11-04T10:00:00Z",
        );
        let indexed = store.search("chip", 10).expect("index");
        assert!(!indexed.is_empty());
        // Take the index away and ask the same question.
        store
            .exec("DELETE FROM article_fts", &[])
            .expect("empty the index");
        let (scanned, matched) = store
            .fts_query_traced("chip", 10, None, None, true)
            .expect("scan");
        assert_eq!(matched, Matched::Scan);
        let ids = |rows: &[Row]| {
            let mut ids = rows.iter().filter_map(|row| row.int("id")).collect::<Vec<i64>>();
            ids.sort_unstable();
            ids
        };
        assert_eq!(ids(&indexed), ids(&scanned));
        // …and the rebuild closes the gap the delete opened.
        assert!(store.fts_rebuild().expect("rebuild").in_sync());
    }

    #[test]
    fn a_selective_query_is_planned_through_the_fts_index_and_here_is_what_it_costs() {
        // The numbers behind "the scan is the fallback, not the search": 1 000
        // indexed articles with 2 kB bodies, two queries answered once by each path,
        // warm, median of five, printed below (run with `--nocapture` to see them).
        //
        // What is *asserted* here is the plan, not the ratio. This test used to
        // assert `index_ms * 2.0 < scan_ms` and flaked about once in eight runs of a
        // full parallel `cargo test`: on a loaded machine the two medians close, and
        // a wall clock then measures how busy the box is rather than how the query
        // was answered. `EXPLAIN QUERY PLAN` says the same thing about the statement
        // that no other process can move — the index reads `article_fts`, the
        // fallback scans `articles` — and the milliseconds stay printed, because they
        // are the reason the index path exists at all. (Same shape as
        // `prune::tests::the_retention_cutoff_reads_an_index_and_here_is_what_it_bought`.)
        use std::time::Instant;

        fn median(mut values: Vec<f64>) -> f64 {
            values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
            values[values.len() / 2]
        }

        let store = store();
        let published = now_iso();
        let body = "the harvest report mentions grain and shipping costs ".repeat(40);
        let mut rows: Vec<Vec<SqlValue>> = Vec::with_capacity(1_000);
        for index in 0..1_000 {
            rows.push(vec![
                text(&format!("Story {index} about the harvest")),
                text("summary line"),
                text(&body),
                text(&published),
                int(1),
            ]);
        }
        assert_eq!(
            store
                .insert_many(
                    "INSERT INTO articles(title,summary,content,published,relevant) \
                     VALUES(?,?,?,?,?)",
                    &rows,
                )
                .expect("bulk insert"),
            1_000
        );
        assert!(store.fts_rebuild().expect("rebuild").in_sync());
        store
            .exec(
                "UPDATE articles SET content = content || ' lithium ' WHERE id = 7",
                &[],
            )
            .expect("one rare word");
        assert!(store.fts_rebuild().expect("rebuild again").in_sync());

        // (query, rows expected)
        let mut measured: Vec<(&str, usize, f64, f64)> = Vec::new();
        for (query, expected) in [("lithium", 1usize), ("harvest", 50)] {
            // warm
            let (warm, matched) = store.fts_query_traced(query, 50, None, None, true).expect(query);
            assert_eq!(matched, Matched::Index, "{query}");
            assert_eq!(warm.len(), expected, "{query}");
            let mut index_ms = Vec::new();
            for _ in 0..5 {
                let started = Instant::now();
                let (found, matched) = store.fts_query_traced(query, 50, None, None, true).expect(query);
                index_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
                assert_eq!(matched, Matched::Index);
                assert_eq!(found.len(), expected);
            }
            store
                .exec("DELETE FROM article_fts", &[])
                .expect("empty the index");
            let (warm, matched) = store.fts_query_traced(query, 50, None, None, true).expect(query);
            assert_eq!(matched, Matched::Scan, "{query}");
            assert_eq!(warm.len(), expected, "{query}");
            let mut scan_ms = Vec::new();
            for _ in 0..5 {
                let started = Instant::now();
                let (found, matched) = store.fts_query_traced(query, 50, None, None, true).expect(query);
                scan_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
                assert_eq!(matched, Matched::Scan);
                assert_eq!(found.len(), expected);
            }
            store.fts_rebuild().expect("rebuild after the scan");
            measured.push((query, expected, median(index_ms), median(scan_ms)));
        }

        for (query, count, index_ms, scan_ms) in &measured {
            println!(
                "search {query:?} over 1 000 articles (2 kB bodies, {count} rows): \
                 index {index_ms:.2} ms, scan {scan_ms:.2} ms"
            );
        }

        // The claim, as a plan: the index statement reads `article_fts` and the
        // fallback reads every row of `articles`. Both queries, so a change that
        // only routes one of them through the index cannot pass.
        let plan_of = |sql: &str, params: &[SqlValue]| {
            store
                .query(&format!("EXPLAIN QUERY PLAN {sql}"), params)
                .expect("a plan")
                .iter()
                .filter_map(|row| row.text("detail").map(|detail| detail.to_string()))
                .collect::<Vec<String>>()
                .join(" | ")
        };
        for (query, _, _, _) in &measured {
            let query = *query;
            let params = [text(&match_query(query)), int(50)];
            let plan = plan_of(&index_sql(true, None, None), &params).to_ascii_uppercase();
            assert!(
                plan.contains("VIRTUAL TABLE INDEX"),
                "{query:?} must be answered by the FTS index, not by reading the \
                 table: {plan}"
            );
            // …and the page is chosen *before* any article row is read: the plan
            // shows a co-routine that returns the ids, then a rowid lookup per id.
            // Without that, SQLite walks every match and sorts full rows through a
            // temp b-tree — `search::index_sql` has the measurement (6.1 ms against
            // 2.0 ms on 5 000 articles with one term in all of them).
            assert!(
                plan.contains("CO-ROUTINE"),
                "the index statement must cut the page in the co-routine before it \
                 reads article rows: {plan}"
            );
            // …and the same query on the fallback is the scan it is supposed to be,
            // which is what makes the line above a difference rather than a name.
            let terms = match_terms(query);
            let params: Vec<SqlValue> = terms
                .iter()
                .flat_map(|term| {
                    let like = SqlValue::Text(like_pattern(term));
                    [like.clone(), like.clone(), like]
                })
                .chain(std::iter::once(SqlValue::Integer(50)))
                .collect();
            let plan = plan_of(&scan_sql(true, None, None, terms.len()), &params).to_ascii_uppercase();
            assert!(
                plan.contains("ARTICLES") && !plan.contains("VIRTUAL TABLE"),
                "the fallback must read the table and never the index, or it is not a \
                 fallback: {plan}"
            );
        }
        // A word in every article is the scan's best case, and bm25 has to score all
        // 1 000 matches, so the two printed numbers are within a factor of the noise
        // on a busy machine — deliberately not asserted, for the reason in this
        // test's comment.
        let (_, count, _, _) = measured[1];
        assert_eq!(count, 50);
    }

    #[test]
    fn the_snippet_is_not_computed_by_the_database_for_rows_nobody_sees() {
        // The measurement above says why: `snippet()` in the statement is computed
        // for every row the MATCH produced (32.4 ms for 50 rows out of 1 000), not
        // for the 50 that are returned. This pins the fix without a clock: the
        // index statement never calls `snippet()`, the snippet is located in Rust
        // for the rows that leave the function.
        for (relevant_only, hours, iso) in [
            (true, None, None),
            (false, None, None),
            (true, Some(24), Some("FRA")),
            (false, Some(168), None),
        ] {
            let sql = index_sql(relevant_only, hours, iso);
            assert!(!sql.contains("snippet("), "{sql}");
            // The page is cut by the index's own score before any article row is
            // read (see `index_sql`), and ordered by that same number afterwards —
            // so a rewrite that goes back to sorting every matched article row
            // fails here without a clock.
            assert!(sql.contains("bm25(article_fts) AS score"), "{sql}");
            assert!(sql.contains("ORDER BY score"), "{sql}");
            assert!(
                sql.matches("LIMIT ?").count() == 1,
                "one page per statement: {sql}"
            );
        }
        // …and the scan builds one clause group per word it was given.
        assert_eq!(scan_sql(true, None, None, 0).matches("LIKE ?").count(), 0);
        assert_eq!(scan_sql(true, None, None, 2).matches("LIKE ?").count(), 6);
    }

    /// Indexed documents: `article_fts_docsize` holds one row per indexed doc.
    fn fts_docs(store: &Store) -> i64 {
        store
            .query("SELECT COUNT(*) c FROM article_fts_docsize", &[])
            .expect("docsize")
            .first()
            .and_then(|row| row.int("c"))
            .unwrap_or(-1)
    }
}

