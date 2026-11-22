//! Cross-module behaviour: one check, the scheduler, the alerts, and the rules a
//! check may not break.
//!
//! The seams are fakes here on purpose — a fake research pass says exactly what it
//! stored, a fixed judge says exactly which score it gave, and a fixed clock says
//! exactly when "now" is. Nothing in these tests talks to a model, opens a socket or
//! reads a page. The tests that do wire a (scripted) model live in
//! [`crate::model_tests`].

use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use crate::fixtures::{
    add_monitor, insert_article, service, store, BlockingResearch, FakeResearch, FixedJudge,
};
use crate::judge::Judge;
use crate::research::Research;
use crate::run::RunStatus;
use crate::schedule::{Clock, FixedClock, Scheduler, SystemClock};
use crate::service::MonitorService;
use crate::spec::{Draft, Reason};
use crate::wiring::Settings;

// -- the check -------------------------------------------------------------

#[test]
fn a_check_with_nothing_new_alerts_nothing_and_asks_the_model_nothing() {
    let store = store();
    let research = FakeResearch::new(Vec::new());
    let judge = FixedJudge::scoring(9);
    let service = service(
        Arc::clone(&store),
        Arc::clone(&research) as Arc<dyn Research>,
        Arc::clone(&judge) as Arc<dyn Judge>,
        Arc::new(SystemClock::new()),
    );
    let id = add_monitor(&service, "chip export rules", 6);
    let report = service.run(id, Reason::Button);
    assert_eq!(report.status, RunStatus::Ok);
    assert_eq!(report.new_alerts(), 0);
    assert_eq!(report.candidates, 0);
    assert_eq!(judge.calls(), 0, "no candidate, no model call");
    assert_eq!(research.calls(), 1, "one check is one research pass");
    let runs = service.runs(Some(id), 10).expect("runs");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["reason"], "button");
    assert_eq!(runs[0]["status"], "ok");
}

#[test]
fn crossing_the_threshold_raises_one_alert_that_says_why_it_fired() {
    let store = store();
    let research = FakeResearch::new(vec![1]);
    let judge = FixedJudge::scoring(8);
    let service = service(
        Arc::clone(&store),
        Arc::clone(&research) as Arc<dyn Research>,
        Arc::clone(&judge) as Arc<dyn Judge>,
        Arc::new(SystemClock::new()),
    );
    let id = add_monitor(&service, "chip export rules", 6);
    insert_article(&store, 1, &atlas_store::now_iso(), "New chip export rules", true);
    let report = service.run(id, Reason::Button);
    assert_eq!(report.status, RunStatus::Ok);
    assert_eq!(report.judged, 1);
    assert_eq!(report.new_alerts(), 1);
    let alert = &report.alerted[0];
    assert_eq!(alert.article_id, Some(1));
    assert_eq!(alert.why.score, 8);
    assert_eq!(alert.why.threshold, 6);
    assert_eq!(alert.why.topic, "Chips");
    assert_eq!(alert.why.terms, vec!["export".to_string()]);
    assert!(alert.why.line().contains("Chips · 8/10 (threshold 6)"));
    assert!(alert.summary.contains("the summary of the story"));
    assert_eq!(store.monitors().unseen_count().expect("unseen"), 1);
    // The hit counter and last_run moved with the alert.
    let monitor = service.get(id).expect("get").expect("monitor");
    assert_eq!(monitor.hits, 1);
    assert!(!monitor.last_run.is_empty());
}

#[test]
fn an_article_below_the_threshold_is_remembered_and_never_asked_again() {
    let store = store();
    let research = FakeResearch::new(vec![1]);
    let judge = FixedJudge::scoring(3);
    let service = service(
        Arc::clone(&store),
        Arc::clone(&research) as Arc<dyn Research>,
        Arc::clone(&judge) as Arc<dyn Judge>,
        Arc::new(SystemClock::new()),
    );
    let id = add_monitor(&service, "chip export rules", 6);
    insert_article(&store, 1, &atlas_store::now_iso(), "A distant chip story", true);
    let first = service.run(id, Reason::Button);
    assert_eq!(first.below_threshold, 1);
    assert_eq!(first.new_alerts(), 0);
    assert_eq!(judge.calls(), 1);
    let second = service.run(id, Reason::Button);
    assert_eq!(second.judged, 0, "a remembered verdict is not asked again");
    assert_eq!(second.new_alerts(), 0);
    assert_eq!(judge.calls(), 1);
}

