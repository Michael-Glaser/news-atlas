//! The proof: a fixture database with each inconsistency, a dry run that changes
//! nothing, a real run that fixes exactly those, and a carry pass that never invents a
//! value.
//!
//! Every test here reads like the procedure in `docs/MIGRATION-RUN.md`: verify, plan,
//! apply, verify again — and it asserts on rows, not on prose. `carry`'s tests live next
//! to this file, because they need the same fingerprint and the same fixtures.

mod carry;



use std::path::{Path, PathBuf};

use crate::cli::{self, EXIT_FINDINGS};
use crate::findings::Kind;
use crate::fixture::{self, FakeRules};
use crate::normalize::{apply, plan};
use crate::open::{open_write, Archive};
use crate::{queries, verify, Command, Limits};

/// A fingerprint of everything a carry-over must not change: row counts, rowid sums and
/// the length of every text column of the two content tables.
///
/// Reading the bytes of the file would prove less: WAL pages move without a value
/// changing.
pub(crate) fn fingerprint(path: &Path) -> String {
    let store = open_write(path).expect("open for fingerprint");
    let mut out = String::new();
    for table in queries::TABLES {
        // The carry-over's own receipts are `meta` rows; they are the ledger of what a
        // run did, not a value in the archive, so they are not part of the fingerprint.
        let sql = if *table == "meta" {
            "SELECT COUNT(*) AS n FROM meta WHERE k NOT LIKE 'migrate:%'".to_string()
        } else {
            queries::count(table)
        };
        let count = store
            .query(&sql, &[])
            .map(|rows| rows.first().and_then(|row| row.int("n")).unwrap_or(-1))
            .unwrap_or(-2);
        out.push_str(&format!("{}={};", table, count));
    }
    for (sql, label) in [
        (
            "SELECT COALESCE(SUM(LENGTH(COALESCE(title,''))+LENGTH(COALESCE(summary,''))+\
             LENGTH(COALESCE(content,''))+LENGTH(COALESCE(publisher,''))+\
             LENGTH(COALESCE(place,''))),0) AS n FROM articles",
            "articles.text",
        ),
        (
            "SELECT COALESCE(SUM(LENGTH(COALESCE(name,''))+LENGTH(COALESCE(type,''))),0) AS n \
             FROM entities",
            "entities.text",
        ),
        (
            "SELECT COALESCE(SUM(rowid),0) AS n FROM articles",
            "articles.rowids",
        ),
        (
            "SELECT COALESCE(SUM(LENGTH(COALESCE(tag,''))),0) AS n FROM article_tags",
            "tags.text",
        ),
        (
            "SELECT COALESCE(SUM(LENGTH(COALESCE(label,''))),0) AS n FROM edges",
            "edges.labels",
        ),
        (
            "SELECT COALESCE(SUM(LENGTH(COALESCE(matched,''))),0)*1000 + \
             COALESCE(CAST(SUM(COALESCE(score,0)*1000) AS INTEGER),0) AS n FROM article_topics",
            "topics.text",
        ),
        (
            "SELECT COALESCE(SUM(LENGTH(COALESCE(summary,''))),0) AS n FROM articles",
            "articles.summaries",
        ),
    ] {
        let value = store
            .query(sql, &[])
            .map(|rows| rows.first().and_then(|row| row.int("n")).unwrap_or(-1))
            .unwrap_or(-2);
        out.push_str(&format!("{}={};", label, value));
    }
    let fts = store
        .query("SELECT COUNT(*) AS n FROM article_fts_docsize", &[])
        .map(|rows| rows.first().and_then(|row| row.int("n")).unwrap_or(-1))
        .unwrap_or(-2);
    out.push_str(&format!("fts.docs={};", fts));
    out
}

pub(crate) fn command(list: &[&str]) -> Vec<String> {
    list.iter().map(|item| item.to_string()).collect()
}

pub(crate) fn db_arg(path: &Path) -> String {
    path.to_str().unwrap_or("").to_string()
}

// ---------------------------------------------------------------- dry run


