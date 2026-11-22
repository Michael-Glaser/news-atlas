//! The model's port comes from `data/config.json`, and from nowhere else.
//!
//! The window used to carry the model address as a constant (`http://127.0.0.1:8080`),
//! while the model server is started on the port `data/config.json` names — out of the
//! box the two disagreed, so the Chat page dialled a port nothing was listening on and
//! went straight to "not answering". The fix is a *read*, not a second constant:
//! [`assets::model_port`] parses the one setting that matters and hands the caller a
//! `u16` it may put after the `127.0.0.1` literal; the caller builds the address.
//!
//! What this file proves, and why each part is a security decision rather than a
//! convenience:
//!
//! 1. **The shipped config is read and it says 8081.** Measured against the real
//!    `data/config.json` in this checkout, and cross-checked against an independent
//!    parse of the same file, so a wrong answer here cannot be an artefact of one
//!    parser agreeing with itself.
//! 2. **A value that is not a port is `None`, never a guess.** A missing file, a
//!    missing key, a string, a float, a negative number, zero, 65536, a boolean, an
//!    array, broken JSON, a directory where the file should be, and a config far larger
//!    than three keys: each is `None` so the caller falls back to its own default
//!    instead of dialling something nobody chose.
//! 3. **Only the port survives the parse.** `data/config.json` is a file in the user's
//!    own data directory, so it is writable by the user and by anything running as the
//!    user. A hostile key such as `"llama_host": "evil.example"` or
//!    `"model_url": "http://evil.example:9"` changes nothing: there is no code path
//!    from this file to a host, a scheme or a url — the answer is a `u16`, and the host
//!    is the `127.0.0.1` literal in `http::endpoint`. This is the difference between a
//!    setting and a redirect: a config can pick the port on this machine, and cannot
//!    pick the machine.
//! 4. **What the config names is a port the dial accepts.** The two ends are joined
//!    here rather than asserted separately: the value from the file, put after the
//!    literal, is an address [`http::endpoint`] answers `Ok` for — including the
//!    boundaries 1 and 65535.
//!
//! What this file does not decide: the fallback (that is the app's, and it is still
//! `DEFAULT_MODEL_URL`), the dial itself (`tests/model_url.rs`), and the disclosure
//! rules for a value on screen (`tests/disclosure.rs`).
//!
//! How the cases were shown to be load-bearing, measured rather than argued:
//!
//! * this file was written first and run against `src/assets.rs` before the function
//!   existed — it fails to compile with `cannot find function 'model_port' in module
//!   'assets'` (`E0425`), so every assertion here is a new requirement;
//! * with the function in place, the parser was widened to a lenient one (a default of
//!   8081 when the key is missing or unreadable, a string coerced with `parse()`, the
//!   range unchecked) and **18 of the 22** table cases below were read as a port — the
//!   four that survived are the ones that are not JSON at all, which the parse itself
//!   refuses;
//! * deleting only the `MAX_CONFIG` check made the 128 kB case read `Some(8081)`.

use atlas_desktop::{assets, http};

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

// --------------------------------------------------------------------------- fixtures

/// A temporary directory, removed when the value is dropped.
///
/// Unique per process *and* per case, so two tests in parallel (and two `cargo test`
/// runs) cannot share a tree. Everything this file writes lives here: the tests never
/// write under `data/`, which is the reader's archive.
struct Temp(PathBuf);

