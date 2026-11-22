//! What the carry-over does to a real archive, driven by a fake model.
//!
//! Decisions asserted here: the model's answer is what lands, an item the model did not
//! classify stays unclassified, an offline model writes nothing, a heuristic answer is
//! reverted whole, the topic pass is deterministic, and a resumed run does not ask twice.

use super::{command, db_arg, fingerprint};
use std::path::{Path, PathBuf};

use atlas_store::SqlValue;

use crate::carry::{self, Options};
use crate::cli;
use crate::fixture::{self, FakeModel};
use crate::open::{open_write, Archive};

// ---------------------------------------------------------------- carry

#[test]
fn the_carry_pass_takes_the_models_types_and_leaves_the_rest_unclassified() {
    let path = fixture::database_with_inconsistencies("carry-model");
    let model = FakeModel::answering(fixture::model_answer());
    let pipeline = fixture::pipeline(&path, model.clone());

    // The two articles that link entities: 3 and 4.
    let options = Options {
        dry_run: false,
        rate_ms: 0,
        ids: vec![3, 4],
        ..Options::default()
    };
    let outcome = carry::run_with(&pipeline, &options).expect("carry");
    assert_eq!(outcome.classified, 2, "the model answered for both");
    assert!(model.questions() >= 2, "the model was actually asked");
    assert_eq!(outcome.reverted, 0, "no heuristic write to undo");

    let archive = Archive::open_read_only(&path).expect("open");
    let kind = |name: &str| {
        archive
            .query_one("SELECT type FROM entities WHERE name=?", &[SqlValue::Text(name.to_string())])
            .expect("entity")
            .and_then(|row| row.text("type").map(|text| text.to_string()))
            .unwrap_or_default()
    };
    // The model named Aurora Systems: the guessed type is replaced by the model's.
    assert_eq!(kind("Aurora Systems"), "ORG");
    assert_eq!(kind("Shenzhen"), "LOC");
    // The model did not name Nvidia or "Nobody Links This": unclassified, not a guess.
    assert_eq!(kind("Nvidia"), "", "a name the model did not classify must stay empty");
    assert_eq!(
        kind("Nobody Links This"),
        "",
        "the guessed ORG must not survive a pass that did not classify it"
    );
    assert!(outcome.untyped >= 1, "the run counts the names left unclassified");
    let category = archive
        .query_one("SELECT category_id FROM entities WHERE name='Nvidia'", &[])
        .expect("entity")
        .and_then(|row| row.get("category_id").cloned());
    assert_eq!(category, Some(SqlValue::Null), "the old category must go with the type");
    // Links survive: article 3 still has its two entities (plus whatever the model added).
    let links = archive
        .count("SELECT COUNT(*) AS n FROM article_entities WHERE article_id=3", &[])
        .expect("links");
    assert!(links >= 2, "the collector's links were not restored");
    // The model's category landed on the article through atlas-ingest.
    assert!(archive
        .count("SELECT COUNT(*) AS n FROM article_categories WHERE article_id=4", &[])
        .expect("categories")
        >= 1);
}

#[test]
fn an_offline_model_writes_nothing_at_all() {
    let path = fixture::database_with_inconsistencies("carry-offline");
    let before = fingerprint(&path);
    let model = FakeModel::offline();
    let pipeline = fixture::pipeline(&path, model);
    let options = Options {
        dry_run: false,
        rate_ms: 0,
        ..Options::default()
    };
    let outcome = carry::run_with(&pipeline, &options).expect("carry");
    assert!(outcome.stopped.contains("not answering"));
    assert_eq!(outcome.classified, 0);
    assert_eq!(fingerprint(&path), before, "an offline model wrote something");
}