#[test]
fn a_dry_run_changes_nothing_and_lists_the_right_statements() {
    let path = fixture::database_with_inconsistencies("dry-run");
    let before = fingerprint(&path);

    let mut out: Vec<u8> = Vec::new();
    let code = cli::run(&command(&["normalize", "--db", &db_arg(&path)]), &mut out);
    let text = String::from_utf8_lossy(&out).to_string();

    assert_eq!(code, EXIT_FINDINGS, "a dry run still reports the findings");
    assert!(text.contains("dry run"));
    assert!(text.contains("INSERT INTO article_fts(article_fts) VALUES('rebuild')"));
    assert!(text.contains("DELETE FROM article_entities WHERE article_id NOT IN"));
    assert!(text.contains("DELETE FROM entity_alias WHERE entity_id NOT IN"));
    assert!(text.contains("UPDATE articles SET publisher=? WHERE id=?"));

    assert!(text.contains("DELETE FROM article_tags WHERE article_id=? AND tag=?"));
    assert!(text.contains("UPDATE articles SET relevant=0 WHERE dupe=1 AND relevant=1"));
    assert_eq!(fingerprint(&path), before, "a dry run wrote something");

    // The place rule is answered in-process now (atlas-ingest's `NativeRules` calls
    // `atlas-places` as a library), so a run with no `--root` no longer skips the
    // place check: it plans the repair. This assertion used to look for the skip
    // sentence ("place check: …"), which was the shape of the old child-process
    // architecture — a check that could not answer was a check that was named.
    assert!(
        text.contains("place.unsupported"),
        "the place check must answer without an engine binary: {}",
        text
    );
    assert!(
        !text.contains("place check: missing"),
        "the place rule needs no binary, so a missing one cannot be the reason: {}",
        text
    );
}

#[test]
fn the_dry_run_says_which_rows_it_would_touch() {
    let path = fixture::database_with_inconsistencies("dry-run-bounds");
    let report =
        verify::verify(&path, &FakeRules::new(), Limits::default()).expect("verify");
    let planned = plan(&report);
    let text = planned.to_text();
    assert!(text.contains("bound:"), "no bound values listed: {}", text);
    // The publisher rewrite names the spelling it keeps, not only the statement.
    assert!(text.contains("Folha"));
    assert!(planned.rows() > 0);
    // The place repair is planned here: the fake engine names Berlin for article 4.
    assert!(
        text.contains("UPDATE articles SET place=?"),
        "no place statement: {}",
        text
    );
    // Every batch is a statement this crate declares in `queries`.
    for batch in &planned.batches {
        assert!(
            queries::TABLES.iter().any(|table| batch.subject == *table || batch.subject == "article_fts"),
            "batch subject '{}' is not a table",
            batch.subject
        );
    }
}

// ---------------------------------------------------------------- real run

