//! The newest data paths: what the window queues, what the Reader's toolbar names, and what
//! the two files in `data/` would tell a person who opened them.
//!
//! Three questions, all answered by measurement rather than by reading the code and believing
//! it:
//!
//! 1. **The queue.** `data/requests.db` is the one file the window writes, and it is read by
//!    another process and survives the window. Five payload shapes are pushed today —
//!    `brief`, `research`, `topic_add`, `monitor_run`, `settings_set` — and this file builds
//!    each one the way the shipped code builds it and asks whether a path, a home directory, a
//!    credential, a URL with user info or the reader's own words can be in it.
//! 2. **The toolbar.** The Reader now names the *page's* current address
//!    (`webview::Page::url`) rather than the story's, and a page can follow a redirect
//!    anywhere the engine will go. The label is `reader::host(shown)`, one function for both
//!    sources (`views/reader.rs:145-150`), so the measurement here — a crafted address handed
//!    in as the story's — is the engine's case with the source string swapped. A `Page` cannot
//!    be constructed without a window, and this crate's tests never open one.
//! 3. **The two databases.** Row counts, the newest row, and a structural scan for the shapes
//!    that must not be in them.
//!
//! Everything here is read-only and offline: the archive and the queue are opened with
//! `SQLITE_OPEN_READ_ONLY` plus `PRAGMA query_only`, and the one write in the file is to a
//! temporary queue under `std::env::temp_dir()`, removed before the test returns. Nothing
//! under `data/` is written.
//!
//! A test that is `#[ignore]`d is an **open** item — the crate's convention, set by
//! `tests/disclosure.rs`: the assertion is unchanged and runs with `--ignored`, and closing the
//! leak is deleting one attribute.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use atlas_desktop::{assets, requests};
use atlas_desktop::views::{brief, reader, settings};

// ---------------------------------------------------------------------------------------
// the corpus: the two files in data/, read-only
// ---------------------------------------------------------------------------------------

fn data_file(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("data")
        .join(name)
}

/// Open one of the app's own files read-only, or `None` when the machine has none yet.
///
/// Two locks: the flag keeps the connection from writing, and `query_only` refuses a write
/// statement even if a later edit drops the flag.
fn open_read_only(path: &Path) -> Option<Connection> {
    if !path.is_file() {
        eprintln!("SKIP: no {} on this machine", path.display());
        return None;
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    let conn = Connection::open_with_flags(path, flags)
        .unwrap_or_else(|error| panic!("open {} read-only: {error}", path.display()));
    conn.pragma_update(None, "query_only", true)
        .expect("pragma query_only");
    Some(conn)
}

/// The shapes that must never appear in a payload the window wrote or in a database row it
/// can read. Structural, not a word list: a home directory is *a path under a home*, and a
/// credential in an address is *user info before the host*.
fn forbidden_shapes(text: &str) -> Vec<&'static str> {
    let mut found = Vec::new();
    if text.contains("/Users/") {
        found.push("a macOS home directory");
    }
    if text.contains("/home/") {
        found.push("a Linux home directory");
    }
    if text.contains("\\\\Users\\\\") || text.contains("C:\\\\") {
        found.push("a Windows home directory");
    }
    // `scheme://user:pass@host` — the user-info form. A bare `@` is not a finding (an e-mail
    // in a place label is not a credential), so the test looks for the authority shape.
    for scheme in ["http://", "https://", "file://"] {
        if let Some(rest) = text.split(scheme).nth(1) {
            let authority = rest
                .split(['/', '?', '#', '"', ' '])
                .next()
                .unwrap_or_default();
            if authority.contains('@') {
                found.push("a URL with user info");
            }
        }
    }
    found
}

// ---------------------------------------------------------------------------------------
// 1a — the brief payload, over every scope the archive can produce
// ---------------------------------------------------------------------------------------