impl Temp {
    fn new(case: &str) -> Temp {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let unique = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "atlas-model-config-{}-{}-{}",
            std::process::id(),
            unique,
            case
        ));
        std::fs::create_dir_all(&dir)
            .unwrap_or_else(|error| panic!("cannot make {}: {error}", dir.display()));
        Temp(dir)
    }

    /// Write `text` as `config.json` in this tree and answer its path.
    fn config(&self, text: &str) -> PathBuf {
        let path = self.0.join("config.json");
        std::fs::write(&path, text)
            .unwrap_or_else(|error| panic!("cannot write {}: {error}", path.display()));
        path
    }

    /// A path in this tree that is deliberately never created.
    fn missing(&self) -> PathBuf {
        self.0.join("config.json")
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The address a caller builds from a port: the host is the literal, always, and only
/// the number after it came from the file.
fn address(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// The config as this checkout ships it, read by the test rather than by the crate: the
/// yardstick for [`assets::model_port`] must not be the crate's own parse.
fn shipped_config_text() -> String {
    let path = assets::root().join("data").join("config.json");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

// --------------------------------------------------------------------------- 1. the real file

/// The shipped `data/config.json` names the model server's port, and this crate reads it.
///
/// The model server is started on `llama_port`, so a window that dials anything else is
/// broken out of the box — that is the defect this pins. The number is asserted, not
/// merely "some port": 8081 is what the file in this checkout says, and a change to it
/// should send whoever changed it to this line (and to the app's fallback).
#[test]
fn the_shipped_config_names_the_port_the_model_server_listens_on() {
    let text = shipped_config_text();
    let value: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("data/config.json is not JSON: {error}\n{text}"));

    // Read the file the way the caller would, through the shipped function.
    let port = assets::model_port();
    assert_eq!(
        port,
        Some(8081),
        "data/config.json says llama_port = {}, and assets::model_port() answered {port:?}",
        value.get("llama_port").map_or_else(
            || "nothing".to_string(),
            |named| named.to_string()
        )
    );

    // And the same answer, from an independent parse of the same bytes: two parsers
    // agreeing is a fact about the file, one parser agreeing with itself is not.
    let independent = value
        .get("llama_port")
        .and_then(serde_json::Value::as_u64)
        .and_then(|named| u16::try_from(named).ok())
        .filter(|named| *named != 0);
    assert_eq!(
        port, independent,
        "the function and a direct parse of data/config.json disagree: {port:?} vs {independent:?}"
    );

    // The value is a port the dial accepts, on the literal host this window may reach.
    let url = address(port.expect("the shipped config names a port"));
    let (host, socket) = http::endpoint(&url)
        .unwrap_or_else(|sentence| panic!("{url} is not an address the dial takes: {sentence}"));
    assert_eq!(host, "127.0.0.1:8081");
    assert!(
        socket.ip().is_loopback() && socket.port() == 8081,
        "{url} became {socket}"
    );
}

// --------------------------------------------------------------------------- 2. the refusals

/// Every shape that is not "a port between 1 and 65535" is `None`, so the caller keeps
/// its own default rather than dialling a value nobody chose.
///
/// `None` is the only safe answer to all of these: a fallback is a decision the app can
/// explain in its status row, while a guess is an address with no author.
#[test]
fn a_file_that_does_not_name_a_port_is_none() {
    let tree = Temp::new("refusals");
    let cases: &[(&str, &str)] = &[
        ("an empty object", "{}"),
        ("the other settings but not the port", r#"{"app_port":8787,"cdp_port":9222}"#),
        ("the key spelled differently", r#"{"llama-port":8081,"port":8081,"LLAMA_PORT":8081}"#),
        ("a port as a string", r#"{"llama_port":"8081"}"#),
        ("a port with a newline in it", r#"{"llama_port":"8081\nhttp://evil.example"}"#),
        ("a port as a float", r#"{"llama_port":8081.5}"#),
        ("a float that is whole", r#"{"llama_port":8081.0}"#),
        ("a negative port", r#"{"llama_port":-1}"#),
        ("port zero", r#"{"llama_port":0}"#),
        ("one past the last port", r#"{"llama_port":65536}"#),
        ("a number that is not a u16 at all", r#"{"llama_port":4294967296}"#),
        ("a port as a boolean", r#"{"llama_port":true}"#),
        ("a port as null", r#"{"llama_port":null}"#),
        ("a port as an array", r#"{"llama_port":[8081]}"#),
        ("a port as an object", r#"{"llama_port":{"value":8081}}"#),
        ("an empty file", ""),
        ("text that is not JSON", "llama_port = 8081\n"),
        ("JSON cut before the value", r#"{"llama_port": 8081"#),
        ("JSON cut inside the value", r#"{"llama_port": ""#),
        ("an array at the top level", "[8081]"),
        ("a bare number at the top level", "8081"),
        ("a string at the top level", "\"http://127.0.0.1:8081\""),
    ];
    // Every case is asked, and every case that was read as a port is named: a table test
    // that stops at the first refusal it fails tells the reader one case and hides the
    // rest, which is exactly the state a lenient parser would put this file in.
    let mut read_as_a_port = Vec::new();
    for (case, text) in cases {
        let path = tree.config(text);
        if let Some(port) = assets::model_port_from(&path) {
            read_as_a_port.push(format!("{case} ({text:?}) was read as port {port}"));
        }
    }
    assert!(
        read_as_a_port.is_empty(),
        "{} of {} non-port configs were read as a port:\n  {}",
        read_as_a_port.len(),
        cases.len(),
        read_as_a_port.join("\n  ")
    );
    assert!(!cases.is_empty(), "the table is the test");

    // A file that is not there at all: the caller's fallback, not a panic and not a
    // zero. The archive is never opened instead.
    assert_eq!(assets::model_port_from(&tree.missing()), None, "no file, no port");

    // A directory wearing the file's name: opening it is an error, and an error is
    // `None`. (A `/data/config.json` that is a directory is what a half-finished copy
    // leaves behind.)
    let as_dir = tree.0.join("as-a-directory");
    std::fs::create_dir_all(as_dir.join("config.json")).expect("a directory named config.json");
    assert_eq!(assets::model_port_from(&as_dir.join("config.json")), None);

    // A config far bigger than three keys: not read at all. The file is in the user's
    // data directory, so its size is not a promise this window can rely on.
    let huge = format!(
        "{{\"llama_port\":8081,\"padding\":\"{}\"}}",
        "x".repeat(128 * 1024)
    );
    let path = tree.config(&huge);
    assert_eq!(
        assets::model_port_from(&path),
        None,
        "a 128 kB config was read as a port — the read has no bound"
    );
}

/// The two ends of the range are ports, and the dial takes both.
#[test]
fn the_boundaries_are_ports() {
    let tree = Temp::new("boundaries");
    for port in [1u16, 8081, 65535] {
        let path = tree.config(&format!("{{\"llama_port\":{port}}}"));
        assert_eq!(
            assets::model_port_from(&path),
            Some(port),
            "{port} is a port and the file names it"
        );
        let url = address(port);
        assert!(
            http::endpoint(&url).is_ok(),
            "{url} came from the config and the dial refuses it"
        );
    }
}

// --------------------------------------------------------------------------- 3. only the port

/// A hostile `data/config.json` can choose this machine's port. It cannot choose the
/// machine: there is no key, spelling or nesting that puts a host, a scheme or a whole
/// url into the answer, because the answer is a `u16`.
#[test]
fn nothing_but_the_port_survives_the_parse() {
    let tree = Temp::new("hostile");
    let hostile = r#"{
        "app_port": 8787,
        "llama_port": 8081,
        "llama_host": "evil.example",
        "llama_url": "http://evil.example:9/v1/chat/completions",
        "model_url": "http://alice:secret@evil.example:9",
        "host": "10.0.0.1",
        "scheme": "https",
        "path": "/../../etc/passwd",
        "env": {"ATLAS_MODEL_URL": "http://evil.example:9"},
        "extra": [1, 2, 3]
    }"#;
    let path = tree.config(hostile);
    let port = assets::model_port_from(&path).expect("the port is in there");
    assert_eq!(port, 8081, "the port is the only value this function answers with");

    // The address a caller builds from it is the literal host and that one number:
    // nothing in the file above shows up in the dial.
    let (host, socket) = http::endpoint(&address(port)).expect("a loopback literal and a port");
    assert_eq!(host, "127.0.0.1:8081");
    assert!(socket.ip().is_loopback());
    for leak in ["evil", "alice", "secret", "10.0.0.1", "https", "passwd"] {
        assert!(
            !host.contains(leak),
            "the config put {leak:?} into the address: {host}"
        );
    }

    // A file that is not a config but names a port where a port would be: still just a
    // port, and still only reachable on the literal. (There is nothing to strip — the
    // value never becomes text.)
    // `\u202e` is a right-to-left override in JSON: a note that would reverse a label if
    // anything ever printed it. Nothing here does — the port is a number.
    let quoted = tree.config(r#"{"llama_port": 8081, "note": "\u202ehttp://evil.example"}"#);
    assert_eq!(assets::model_port_from(&quoted), Some(8081));
}

/// The function is the config's port and nothing else: it answers the same for a fixture
/// wherever it sits, so no path, no environment variable and no archive decides it.
#[test]
fn the_answer_depends_on_the_file_and_not_on_where_it_sits() {
    let text = r#"{"app_port":8787,"llama_port":8081,"cdp_port":9222}"#;
    let first = Temp::new("same-1");
    let second = Temp::new("same-2");
    let one = first.config(text);
    let two = second.config(text);
    assert_eq!(assets::model_port_from(&one), assets::model_port_from(&two));
    assert_eq!(assets::model_port_from(&one), Some(8081));

    // And a different port in a different file is a different answer: the function is
    // reading the file it was given, not a cached or compiled-in number.
    let other = second.config(r#"{"llama_port":9091}"#);
    assert_eq!(assets::model_port_from(&other), Some(9091));
    assert_eq!(
        assets::model_port_from(&one),
        Some(8081),
        "the first file still says what it says"
    );
}

// --------------------------------------------------------------------------- the seam

/// `model_port()` reads `<root>/data/config.json` and no other file, and the path it
/// reads is the one the archive sits beside.
///
/// The root is discovered by walking up to `rust/Cargo.toml` (assets.rs), so from this
/// test binary — under `rust/target/…` — the answer is the checkout's own config.
#[test]
fn the_shipped_path_is_the_archive_s_directory() {
    let root = assets::root();
    assert_eq!(
        assets::archive(),
        root.join("data").join("atlas.db"),
        "the archive moved, so the config beside it did too"
    );
    assert!(
        root.join("data").join("config.json").is_file(),
        "there is no data/config.json beside the archive under {} — model_port() can only \
         answer None there",
        root.display()
    );

    // The path is a file this test may read; nothing here writes it.
    let path = root.join("data").join("config.json");
    assert!(path.is_file(), "{} is not a file", path.display());
}

/// The one function the app calls, asked once, the way the app asks it.
#[test]
fn the_call_the_app_makes_answers_a_port_on_this_checkout() {
    match assets::model_port() {
        Some(port) => {
            assert!((1..=65535).contains(&port));
            assert!(
                http::endpoint(&address(port)).is_ok(),
                "{port} came from data/config.json and is not dialable"
            );
        }
        None => panic!(
            "assets::model_port() answered None on this checkout, so the Chat page would fall \
             back to the default port while the model server listens on {}",
            assets::root().join("data/config.json").display()
        ),
    }
}
