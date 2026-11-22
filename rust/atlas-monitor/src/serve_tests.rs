//! The agent's loop, proved without a model.
//!
//! What is real here: the queue (`atlas_store::requests` over a temporary `requests.db`
//! file, so the lease and the claim statement are the production ones), the check service
//! (over an in-memory archive, so a monitor run is a real check), the clock (`FixedClock`,
//! so "stale" is a fact and not a sleep) and the loop itself.
//!
//! What is scripted: the research pass and the enrichment pass — the two seams that would
//! otherwise need a 6.5 GB model. They are the crate's own seams
//! ([`crate::research::Research`], [`crate::enrich::Enrich`]), which is the point of them:
//! the loop calls a `Pipeline`-shaped thing and reports what it did.
//!
//! Every assertion in this file is about one of five promises:
//!
//! * a row is claimed **exactly once** per lease,
//! * a stale claim (a child that died holding it) is **re-claimed**,
//! * a finished row **never** comes back, whatever it did,
//! * the loop **stops when there is nothing to do** — and its pause is never zero,
//! * one JSON object per action reaches the report, and nothing else does.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use atlas_store::requests::{Request, Requests};
use atlas_store::Store;
use serde_json::Value;

use crate::enrich::Enrich;
use crate::fixtures::{FakeBriefs, FakeEnrich, FakeResearch, FixedJudge};
use crate::research::Research;
use crate::schedule::{Clock, FixedClock};
use crate::serve::{
    Kept, Pass, Serve, BUSY_WAIT, ENRICH_LIMIT, IDLE_WAIT, MIN_WAIT, STALE_AFTER_SECS,
};
use crate::service::MonitorService;
use crate::spec::Draft;
use crate::stop::Stop;
use crate::wiring::{Native, Settings};

/// The instant every test starts from: a fixed clock, so a lease is arithmetic.
const T0: i64 = 1_800_000_000;

/// A directory of this test's own under the OS temp dir, removed when the test ends.
struct Tree {
    at: PathBuf,
}

impl Tree {
    fn new(label: &str) -> Tree {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let at = std::env::temp_dir().join(format!(
            "atlas-serve-{}-{}-{}",
            label,
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(&at).expect("temp dir");
        Tree { at }
    }

    fn queue(&self) -> Requests {
        Requests::open(self.at.join("requests.db")).expect("open the queue")
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.at);
    }
}

/// One wired app with scripted seams.
struct Wired {
    native: Native,
    research: Arc<FakeResearch>,
    enrich: Arc<FakeEnrich>,
    clock: Arc<FixedClock>,
}

/// Build the production shape with the two passes scripted.
fn wired(store: Arc<Store>, research: Arc<FakeResearch>, enrich: Arc<FakeEnrich>) -> Wired {
    let clock = Arc::new(FixedClock::new(T0));
    let dynamic: Arc<dyn Research> = Arc::clone(&research) as Arc<dyn Research>;
    let service = MonitorService::new(
        Arc::clone(&store),
        Arc::clone(&dynamic),
        FixedJudge::scoring(7) as Arc<dyn crate::judge::Judge>,
        Arc::clone(&clock) as Arc<dyn Clock>,
        Settings::default(),
    );
    Wired {
        native: Native {
            service,
            research: dynamic,
            enrich: Arc::clone(&enrich) as Arc<dyn Enrich>,
            briefs: Arc::new(crate::brief::NoBrief::default()),
        },
        research,
        enrich,
        clock,
    }
}

/// The report the loop writes into, and the loop itself.
fn serve<'a>(
    queue: &'a Requests,
    wired: &'a Wired,
    report: &'a Kept,
) -> Serve<'a> {
    Serve::new(queue, &wired.native, wired.clock.as_ref(), report)
}

/// Every line must be one JSON object with the five keys a reader may rely on.
fn assert_line_shape(line: &Value) {
    assert!(line.is_object(), "a line is one object: {line}");
    for key in ["kind", "id", "tool", "ms", "stored"] {
        assert!(
            line.get(key).is_some(),
            "the line lost {key:?}: {line}"
        );
    }
    assert!(
        line.get("text").is_none(),
        "a line carries no article text: {line}"
    );
}

// ---------------------------------------------------------------------------
// 1. one claim, one run, one finish
// ---------------------------------------------------------------------------