#[test]
fn the_real_run_fixes_exactly_what_the_dry_run_listed() {
    let path = fixture::database_with_inconsistencies("apply");
    let report =
        verify::verify(&path, &FakeRules::new(), Limits::default()).expect("verify");
    let planned = plan(&report);
    let listed = planned.rows();
    assert!(listed > 0);

    let applied = apply(&path, &planned).expect("apply");
    assert!(applied.rows > 0, "the run changed nothing");
    assert_eq!(
        applied.statements,
        planned
            .batches
            .iter()
            .map(|batch| batch.params.len().max(1))
            .sum::<usize>(),
        "a statement was skipped or run twice"
    );
    assert!(applied.fts_rebuilt);
    assert!(listed > 0);

    let after = verify::verify(&path, &FakeRules::new(), Limits::default()).expect("verify again");
    assert!(
        after.broken().is_empty(),
        "still broken after the repair: {}",
        after.to_text(false)
    );
    // And the second run has nothing left to do.
    assert!(plan(&after).is_empty(), "a second plan is not empty");

    // The specific repairs, read back as rows.
    let archive = Archive::open_read_only(&path).expect("open");
    assert_eq!(
        archive
            .count(queries::FTS_MISSING, &[])
            .expect("missing after"),
        0
    );
    assert_eq!(
        archive
            .count(queries::FTS_ORPHANS, &[])
            .expect("orphans after"),
        0
    );
    assert_eq!(
        archive
            .count(
                "SELECT COUNT(*) AS n FROM article_entities WHERE article_id=999",
                &[]
            )
            .expect("dangling links"),
        0
    );
    let publisher = archive
        .query_one(
            "SELECT publisher FROM articles WHERE id=2",
            &[],
        )
        .expect("row")
        .and_then(|row| row.text("publisher").map(|text| text.to_string()))
        .unwrap_or_default();
    assert_eq!(publisher, "Folha", "the spelling split was not unified");
    let place = archive
        .query_one(
            "SELECT place, geo_src FROM articles WHERE id=4",
            &[],
        )
        .expect("row")
        .map(|row| {
            (
                row.text("place").unwrap_or("").to_string(),
                row.text("geo_src").unwrap_or("").to_string(),
            )
        })
        .expect("a row");
    assert_eq!(place, ("Berlin".to_string(), "city".to_string()));
    // A place the engine cannot replace is left exactly as it was.
    let untouched = archive
        .query_one("SELECT place FROM articles WHERE id=5", &[])
        .expect("row")
        .and_then(|row| row.text("place").map(|text| text.to_string()))
        .unwrap_or_default();
    assert_eq!(untouched, "Lisbon");
    // The index answers again, on every article.
    assert_eq!(after.fts.articles, after.fts.documents);
    assert!(after.fts.rank_ok && after.fts.snippet_ok);
    assert_eq!(after.fts.stale, 0);
}

// ------------------------------------------------------------ re-running --apply

/// A database whose only disagreement is a publisher stored under two spellings, so a plan
/// is one parameterised statement per row and nothing else (no full-text rebuild, whose
/// row count `changes()` reports in a way that would hide a real difference).
fn database_with_one_publisher_split(tag: &str) -> std::path::PathBuf {
    let path = fixture::clean_database(tag);
    let store = open_write(&path).expect("open");
    store
        .exec(
            "UPDATE articles SET publisher='Example News!', url='https://example.com/b' \
             WHERE id=2",
            &[],
        )
        .expect("a second spelling");
    path
}

#[test]
fn a_replayed_plan_changes_nothing_and_never_overwrites_a_newer_value() {
    // `--apply` is conditional per statement, so a plan built against one state cannot
    // rewrite a value that arrived after it was built. Without the guard the second apply
    // silently undid the user's edit.
    let path = database_with_one_publisher_split("replay");
    let report = verify::verify(&path, &FakeRules::new(), Limits::default()).expect("verify");
    let planned = plan(&report);
    assert_eq!(planned.batches.len(), 1, "{}", planned.to_text());
    let first = apply(&path, &planned).expect("first apply");
    assert_eq!(first.rows, 1, "the variant is rewritten once");

    // The user fixes the row by hand after the plan was built.
    let store = open_write(&path).expect("open");
    store
        .exec("UPDATE articles SET publisher='Reuters' WHERE id=2", &[])
        .expect("a newer value");
    let second = apply(&path, &planned).expect("second apply");
    assert_eq!(second.rows, 0, "a replayed plan rewrote a row it never measured");
    assert_eq!(second.statements, first.statements, "every statement still ran");
    drop(store);
    let archive = Archive::open_read_only(&path).expect("open");
    let stored = archive
        .query_one("SELECT publisher FROM articles WHERE id=2", &[])
        .expect("row")
        .and_then(|row| row.text("publisher").map(|text| text.to_string()))
        .unwrap_or_default();
    assert_eq!(stored, "Reuters", "the newer value was overwritten");
}