#[test]
fn a_second_check_never_stores_the_same_alert_twice() {
    let store = store();
    let research = FakeResearch::new(vec![1]);
    let judge = FixedJudge::scoring(9);
    let service = service(
        Arc::clone(&store),
        Arc::clone(&research) as Arc<dyn Research>,
        Arc::clone(&judge) as Arc<dyn Judge>,
        Arc::new(SystemClock::new()),
    );
    let id = add_monitor(&service, "chip export rules", 6);
    insert_article(&store, 1, &atlas_store::now_iso(), "New chip export rules", true);
    assert_eq!(service.run(id, Reason::Button).new_alerts(), 1);
    let second = service.run(id, Reason::Button);
    assert_eq!(second.new_alerts(), 0);
    assert_eq!(second.duplicates, 1, "it was seen, and refused");
    assert_eq!(store.monitors().unseen_count().expect("unseen"), 1);
    assert_eq!(service.alerts(72, 10, None).expect("alerts").len(), 1);
}

#[test]
fn the_model_saying_it_is_not_reporting_is_remembered_and_never_alerts() {
    let store = store();
    let research = FakeResearch::new(vec![1]);
    let judge = FixedJudge::not_news();
    let service = service(
        Arc::clone(&store),
        Arc::clone(&research) as Arc<dyn Research>,
        Arc::clone(&judge) as Arc<dyn Judge>,
        Arc::new(SystemClock::new()),
    );
    let id = add_monitor(&service, "chip export rules", 6);
    insert_article(&store, 1, &atlas_store::now_iso(), "How chip export works", true);
    let report = service.run(id, Reason::Button);
    assert_eq!(report.not_news, 1);
    assert_eq!(report.new_alerts(), 0);
    assert_eq!(service.run(id, Reason::Button).judged, 0);
}

#[test]
fn a_monitor_whose_topic_was_deleted_still_checks_and_says_so() {
    let store = store();
    let research = FakeResearch::new(vec![1]);
    let judge = FixedJudge::scoring(7);
    let service = service(
        Arc::clone(&store),
        Arc::clone(&research) as Arc<dyn Research>,
        Arc::clone(&judge) as Arc<dyn Judge>,
        Arc::new(SystemClock::new()),
    );
    let id = service
        .add(&Draft::parse("Gone", "chip export", Some(6), None, Some(999)).expect("draft"))
        .expect("add");
    insert_article(&store, 1, &atlas_store::now_iso(), "New chip export rules", true);
    let report = service.run(id, Reason::Tool);
    assert_eq!(report.status, RunStatus::Ok);
    assert!(report.note.contains("no longer exists"), "note: {}", report.note);
    assert_eq!(report.new_alerts(), 1);
    assert_eq!(report.reason.as_str(), "tool");
}

#[test]
fn a_monitor_that_no_longer_exists_is_not_found_and_still_logged() {
    let store = store();
    let service = service(
        Arc::clone(&store),
        FakeResearch::new(Vec::new()) as Arc<dyn Research>,
        FixedJudge::scoring(5) as Arc<dyn Judge>,
        Arc::new(SystemClock::new()),
    );
    let report = service.run(404, Reason::Button);
    assert_eq!(report.status, RunStatus::NotFound);
    assert!(!report.ok());
    let runs = store.monitors().runs(Some(404), 10).expect("runs");
    assert_eq!(runs.len(), 1, "a refusal is information");
    assert_eq!(runs[0].text("status"), Some("not-found"));
    // The log row says it once: `RunReport::refused` used to put the sentence in
    // both `note` and `error`, and the log writes both.
    let note = runs[0].text("note").unwrap_or_default();
    assert_eq!(note.matches("monitor not found").count(), 1, "{note}");
}