#[test]
fn a_silent_model_leaves_no_value_a_heuristic_guessed() {
    // The model is online but answers nothing usable: `atlas-ingest` falls back to its
    // heuristic receipt, and the guard must put every one of those writes back.
    let path = fixture::database_with_inconsistencies("carry-silent");
    let before = fingerprint(&path);
    let model = FakeModel::silent();
    let pipeline = fixture::pipeline(&path, model.clone());
    let options = Options {
        dry_run: false,
        rate_ms: 0,
        ids: vec![3, 4],
        retype: false,
        ..Options::default()
    };
    let outcome = carry::run_with(&pipeline, &options).expect("carry");
    assert_eq!(outcome.classified, 0);
    assert_eq!(outcome.unclassified, 2);
    assert!(model.questions() >= 2);
    let store = open_write(&path).expect("open");
    // The label's provenance is a `meta` row too — `reason:<a>:<b>`, written by
    // `atlas-ingest` beside the label it explains — and the guard must take it back with
    // the label. Measured before the guard: this run left three of them behind
    // (`reason:1:2` and `reason:4:4` for article 3, `reason:2:3` for article 4), each
    // one asserting that an edge was labelled for a reason while carrying no label.
    //
    // These come before the whole-archive fingerprint on purpose: when one of them is
    // wrong, the failure should name the row and not a count.
    for key in ["reason:1:2", "reason:4:4", "reason:2:3"] {
        assert_eq!(
            store.get_meta(key).expect("meta"),
            None,
            "{key} outlived the label it explains and claims a heuristic's decision"
        );
    }
    let provenance = store
        .query_one("SELECT COUNT(*) AS n FROM meta WHERE k LIKE 'reason:%'", &[])
        .expect("count")
        .and_then(|row| row.int("n"));
    assert_eq!(
        provenance,
        Some(0),
        "a pass that classified nothing left a label's provenance behind"
    );
    let after = fingerprint(&path);
    assert_eq!(
        after, before,
        "heuristic output survived: an item the model did not classify changed\nbefore: {}\nafter:  {}",
        before, after
    );
    assert!(
        crate::ledger::read(&store, 3).is_some(),
        "the article must be recorded as unclassified, not as carried"
    );
    assert_eq!(
        store
            .query_one("SELECT v FROM meta WHERE k='enrich:3'", &[])
            .is_ok_and(|row| row.is_none()),
        true,
        "the heuristic receipt must be dropped so a later run asks again"
    );

    // The label's provenance is a `meta` row too — `reason:<a>:<b>`, written by
    // `atlas-ingest` beside the label it explains — and the guard must take it back with
    // the label. Measured before the guard: this run left three of them behind
    // (`reason:1:2` and `reason:4:4` for article 3, `reason:2:3` for article 4), each
    // one asserting that an edge was labelled for a reason while carrying no label.

    // What *does* remain is the ledger record, and it says in its own words that the
    // model did not decide: `source: "none"`, plus how many writes were undone. That is
    // the only shape an unclassified article may leave in `meta`.
    let record = crate::ledger::read(&store, 3).expect("record");
    assert_eq!(record.source, "none", "a ledger row may not claim the model decided");
    assert!(record.reverted >= 1, "the record counts what the guard undid");
}

#[test]
fn a_models_label_keeps_the_provenance_that_explains_it() {
    // The other direction of the same rule: a marker that *is* needed keeps its shape.
    // The model labels an edge, `atlas-ingest` records why, and the carry-over does not
    // touch either — while the heuristic's version of the same row is dropped by the
    // test above.
    let path = fixture::database_with_inconsistencies("carry-provenance");
    let model = FakeModel::answering(fixture::model_answer());
    let pipeline = fixture::pipeline(&path, model.clone());
    let options = Options {
        dry_run: false,
        rate_ms: 0,
        ids: vec![3],
        ..Options::default()
    };
    let outcome = carry::run_with(&pipeline, &options).expect("carry");
    assert_eq!(outcome.classified, 1, "the model answered");

    let store = open_write(&path).expect("open");
    // The answer's relation is Aurora Systems (2) -> Shenzhen (1), "located in".
    let label = store
        .query_one(
            "SELECT COALESCE(label,'') AS l FROM edges WHERE a=? AND b=?",
            &[SqlValue::Integer(1), SqlValue::Integer(2)],
        )
        .expect("edge")
        .and_then(|row| row.text("l").map(|text| text.to_string()))
        .unwrap_or_default();
    assert_eq!(label, "located in", "the model's label is what landed");

    let reason = store
        .get_meta("reason:1:2")
        .expect("meta")
        .expect("the model's label keeps the row that explains it");
    assert!(
        reason.contains("\"source\":\"relations\""),
        "the row names the answer's own relations as its source, not a guess: {reason}"
    );
    assert!(
        reason.contains("\"article\":3"),
        "and the article whose answer produced it: {reason}"
    );
}