/// `brief` is the payload the window writes **without a press**, on every scope it visits
/// (`app.rs::maybe_ask_for_briefing`). Its `why` is built from the page's scope note, and for
/// a country or a place that note is the archive's own label — so the payload is only as clean
/// as the archive's `place` and `iso` columns are, which is what this test measures: every
/// distinct label in the live archive, through the shipped builder.
#[test]
fn the_brief_payload_carries_a_place_and_nothing_else() {
    let Some(conn) = open_read_only(&data_file("atlas.db")) else {
        return;
    };

    let mut keys: Vec<(&'static str, String)> = Vec::new();
    for (scope, sql) in [
        ("country", "SELECT DISTINCT iso FROM articles WHERE iso <> '' ORDER BY iso"),
        ("place", "SELECT DISTINCT place FROM articles WHERE place <> '' ORDER BY place"),
    ] {
        let mut stmt = conn.prepare(sql).expect("prepare labels");
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query labels")
            .collect::<Result<Vec<_>, _>>()
            .expect("read labels");
        keys.extend(rows.into_iter().map(|label| (scope, label)));
    }
    assert!(!keys.is_empty(), "the archive produced no scope labels");

    let mut scanned = 0usize;
    for (scope, key) in &keys {
        // The sentence `app.rs::maybe_ask_for_briefing` builds, word for word.
        let why = format!("the page about {key} has stories and no briefing yet");
        let payload = brief::payload(scope, key, 72, &why);
        let value: serde_json::Value = serde_json::from_str(&payload).expect("valid JSON");

        let mut fields: Vec<&str> = value
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        fields.sort_unstable();
        assert_eq!(
            fields,
            ["hours", "key", "scope", "why"],
            "the brief payload grew a field, and this test does not know what it holds: {payload}"
        );
        assert!(
            !why.contains('\n'),
            "the sentence is not one line: {why:?}"
        );
        assert!(
            forbidden_shapes(&payload).is_empty(),
            "a brief payload for {scope} {key:?} carries {:?}: {payload}",
            forbidden_shapes(&payload)
        );
        scanned += 1;
    }
    println!(
        "brief: {scanned} scopes built from the live archive ({} countries, {} places), none \
         carrying a path, a home directory, a credential or a URL",
        keys.iter().filter(|(scope, _)| *scope == "country").count(),
        keys.iter().filter(|(scope, _)| *scope == "place").count()
    );
}

// ---------------------------------------------------------------------------------------
// 1b — the queue as it is on this machine
// ---------------------------------------------------------------------------------------

/// What the window has actually queued, row by row.
///
/// The payloads are checked against the shape each kind is documented to have, so a kind that
/// starts carrying a new value fails here rather than leaking quietly; and every payload is
/// scanned for the shapes [`forbidden_shapes`] names.
#[test]
fn the_live_queue_holds_payloads_and_no_paths() {
    let Some(conn) = open_read_only(&data_file("requests.db")) else {
        return;
    };
    let mut stmt = conn
        .prepare("SELECT id, kind, payload, created_at FROM requests ORDER BY id")
        .expect("prepare the queue");
    let rows: Vec<(i64, String, String, String)> = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("query the queue")
        .collect::<Result<Vec<_>, _>>()
        .expect("read the queue");
    println!("queue: {} row(s) in data/requests.db", rows.len());

    // The documented shape of each kind this file knows about. `enrich` has no payload fields
    // yet (the window does not push it), so it is not listed.
    let allowed: [(&str, &[&str]); 5] = [
        ("brief", &["scope", "key", "hours", "why"]),
        ("research", &["topic", "max_steps"]),
        ("topic_add", &["name", "query"]),
        ("monitor_run", &["id", "reason"]),
        ("settings_set", &["settings", "why"]),
    ];

    for (id, kind, payload, created) in &rows {
        println!("  {id} {kind} {created} {} bytes", payload.len());
        let value: serde_json::Value = serde_json::from_str(payload)
            .unwrap_or_else(|error| panic!("row {id} is not JSON ({error}): {payload}"));
        let object = value.as_object().unwrap_or_else(|| panic!("row {id} is not an object"));
        let known = allowed
            .iter()
            .find(|(name, _)| name == kind)
            .map(|(_, fields)| *fields)
            .unwrap_or_else(|| panic!("row {id} has a kind this audit does not know: {kind}"));
        for field in object.keys() {
            assert!(
                known.contains(&field.as_str()),
                "row {id} ({kind}) has a field {field:?} this audit does not know: {payload}"
            );
        }
        // A `settings_set` row for a model is the one row allowed to carry a path, and it must:
        // `atlas-config` refuses a relative `model_local` by name (see
        // `the_settings_payload_names_the_file_the_agent_will_serve`). Its safety is the queue
        // file's `0600` mode, not a rewritten value, so the scan runs on the row with that one
        // value taken out — the rest of the row, and every other kind, is unchanged.
        let scanned = if kind == "settings_set" {
            match value["settings"]["model_local"].as_str() {
                Some(path) => payload.replace(path, "<model>"),
                None => payload.clone(),
            }
        } else {
            payload.clone()
        };
        let found = forbidden_shapes(&scanned);
        assert!(
            found.is_empty(),
            "queue row {id} ({kind}) carries {found:?}: {payload}"
        );
    }
    if let Some((id, kind, _, created)) = rows.last() {
        println!("queue: newest row is {id} ({kind}) at {created}");
    }
}

// ---------------------------------------------------------------------------------------
// 1c — `research`, over every address the archive holds
// ---------------------------------------------------------------------------------------

/// "Tag it" queues `research` with **the story's address** (`app.rs:1601`), so the payload
/// carries whatever URL the archive stored. This test takes all of them through the shipped
/// gate (`requests::push_to`, into a temporary queue) and counts the refusals: a refusal is a
/// story the reader cannot hand to the agent.
#[test]
fn every_archived_address_can_be_queued_for_research() {
    let Some(conn) = open_read_only(&data_file("atlas.db")) else {
        return;
    };
    let mut stmt = conn
        .prepare("SELECT id, url FROM articles WHERE url <> '' ORDER BY id")
        .expect("prepare addresses");
    let rows: Vec<(i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query addresses")
        .collect::<Result<Vec<_>, _>>()
        .expect("read addresses");

    // A queue of this test's own, under the system temp directory: `data/` is never written.
    let queue = std::env::temp_dir().join(format!("atlas-queue-audit-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&queue);
    let mut refused: Vec<(i64, String, String)> = Vec::new();
    let mut queued = 0usize;
    for (id, url) in &rows {
        let payload = requests::research_payload(url, 2);
        assert!(
            forbidden_shapes(&payload).is_empty(),
            "the research payload for row {id} carries {:?}",
            forbidden_shapes(&payload)
        );
        match requests::push_to(&queue, "research", &payload) {
            Ok(_) => queued += 1,
            Err(problem) => refused.push((*id, url.clone(), problem)),
        }
    }
    let _ = std::fs::remove_file(&queue);

    println!(
        "research: {} address(es), {queued} queued, {} refused",
        rows.len(),
        refused.len()
    );
    for (id, url, problem) in refused.iter().take(5) {
        println!("  row {id} refused: {problem}  ({url})");
    }
    assert!(
        refused.is_empty(),
        "{} archived address(es) cannot be tagged: {:?}",
        refused.len(),
        refused
            .iter()
            .take(5)
            .map(|(id, url, problem)| format!("row {id}: {problem} ({url})"))
            .collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------------------
// 1d — the reader's own words, and the one payload that carries a file path
// ---------------------------------------------------------------------------------------

/// `topic_add` carries what the reader typed, clipped (`views/monitors.rs:349-352`). That is
/// the point of the request, so the test pins only what must still be true: the two fields are
/// the reader's text, the payload is bounded, and — because the reader may paste anything —
/// what the window does with a *credential* is refuse it rather than store it.
#[test]
fn the_readers_own_words_are_stored_and_a_credential_is_not() {
    let typed = "  Semiconductor export controls  ";
    let name = typed.trim();
    let payload = requests::topic_add_payload(name, name);
    let value: serde_json::Value = serde_json::from_str(&payload).expect("valid JSON");
    assert_eq!(value["name"], name);
    assert_eq!(value["query"], name);
    assert!(!payload.contains('\n'), "a typed line break survived: {payload:?}");

    // A pasted credential is refused at the queue's door, whatever kind carried it.
    let queue = std::env::temp_dir().join(format!("atlas-queue-cred-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&queue);
    let secret = requests::topic_add_payload("token = Ab3dEf6hIj9Kl2Mn5Op8Qr1St4Uv7Wx0Yz3A2bC1", "x");
    let refused = requests::push_to(&queue, "topic_add", &secret);
    let _ = std::fs::remove_file(&queue);
    match refused {
        Err(problem) => println!("a pasted credential is refused: {problem}"),
        Ok(id) => panic!("a pasted credential was queued as row {id}: {secret}"),
    }
}

/// The model picker queues `settings_set` with `model_local` set to **the file's absolute
/// path**, and that is the agent's own requirement: `atlas-config::checks::model_local_path`
/// refuses a relative name by name, because the window and the agent are two processes with two
/// working directories. This test pins the three things that make the row acceptable instead of
/// pretending the path is not there — it is the file the reader picked, it ends in `.gguf`, and
/// it is the *only* string the row carries (no question, no credential, no address).
///
/// What keeps that path from being a disclosure is the queue file's mode, not a filtered value:
/// `requests::open` creates it `0600` (`the_queue_file_is_owner_only`, measured on this machine
/// as `0o644` before that block existed), and the window paints `assets::label`'s name, never
/// the path. Built by the page's own function, so the test cannot drift from what a press sends.
#[test]
fn the_settings_payload_names_the_file_the_agent_will_serve() {
    let files = settings::model_files(&assets::models_dir());
    if files.is_empty() {
        eprintln!("SKIP: no model file in {}", assets::models_dir().display());
        return;
    }
    let file = &files[0];
    let payload = settings::pick_payload(file);
    println!("settings_set payload: {payload}");

    let value: serde_json::Value = serde_json::from_str(&payload).expect("valid JSON");
    let object = value.as_object().expect("an object");
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["settings", "why"],
        "the picker queues fields the agent does not read: {payload}"
    );
    let settings = value["settings"].as_object().expect("a settings object");
    let mut keys: Vec<&str> = settings.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["model_local"],
        "one press is one key, and this press changes the model: {payload}"
    );
    let named = value["settings"]["model_local"]
        .as_str()
        .expect("a string path");
    assert_eq!(
        named, file.path,
        "the row does not name the file the reader pressed: {named:?} against {:?}. The agent \
         resolves a relative name against its own working directory, so the row has to carry \
         the one spelling that cannot be read two ways.",
        file.path
    );
    assert!(
        named.to_ascii_lowercase().ends_with(assets::MODEL_EXTENSION),
        "the agent refuses a path that does not end in {}: {named:?}",
        assets::MODEL_EXTENSION
    );
    // The path is the row's one string; everything else is the sentence the page shows.
    assert!(
        payload.matches('/').count() <= named.matches('/').count() + 1,
        "the row carries a second path-shaped value besides the model's own: {payload}"
    );
}

// ---------------------------------------------------------------------------------------
// 2 — the toolbar, on a crafted address
// ---------------------------------------------------------------------------------------

/// Collect every string egui painted in one frame, two frames deep (a scroll area measures
/// after drawing).
fn painted_reader(url: &str) -> Vec<String> {
    let story = reader::Story {
        id: 1,
        title: "A story".to_string(),
        source: "Example News".to_string(),
        published: "2026-11-22T10:00:00Z".to_string(),
        url: url.to_string(),
        text: "Body text.".to_string(),
        full_text: true,
    };
    let ctx = egui::Context::default();
    atlas_desktop::theme::install(&ctx);
    let mut painted: Vec<String> = Vec::new();
    for _ in 0..2 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 620.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                reader::show(ui, &story, None, false);
            });
        });
        painted.clear();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut painted);
        }
        output.drop_without_applying_deltas();
    }
    painted
}