#[test]
fn a_failed_research_pass_keeps_the_window_open() {
    let store = store();
    let service = service(
        Arc::clone(&store),
        FakeResearch::failing("no model") as Arc<dyn Research>,
        FixedJudge::scoring(9) as Arc<dyn Judge>,
        Arc::new(SystemClock::new()),
    );
    let id = add_monitor(&service, "chip export", 6);
    let report = service.run(id, Reason::Button);
    assert_eq!(report.status, RunStatus::Error);
    assert!(report.error.is_some());
    let monitor = service.get(id).expect("get").expect("monitor");
    assert!(
        monitor.last_run.is_empty(),
        "a check that did not progress must not move last_run"
    );
}

// -- one check at a time ---------------------------------------------------

#[test]
fn a_second_check_of_the_same_monitor_is_refused_while_one_is_running() {
    let store = store();
    let entered = Arc::new((Mutex::new(false), Condvar::new()));
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let service = service(
        Arc::clone(&store),
        BlockingResearch::new(Arc::clone(&entered), Arc::clone(&release), vec![1])
            as Arc<dyn Research>,
        FixedJudge::scoring(9) as Arc<dyn Judge>,
        Arc::new(SystemClock::new()),
    );
    let id = add_monitor(&service, "chip export", 6);
    insert_article(&store, 1, &atlas_store::now_iso(), "New chip export rules", true);

    let first_service = Arc::clone(&service);
    let first = std::thread::spawn(move || first_service.run(id, Reason::Button));

    // Wait until the first check is inside its research pass, then try again.
    {
        let (lock, condvar) = &*entered;
        let mut seen = lock.lock().expect("entered");
        while !*seen {
            let (next, waited) = condvar
                .wait_timeout(seen, std::time::Duration::from_secs(10))
                .expect("wait");
            seen = next;
            assert!(!waited.timed_out(), "the first check never started");
        }
    }
    assert_eq!(service.running(), vec![id]);

    let second = service.run(id, Reason::Tool);
    assert_eq!(second.status, RunStatus::Skipped);
    assert!(second.error.unwrap().contains("already running"));

    {
        let (lock, condvar) = &*release;
        *lock.lock().expect("release") = true;
        condvar.notify_all();
    }
    let first = first.join().expect("thread");
    assert_eq!(first.status, RunStatus::Ok);
    assert_eq!(first.new_alerts(), 1);
    assert!(service.running().is_empty());

    // One alert, and two log rows: the check and the refusal.
    assert_eq!(store.monitors().unseen_count().expect("unseen"), 1);
    let runs = service.runs(Some(id), 10).expect("runs");
    assert_eq!(runs.len(), 2);
    let reasons: Vec<&str> = runs
        .iter()
        .map(|row| match row.get("reason") {
            Some(serde_json::Value::String(reason)) => reason.as_str(),
            _ => "",
        })
        .collect();
    assert!(reasons.contains(&"tool"));
    assert!(reasons.contains(&"button"));
    let statuses: Vec<&str> = runs
        .iter()
        .map(|row| match row.get("status") {
            Some(serde_json::Value::String(status)) => status.as_str(),
            _ => "",
        })
        .collect();
    assert!(statuses.contains(&"skipped"));
}

// -- the scheduler ---------------------------------------------------------

#[test]
fn the_scheduler_runs_at_the_cadence_and_not_before() {
    let store = store();
    let research = FakeResearch::new(Vec::new());
    let clock = Arc::new(FixedClock::new(atlas_store::parse_epoch("2026-11-05T12:00:00Z")));
    let service = service(
        Arc::clone(&store),
        Arc::clone(&research) as Arc<dyn Research>,
        FixedJudge::scoring(9) as Arc<dyn Judge>,
        clock.clone() as Arc<dyn Clock>,
    );
    let id = service
        .add(&Draft::parse("Chips", "chip export", Some(6), Some(30), None).expect("draft"))
        .expect("add");

    // Never checked: due immediately.
    let first = service.tick(true).expect("tick");
    assert!(first.enabled);
    assert_eq!(first.due, vec![id]);
    assert_eq!(first.ran.len(), 1);
    assert_eq!(first.ran[0].reason, Reason::Scheduler);

    // Same instant: not due, nothing runs.
    let second = service.tick(true).expect("tick");
    assert!(second.due.is_empty());
    assert!(second.ran.is_empty());

    // 29 minutes later: still not due. 31: due.
    clock.advance(29 * 60);
    assert!(service.tick(true).expect("tick").ran.is_empty());
    clock.advance(2 * 60);
    assert_eq!(service.tick(true).expect("tick").due, vec![id]);
    assert_eq!(research.calls(), 2, "one pass per due tick, no more");
}