#[test]
fn the_topic_pass_is_deterministic_and_never_hides_a_story() {
    let path = fixture::clean_database("carry-topics");
    let model = FakeModel::offline();
    let pipeline = fixture::pipeline(&path, model);
    let options = Options {
        dry_run: false,
        rate_ms: 0,
        topics_only: true,
        ..Options::default()
    };
    let outcome = carry::run_with(&pipeline, &options).expect("carry");
    assert!(outcome.topics >= 1, "the stored topic matches article 2");
    let archive = Archive::open_read_only(&path).expect("open");
    let matched = archive
        .query_one(
            "SELECT matched, score FROM article_topics WHERE article_id=2 AND topic_id=1",
            &[],
        )
        .expect("assignment")
        .expect("article 2 matches the chip topic");
    assert!(matched.text("matched").unwrap_or("").contains("chip"));
    // Running it again writes nothing: the same assignment, unchanged.
    let again = carry::run_with(&pipeline, &options).expect("carry again");
    assert_eq!(again.topics, 0, "the topic pass is not idempotent");
    // Nothing was hidden: `relevant` stays as it was.
    assert_eq!(
        archive
            .count("SELECT COUNT(*) AS n FROM articles WHERE relevant=0", &[])
            .expect("hidden"),
        0
    );
}

#[test]
fn a_carried_article_is_skipped_by_the_next_run_and_a_resume_is_cheap() {
    let path = fixture::database_with_inconsistencies("carry-resume");
    let model = FakeModel::answering(fixture::model_answer());
    let pipeline = fixture::pipeline(&path, model.clone());
    let options = Options {
        dry_run: false,
        rate_ms: 0,
        limit: 2,
        ..Options::default()
    };
    let both = Options {
        ids: vec![3, 4],
        ..options.clone()
    };
    let first = carry::run_with(&pipeline, &both).expect("first run");
    assert_eq!(first.classified, 2);
    let asked = model.questions();
    let second = carry::run_with(&pipeline, &both).expect("second run");
    assert_eq!(second.classified, 0, "a receipt must be honoured");
    assert_eq!(second.already, 2);
    assert_eq!(model.questions(), asked, "the model was asked again for a carried article");
    // And a run that starts over can force it.
    let forced = Options {
        force: true,
        ..both.clone()
    };
    let third = carry::run_with(&pipeline, &forced).expect("forced run");
    assert_eq!(third.classified, 2);
}

/// Make a file unwritable for the current user, so a write-open fails at the syscall and
/// not only in a promise. Restored before the test returns.
struct Unwritable {
    path: PathBuf,
    mode: u32,
}

impl Unwritable {
    fn new(path: &Path) -> Unwritable {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path).map(|meta| meta.permissions().mode()).unwrap_or(0o644);
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444));
            Unwritable { path: path.to_path_buf(), mode }
        }
        #[cfg(not(unix))]
        {
            Unwritable { path: path.to_path_buf(), mode: 0 }
        }
    }
}

impl Drop for Unwritable {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(self.mode));
        }
    }
}