#[test]
fn a_research_request_is_claimed_once_run_once_and_then_never_again() {
    let tree = Tree::new("research");
    let queue = tree.queue();
    let id = queue
        .push("research", r#"{"query":"chip export rules"}"#)
        .expect("push");
    let store = crate::fixtures::store();
    crate::fixtures::insert_article(&store, 1, "2026-11-22T09:00:00Z", "Chip rules", true);
    let wired = wired(store, FakeResearch::new(vec![1]), FakeEnrich::new(1));
    let report = Kept::new();
    let serve = serve(&queue, &wired, &report);

    assert_eq!(serve.step(), Pass::Ran(1), "the queued row runs");
    assert_eq!(wired.research.calls(), 1, "one claim, one pass");
    assert_eq!(queue.depth().expect("depth"), 0, "the row was finished");

    let lines = report.lines();
    assert_eq!(lines.len(), 1, "one line per action: {lines:?}");
    assert_line_shape(&lines[0]);
    assert_eq!(lines[0]["kind"], "research");
    assert_eq!(lines[0]["id"], id);
    assert_eq!(lines[0]["stored"], 1);

    // The row is finished, so it never comes back — not on the next step, and not after
    // the lease it was claimed under would have expired.
    assert_eq!(serve.step(), Pass::Idle);
    wired.clock.advance(STALE_AFTER_SECS * 10);
    assert_eq!(serve.step(), Pass::Idle, "a finished row is final");
    assert_eq!(wired.research.calls(), 1);
}

#[test]
fn a_stale_claim_is_taken_again() {
    let tree = Tree::new("stale");
    let queue = tree.queue();
    let id = queue
        .push("research", r#"{"topic":"harbour labour"}"#)
        .expect("push");
    let store = crate::fixtures::store();
    let wired = wired(store, FakeResearch::new(Vec::new()), FakeEnrich::new(0));
    let report = Kept::new();
    let serve = serve(&queue, &wired, &report);

    // A child claimed this row and died holding it: the claim is another process's, and
    // the lease is the only thing that frees it.
    let claimed = queue
        .claim_due(&atlas_store::clock::iso_from_epoch(T0), STALE_AFTER_SECS, 1)
        .expect("claim");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, id);
    assert_eq!(serve.step(), Pass::Idle, "a live lease is not taken");
    assert_eq!(wired.research.calls(), 0);
    // One second later the lease is still live: the window is `STALE_AFTER_SECS` wide,
    // not "any claim older than now" — a loop that re-took a row a second after claiming
    // it would run every request twice.
    wired.clock.advance(1);
    assert_eq!(serve.step(), Pass::Idle, "one second is not a stale lease");

    // One second past the lease.
    wired.clock.advance(STALE_AFTER_SECS + 1);
    assert_eq!(serve.step(), Pass::Ran(1), "the stale row is re-claimed");
    assert_eq!(wired.research.calls(), 1);
    assert_eq!(queue.depth().expect("depth"), 0);
    assert_eq!(
        report.lines().first().map(|line| line["id"].clone()),
        Some(serde_json::json!(id)),
        "the re-claimed row is the one that ran"
    );
}

#[test]
fn a_failed_action_is_finished_with_its_reason_and_is_not_retried() {
    let tree = Tree::new("failing");
    let queue = tree.queue();
    queue
        .push("research", r#"{"query":"chip export rules"}"#)
        .expect("push");
    let wired = wired(
        crate::fixtures::store(),
        FakeResearch::failing("the local model is not answering on 127.0.0.1:8081"),
        FakeEnrich::new(0),
    );
    let report = Kept::new();
    let serve = serve(&queue, &wired, &report);

    assert_eq!(serve.step(), Pass::Ran(1));
    let line = report.lines().remove(0);
    assert!(
        line["error"]
            .as_str()
            .unwrap_or_default()
            .contains("not answering"),
        "a failure is one sentence on the line: {line}"
    );
    // Finished all the same: a failed request is a fact, not a retry loop. The next pass
    // is a new push, and a loop that retried would spend a model turn per second on it.
    assert_eq!(queue.depth().expect("depth"), 0);
    assert_eq!(serve.step(), Pass::Idle);
    assert_eq!(wired.research.calls(), 1);
}

// ---------------------------------------------------------------------------
// 2. one action at a time, and the pause between them
// ---------------------------------------------------------------------------

#[test]
fn one_action_runs_per_step_and_the_pause_is_never_zero() {
    let tree = Tree::new("pace");
    let queue = tree.queue();
    queue
        .push("research", r#"{"query":"one"}"#)
        .expect("push");
    queue
        .push("research", r#"{"query":"two"}"#)
        .expect("push");
    let wired = wired(crate::fixtures::store(), FakeResearch::new(Vec::new()), FakeEnrich::new(0));
    let report = Kept::new();
    let serve = serve(&queue, &wired, &report);

    // The model server has one slot: a step runs one action, never the whole queue.
    assert_eq!(serve.step(), Pass::Ran(1));
    assert_eq!(wired.research.calls(), 1);
    assert_eq!(queue.depth().expect("depth"), 1, "the second row still waits");
    // Work waiting means a short pause, not none.
    assert_eq!(serve.next_wait(), BUSY_WAIT);
    assert_eq!(serve.step(), Pass::Ran(1));
    assert_eq!(queue.depth().expect("depth"), 0);
    // Nothing queued and no monitor: the long pause.
    assert_eq!(serve.next_wait(), IDLE_WAIT);
    assert_eq!(serve.step(), Pass::Idle);
    assert_eq!(report.lines().len(), 2);
}

#[test]
fn a_monitor_that_falls_due_makes_the_pause_short_but_never_zero() {
    let tree = Tree::new("due-soon");
    let queue = tree.queue();
    let wired = wired(crate::fixtures::store(), FakeResearch::new(Vec::new()), FakeEnrich::new(0));
    let report = Kept::new();
    let serve = serve(&queue, &wired, &report);

    // A monitor that has never run is due now. The pause must not be zero: a run that
    // fails leaves the cadence due, and a zero-length pause would spin on the failure.
    let draft = Draft::parse("Chips", "chip export rules", None, Some(60), None).expect("draft");
    wired.native.service.add(&draft).expect("add");
    assert_eq!(serve.next_wait(), MIN_WAIT);

    // Once a check has advanced `last_run`, the next due time is a cadence away — longer
    // than the idle pause, so the idle pause is what the loop waits. Still not zero.
    assert_eq!(serve.step(), Pass::Ran(1), "the due monitor ran");
    assert_eq!(serve.next_wait(), IDLE_WAIT);
    assert!(serve.next_wait() > Duration::ZERO);
}

// ---------------------------------------------------------------------------
// 3. the 24/7 half: a monitor at its cadence
// ---------------------------------------------------------------------------

#[test]
fn a_due_monitor_runs_on_its_own_and_the_check_log_says_why() {
    let tree = Tree::new("24-7");
    let queue = tree.queue();
    let store = crate::fixtures::store();
    crate::fixtures::insert_article(&store, 1, "2026-11-22T09:00:00Z", "Chip rules", true);
    let wired = wired(
        Arc::clone(&store),
        FakeResearch::new(vec![1]),
        FakeEnrich::new(0),
    );
    let report = Kept::new();
    let serve = serve(&queue, &wired, &report);

    let draft = Draft::parse("Chips", "chip export rules", None, Some(60), None).expect("draft");
    let id = wired.native.service.add(&draft).expect("add");

    // Nothing in the queue: the step ran the monitor because its cadence was up.
    assert_eq!(serve.step(), Pass::Ran(1));
    assert_eq!(wired.research.calls(), 1, "a check drives one research pass");
    let line = report.lines().remove(0);
    assert_line_shape(&line);
    assert_eq!(line["kind"], "monitor_run");
    assert_eq!(line["monitor"], id);
    assert!(line["id"].is_null(), "no request id: this was the cadence");

    let runs = wired.native.service.runs(Some(id), 10).expect("runs");
    assert_eq!(runs.len(), 1, "the check log has the run: {runs:?}");
    assert_eq!(runs[0]["reason"], "scheduler", "it says why it ran: {:?}", runs[0]);
    // And it does not run again until its cadence (or the clock) says so.
    assert_eq!(serve.step(), Pass::Idle);
    assert_eq!(wired.research.calls(), 1);
}

// ---------------------------------------------------------------------------
// 4. the other two kinds
// ---------------------------------------------------------------------------

#[test]
fn a_monitor_run_request_runs_that_monitor_with_the_button_reason() {
    let tree = Tree::new("monitor-run");
    let queue = tree.queue();
    let store = crate::fixtures::store();
    crate::fixtures::insert_article(&store, 1, "2026-11-22T09:00:00Z", "Chip rules", true);
    let wired = wired(Arc::clone(&store), FakeResearch::new(vec![1]), FakeEnrich::new(0));
    let report = Kept::new();
    let serve = serve(&queue, &wired, &report);

    // A manual monitor (cadence 0) so the cadence half cannot be what runs it.
    let id = wired
        .native
        .service
        .add(&Draft::parse("Chips", "chip export rules", None, None, None).expect("draft"))
        .expect("add");
    let request = queue
        .push("monitor_run", &format!(r#"{{"monitor_id": {id}}}"#))
        .expect("push");

    assert_eq!(serve.step(), Pass::Ran(1));
    let line = report.lines().remove(0);
    assert_eq!(line["kind"], "monitor_run");
    assert_eq!(line["id"], request);
    assert_eq!(line["monitor"], id);
    let runs = wired.native.service.runs(Some(id), 10).expect("runs");
    assert_eq!(runs[0]["reason"], "button", "a click ran it: {:?}", runs[0]);
}

#[test]
fn a_topic_add_creates_the_monitor_and_researches_it_once() {
    let tree = Tree::new("topic-add");
    let queue = tree.queue();
    let wired = wired(crate::fixtures::store(), FakeResearch::new(Vec::new()), FakeEnrich::new(0));
    let report = Kept::new();
    let serve = serve(&queue, &wired, &report);

    queue
        .push(
            "topic_add",
            r#"{"topic":"Chips","query":"chip export rules","cadence_minutes":120}"#,
        )
        .expect("push");
    assert_eq!(serve.step(), Pass::Ran(1));

    let monitors = wired.native.service.list().expect("list");
    assert_eq!(monitors.len(), 1, "the click became a monitor");
    assert_eq!(monitors[0].query, "chip export rules");
    assert_eq!(monitors[0].cadence_minutes, 120);
    assert_eq!(wired.research.calls(), 1, "and it was researched now");
    let runs = wired
        .native
        .service
        .runs(Some(monitors[0].id), 10)
        .expect("runs");
    assert_eq!(runs.len(), 1, "the click is one check: {runs:?}");
    assert_eq!(runs[0]["reason"], "button");
    // One pass, not two: the check advanced `last_run`, so the cadence does not run the
    // same query again on the very next iteration.
    assert_eq!(serve.step(), Pass::Idle);
    assert_eq!(wired.research.calls(), 1);
    let line = report.lines().remove(0);
    assert_eq!(line["kind"], "topic_add");
    assert_eq!(line["monitor"], monitors[0].id);
}

#[test]
fn a_topic_add_delivered_twice_leaves_one_monitor() {
    // A claim is a lease, so one click can be delivered twice: a child that dies
    // holding the row leaves `claimed_at` behind and the row is claimed again
    // (`atlas_store::requests`). The second delivery is the same payload, and it must
    // find the monitor the first one saved instead of minting a twin — two monitors
    // with one saved query mean two alert streams and two research passes for one
    // person's click.
    let tree = Tree::new("topic-add-twice");
    let queue = tree.queue();
    let wired = wired(crate::fixtures::store(), FakeResearch::new(Vec::new()), FakeEnrich::new(0));
    let report = Kept::new();
    let serve = serve(&queue, &wired, &report);

    let payload = r#"{"topic":"Chips","query":"chip export rules","cadence_minutes":120}"#;
    queue.push("topic_add", payload).expect("push");
    assert_eq!(serve.step(), Pass::Ran(1));
    queue.push("topic_add", payload).expect("push");
    assert_eq!(serve.step(), Pass::Ran(1));

    let monitors = wired.native.service.list().expect("list");
    assert_eq!(monitors.len(), 1, "one saved query is one monitor: {monitors:?}");
    assert_eq!(wired.research.calls(), 2, "both deliveries still looked now");
    let lines = report.lines();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["monitor"], monitors[0].id);
    assert_eq!(
        lines[1]["monitor"], monitors[0].id,
        "the replay reports the monitor it found: {:?}",
        lines[1]
    );
}

#[test]
fn an_enrich_request_runs_the_enrichment_pass_with_its_limit() {
    let tree = Tree::new("enrich");
    let queue = tree.queue();
    let wired = wired(crate::fixtures::store(), FakeResearch::new(Vec::new()), FakeEnrich::new(3));
    let report = Kept::new();
    let serve = serve(&queue, &wired, &report);

    queue.push("enrich", r#"{"limit":7}"#).expect("push");
    assert_eq!(serve.step(), Pass::Ran(1));
    assert_eq!(wired.enrich.limits(), vec![7], "the request's own limit");
    let line = report.lines().remove(0);
    assert_eq!(line["kind"], "enrich");
    assert_eq!(line["stored"], 3, "what the pass analysed");

    // A request with no limit uses the pass's default and nothing else happens: the
    // enrichment pass never researches, so the research seam is untouched.
    queue.push("enrich", "{}").expect("push");
    assert_eq!(serve.step(), Pass::Ran(1));
    assert_eq!(wired.enrich.limits(), vec![7, ENRICH_LIMIT]);
    assert_eq!(wired.research.calls(), 0);
}

#[test]
fn a_settings_set_request_writes_the_search_engine_the_window_queued() {
    let tree = Tree::new("settings-set");
    let queue = tree.queue();
    let store = crate::fixtures::store();
    let wired = wired(
        Arc::clone(&store),
        FakeResearch::new(Vec::new()),
        FakeEnrich::new(0),
    );
    let report = Kept::new();
    let serve = serve(&queue, &wired, &report);

    let template = "https://search.example.com/?q={query}&format=RSS";
    queue
        .push(
            "settings_set",
            &format!(
                "{{\"settings\":{{\"search_engine\":\"{template}\"}},\"why\":\"use this engine\"}}"
            ),
        )
        .expect("push");
    assert_eq!(serve.step(), Pass::Ran(1));
    assert_eq!(
        store.get_meta("search_engine").expect("meta"),
        Some(template.to_string()),
        "the agent wrote the engine the reader chose"
    );
    let line = report.lines().pop().expect("a line");
    assert_line_shape(&line);
    assert_eq!(line["kind"], "settings_set");
    assert_eq!(line["stored"], 1);
    assert!(line["error"].is_null(), "{line}");

    // A template the search tool would refuse never reaches the archive: the gate is the
    // same one, so a stored engine is always one the tool can ask.
    for refused in [
        "ftp://search.example.com/?q={query}",
        "https://search.example.com/?q=chips",
        "http://127.0.0.1:9200/?q={query}",
    ] {
        queue
            .push(
                "settings_set",
                &format!(
                    "{{\"settings\":{{\"search_engine\":\"{refused}\"}},\"why\":\"use this engine\"}}"
                ),
            )
            .expect("push");
        assert_eq!(serve.step(), Pass::Ran(1));
        let line = report.lines().pop().expect("a line");
        assert!(!line["error"].is_null(), "{refused} was accepted: {line}");
        assert_eq!(
            store.get_meta("search_engine").expect("meta"),
            Some(template.to_string()),
            "{refused} replaced the engine it may not"
        );
    }

    // And one key, not any key: the payload names the key it may write.
    queue
        .push(
            "settings_set",
            r#"{"settings":{"model_url":"http://example.com/"},"why":"point the model elsewhere"}"#,
        )
        .expect("push");
    assert_eq!(serve.step(), Pass::Ran(1));
    let line = report.lines().pop().expect("a line");
    assert!(
        line["error"].as_str().unwrap_or("").contains("search_engine"),
        "the refusal names the key it would take: {line}"
    );
    assert_eq!(store.get_meta("model_url").expect("meta"), None);
}

#[test]
fn a_model_settings_request_writes_the_config_file_the_launcher_reads() {
    // The one arm that writes outside the archive: a row in the queue, and after one step
    // `data/config.json` holds the numbers — the same file the launcher builds llama.cpp's
    // argv from. The archive is a real file here (`<root>/data/atlas.db`) because the arm
    // finds the project root from the store's own path: an in-memory store has no root, and
    // writing a settings file into an invented one is the bug this shape prevents.
    let tree = Tree::new("settings-write");
    std::fs::create_dir_all(tree.at.join("data")).expect("data");
    let store = Arc::new(Store::open(tree.at.join("data/atlas.db")).expect("open the archive"));
    store.init().expect("schema");
    store.init_monitors().expect("monitors");
    let queue = tree.queue();
    let wired = wired(
        Arc::clone(&store),
        FakeResearch::new(Vec::new()),
        FakeEnrich::new(0),
    );
    let report = Kept::new();
    let serve = serve(&queue, &wired, &report);

    queue
        .push(
            "settings_set",
            r#"{"settings":{"model_context":65536,"model_answer":4096},"why":"the articles are long"}"#,
        )
        .expect("push");
    assert_eq!(serve.step(), Pass::Ran(1));
    let line = report.lines().remove(0);
    assert_line_shape(&line);
    assert_eq!(line["kind"], "settings_set");
    assert_eq!(line["stored"], 2, "two settings were written: {line}");
    assert!(line["error"].is_null(), "{line}");
    assert_eq!(queue.depth().expect("depth"), 0, "the row is finished");

    let text = std::fs::read_to_string(tree.at.join("data/config.json")).expect("config.json");
    let stored: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(stored["model_context"], 65536, "{text}");
    assert_eq!(stored["model_answer"], 4096, "{text}");
}

#[test]
fn an_import_row_runs_the_import_tool_once_and_finishes_the_row() {
    // The whole path a persona's click takes: the Settings page writes a `settings_set` row
    // with `import_model`, the loop claims it, the tool runs as a child, and the row is
    // finished with what the file says. The tool here is the stand-in `atlas-setup`
    // (`crate::import_tests`), because the real one would fetch a multi-gigabyte model.
    let root = match crate::import_tests::Root::new("serve-import") {
        Some(root) => root,
        None => panic!("the stand-in did not compile"),
    };
    let store = Arc::new(Store::open(root.path.join("data/atlas.db")).expect("archive"));
    store.init().expect("schema");
    store.init_monitors().expect("monitors");
    let queue = root.queue();
    let wired = wired(
        Arc::clone(&store),
        FakeResearch::new(Vec::new()),
        FakeEnrich::new(0),
    );
    let report = Kept::new();
    let serve = serve(&queue, &wired, &report);

    queue
        .push(
            "settings_set",
            r#"{"import_model":"/tmp/qwen.gguf","why":"the model I already have"}"#,
        )
        .expect("push");
    assert_eq!(serve.step(), Pass::Ran(1));
    let line = report.lines().remove(0);
    assert_line_shape(&line);
    assert_eq!(line["kind"], "settings_set");
    assert_eq!(line["stored"], 1, "{line}");
    assert!(line["error"].is_null(), "{line}");
    assert_eq!(queue.depth().expect("depth"), 0, "the row is finished");
    // Once: the tool recorded one run, with the value the reader pasted.
    let ran = std::fs::read_to_string(root.path.join("data/ran.txt")).expect("ran.txt");
    assert_eq!(ran, "/tmp/qwen.gguf");
    assert_eq!(serve.step(), Pass::Idle, "a finished row never comes back");
    let ran = std::fs::read_to_string(root.path.join("data/ran.txt")).expect("ran.txt");
    assert_eq!(ran, "/tmp/qwen.gguf", "the tool ran a second time");
}

// ---------------------------------------------------------------------------
// 5. a payload that names nothing is refused, and says what it wanted
// ---------------------------------------------------------------------------

#[test]
fn a_request_without_a_sentence_is_one_sentence_and_the_row_is_finished() {
    let tree = Tree::new("empty-payload");
    let queue = tree.queue();
    queue.push("research", "{}").expect("push");
    let wired = wired(crate::fixtures::store(), FakeResearch::new(Vec::new()), FakeEnrich::new(0));
    let report = Kept::new();
    let serve = serve(&queue, &wired, &report);

    assert_eq!(serve.step(), Pass::Ran(1));
    let line = report.lines().remove(0);
    let error = line["error"].as_str().unwrap_or_default();
    assert!(error.contains("\"query\""), "names the field: {line}");
    assert!(error.contains("\"topic\""), "names the other spelling: {line}");
    assert_eq!(wired.research.calls(), 0, "nothing was researched");
    assert_eq!(queue.depth().expect("depth"), 0, "and the row is closed");
}

#[test]
fn a_monitor_run_without_a_monitor_id_is_refused_by_name() {
    let tree = Tree::new("no-monitor");
    let queue = tree.queue();
    queue.push("monitor_run", "{}").expect("push");
    let wired = wired(crate::fixtures::store(), FakeResearch::new(Vec::new()), FakeEnrich::new(0));
    let report = Kept::new();
    let serve = serve(&queue, &wired, &report);
    assert_eq!(serve.step(), Pass::Ran(1));
    let line = report.lines().remove(0);
    assert!(
        line["error"].as_str().unwrap_or_default().contains("monitor_id"),
        "{line}"
    );
    assert_eq!(wired.research.calls(), 0);
}

// ---------------------------------------------------------------------------
// 6. the stop
// ---------------------------------------------------------------------------

#[test]
fn a_stop_before_the_loop_begins_runs_nothing_and_leaves_the_row_for_the_next_start() {
    let tree = Tree::new("stop-first");
    let queue = tree.queue();
    queue
        .push("research", r#"{"query":"chip export rules"}"#)
        .expect("push");
    let wired = wired(crate::fixtures::store(), FakeResearch::new(Vec::new()), FakeEnrich::new(0));
    let report = Kept::new();
    let serve = serve(&queue, &wired, &report);

    // A `SIGTERM` that arrived before the first iteration: the loop returns without
    // claiming anything, the row is still queued, and the exit is clean.
    assert_eq!(serve.run(&Stop::fired()), 0);
    assert_eq!(wired.research.calls(), 0);
    assert_eq!(queue.depth().expect("depth"), 1, "the row waits for the next start");
    assert!(report.lines().is_empty(), "nothing ran, so nothing is printed");
}

#[test]
fn a_stop_while_the_loop_is_idle_ends_it_within_a_probe_interval() {
    let tree = Tree::new("stop-idle");
    let queue = tree.queue();
    let wired = wired(crate::fixtures::store(), FakeResearch::new(Vec::new()), FakeEnrich::new(0));
    let report = Kept::new();
    let serve = serve(&queue, &wired, &report);
    let stop = Stop::switch();

    // The loop is idle: its pause is the twenty-second one, and a stop has to be heard
    // from *inside* that pause — that is the whole reason the sleep is sliced.
    let started = std::time::Instant::now();
    let actions = std::thread::scope(|scope| {
        let running = scope.spawn(|| serve.run(&stop));
        std::thread::sleep(Duration::from_millis(100));
        stop.raise();
        running.join().expect("the loop thread ended")
    });
    assert_eq!(actions, 0, "there was nothing to do");
    let spent = started.elapsed();
    assert!(
        spent < Duration::from_secs(2),
        "a stop waited {spent:?} — longer than the paused loop's probe interval"
    );
}

// ---------------------------------------------------------------------------
// 6. briefings: one request, and the bounded refresh after a pass
// ---------------------------------------------------------------------------

/// A wired app whose briefing seam the test can read back.
struct BriefHarness {
    native: Native,
    clock: Arc<FixedClock>,
}

fn brief_harness(
    store: Arc<Store>,
    research: Arc<dyn Research>,
    briefs: Arc<FakeBriefs>,
) -> BriefHarness {
    let clock = Arc::new(FixedClock::new(T0));
    let service = MonitorService::new(
        Arc::clone(&store),
        Arc::clone(&research),
        FixedJudge::scoring(7) as Arc<dyn crate::judge::Judge>,
        Arc::clone(&clock) as Arc<dyn Clock>,
        Settings::default(),
    );
    BriefHarness {
        native: Native {
            service,
            research,
            enrich: FakeEnrich::new(0) as Arc<dyn Enrich>,
            briefs: Arc::clone(&briefs) as Arc<dyn crate::brief::Briefs>,
        },
        clock,
    }
}

/// One hand-built `brief` row, for the payload refusals.
///
/// A refusal never reaches the queue (`atlas-store` lists the kind, so a row would be
/// stored and claimed), and the arm is where the sentence is written — this helper hands
/// it one payload without a database behind it. The whole-door case has its own test.
fn brief_request(payload: &str) -> Request {
    Request {
        id: 1,
        kind: "brief".to_string(),
        payload: payload.to_string(),
        created_at: "2026-11-22T09:00:00Z".to_string(),
    }
}

#[test]
fn a_brief_request_answers_stored_1_and_the_seam_got_one_call() {
    let tree = Tree::new("brief-arm");
    let queue = tree.queue();
    let briefs = FakeBriefs::recording();
    let wired = brief_harness(
        crate::fixtures::store(),
        FakeResearch::new(Vec::new()) as Arc<dyn Research>,
        Arc::clone(&briefs),
    );
    let report = Kept::new();
    let serve = Serve::new(&queue, &wired.native, wired.clock.as_ref(), &report);

    let (kind, acted) = serve.act(
        &brief_request(r#"{"scope":"country","key":"de","hours":72,"why":"the reader asked"}"#),
        &Stop::quiet(),
    );
    assert_eq!(kind, "brief");
    assert_eq!(acted.stored, 1, "one briefing was written");
    assert_eq!(acted.error, None);
    assert_eq!(
        briefs.calls(),
        vec![("country".to_string(), "de".to_string())],
        "exactly one call, for the scope the payload named"
    );
}

#[test]
fn a_brief_request_naming_an_unknown_scope_is_refused_by_name_before_any_model_call() {
    let tree = Tree::new("brief-scope");
    let queue = tree.queue();
    let briefs = FakeBriefs::recording();
    let wired = brief_harness(
        crate::fixtures::store(),
        FakeResearch::new(Vec::new()) as Arc<dyn Research>,
        Arc::clone(&briefs),
    );
    let report = Kept::new();
    let serve = Serve::new(&queue, &wired.native, wired.clock.as_ref(), &report);

    let (_, acted) = serve.act(
        &brief_request(r#"{"scope":"planet","key":"mars","hours":24}"#),
        &Stop::quiet(),
    );
    assert_eq!(acted.stored, 0);
    let error = acted.error.unwrap_or_default();
    assert!(error.contains("\"planet\""), "names what it refused: {error}");
    assert!(error.contains("window, country, place, topic, monitor, entity"), "{error}");
    assert!(briefs.calls().is_empty(), "no model call was paid for");
}

#[test]
fn a_brief_request_carrying_a_token_is_refused_before_the_model_reads_it() {
    let tree = Tree::new("brief-token");
    let queue = tree.queue();
    let briefs = FakeBriefs::recording();
    let wired = brief_harness(
        crate::fixtures::store(),
        FakeResearch::new(Vec::new()) as Arc<dyn Research>,
        Arc::clone(&briefs),
    );
    let report = Kept::new();
    let serve = Serve::new(&queue, &wired.native, wired.clock.as_ref(), &report);

    let (_, acted) = serve.act(
        &brief_request(
            r#"{"scope":"topic","key":"chips","hours":24,"why":"sk-live-9f8e7d6c5b4a3210"}"#,
        ),
        &Stop::quiet(),
    );
    assert_eq!(acted.stored, 0);
    assert_eq!(
        acted.error.as_deref(),
        Some(atlas_store::requests::CREDENTIAL_REFUSAL),
        "the queue's own sentence, so a reader sees one rule and not two"
    );
    assert!(briefs.calls().is_empty(), "nothing reached the model");
}

#[test]
fn a_pass_that_stored_five_stories_refreshes_the_window_then_the_busiest_countries() {
    let tree = Tree::new("brief-refresh");
    let queue = tree.queue();
    let store = crate::fixtures::store();
    let research = crate::fixtures::StoringResearch::new(
        Arc::clone(&store),
        vec![
            (101, Some("US")),
            (102, Some("US")),
            (103, Some("US")),
            (104, Some("DE")),
            (105, Some("JP")),
        ],
    );
    let briefs = FakeBriefs::recording();
    let wired = brief_harness(
        Arc::clone(&store),
        Arc::clone(&research) as Arc<dyn Research>,
        Arc::clone(&briefs),
    );
    let report = Kept::new();
    let serve = Serve::new(&queue, &wired.native, wired.clock.as_ref(), &report);

    // A monitor that watches a topic, so the run has an own scope of its own.
    let id = wired
        .native
        .service
        .add(&Draft::parse("Chips", "chip export rules", None, None, Some(7)).expect("draft"))
        .expect("add");
    queue
        .push("monitor_run", &format!(r#"{{"id": {id}}}"#))
        .expect("push");
    assert_eq!(serve.step(), Pass::Ran(1));

    assert_eq!(
        briefs.calls(),
        vec![
            ("window".to_string(), String::new()),
            ("country".to_string(), "US".to_string()),
            ("country".to_string(), "DE".to_string()),
        ],
        "window first, then the busiest countries, and three calls at most"
    );
    let line = report.lines().remove(0);
    assert_eq!(line["stored"], 5, "the run's own line is unchanged by the refresh");
    assert_eq!(line["error"], Value::Null, "a briefing cannot fail the run");
}

#[test]
fn a_briefing_the_model_refused_is_logged_and_leaves_the_runs_line_intact() {
    let tree = Tree::new("brief-refused");
    let queue = tree.queue();
    let store = crate::fixtures::store();
    let research = crate::fixtures::StoringResearch::new(
        Arc::clone(&store),
        vec![(201, Some("FR")), (202, None)],
    );
    let briefs = FakeBriefs::recording();
    briefs.fail("window");
    let wired = brief_harness(
        Arc::clone(&store),
        Arc::clone(&research) as Arc<dyn Research>,
        Arc::clone(&briefs),
    );
    let report = Kept::new();
    let serve = Serve::new(&queue, &wired.native, wired.clock.as_ref(), &report);

    queue
        .push("research", r#"{"query":"french grid rules"}"#)
        .expect("push");
    assert_eq!(serve.step(), Pass::Ran(1));

    let line = report.lines().remove(0);
    assert_eq!(line["stored"], 2, "the pass stored both stories");
    assert_eq!(line["error"], Value::Null, "the refused briefing did not fail it");
    assert_eq!(
        briefs.calls(),
        vec![
            ("window".to_string(), String::new()),
            ("country".to_string(), "FR".to_string()),
        ],
        "a research request has no own scope: the window and the story's country"
    );
}


#[test]
fn a_brief_row_pushed_through_the_queue_is_claimed_written_and_finished() {
    // The whole door, not just the arm: `atlas-store` lists the kind, the queue stores it,
    // the loop claims it once, the seam writes one briefing, and the row is closed.
    let tree = Tree::new("brief-queue");
    let queue = tree.queue();
    let briefs = FakeBriefs::recording();
    let wired = brief_harness(
        crate::fixtures::store(),
        FakeResearch::new(Vec::new()) as Arc<dyn Research>,
        Arc::clone(&briefs),
    );
    let report = Kept::new();
    let serve = Serve::new(&queue, &wired.native, wired.clock.as_ref(), &report);

    let id = queue
        .push(
            "brief",
            r#"{"scope":"window","key":"","hours":72,"why":"opened the app"}"#,
        )
        .expect("the store lists the kind");
    assert_eq!(serve.step(), Pass::Ran(1));

    let line = report.lines().remove(0);
    assert_eq!(line["kind"], "brief");
    assert_eq!(line["id"], id);
    assert_eq!(line["stored"], 1);
    assert_eq!(line["error"], Value::Null);
    assert_eq!(queue.depth().expect("depth"), 0, "the row is closed");
    assert_eq!(
        briefs.calls(),
        vec![("window".to_string(), String::new())],
        "one model call, for the window"
    );
    assert_eq!(serve.step(), Pass::Idle, "a finished row never comes back");
}
