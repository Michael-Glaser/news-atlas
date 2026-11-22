//! Nothing about the reader may land in `data/config.json` or in the archive's `meta` table.
//!
//! The reader is a person reading a story: which page they opened, which article they had
//! selected, which link they clicked. None of that is a *setting*, and none of it is archive
//! metadata — the settings file is what the launcher reads to start llama.cpp, and `meta` is
//! where the agent stores its own markers (`search_engine`, the model's two token budgets).
//! A window that started writing "the story the reader last opened" into either file would be
//! building a reading history, in a product whose whole claim is that it stays on this
//! machine and holds what the reader chose to keep, nothing else.
//!
//! Three properties, each pinned where it can be measured:
//!
//! 1. **The meta keys this window reads are a closed list.** Every `.meta(…)` call in the
//!    shipped sources is scanned and its argument resolved (a string literal, or a `const`
//!    resolved through the crate's own sources). A key that arrives as a computed string —
//!    `&format!("reader:{}", story.id)`, a variable, a field — fails the test rather than
//!    silently becoming a per-story key. The list also fails the test if it grows.
//! 2. **Nothing is written to either file.** The archive handle is opened
//!    `SQLITE_OPEN_READ_ONLY` with `PRAGMA query_only` *set and read back*; `meta` is one
//!    `SELECT`; there is no `set_meta`, no `INSERT`/`UPDATE`/`DELETE` against `meta`, and no
//!    file write at all in the shipped sources. The one write this window has is the request
//!    queue, and there is a runtime assertion that queueing a request which *names a URL*
//!    leaves no `config.json` and no `meta` table behind.
//! 3. **An address carrying a user name and password never travels.** The window's page
//!    opener refuses it (`link::openable`), the queue refuses what it can see (a token), and
//!    the table's agent-side door — the same rule for both doors — refuses userinfo. The
//!    window's own door does not see userinfo yet: that gap is a named `#[ignore]` below, with
//!    the one-line fix, rather than a quiet assumption.
//!
//! What this file does **not** prove: that the agent writes nothing about the reader (it owns
//! its own sources and its own tests), and that a future view could not put a story id in a
//! *queue payload* — that is the request format's business, and it is checked where the
//! payloads are built (`src/requests.rs`'s constructors take a topic, an id or a setting, and
//! its tests pin the JSON).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The meta keys the window reads, sorted: the search engine the reader configured, and the
/// two token budgets the model may spend. This is the closed list — a fourth key fails
/// `the_meta_keys_the_window_reads_are_a_closed_list`, and the failure says why.
///
/// Deliberately absent, and each would be a leak of the reader's session: a last-opened URL,
/// a per-article marker, a reading position, a scroll offset, a "seen" flag, a sort order.
const META_KEYS: [&str; 3] = ["model_answer", "model_reasoning", "search_engine"];

// --------------------------------------------------------------------------- the sources

/// The crate's own directory: this file lives in `tests/`, so the sources are one level up.
fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every shipped source under `src/`, with its test module cut off.
///
/// A `#[cfg(test)]` module is not shipped: it compiles only for `cargo test`, it is where the
/// fixtures live, and its sample keys are not read by the window. Cutting at the marker is the
/// same rule `tests/config_secrets.rs` applies to the environment scan. `src/data/tests.rs` is
/// such a module too — declared from `data.rs` under `#[cfg(test)]` — so it is skipped by name
/// rather than by content.
fn shipped() -> Vec<(String, String)> {
    shipped_in(&manifest())
}

