//! Row counts and the sampled rows of every table.
//!
//! Decisions: which tables are reported ([`crate::queries::TABLES`]), that a sample is
//! the first `N` rows by rowid so two runs on one archive print the same report, and
//! that every cell goes through [`crate::redact`] — a body column is reported as a
//! length, never as text.
//!
//! It does not: judge a row, paginate, or hold more of the archive than the sample.

use crate::error::Result;
use crate::open::Archive;
use crate::queries;
use crate::redact;

/// One table and how many rows it holds.
#[derive(Clone, Debug)]
pub struct TableCount {
    pub table: &'static str,
    pub rows: i64,
}

/// The sampled rows of one table, already rendered.
#[derive(Clone, Debug)]
pub struct TableSample {
    pub table: &'static str,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<(String, String)>>,
}

impl TableSample {
    /// `id=1 title=… publisher=…` for one row.
    pub fn line(row: &[(String, String)]) -> String {
        row.iter()
            .map(|(column, value)| format!("{}={}", column, value))
            .collect::<Vec<String>>()
            .join(" ")
    }
}

/// Row counts for every table, in the documented order.
pub fn counts(archive: &Archive) -> Result<Vec<TableCount>> {
    let mut out: Vec<TableCount> = Vec::with_capacity(queries::TABLES.len());
    for table in queries::TABLES {
        if !archive.has_table(table)? {
            continue;
        }
        out.push(TableCount {
            table,
            rows: archive.count(&queries::count(table), &[])?,
        });
    }
    Ok(out)
}

/// The first `per_table` rows of every table, rendered.
pub fn samples(archive: &Archive, per_table: usize) -> Result<Vec<TableSample>> {
    let per_table = per_table.max(1);
    let mut out: Vec<TableSample> = Vec::with_capacity(queries::TABLES.len());
    for table in queries::TABLES {
        if !archive.has_table(table)? {
            continue;
        }
        let rows = archive.query(
            &queries::sample(table),
            &[atlas_store::SqlValue::Integer(per_table as i64)],
        )?;
        let columns: Vec<String> = rows
            .first()
            .map(|row| row.columns().to_vec())
            .unwrap_or_else(|| archive.columns(table).unwrap_or_default());
        let rendered: Vec<Vec<(String, String)>> = rows
            .iter()
            .map(|row| {
                row.values()
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        let column = columns
                            .get(index)
                            .cloned()
                            .unwrap_or_else(|| format!("#{}", index));
                        (column.clone(), redact::cell(&column, value))
                    })
                    .collect()
            })
            .collect();
        out.push(TableSample {
            table,
            columns,
            rows: rendered,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sample_is_the_first_rows_and_carries_no_body_text() {
        let path = crate::fixture::database_with_inconsistencies("samples");
        let archive = Archive::open_read_only(&path).expect("open");
        let counts = counts(&archive).expect("counts");
        let articles = counts
            .iter()
            .find(|count| count.table == "articles")
            .expect("articles counted");
        assert!(articles.rows > 0);
        let sampled = samples(&archive, 5).expect("samples");
        let article_sample = sampled
            .iter()
            .find(|sample| sample.table == "articles")
            .expect("articles sampled");
        assert!(!article_sample.rows.is_empty());
        assert!(article_sample.rows.len() <= 5);
        for row in &article_sample.rows {
            for (column, value) in row {
                if column == "content" || column == "summary" {
                    assert!(
                        value == "''"
                            || value == "NULL"
                            || (value.starts_with('<') && value.ends_with(" chars>")),
                        "{} carried text: {}",
                        column,
                        value
                    );
                }
            }
        }
        // Two runs read the same rows in the same order.
        let again = samples(&archive, 5).expect("samples again");
        let first: Vec<String> = article_sample
            .rows
            .iter()
            .map(|row| TableSample::line(row))
            .collect();
        let second: Vec<String> = again
            .iter()
            .find(|sample| sample.table == "articles")
            .expect("articles")
            .rows
            .iter()
            .map(|row| TableSample::line(row))
            .collect();
        assert_eq!(first, second);
    }
}