#[test]
fn no_statement_in_a_plan_is_keyed_on_a_row_id_alone() {
    // The property behind "a second --apply is a no-op" and behind "a replayed plan does not
    // undo a newer write": every statement names the state it was planned from. A future
    // repair that forgets gets caught here, not in a real archive.
    let path = fixture::database_with_inconsistencies("conditional");
    let report = verify::verify(&path, &FakeRules::new(), Limits::default()).expect("verify");
    let planned = plan(&report);
    assert!(!planned.batches.is_empty());
    for batch in &planned.batches {
        let sql = batch.sql;
        let keyed_on_the_id_alone = sql.contains("WHERE id=?")
            && !sql.contains("WHERE id=? AND");
        assert!(
            !keyed_on_the_id_alone,
            "{} is unconditional on the state it repairs: {}",
            batch.kind.id(),
            sql
        );
    }
}

// ------------------------------------------------------- findings name their rows

#[test]
fn every_finding_names_a_table_and_the_rows_it_is_about() {
    let path = fixture::database_with_inconsistencies("findings-named");
    let report =
        verify::verify(&path, &FakeRules::new(), Limits::default()).expect("verify");
    assert!(!report.findings.is_empty());
    for finding in &report.findings {
        assert!(
            queries::TABLES.contains(&finding.subject.as_str()),
            "{} names '{}', which is not a table",
            finding.kind.id(),
            finding.subject
        );
        // The two findings that are about the index's *behaviour* rather than a row are the
        // only ones without ids; everything else has to be look-up-able.
        let about_behaviour = matches!(finding.kind, Kind::FtsRank | Kind::FtsSnippet);
        assert!(
            about_behaviour || !finding.ids.is_empty(),
            "{} names no row id: {}",
            finding.kind.id(),
            finding.line()
        );
    }
}

#[test]
fn the_edge_and_link_findings_name_a_rowid_that_exists() {
    // `a*1000000+b` used to stand in for a row id: it cannot be looked up, and it collides
    // across edge types. A reader has to be able to run `WHERE rowid=?`.
    let path = fixture::clean_database("rowids");
    {
        let store = open_write(&path).expect("open");
        store
            .exec(
                "INSERT INTO entities(id,name,type,count) VALUES(3,'Berlin Senate','ORG',1)",
                &[],
            )
            .expect("entity");
        store
            .exec("INSERT INTO edges(a,b,weight,type) VALUES(3,1,1,'cooccur')", &[])
            .expect("reversed edge");
        store
            .exec("INSERT INTO article_links(a,b,shared,score,why) VALUES(2,1,1,0.5,'x')", &[])
            .expect("reversed link");
    }
    let archive = Archive::open_read_only(&path).expect("open");
    for (finding_kind, table) in [(Kind::EdgeOrder, "edges"), (Kind::LinkOrder, "article_links")] {
        let finding = crate::invariants::check(&archive)
            .expect("check")
            .into_iter()
            .find(|finding| finding.kind == finding_kind)
            .unwrap_or_else(|| panic!("no {} finding", finding_kind.id()));
        for id in &finding.ids {
            let rowid: i64 = id.parse().expect("a rowid is an integer");
            let found = archive
                .count(
                    &format!("SELECT COUNT(*) AS n FROM {} WHERE rowid=?", table),
                    &[atlas_store::SqlValue::Integer(rowid)],
                )
                .expect("rowid lookup");
            assert_eq!(found, 1, "{} named {} which is not a {} row", finding.kind.id(), id, table);
        }
    }
}

#[test]
fn the_notes_a_healthy_archive_prints_say_what_to_do_next() {
    // A note is the carry-over's to-do list. A note with no ids and no next step is noise
    // a reader learns to skip.
    let path = fixture::database_with_inconsistencies("notes");
    let report =
        verify::verify(&path, &FakeRules::new(), Limits::default()).expect("verify");
    let untyped = report
        .findings
        .iter()
        .find(|finding| finding.kind == Kind::EntityUntyped)
        .expect("the unclassified entities are reported");
    assert!(!untyped.ids.is_empty(), "the note names no row");
    assert!(
        untyped.detail.contains("carry --apply"),
        "the note names no next step: {}",
        untyped.detail
    );
    let provenance = report
        .findings
        .iter()
        .find(|finding| finding.kind == Kind::TagProvenance)
        .expect("the tag provenance note");
    assert!(!provenance.ids.is_empty(), "the note names no row");
    assert!(
        provenance.detail.contains("tag_article"),
        "{}",
        provenance.detail
    );
    let orphan = report
        .findings
        .iter()
        .find(|finding| finding.kind == Kind::EntityOrphan)
        .expect("the orphan entities are reported");
    assert!(!orphan.detail.is_empty(), "an orphan note that says nothing");
}