/// [`shipped`] for any directory holding a `src/` — the crate itself, or a copy a test mutated
/// on purpose (`a_reader_key_added_to_the_real_tree_is_caught_by_the_real_scan`).
fn shipped_in(crate_dir: &Path) -> Vec<(String, String)> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                walk(&path, &format!("{prefix}{name}/"), out);
                continue;
            }
            if !name.ends_with(".rs") || name == "tests.rs" {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            out.push((format!("{prefix}{name}"), shipped_part(&text)));
        }
    }
    let mut out = Vec::new();
    walk(&crate_dir.join("src"), "", &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// A source file up to its first `#[cfg(test)]`.
fn shipped_part(text: &str) -> String {
    match text.find("#[cfg(test)]") {
        Some(at) => text[..at].to_string(),
        None => text.to_string(),
    }
}

/// The `const NAME: &str = "…";` declarations of the shipped sources, by name.
///
/// The meta scan resolves an identifier argument through this map, so a key written as
/// `views::settings::ENGINE_KEY` is read as the string it is instead of being skipped as
/// "not a literal". A declaration with a computed value is left out — and then the call site
/// that uses it fails to resolve, which is the point.
fn constants(sources: &[(String, String)]) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    for (_, text) in sources {
        for line in text.lines() {
            let line = line.trim();
            let Some(rest) = line
                .strip_prefix("pub const ")
                .or_else(|| line.strip_prefix("const "))
            else {
                continue;
            };
            let Some((name, tail)) = rest.split_once(':') else {
                continue;
            };
            if !tail.trim_start().starts_with("&str") {
                continue;
            }
            let Some(rest) = tail.split_once('=') else {
                continue;
            };
            let value = rest.1.trim();
            let Some(value) = value.strip_prefix('"') else {
                continue;
            };
            let Some(value) = value.split('"').next() else {
                continue;
            };
            found.push((name.trim().to_string(), value.to_string()));
        }
    }
    found
}

/// One `.meta(…)` read: where it is, what it asks for, and whether the argument could be read.
struct MetaRead {
    site: String,
    key: Option<String>,
    argument: String,
}

/// Every `.meta(…)` call in the shipped sources, in file order.
///
/// A definition (`pub fn meta(&self, key: &str)`) does not match: the needle needs the dot of a
/// *call*. `Budget::from_meta(…)` does not match either — different name.
fn meta_reads(sources: &[(String, String)], consts: &[(String, String)]) -> Vec<MetaRead> {
    const NEEDLE: &str = ".meta(";
    let mut reads = Vec::new();
    for (file, text) in sources {
        for (index, _) in text.match_indices(NEEDLE) {
            let after = &text[index + NEEDLE.len()..];
            let argument: String = after
                .chars()
                .take_while(|ch| *ch != ')')
                .collect::<String>()
                .trim()
                .to_string();
            let line = text[..index].matches('\n').count() + 1;
            let key = if argument.len() >= 2 && argument.starts_with('"') {
                // A literal: the closing quote has to be the last character, or this is a
                // concatenation (`"reader:" + …`) and not a name.
                argument[1..]
                    .strip_suffix('"')
                    .map(str::to_string)
                    .filter(|value| !value.contains('"'))
            } else {
                let name = argument.rsplit("::").next().unwrap_or(&argument).trim();
                let name = name.split('.').next().unwrap_or(name).trim();
                consts
                    .iter()
                    .find(|(known, _)| known == name)
                    .map(|(_, value)| value.clone())
            };
            reads.push(MetaRead {
                site: format!("src/{file}:{line}"),
                key,
                argument,
            });
        }
    }
    reads
}

/// A scratch directory that does not exist yet, unique to this process and test.
fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|spent| spent.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "atlas-reader-config-{}-{}-{}",
        tag,
        std::process::id(),
        nanos
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

// ------------------------------------------------------------------------ 1. the closed list

#[test]
fn the_meta_keys_the_window_reads_are_a_closed_list() {
    let sources = shipped();
    assert!(
        sources.len() > 20,
        "the walk found {} source files — it is not reading the crate",
        sources.len()
    );
    let consts = constants(&sources);
    let reads = meta_reads(&sources, &consts);

    // (a) Every read has to be resolvable. A computed key is how a reading history would get
    //     in: `meta(&format!("reader:{}", story.id))` is one line away from the three reads
    //     below and would never be noticed by a grep for the three known names.
    let computed: Vec<&MetaRead> = reads.iter().filter(|read| read.key.is_none()).collect();
    assert!(
        computed.is_empty(),
        "a meta key is computed instead of named: {}. The meta keys this window reads are a \
         closed list of {}, because the table is the agent's and nothing about the reader \
         (a page they opened, an article they read) may become a key.\n{}",
        computed
            .iter()
            .map(|read| format!("{} reads {:?}", read.site, read.argument))
            .collect::<Vec<String>>()
            .join("; "),
        META_KEYS.join(", "),
        read_table(&reads),
    );

    // (b) The list is exactly the three settings, and nothing else.
    let keys: BTreeSet<String> = reads
        .iter()
        .filter_map(|read| read.key.clone())
        .collect();
    let expected: BTreeSet<String> = META_KEYS.iter().map(|key| key.to_string()).collect();
    assert_eq!(
        keys,
        expected,
        "the meta keys the window reads changed.\n  reads: {}\n  closed list: {}\n{}",
        keys.iter().cloned().collect::<Vec<String>>().join(", "),
        META_KEYS.join(", "),
        read_table(&reads),
    );
}