#[test]
fn a_switched_off_scheduler_runs_nothing_and_says_why() {
    let store = store();
    let clock = Arc::new(FixedClock::new(atlas_store::parse_epoch("2026-11-05T12:00:00Z")));
    let service = service(
        Arc::clone(&store),
        FakeResearch::new(Vec::new()) as Arc<dyn Research>,
        FixedJudge::scoring(9) as Arc<dyn Judge>,
        clock as Arc<dyn Clock>,
    );
    let id = service
        .add(&Draft::parse("Chips", "chip export", Some(6), Some(30), None).expect("draft"))
        .expect("add");
    service.set_scheduler_enabled(false);
    let tick = service.tick(true).expect("tick");
    assert!(!tick.enabled);
    assert!(tick.due.is_empty());
    assert!(tick.note.contains("switched off"));
    assert!(!service.scheduler().enabled());
    // A button still works with the scheduler off.
    assert_eq!(service.run(id, Reason::Button).status, RunStatus::Ok);
    service.set_scheduler_enabled(true);
    assert_eq!(service.scheduler(), Scheduler::new(true));
}

#[test]
fn a_manual_monitor_is_never_scheduled() {
    let store = store();
    let clock = Arc::new(FixedClock::new(atlas_store::parse_epoch("2026-11-05T12:00:00Z")));
    let service = service(
        Arc::clone(&store),
        FakeResearch::new(Vec::new()) as Arc<dyn Research>,
        FixedJudge::scoring(9) as Arc<dyn Judge>,
        clock as Arc<dyn Clock>,
    );
    add_monitor(&service, "chip export", 6);
    let tick = service.tick(true).expect("tick");
    assert!(tick.due.is_empty());
    assert!(tick.note.contains("no monitor"));
}

// -- alerts ----------------------------------------------------------------

#[test]
fn alerts_come_back_newest_first_with_the_monitor_named() {
    let store = store();
    let service = service(
        Arc::clone(&store),
        FakeResearch::new(Vec::new()) as Arc<dyn Research>,
        FixedJudge::scoring(9) as Arc<dyn Judge>,
        Arc::new(SystemClock::new()),
    );
    let id = add_monitor(&service, "chip export", 6);
    for (index, created) in ["2026-11-05T09:00:00Z", "2026-11-05T10:00:00Z", "2026-11-05T11:00:00Z"]
        .into_iter()
        .enumerate()
    {
        store
            .monitors()
            .add_alert(&atlas_store::monitors::NewAlert {
                monitor_id: id,
                kind: "news",
                article_id: Some(index as i64 + 1),
                url: &format!("https://example.com/{}", index),
                title: "A title",
                relevance: 7,
                summary: "the summary",
                analysis: "why",
                entities_json: "[\"Ministry\"]",
                terms_json: "[\"export\"]",
                created,
            })
            .expect("alert");
    }
    let alerts = service.alerts(24 * 365, 10, None).expect("alerts");
    assert_eq!(alerts.len(), 3);
    assert_eq!(alerts[0]["created"], "2026-11-05T11:00:00Z");
    assert_eq!(alerts[2]["created"], "2026-11-05T09:00:00Z");
    assert_eq!(alerts[0]["monitor_name"], "Chips");
    assert_eq!(alerts[0]["entities"], serde_json::json!(["Ministry"]));
    assert_eq!(alerts[0]["terms"], serde_json::json!(["export"]));

    // Seen and unseen are the reader's flags, and the count follows them.
    let alert_id = alerts[0]["id"].as_i64().expect("id");
    assert_eq!(service.unseen().expect("unseen"), 3);
    service.mark_seen(alert_id, true).expect("seen");
    assert_eq!(service.unseen().expect("unseen"), 2);
    service.mark_seen(alert_id, false).expect("unseen again");
    assert_eq!(service.unseen().expect("unseen"), 3);
}