#[test]
fn a_carry_dry_run_reports_the_bounds_the_real_run_would_use() {
    // The dry run used to ignore `--limit` and accept any id: it could print "considered
    // 1 405" for a run that would consider 5, and count an article that does not exist as
    // work it could do.
    let path = fixture::database_with_inconsistencies("carry-dry-bounds");
    let model = FakeModel::answering(fixture::model_answer());
    let pipeline = fixture::pipeline(&path, model);
    let options = Options {
        limit: 2,
        ..Options::default()
    };
    let outcome = carry::run_with(&pipeline, &options).expect("dry run");
    assert!(outcome.candidates > 2, "the fixture has more than two articles");
    assert_eq!(outcome.considered, 2, "the run would stop at the limit");
    assert!(
        outcome.stopped.contains("--limit 2"),
        "the bound it applied is not stated: {}",
        outcome.stopped
    );

    let unknown = Options {
        ids: vec![3, 999_999],
        ..Options::default()
    };
    let outcome = carry::run_with(&pipeline, &unknown).expect("dry run with ids");
    assert_eq!(outcome.missing_ids, vec![999_999]);
    assert_eq!(outcome.considered, 1, "an id that names no article is not work");
    assert!(
        outcome.stopped.contains("999999"),
        "the id that named nothing is not reported: {}",
        outcome.stopped
    );
}

#[test]
fn a_type_the_model_confirmed_survives_a_later_pass_that_does_not_confirm_it() {
    // A pass that answered with the type the entity already carried is still the model's
    // answer. Without a receipt for it, the next pass treats the type as a guess, clears it,
    // and a silent model leaves it empty — the classification is gone.
    let path = fixture::database_with_inconsistencies("carry-confirmed");
    let options = Options {
        dry_run: false,
        rate_ms: 0,
        ids: vec![3],
        ..Options::default()
    };
    let answering = fixture::pipeline(&path, FakeModel::answering(fixture::model_answer()));
    let first = carry::run_with(&answering, &options).expect("first pass");
    assert_eq!(first.classified, 1);
    let store = open_write(&path).expect("open");
    assert!(
        crate::ledger::entity_verified(&store, 1),
        "a type the model answered with was left looking like a guess"
    );
    drop(store);
    let kind = |name: &str| {
        let archive = Archive::open_read_only(&path).expect("open");
        archive
            .query_one(
                "SELECT type FROM entities WHERE name=?",
                &[SqlValue::Text(name.to_string())],
            )
            .expect("entity")
            .and_then(|row| row.text("type").map(|text| text.to_string()))
            .unwrap_or_default()
    };
    assert_eq!(kind("Shenzhen"), "LOC");

    // The same article again, forced, with a model that names nothing this time.
    let forced = Options {
        force: true,
        ..options.clone()
    };
    let silent = fixture::pipeline(&path, FakeModel::silent());
    let second = carry::run_with(&silent, &forced).expect("second pass");
    assert_eq!(second.classified, 0, "the silent model classified nothing");
    assert_eq!(
        kind("Shenzhen"),
        "LOC",
        "the type the model confirmed in the first pass was thrown away by the second"
    );
}

#[test]
fn the_topic_pass_never_marks_a_hidden_duplicate_relevant() {
    // `relevant=1` on a `dupe=1` row is the contradiction `normalize` repairs, so the topic
    // pass must not create it — including when the caller names the duplicate explicitly.
    let path = fixture::database_with_inconsistencies("carry-topics-dupe");
    // Article 6 is the hidden duplicate; give it a headline the fixture's topic matches.
    {
        let store = open_write(&path).expect("open");
        store
            .exec(
                "UPDATE articles SET title='Chip exports record', relevant=0 WHERE id=6",
                &[],
            )
            .expect("a hidden duplicate with a matching headline");
        store
            .exec(
                "INSERT INTO topics(id,name,keywords,min_matches,active) \
                 VALUES(9,'Semiconductors','chip, exports',1,1)",
                &[],
            )
            .expect("a topic");
    }
    let pipeline = fixture::pipeline(&path, FakeModel::offline());
    let options = Options {
        dry_run: false,
        rate_ms: 0,
        topics_only: true,
        include_dupes: true,
        ..Options::default()
    };
    carry::run_with(&pipeline, &options).expect("topic pass");
    let archive = Archive::open_read_only(&path).expect("open");
    let hidden = archive
        .query_one("SELECT relevant, dupe FROM articles WHERE id=6", &[])
        .expect("row")
        .expect("article 6");
    assert_eq!(hidden.int("dupe"), Some(1));
    assert_eq!(
        hidden.int("relevant"),
        Some(0),
        "the topic pass re-showed a hidden duplicate"
    );
    assert!(
        archive
            .count("SELECT COUNT(*) AS n FROM articles WHERE dupe=1 AND relevant=1", &[])
            .expect("count")
            == 0,
        "the carry-over manufactured the fault it repairs"
    );
}