/// The reads as a table, for a failure message that is worth reading.
fn read_table(reads: &[MetaRead]) -> String {
    let mut lines = vec!["  reads:".to_string()];
    for read in reads {
        lines.push(format!(
            "    {}  {:<20} ({})",
            read.site,
            read.key.clone().unwrap_or_else(|| "<computed>".to_string()),
            read.argument
        ));
    }
    lines.join("\n")
}

// -------------------------------------------------------- 2. nothing is written to either file

/// The first forbidden needle in `text`, for the two write pins below.
///
/// A function rather than an inline loop so the pin can be tested against a *synthetic* source:
/// `the_scan_sees_a_write_and_a_computed_key` feeds it a line that writes and a call that
/// computes its key, and fails if either goes unseen. That is how these scans are kept honest
/// without a second copy of them in a test.
fn forbidden<'a>(text: &str, needles: &[&'a str]) -> Option<&'a str> {
    needles.iter().copied().find(|needle| text.contains(needle))
}

#[test]
fn the_window_has_no_write_path_into_the_meta_table() {
    let sources = shipped();
    let all = join(&sources);
    if let Some(needle) = forbidden(
        &all,
        &[
            "set_meta",
            "INSERT INTO meta",
            "INSERT OR REPLACE INTO meta",
            "REPLACE INTO meta",
            "UPDATE meta",
            "DELETE FROM meta",
            "DROP TABLE meta",
        ],
    ) {
        panic!(
            "the window contains {needle:?}: the meta table is the agent's, and the window's \
             archive handle is read-only. Nothing about the reader may be stored there."
        );
    }
    // The read itself is one SELECT, on a connection that cannot write.
    let data = sources
        .iter()
        .find(|(file, _)| file == "data.rs")
        .map(|(_, text)| text.clone())
        .expect("src/data.rs");
    assert!(
        data.contains("SELECT v FROM meta WHERE k = ?1"),
        "the meta read is no longer the one SELECT this file pins"
    );
    for needle in [
        "SQLITE_OPEN_READ_ONLY",
        "pragma_update(None, \"query_only\", true)",
        "PRAGMA query_only",
    ] {
        assert!(
            data.contains(needle),
            "src/data.rs no longer opens the archive with {needle:?}: a read-write handle is a \
             write path into meta, whatever the rest of the crate says"
        );
    }
}

#[test]
fn the_settings_file_is_read_and_never_written() {
    let sources = shipped();
    let all = join(&sources);
    if let Some(needle) = forbidden(&all, &["fs::write(", "File::create(", "OpenOptions::new("]) {
        panic!(
            "the window contains {needle:?} in shipped code: the window writes no file but the \
             request queue (through rusqlite), and `data/config.json` belongs to \
             `atlas-config`'s atomic writer in the agent."
        );
    }
    // `write_all(` exists once, and it is the model socket, not a file.
    for (file, text) in &sources {
        for (index, line) in text.lines().enumerate() {
            if line.contains("write_all(") {
                assert_eq!(
                    file,
                    "http.rs",
                    "{}:{} writes bytes somewhere other than the model socket: {}",
                    file,
                    index + 1,
                    line.trim()
                );
            }
        }
    }
    // Every use of the config path is a read.
    let mut reads = 0usize;
    for (file, text) in &sources {
        for (index, line) in text.lines().enumerate() {
            if !line.contains("config()") || file == "assets.rs" {
                continue;
            }
            reads += 1;
            assert!(
                line.contains("read_to_string"),
                "{}:{} touches `data/config.json` through something other than a read: {}",
                file,
                index + 1,
                line.trim()
            );
        }
    }
    assert_eq!(
        reads, 1,
        "expected exactly one read of `data/config.json` in the shipped views, found {reads}"
    );
}