#[test]
fn a_service_with_no_model_scores_nothing_and_says_so() {
    // This test used to pin the opposite: a service with no model scored the article with
    // the monitor's own query terms and raised an alert saying "scored without the model"
    // (measured: run `monitor_runs` id 1, 8 articles, `scorer=query-terms` for all of them).
    // The reader forbade a word filter deciding relevance, so the honest outcome is a
    // `partial` check, a named reason, no score and the article left for the reader.
    let store = store();
    let service = MonitorService::new(
        Arc::clone(&store),
        FakeResearch::new(vec![1]) as Arc<dyn Research>,
        Arc::new(crate::judge::NoJudge::new("no model in this test")),
        Arc::new(SystemClock::new()),
        Settings::default(),
    );
    let id = add_monitor(&service, "chip export", 6);
    insert_article(&store, 1, &atlas_store::now_iso(), "New chip export rules", true);
    let report = service.run(id, Reason::Button);
    assert_eq!(report.status, RunStatus::Partial, "nobody judged, and it says so");
    assert_eq!(report.judged, 0);
    assert_eq!(report.scorer(), "none");
    assert_eq!(report.new_alerts(), 0);
    let note = report.note.clone();
    assert!(
        note.contains("nothing was scored for them"),
        "the log says the articles were left alone: {note}"
    );
}

#[test]
fn a_wired_fallback_scorer_is_named_where_the_reader_sees_it() {
    // The other half of the same rule: term scoring still exists, but only when a caller
    // wires it — and then the run row says the query's own words scored, not the model.
    let store = store();
    let service = MonitorService::new(
        Arc::clone(&store),
        FakeResearch::new(vec![1]) as Arc<dyn Research>,
        Arc::new(crate::judge::NoJudge::new("no model in this test")),
        Arc::new(SystemClock::new()),
        Settings::default(),
    )
    .with_fallback(crate::wiring::fallback_judge());
    let id = add_monitor(&service, "chip export", 6);
    insert_article(&store, 1, &atlas_store::now_iso(), "New chip export rules", true);
    let report = service.run(id, Reason::Button);
    assert_eq!(report.status, RunStatus::Partial, "the model still did not answer");
    assert_eq!(report.judged, 1);
    assert_eq!(report.scorer(), "query-terms");
    assert!(
        report.note.contains("which is not the model"),
        "the note names who scored: {}",
        report.note
    );
    assert!(report.alerted[0].why.line().contains("scored without the model"));
}

// -- alerts dedupe by key --------------------------------------------------

#[test]
fn an_article_that_already_alerted_is_not_even_handed_to_the_model() {
    // The key is (monitor, article). A check that meets an article it already
    // alerted must refuse it before spending a model call, not after.
    let store = store();
    let research = FakeResearch::new(vec![1]);
    let judge = FixedJudge::scoring(9);
    let service = service(
        Arc::clone(&store),
        Arc::clone(&research) as Arc<dyn Research>,
        Arc::clone(&judge) as Arc<dyn Judge>,
        Arc::new(SystemClock::new()),
    );
    let id = add_monitor(&service, "chip export", 6);
    insert_article(&store, 1, &atlas_store::now_iso(), "New chip export rules", true);
    assert_eq!(service.run(id, Reason::Button).new_alerts(), 1);
    assert_eq!(judge.calls(), 1);

    // The research pass reports the same article again, so the window cannot be
    // what keeps it out: the alert key does.
    let second = service.run(id, Reason::Button);
    assert_eq!(second.candidates, 0);
    assert_eq!(second.duplicates, 1, "refused, and counted as refused");
    assert_eq!(second.new_alerts(), 0);
    assert_eq!(judge.calls(), 1, "no second model call for a story already raised");
    assert_eq!(store.monitors().unseen_count().expect("unseen"), 1);
}

#[test]
fn two_monitors_watching_the_same_story_each_raise_their_own_alert() {
    let store = store();
    let service = service(
        Arc::clone(&store),
        FakeResearch::new(vec![1]) as Arc<dyn Research>,
        FixedJudge::scoring(9) as Arc<dyn Judge>,
        Arc::new(SystemClock::new()),
    );
    let chips = add_monitor(&service, "chip export", 6);
    let trade = add_monitor(&service, "export licence", 6);
    insert_article(&store, 1, &atlas_store::now_iso(), "New chip export rules", true);
    assert_eq!(service.run(chips, Reason::Button).new_alerts(), 1);
    assert_eq!(service.run(trade, Reason::Button).new_alerts(), 1);
    assert_eq!(store.monitors().unseen_count().expect("unseen"), 2);
}