#[test]
fn every_read_only_step_works_on_an_archive_that_cannot_be_written() {
    let path = fixture::database_with_inconsistencies("readonly-file");
    let before = fingerprint(&path);
    let guard = Unwritable::new(&path);

    // verify, normalize --dry-run and carry --dry-run all open read-only, so all three
    // run while the file is 0444.
    let mut out: Vec<u8> = Vec::new();
    let code = cli::run(&command(&["verify", "--db", &db_arg(&path)]), &mut out);
    assert_eq!(code, cli::EXIT_FINDINGS, "{}", String::from_utf8_lossy(&out));

    let mut out: Vec<u8> = Vec::new();
    let code = cli::run(&command(&["normalize", "--db", &db_arg(&path)]), &mut out);
    assert_eq!(code, cli::EXIT_FINDINGS);
    assert!(String::from_utf8_lossy(&out).contains("would change"));

    let mut out: Vec<u8> = Vec::new();
    let code = cli::run(&command(&["carry", "--db", &db_arg(&path)]), &mut out);
    assert_eq!(code, cli::EXIT_OK, "{}", String::from_utf8_lossy(&out));
    assert!(String::from_utf8_lossy(&out).contains("dry run"));

    drop(guard);
    assert_eq!(fingerprint(&path), before);
}

#[test]
fn a_carry_dry_run_asks_the_model_nothing_and_writes_nothing() {
    let path = fixture::database_with_inconsistencies("carry-dry");
    let before = fingerprint(&path);
    let model = FakeModel::answering(fixture::model_answer());
    let pipeline = fixture::pipeline(&path, model.clone());
    let outcome = carry::run_with(&pipeline, &Options::default()).expect("carry");
    assert_eq!(outcome.classified, 0);
    assert_eq!(model.questions(), 0, "a dry run called the model");
    assert_eq!(fingerprint(&path), before);
    assert!(outcome.stopped.contains("dry run"));
}

// ------------------------------------------------------- the CLI, no model

#[test]
fn carry_without_a_model_exits_could_not_run_and_writes_nothing() {
    let path = fixture::database_with_inconsistencies("cli-carry-offline");
    let before = fingerprint(&path);
    let mut out: Vec<u8> = Vec::new();
    // In a test environment there is nothing on the loopback model port. If a developer
    // happens to have one running, the run is allowed to classify — but a refused run must
    // never claim to have changed the archive.
    let code = cli::run(
        &command(&["carry", "--db", &db_arg(&path), "--apply", "--ids", "3"]),
        &mut out,
    );
    let text = String::from_utf8_lossy(&out);
    if code == cli::EXIT_REFUSED {
        assert!(
            text.contains("not answering") || text.contains("unavailable") || text.contains("not built"),
            "a refused run says why: {}",
            text
        );
        assert!(
            !text.contains("— applied"),
            "a run that could not ask the model must not say it was applied: {}",
            text
        );
        assert_eq!(before, fingerprint(&path), "a refused carry wrote to the archive");
    } else {
        assert_eq!(code, cli::EXIT_OK, "{}", text);
    }
}