#[test]
fn a_queued_request_that_names_a_url_leaves_no_settings_file_and_no_meta_table() {
    // The runtime half: the window's one write is the queue, and a request naming a URL must
    // land there and nowhere else. `https://user:pw@host` is the shape this file is about; the
    // queue refuses it *there* (see the door tests below), so the URL used here is an ordinary
    // citation, which the queue accepts by design: the agent needs the reader's words.
    let dir = scratch("queue-url");
    let file = dir.join("requests.db");
    let url = "https://www.reuters.com/technology/tsmc-halts-china-chip-shipments-2026-11-21/";
    let payload = atlas_desktop::requests::settings_payload(
        &atlas_desktop::requests::settings_of(
            "search_engine",
            serde_json::Value::String("https://search.example/?q={query}".to_string()),
        ),
        &format!("I read {url} and want this engine"),
    );
    let id = atlas_desktop::requests::push_to(&file, "settings_set", &payload)
        .unwrap_or_else(|error| panic!("an ordinary settings request was refused: {error}"));
    assert!(id > 0, "the row was not written");
    assert!(file.is_file(), "the queue is not where it was told to be");

    // Not the settings file, anywhere under the root this test gave the window.
    let mut settings_files: Vec<PathBuf> = Vec::new();
    find_named(&dir, "config.json", &mut settings_files);
    assert!(
        settings_files.is_empty(),
        "queueing a request created a settings file: {settings_files:?}"
    );

    // Not the meta table: the queue's database has the queue's tables and nothing else.
    let connection = rusqlite::Connection::open(&file).expect("the queue file");
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .expect("sqlite_master");
    let tables: Vec<String> = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("rows")
        .flatten()
        .collect();
    assert!(
        !tables.iter().any(|table| table == "meta"),
        "the queue file has a meta table: {tables:?}"
    );
    assert!(
        tables.iter().all(|table| ["acceptance", "requests", "sqlite_sequence"]
            .contains(&table.as_str())),
        "the queue file has a table this window never creates: {tables:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every file called `name` under `dir`, recursively.
fn find_named(dir: &Path, name: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            find_named(&path, name, out);
        } else if entry.file_name().to_string_lossy() == name {
            out.push(path);
        }
    }
}

/// One string for a needle scan, with the file names attached to each line.
fn join(sources: &[(String, String)]) -> String {
    sources
        .iter()
        .map(|(file, text)| format!("// {file}\n{text}"))
        .collect::<Vec<String>>()
        .join("\n")
}

// --------------------------------------------------------------- 3. an address with a password

#[test]
fn an_address_with_a_user_name_and_password_is_never_opened() {
    // The reader's page address: the window refuses to hand a URL with userinfo to the system
    // opener or to its own web view, because the credential would be sent to that host.
    for url in [
        "https://user:pw@host.example/story",
        "https://user@host.example/story",
        "http://user:pw@host.example/story",
    ] {
        assert_eq!(
            atlas_desktop::link::openable(url),
            None,
            "{url} was accepted as an address to open"
        );
    }
    assert_eq!(
        atlas_desktop::link::openable("https://host.example/story"),
        Some("https://host.example/story"),
        "an ordinary address has to stay openable"
    );
}