#[test]
fn calling_a_monitor_again_in_one_tick_never_writes_a_second_alert() {
    let store = store();
    let service = service(
        Arc::clone(&store),
        FakeResearch::new(vec![1]) as Arc<dyn Research>,
        FixedJudge::scoring(9) as Arc<dyn Judge>,
        Arc::new(SystemClock::new()),
    );
    let id = add_monitor(&service, "chip export", 6);
    insert_article(&store, 1, &atlas_store::now_iso(), "New chip export rules", true);
    for _ in 0..3 {
        service.run(id, Reason::Tool);
    }
    assert_eq!(service.alerts(72, 10, None).expect("alerts").len(), 1);
}

// -- measurement -----------------------------------------------------------

/// One check, measured. `cargo test --offline -p atlas-monitor -- --nocapture`
/// prints the numbers; the assertions are loose bounds, not performance claims.
#[test]
fn one_check_is_measured_end_to_end() {
    let store = store();
    let research = FakeResearch::new((1..=8).collect());
    let service = service(
        Arc::clone(&store),
        Arc::clone(&research) as Arc<dyn Research>,
        Arc::new(crate::judge::QueryTermsJudge) as Arc<dyn Judge>,
        Arc::new(SystemClock::new()),
    );
    let id = add_monitor(&service, "chip export rules", 6);
    for index in 1..=8 {
        insert_article(
            &store,
            index,
            &atlas_store::now_iso(),
            "New chip export rules tighten",
            true,
        );
    }
    let started = Instant::now();
    let report = service.run(id, Reason::Button);
    let elapsed = started.elapsed();
    println!(
        "atlas-monitor: one check, 8 candidates, no model: {:?} ({} judged, {} alerts)",
        elapsed,
        report.judged,
        report.new_alerts()
    );
    assert_eq!(report.judged, 8);
    assert!(
        elapsed.as_millis() < 2_000,
        "a check without a model must not take seconds: {:?}",
        elapsed
    );

    let started = Instant::now();
    let listed = service.list().expect("list");
    println!(
        "atlas-monitor: list one monitor: {:?} ({} monitors)",
        started.elapsed(),
        listed.len()
    );

    let started = Instant::now();
    let alerts = service.alerts(72, 30, None).expect("alerts");
    println!(
        "atlas-monitor: read {} alerts: {:?}",
        alerts.len(),
        started.elapsed()
    );

    let started = Instant::now();
    let tick = service.tick(false).expect("tick");
    println!(
        "atlas-monitor: a scheduler tick over {} monitors: {:?} ({} due)",
        listed.len(),
        started.elapsed(),
        tick.due.len()
    );
}

// -- the reader's budgets reach the log ------------------------------------

#[test]
fn a_budget_note_wired_at_construction_reaches_the_check_log() {
    // The wiring reconciles the reader's `model_context` with what the model server reports
    // (`atlas_ai::Budgets::against`). A server loaded with a smaller `-c` than the config asks
    // for is obeyed — and the reader is told, in the log they look at after a thin answer,
    // instead of being trimmed to in silence.
    let store = store();
    let judge = FixedJudge::scoring(9);
    let service = Arc::new(
        MonitorService::new(
            Arc::clone(&store),
            FakeResearch::new(vec![1]) as Arc<dyn Research>,
            Arc::clone(&judge) as Arc<dyn Judge>,
            Arc::new(SystemClock::new()),
            Settings::default(),
        )
        .with_note(
            "The model server reports a 8192-token context but this computer is configured for 32768.",
        ),
    );
    let id = add_monitor(&service, "chip export", 6);
    insert_article(&store, 1, &atlas_store::now_iso(), "New chip export rules", true);
    let report = service.run(id, Reason::Button);
    assert!(
        report.note.contains("8192-token context"),
        "the sentence is in the note: {}",
        report.note
    );
    // And it is not a failure: the check ran and alerted.
    assert_eq!(report.status, RunStatus::Ok);
    assert_eq!(report.new_alerts(), 1);
}