fn walk(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
    match shape {
        egui::epaint::Shape::Text(text) => out.push(text.galley.text().to_owned()),
        egui::epaint::Shape::Vec(items) => {
            for item in items {
                walk(item, out);
            }
        }
        _ => {}
    }
}

/// The half of the toolbar's rule that holds: the label is the **host**, so a query string —
/// where a token lives — is never painted. Measured on the address a page can actually be on
/// after a redirect: a token in the query, and a fragment after it.
#[test]
fn the_toolbar_does_not_paint_a_query_string() {
    for url in [
        "https://news.example.com/story?token=T0PSECRET9&page=2",
        "https://news.example.com/story?code=4/0AeanS0b#access_token=T0PSECRET9",
    ] {
        let painted = painted_reader(url).join("\n");
        println!("toolbar paints (query with a token): {painted:?}");
        assert!(
            !painted.contains("T0PSECRET9") && !painted.contains("token="),
            "the toolbar painted the query of {url}:\n{painted}"
        );
        assert!(
            painted.contains("news.example.com"),
            "the toolbar must still name the host of {url}:\n{painted}"
        );
    }
}

/// The half that does not hold, and the reason it matters for `Page::url`: the toolbar's label
/// is `reader::host(shown)` for **both** sources, and `host` prints the authority it is given
/// — user info included. An engine that lands on such an address (a redirect, a link inside a
/// page the engine followed) paints the password in the row above the page.
#[test]
fn the_toolbar_does_not_paint_user_info() {
    let url = "https://reader:hunter2@news.example.com/story-1";
    assert_eq!(
        atlas_desktop::link::openable(url),
        None,
        "the gate refuses a credentialed address, which is the branch that leaks"
    );
    let painted = painted_reader(url).join("\n");
    println!("toolbar paints (credentialed address): {painted:?}");
    assert!(
        !painted.contains("hunter2"),
        "the toolbar painted the password of {url}:\n{painted}"
    );
}