#[test]
fn a_settings_payload_that_carries_a_token_is_refused_and_writes_no_row() {
    // What this window's door *can* see: a run of 40+ token bytes mixing case and digits.
    let dir = scratch("token");
    let file = dir.join("requests.db");
    let token = "Ab3XyZ9Qw8Er7Ty6Ui5Op4As3Df2Gh1Jk0LmN2Pq";
    let payload = atlas_desktop::requests::settings_payload(
        &atlas_desktop::requests::settings_of(
            "model_context",
            serde_json::Value::from(8192),
        ),
        &format!("the key from the vendor mail was {token}"),
    );
    let refused = atlas_desktop::requests::push_to(&file, "settings_set", &payload)
        .expect_err("a token-bearing settings payload was queued");
    assert_eq!(
        refused,
        atlas_desktop::requests::CREDENTIAL_REFUSAL,
        "the refusal is not the one sentence both doors use"
    );
    assert!(!refused.contains(token), "the refusal repeated the token");
    assert!(
        !file.is_file(),
        "a refused request left a queue file: {}",
        file.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_queues_credential_rule_refuses_userinfo_at_the_agents_door() {
    // The window cannot link `atlas-guard` (this crate links no workspace crate), so the
    // rule that refuses `https://user:pw@host` lives in the store's door, one level up from
    // this test's reach. Two assertions make that a fact this file can see:
    //
    //  * the store's queue door asks `carries_a_credential` — not a token scan, not a word
    //    list — and guard's credential pipeline includes `userinfo_spans`, so `user:pw@`
    //    is refused as a credential by shape;
    //  * both doors use the same sentence, character for character, so a reader who sees it
    //    once is not learning two rules.
    //
    // Measured elsewhere, so this file does not duplicate it: `atlas-store`'s
    // `config_value_secrets::the_queues_other_door_refuses_a_credential_and_writes_no_row`
    // queues exactly such a payload against the real door and asserts the row count.
    let crates = manifest().parent().map(Path::to_path_buf).expect("the crates directory");
    let store = std::fs::read_to_string(crates.join("atlas-store/src/requests.rs"))
        .expect("atlas-store/src/requests.rs");
    assert!(
        store.contains("atlas_guard::redact::carries_a_credential(payload)"),
        "the store's queue door no longer asks the credential rule this window quotes"
    );
    let guard = std::fs::read_to_string(crates.join("atlas-guard/src/redact.rs"))
        .expect("atlas-guard/src/redact.rs");
    assert!(
        guard.contains("userinfo_spans(&assigned)"),
        "guard's credential pipeline no longer strips URL userinfo, so `https://user:pw@host` \
         would pass the queue's agent-side door too"
    );
    assert!(
        store.contains("pub const CREDENTIAL_REFUSAL"),
        "the store's refusal sentence moved or lost its name"
    );
    assert_eq!(
        atlas_desktop::requests::CREDENTIAL_REFUSAL,
        "this request carries a credential; ATLAS will not store it — remove it from your \
         question and try again",
        "the window's sentence is not the one both doors are supposed to share"
    );
}

#[test]
#[ignore = "open gap: the window's own door sees token shapes only — it links no atlas-guard, \
            so `https://user:pw@host` (short userinfo, no token run) is queued here and refused \
            by the agent's door afterwards. Un-ignore when `requests::check` asks a userinfo \
            rule of its own; see the report."]
fn a_settings_payload_with_userinfo_is_refused_by_the_window_door_too() {
    // The shape the task names, measured against the window's own door. It fails today, and
    // the failure is the finding: the row is written and the agent refuses it later, which
    // means a credential sat in `data/requests.db` between the two.
    let dir = scratch("userinfo");
    let file = dir.join("requests.db");
    let payload = atlas_desktop::requests::settings_payload(
        &atlas_desktop::requests::settings_of(
            "search_engine",
            serde_json::Value::String("https://user:pw@host.example/?q={query}".to_string()),
        ),
        "the address my old browser had bookmarked",
    );
    let refused = atlas_desktop::requests::push_to(&file, "settings_set", &payload)
        .expect_err("a settings payload with userinfo was queued");
    assert_eq!(refused, atlas_desktop::requests::CREDENTIAL_REFUSAL);
    assert!(
        !file.is_file(),
        "a refused request left a queue file: {}",
        file.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------- the scans, against a fixture

#[test]
fn the_scan_sees_a_new_key_a_computed_key_and_a_write() {
    // The three scans above are only worth their green if they would notice the change they
    // exist for. Each is fed a synthetic source here rather than a mutated crate: the real
    // sources are in flight in other hands, and a scan proved by editing them is a scan nobody
    // can prove twice.
    let fixture = [
        (
            "fake.rs".to_string(),
            concat!(
                "fn frame(archive: &Archive, story: &Story) {\n",
                "    let engine = archive.meta(\"search_engine\");\n",
                "    let pending = archive.meta(\"reader_last_url\");      // a key that grew\n",
                "    let seen = archive.meta(&format!(\"read:{}\", story.id)); // a computed key\n",
                "    let keyed = archive.meta(ENGINE_KEY);\n",
                "    std::fs::write(path, b\"x\");                          // a write\n",
                "}\n",
            )
            .to_string(),
        ),
        (
            "elsewhere.rs".to_string(),
            "pub const ENGINE_KEY: &str = \"search_engine\";\n".to_string(),
        ),
    ];
    let consts = constants(&fixture);
    assert_eq!(
        consts,
        vec![("ENGINE_KEY".to_string(), "search_engine".to_string())],
        "the constant resolver missed the declaration it is given"
    );
    let reads = meta_reads(&fixture, &consts);

    // Four calls are seen, not three: the needle does not stop at the keys this list knows.
    assert_eq!(reads.len(), 4, "{}", read_table(&reads));
    let keys: BTreeSet<String> = reads.iter().filter_map(|read| read.key.clone()).collect();
    assert_eq!(
        keys,
        ["reader_last_url", "search_engine"]
            .iter()
            .map(|key| key.to_string())
            .collect::<BTreeSet<String>>(),
        "a new literal key or the constant was not resolved: {}",
        read_table(&reads)
    );
    // The computed one is refused, and its site is named.
    let computed = reads
        .iter()
        .find(|read| read.key.is_none())
        .expect("the computed key was read as a literal");
    assert_eq!(computed.site, "src/fake.rs:4", "{}", read_table(&reads));
    assert!(
        computed.argument.starts_with("&format!"),
        "{:?}",
        computed.argument
    );

    // And the write pin sees the write the fixture carries.
    let text = fixture
        .iter()
        .map(|(_, text)| text.clone())
        .collect::<Vec<String>>()
        .join("\n");
    assert_eq!(forbidden(&text, &["fs::write("]), Some("fs::write("));
    assert_eq!(forbidden(&text, &["set_meta", "UPDATE meta"]), None);
}

/// Every file under `from` into `to`, recursively — a copy of `src/` a mutation test may edit.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("copy directory");
    let Ok(entries) = std::fs::read_dir(from) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else if std::fs::copy(&path, &target).is_err() {
            panic!("could not copy {}", path.display());
        }
    }
}

#[test]
fn a_reader_key_added_to_the_real_tree_is_caught_by_the_real_scan() {
    // The mutation the closed list exists for, run over a *copy* of this crate's sources: one
    // line added to the file that already reads meta, asking for a key about the reader. The
    // copy means the shipped tree is not edited — three other agents are working in it — and
    // the scan under test is the same function the green test above runs.
    let copy = scratch("mutant");
    copy_dir(&manifest().join("src"), &copy.join("src"));
    let app = copy.join("src/app.rs");
    let text = std::fs::read_to_string(&app).expect("a copy of app.rs");
    // Prepended, because the scan stops at a file's `#[cfg(test)]` module and the fixture line
    // has to sit in shipped code.
    std::fs::write(
        &app,
        format!(
            "fn last_opened(archive: &Archive) {{ let _ = archive.meta(\"reader_last_url\"); }}\n{text}"
        ),
    )
    .expect("the mutated copy");

    let sources = shipped_in(&copy);
    assert_eq!(
        sources.len(),
        shipped().len(),
        "the copy is not a copy of the same tree"
    );
    let reads = meta_reads(&sources, &constants(&sources));
    let keys: BTreeSet<String> = reads.iter().filter_map(|read| read.key.clone()).collect();
    let expected: BTreeSet<String> = META_KEYS.iter().map(|key| key.to_string()).collect();
    assert_ne!(
        keys, expected,
        "the scan read the same closed list off a tree with an extra key in it — the test above \
         would not have caught the reader's last URL becoming a meta key: {}",
        read_table(&reads)
    );
    assert!(keys.contains("reader_last_url"), "{:?}", keys);
    let _ = std::fs::remove_dir_all(&copy);
}