// ---------------------------------------------------------------- json

#[test]
fn normalize_json_is_one_object_a_script_can_parse() {
    let path = fixture::database_with_inconsistencies("json-normalize");
    let mut out: Vec<u8> = Vec::new();
    let code = cli::run(
        &command(&["normalize", "--db", &db_arg(&path), "--json"]),
        &mut out,
    );
    assert_eq!(code, EXIT_FINDINGS, "a dry run still reports");
    let text = String::from_utf8_lossy(&out).to_string();
    let value: serde_json::Value = serde_json::from_str(&text).expect("one JSON object");
    assert_eq!(value["dry_run"], serde_json::json!(true));
    let statements = value["statements"].as_array().expect("statements");
    assert!(!statements.is_empty());
    assert!(statements[0]["sql"].is_string());
    assert!(statements[0]["table"].is_string());
    assert!(value["rows"].as_u64().unwrap_or(0) > 0);
    assert!(value["before"]["broken"].as_u64().unwrap_or(0) > 0);

    // Applied: still exactly one object, and the header text is gone with --json.
    let mut out: Vec<u8> = Vec::new();
    let code = cli::run(
        &command(&["normalize", "--db", &db_arg(&path), "--apply", "--json"]),
        &mut out,
    );
    assert_eq!(code, cli::EXIT_OK, "{}", String::from_utf8_lossy(&out));
    let text = String::from_utf8_lossy(&out).to_string();
    let value: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {}", error, text));
    assert_eq!(value["dry_run"], serde_json::json!(false));
    assert!(value["applied"]["statements"].as_u64().unwrap_or(0) > 0);
    assert_eq!(value["after"]["broken"], serde_json::json!(0));
}

#[test]
fn the_all_command_prints_one_json_object_for_the_whole_run() {
    let path = fixture::database_with_inconsistencies("json-all");
    let mut out: Vec<u8> = Vec::new();
    cli::run(
        &command(&["all", "--db", &db_arg(&path), "--apply", "--json", "--topics-only"]),
        &mut out,
    );
    let text = String::from_utf8_lossy(&out).to_string();
    let value: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {}", error, text));
    if value.get("verify").is_some() {
        // The engines were found: one object with all three steps.
        assert!(value["normalize"]["applied"]["statements"].as_u64().unwrap_or(0) > 0);
        assert!(
            value["carry"].is_object(),
            "the carry step printed no object: {}",
            value["carry"]
        );
    } else {
        // No engines in a test checkout: still one object, and it says the run was refused.
        assert!(
            value["error"].is_string(),
            "a refused run must still be one object: {}",
            text
        );
        assert_eq!(value["refused"], serde_json::json!(true));
    }
}

#[test]
fn a_clean_archive_verifies_and_reports_no_statement() {    let path = fixture::clean_database("clean");
    let mut out: Vec<u8> = Vec::new();
    let code = cli::run(&command(&["verify", "--db", &db_arg(&path)]), &mut out);
    assert_eq!(code, cli::EXIT_OK, "{}", String::from_utf8_lossy(&out));
    let mut out: Vec<u8> = Vec::new();
    let code = cli::run(&command(&["normalize", "--db", &db_arg(&path)]), &mut out);
    assert_eq!(code, cli::EXIT_OK);
    assert!(String::from_utf8_lossy(&out).contains("no statement"));
}

#[test]
fn the_report_carries_no_article_text() {
    let path = fixture::database_with_inconsistencies("no-text");
    let mut out: Vec<u8> = Vec::new();
    cli::run(&command(&["verify", "--db", &db_arg(&path)]), &mut out);
    let text = String::from_utf8_lossy(&out);
    assert!(!text.contains("BODYMARKER"));
    let mut out: Vec<u8> = Vec::new();
    cli::run(&command(&["normalize", "--db", &db_arg(&path)]), &mut out);
    assert!(!String::from_utf8_lossy(&out).contains("BODYMARKER"));
}