// ---------------------------------------------------------------------------------------
// 3 — the two databases
// ---------------------------------------------------------------------------------------

/// `data/atlas.db`: how much there is, how new it is, and whether any address or label in it
/// would hand a reader's machine or account to whoever opens the file.
#[test]
fn the_archive_holds_no_home_directory_and_no_address_with_user_info() {
    let Some(conn) = open_read_only(&data_file("atlas.db")) else {
        return;
    };
    let articles: i64 = conn
        .query_row("SELECT count(*) FROM articles", [], |row| row.get(0))
        .expect("count articles");
    let newest: String = conn
        .query_row("SELECT max(published) FROM articles", [], |row| row.get(0))
        .expect("newest stamp");
    let last_id: i64 = conn
        .query_row("SELECT max(id) FROM articles", [], |row| row.get(0))
        .expect("last id");
    let urls: i64 = conn
        .query_row("SELECT count(*) FROM articles WHERE url <> ''", [], |row| row.get(0))
        .expect("count urls");
    println!(
        "atlas.db: {articles} article(s), {urls} with an address, newest {newest} (id {last_id})"
    );

    // Every address, and the labels a page can show, scanned for the two shapes that matter.
    let mut stmt = conn
        .prepare("SELECT id, url FROM articles WHERE url <> '' ORDER BY id")
        .expect("prepare addresses");
    let addresses: Vec<(i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query addresses")
        .collect::<Result<Vec<_>, _>>()
        .expect("read addresses");
    let mut with_user_info = Vec::new();
    let mut with_a_query = 0usize;
    for (id, url) in &addresses {
        if forbidden_shapes(url).contains(&"a URL with user info") {
            with_user_info.push((*id, url.clone()));
        }
        if url.contains('?') {
            with_a_query += 1;
        }
    }
    println!(
        "atlas.db: {with_a_query} address(es) carry a query, {} carry user info",
        with_user_info.len()
    );
    assert!(
        with_user_info.is_empty(),
        "stored addresses carry user info: {with_user_info:?}"
    );

    // The meta table: settings and the model's own prose. A path here would be a path the
    // window wrote and can print.
    let mut stmt = conn
        .prepare("SELECT k, v FROM meta ORDER BY k")
        .expect("prepare meta");
    let meta: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query meta")
        .collect::<Result<Vec<_>, _>>()
        .expect("read meta");
    let mut offenders = Vec::new();
    for (key, value) in &meta {
        if !forbidden_shapes(value).is_empty() {
            offenders.push(key.clone());
        }
    }
    println!("atlas.db: {} meta row(s), {} with a path shape", meta.len(), offenders.len());
    assert!(
        offenders.is_empty(),
        "meta values (never printed: they can be the reader's own prose) carry a path shape: {offenders:?}"
    );
}

// ---------------------------------------------------------------------------------------
// 3b — the queue file's own mode
// ---------------------------------------------------------------------------------------

/// `data/requests.db` holds the reader's own words, so it is owner-only like the archive.
///
/// Measured before this was pinned: `-rw-r--r--` for `data/requests.db` beside `-rw-------`
/// for `data/atlas.db` — SQLite creates its file 0644 under the usual umask, and every other
/// account on the machine could read what the reader typed into a monitor. The repair lives in
/// `requests.rs::open`, and this test creates a queue of its own (in the system temp
/// directory, never `data/`) and reads the mode the file system reports.
#[test]
#[cfg(unix)]
fn the_queue_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let queue = std::env::temp_dir().join(format!("atlas-queue-mode-{}.db", std::process::id()));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", queue.display()));
    }
    requests::push_to(&queue, "monitor_run", &requests::monitor_run_payload(1, "button"))
        .expect("a request the queue takes");

    let mode = std::fs::metadata(&queue)
        .expect("the queue exists after a push")
        .permissions()
        .mode()
        & 0o777;
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", queue.display()));
    }
    println!("queue file mode: 0o{mode:o}");
    assert_eq!(
        mode, 0o600,
        "the queue holds the reader's own words and is 0o{mode:o}, not owner-only"
    );
}