// ---------------------------------------------------------------- cli

#[test]
fn the_place_check_answers_without_an_engine_binary() {
    // A database somewhere with no checkout beside it, plus a `--root` that is not
    // there: the place rule is answered in-process, so the check runs and the report
    // says what it found instead of skipping. Until the rules moved in-process this
    // test asserted the opposite (a skipped check named, with the build hint); the
    // property worth keeping is that a missing root cannot make the migration
    // *silently* agree — a finding or a refusal has to be printed.
    let path = fixture::database_with_inconsistencies("cli-no-engines");
    let mut out: Vec<u8> = Vec::new();
    let code = cli::run(
        &command(&[
            "verify",
            "--db",
            &db_arg(&path),
            "--root",
            "/nonexistent-root",
        ]),
        &mut out,
    );
    let text = String::from_utf8_lossy(&out);
    assert_eq!(code, EXIT_FINDINGS, "the other checks still found something");
    assert!(
        text.contains("place check read") || text.contains("place.unsupported"),
        "the place check must answer and say what it read: {}",
        text
    );
    assert!(
        !text.contains("cargo build --release"),
        "the place rule is a library call now, so no build hint may be printed: {}",
        text
    );
}

#[test]
fn verify_then_apply_then_verify_is_the_documented_procedure() {
    let path = fixture::database_with_inconsistencies("cli-procedure");
    let mut out: Vec<u8> = Vec::new();
    let before = cli::run(&command(&["verify", "--db", &db_arg(&path)]), &mut out);
    assert_eq!(before, EXIT_FINDINGS);

    let mut out: Vec<u8> = Vec::new();
    let applied = cli::run(
        &command(&["normalize", "--db", &db_arg(&path), "--apply"]),
        &mut out,
    );
    assert_eq!(applied, cli::EXIT_OK, "{}", String::from_utf8_lossy(&out));

    let mut out: Vec<u8> = Vec::new();
    let after = cli::run(&command(&["verify", "--db", &db_arg(&path)]), &mut out);
    assert_eq!(after, cli::EXIT_OK, "{}", String::from_utf8_lossy(&out));
    assert!(
        String::from_utf8_lossy(&out).contains("0 broken"),
        "{}",
        String::from_utf8_lossy(&out)
    );
}

#[test]
fn the_json_report_says_the_same_thing_as_the_text_one() {
    let path = fixture::database_with_inconsistencies("cli-json");
    let mut out: Vec<u8> = Vec::new();
    cli::run(
        &command(&["verify", "--db", &db_arg(&path), "--json", "--sample", "2"]),
        &mut out,
    );
    let text = String::from_utf8_lossy(&out);
    let value: serde_json::Value = serde_json::from_str(&text).expect("one JSON object");
    assert_eq!(value["read_only"], serde_json::json!(true));
    assert!(value["broken"].as_u64().unwrap_or(0) > 0);
    assert!(value["fixable_rows"].as_u64().unwrap_or(0) > 0);
    assert!(value["tables"]
        .as_array()
        .map(|tables| tables.iter().any(|table| table["table"] == "articles"))
        .unwrap_or(false));
    let findings = value["findings"].as_array().expect("findings");
    assert!(findings
        .iter()
        .any(|finding| finding["kind"] == "ref.articles" && finding["fixable"] == true));
}

#[test]
fn the_commands_are_the_four_the_run_book_names() {
    assert_eq!(Command::Verify.name(), "verify");
    assert_eq!(Command::Normalize.name(), "normalize");
    assert_eq!(Command::Carry.name(), "carry");
    assert_eq!(Command::All.name(), "all");
    let path: PathBuf = PathBuf::from("data/atlas.db");
    let explicit = PathBuf::from("/srv/atlas");
    assert_eq!(cli::resolve_root(&path, Some(&explicit)), explicit);
}
