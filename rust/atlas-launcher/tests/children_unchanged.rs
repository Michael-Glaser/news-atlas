//! The launcher's child list must not grow because of the reader.
//!
//! ATLAS reads pages in a webview that lives *inside the window* (`atlas-desktop`), and
//! every page it reads can carry a link the reader clicks. None of that may reach this
//! process: `atlas start` starts three children and nothing else, so a clicked link stays
//! a message between the window and the archive — never a fourth process, and never an
//! argument of one.
//!
//! Four claims, each pinned against the shipped source and the public builders:
//!
//! 1. **Three children, in one order.** `start::run` starts the model server, then the
//!    agent, then the window, and stops them the other way round
//!    (`shutdown::ORDER`: window, agent, model server). The provisioning step
//!    (`setup::run`) precedes all three and starts nothing on a provisioned machine.
//!
//!    The brief for this file lists the order as "llama-server, atlas-desktop,
//!    atlas-monitor". The shipped order is **llama-server, atlas-monitor, atlas-desktop**,
//!    deliberately: `start.rs` starts the agent before the window ("7b", "8") so the first
//!    click in a freshly opened window has an agent to claim it, and the shutdown order is
//!    the reverse of that. This test pins what ships; the mismatch is reported rather than
//!    papered over, and a reordering now fails here with the measured order in the message.
//! 2. **The list cannot grow.** Every process-creating call in shipped code is one of six
//!    functions, each named with its reason. A webview helper, a reader, a child for a
//!    clicked link — any of them would add a name and fail.
//! 3. **No argv can carry a URL a reader clicked.** The model server's line is built by one
//!    function from three numbers, the model file and the port; the window takes no
//!    argument at all; the agent takes one word (`serve`); the install child takes
//!    `--install --root <root>`. None of those four modules names a URL.
//! 4. **The launcher never names wry or the webview module.** The webview belongs to the
//!    window; this process only `exec`s the window's binary.
//!
//! Nothing here spawns anything: every claim is read from the shipped source, the manifest
//! or the public builders, so this file opens no port, starts no child and needs no
//! cleanup.

use std::path::{Path, PathBuf};

use atlas_launcher::{agent, llama, setup, shutdown, window};

// ---------------------------------------------------------------------------
// reading the shipped source
// ---------------------------------------------------------------------------

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// One source file's shipped half: `src/<name>`, cut at its `#[cfg(test)]` module.
fn shipped(name: &str) -> String {
    let path = src_dir().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {}", path.display(), error));
    match text.find("#[cfg(test)]") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// A file name that is a Rust module name. A copy somebody left in `src/` as
/// `start 2.rs` is not compiled and is not this crate's code.
fn is_module_name(stem: &str) -> bool {
    !stem.is_empty()
        && stem
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

/// Every shipped source file, as `(path relative to src/, code without the test module)`.
fn shipped_all() -> Vec<(String, String)> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, String)>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = match path.file_name().and_then(|name| name.to_str()) {
                Some(name) => name.to_string(),
                None => continue,
            };
            if path.is_dir() {
                // `tests/` is test code by its directory.
                if name != "tests" && is_module_name(&name) {
                    walk(&path, &format!("{}{}/", prefix, name), out);
                }
                continue;
            }
            let stem = match name.strip_suffix(".rs") {
                Some(stem) => stem,
                None => continue,
            };
            // `tests.rs`, `tests_*.rs`, `*_tests.rs` are test code by their name.
            if !is_module_name(stem) || stem == "tests" || stem.ends_with("_tests") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                let code = match text.find("#[cfg(test)]") {
                    Some(at) => text[..at].to_string(),
                    None => text,
                };
                out.push((format!("{}{}", prefix, name), code));
            }
        }
    }
    let mut files: Vec<(String, String)> = Vec::new();
    walk(&src_dir(), "", &mut files);
    files.sort();
    files
}

/// One function body: from its `header` to the line that closes it at its own indent.
fn body_of(source: &str, header: &str) -> Option<String> {
    let start = source.find(header)?;
    let mut depth = 0i32;
    let mut body = String::new();
    for line in source[start..].lines() {
        body.push_str(line);
        body.push('\n');
        depth += line.matches('{').count() as i32;
        depth -= line.matches('}').count() as i32;
        if depth == 0 && body.contains('{') {
            return Some(body);
        }
    }
    Some(body)
}

/// The code of a body with whole-line comments dropped: prose that *names* `.arg(` is not
/// an argument.
fn code_only(body: &str) -> String {
    body.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<&str>>()
        .join("\n")
}

/// The name of the function a position sits in, so a launch site can be named in a report.
fn enclosing_function(code: &str, at: usize) -> String {
    let mut name = String::from("?");
    for line in code[..at].lines() {
        let text = line.trim_start();
        for prefix in ["pub fn ", "pub(crate) fn ", "fn "] {
            if let Some(rest) = text.strip_prefix(prefix) {
                name = rest
                    .split(|ch: char| ch == '(' || ch.is_whitespace())
                    .next()
                    .unwrap_or("?")
                    .to_string();
                break;
            }
        }
    }
    name
}

/// Case-insensitive "does this text name that thing".
fn mentions(text: &str, needle: &str) -> bool {
    text.to_lowercase().contains(&needle.to_lowercase())
}

/// Where a needle is in a body, or a panic that says what was measured instead.
fn at(body: &str, needle: &str) -> usize {
    body.find(needle).unwrap_or_else(|| {
        panic!(
            "the boot no longer contains {:?}; what it contains, in order, is:\n{}",
            needle, body
        )
    })
}

/// A temp project root for the one builder that needs a `Config`; removed on drop.
struct Root {
    path: PathBuf,
}

impl Root {
    fn new(tag: &str) -> Root {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "atlas-children-unchanged-{}-{}-{}",
            tag,
            std::process::id(),
            stamp
        ));
        let _ = std::fs::remove_dir_all(&path);
        let _ = std::fs::create_dir_all(path.join("data"));
        let _ = std::fs::create_dir_all(path.join("models"));
        Root { path }
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ---------------------------------------------------------------------------
// 1. three children, in the order the boot documents
// ---------------------------------------------------------------------------

#[test]
fn the_boot_starts_the_model_then_the_agent_then_the_window() {
    // Measured from `start::run`, in source order: `setup::run(` (provisioning, which
    // starts nothing on a provisioned machine), then `start_model(` (the model server),
    // then `agent::Agent::launch(`, then `window::Window::launch(`.
    let source = shipped("start.rs");
    let boot = body_of(&source, "pub fn run(").expect("start::run exists");
    let code = code_only(&boot);

    let provisioning = at(&code, "setup::run(");
    let model = at(&code, "start_model(");
    let agent = at(&code, "agent::Agent::launch(");
    let window = at(&code, "window::Window::launch(");

    let measured = "llama-server (start_model) -> atlas-monitor (agent::Agent::launch) -> \
                    atlas-desktop (window::Window::launch)";
    assert!(
        provisioning < model,
        "provisioning must run before the model is resolved: {}",
        measured
    );
    assert!(
        model < agent,
        "the model server must exist before the agent that drives it: {}",
        measured
    );
    assert!(
        agent < window,
        "the agent must be up before the window whose clicks it claims: {}",
        measured
    );

    // One child each: a second `launch` for any of them would be a second process.
    for (needle, name) in [
        ("start_model(", "the model server"),
        ("agent::Agent::launch(", "the agent"),
        ("window::Window::launch(", "the window"),
    ] {
        assert_eq!(
            code.matches(needle).count(),
            1,
            "the boot starts {} {} times",
            name,
            code.matches(needle).count()
        );
    }

    // The three children by the names a report, a doctor check and this list all use: the
    // window is a *program* this process starts, and its name is not a module of this crate.
    assert_eq!(llama::BINARY_NAME, "llama-server");
    assert_eq!(agent::BINARY_NAME, "atlas-monitor");
    assert_eq!(agent::SERVE, "serve");
    assert_eq!(window::BINARY_NAME, "atlas-desktop");
    assert_eq!(setup::BINARY_NAME, "atlas-setup");
    assert_eq!(setup::INSTALL_ARG, "--install");

    // And nothing else. The only `::launch(` calls in the boot are the two named above,
    // and no process is created here by any other spelling.
    let launches: Vec<&str> = code
        .lines()
        .filter(|line| line.contains("::launch("))
        .collect();
    assert_eq!(
        launches.len(),
        2,
        "the boot starts a child this list does not name: {:#?}",
        launches
    );
    for spelling in ["Command::new", "child::command(", "child::command_with(", "::spawn("] {
        assert!(
            !code.contains(spelling),
            "the boot creates a process with {:?} instead of the three named steps",
            spelling
        );
    }

    // The stop order is the reverse of the order measured above, and that is the structural
    // reason for it: whoever waits on another child starts first and is stopped last. Both
    // sides are measured — the offsets read out of `start::run`, and `shutdown::ORDER` — so
    // neither can move without the other noticing. (This used to reverse a literal and
    // assert the reversed literal: true whatever the boot did.)
    let mut measured_order = [(model, "model server"), (agent, "agent"), (window, "window")];
    measured_order.sort();
    let start_order: Vec<&str> = measured_order.iter().map(|(_, name)| *name).collect();
    assert_eq!(
        start_order,
        vec!["model server", "agent", "window"],
        "the boot's order changed: {}",
        measured
    );
    let stopped: Vec<&str> = shutdown::ORDER.iter().map(|step| step.name()).collect();
    assert_eq!(
        stopped,
        vec!["window", "agent", "model server"],
        "the documented stop order changed"
    );
    let reverse_of_stop: Vec<&str> = stopped.iter().rev().copied().collect();
    assert_eq!(
        start_order, reverse_of_stop,
        "the start order ({}) is no longer the reverse of the stop order ({})",
        start_order.join(" -> "),
        stopped.join(" -> ")
    );
}

// ---------------------------------------------------------------------------
// 2. the child list cannot grow
// ---------------------------------------------------------------------------

#[test]
fn the_shipped_code_creates_processes_in_exactly_six_functions() {
    // Every way this crate can create a process, named with its reason. A child of any
    // other kind — a webview helper, a reader, one process per clicked link — would have
    // to add a name here, which is the review this test exists to force.
    //
    //   child::capture   a `--version`/`--help` probe, bounded by a deadline
    //   child::politely  `kill -TERM <pid>`, for a pid this process started
    //   llama::spawn     the model server
    //   window::launch   the window (`atlas-desktop`) — the only child with a webview
    //   agent::launch    the monitoring agent (`atlas-monitor serve`)
    //   setup::launch    the install tool, once, when a piece is missing
    let mut sites: Vec<String> = Vec::new();
    for (name, code) in shipped_all() {
        for spelling in ["Command::new(", "child::command(", "child::command_with("] {
            let mut at_byte = 0;
            while let Some(found) = code[at_byte..].find(spelling) {
                let start = at_byte + found;
                sites.push(format!("{}::{}", name, enclosing_function(&code, start)));
                at_byte = start + spelling.len();
            }
        }
    }
    sites.sort();
    sites.dedup();
    assert_eq!(
        sites,
        vec![
            "agent.rs::launch".to_string(),
            "child.rs::capture".to_string(),
            "child.rs::politely".to_string(),
            "llama.rs::spawn".to_string(),
            "setup.rs::launch".to_string(),
            "window.rs::launch".to_string(),
        ],
        "the launcher gained or lost a way to create a process: {:?}",
        sites
    );

    // None of those modules is about the reader or a browser: the only reader UI is inside
    // the window, which this process starts as a child and never links.
    for site in &sites {
        let module = site.split("::").next().unwrap_or(site);
        for word in ["webview", "wry", "reader", "browser"] {
            assert!(
                !module.contains(word),
                "a process is created by {} — the reader must not add a child",
                site
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 3. no argv can carry a URL a reader clicked
// ---------------------------------------------------------------------------

/// The `--help` text of a build that offers both build-dependent flags.
const HELP: &str = "\
usage: llama-server [options]
  --host HOST                 or -h
  --no-webui                  disable the Web UI
  --spec-type TYPE            drafting type: none, draft-mtp, ...
";

#[test]
fn the_model_server_command_line_is_a_closed_list() {
    // The one argv the launcher builds is the model server's, and it is built from three
    // numbers (the settings), the model file and the port. Everything else is a literal.
    // A URL would have to enter one of those slots, and none of them is a string the
    // window can write: the settings are numbers, the model is a file in `models/`, the
    // port is a `u16`.
    let root = Root::new("argv");
    let paths = atlas_config::Paths::at(&root.path);
    let mut config = atlas_config::Config::load_at(&paths).expect("a config can be built");
    config.model_context = 4096;
    config.model_gpu_layers = 99;
    config.model_threads = 8;
    let model = Path::new("/opt/atlas/models/qwen.gguf");

    let with_numbers = llama::argv(&config, model, 8081, HELP);
    let expected_with_numbers: Vec<&str> = vec![
        "-m",
        "/opt/atlas/models/qwen.gguf",
        "-c",
        "4096",
        "-ngl",
        "99",
        "-t",
        "8",
        "--host",
        "127.0.0.1",
        "--port",
        "8081",
        "-np",
        "1",
        "--jinja",
        "--log-disable",
        "--no-webui",
        "--spec-type",
        "draft-mtp",
    ];
    assert_eq!(with_numbers, expected_with_numbers, "the model server's argv changed");

    // The two flags that are absent mean "llama.cpp decides": at -1 and 0 the argv must
    // not grow a number nobody chose.
    config.model_gpu_layers = -1;
    config.model_threads = 0;
    let plain = llama::argv(&config, model, 8081, "");
    let expected_plain: Vec<&str> = vec![
        "-m",
        "/opt/atlas/models/qwen.gguf",
        "-c",
        "4096",
        "--host",
        "127.0.0.1",
        "--port",
        "8081",
        "-np",
        "1",
        "--jinja",
        "--log-disable",
    ];
    assert_eq!(plain, expected_plain, "the model server's argv changed");

    // A hostile model name — the file name is chosen by whoever put the file in `models/`,
    // and a page can put a name in a page — stays one entry, and nothing else in the line
    // becomes a URL.
    let hostile = "/opt/atlas/models/reader https://evil.example/x --url http://127.0.0.1:9/?q=1 .gguf";
    let hostile_args = llama::argv(&config, Path::new(hostile), 8081, HELP);
    assert_eq!(
        hostile_args.iter().filter(|arg| *arg == hostile).count(),
        1,
        "the hostile model name is not exactly one argument: {:?}",
        hostile_args
    );
    assert_eq!(
        hostile_args
            .iter()
            .position(|arg| arg == hostile)
            .expect("the model is in the line"),
        1,
        "the model file must be the value of `-m` and nothing else: {:?}",
        hostile_args
    );
    let urlish: Vec<&String> = hostile_args
        .iter()
        .filter(|arg| arg.contains("://"))
        .collect();
    assert_eq!(
        urlish,
        vec![&hostile.to_string()],
        "a URL reached the argv somewhere other than the model file's own name: {:?}",
        hostile_args
    );

    // And the only thing that can vary between two starts is the model file and the port:
    // every other entry is a literal. Measured by building the line twice.
    let other = llama::argv(&config, Path::new("/opt/atlas/models/other.gguf"), 8082, HELP);
    let differing: Vec<usize> = plain
        .iter()
        .zip(other.iter())
        .enumerate()
        .filter(|(_, (left, right))| left != right)
        .map(|(index, _)| index)
        .collect();
    let model_at = plain.iter().position(|arg| arg == "-m").expect("a -m") + 1;
    let port_at = plain.iter().position(|arg| arg == "--port").expect("a --port") + 1;
    assert_eq!(
        differing,
        vec![model_at, port_at],
        "an argument other than the model file and the port can change between starts: \
         {:?}",
        differing
    );

    // The builder takes no string from anywhere but those slots: the settings it reads are
    // numbers (`u32`, `i32`, `u16`), which a page cannot make into a URL.
    let argv_body = body_of(&shipped("llama.rs"), "pub fn argv(").expect("llama::argv exists");
    let argv_body = code_only(&argv_body);
    let mut fields: Vec<String> = Vec::new();
    for part in argv_body.split("config.").skip(1) {
        let field: String = part
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .collect();
        if !field.is_empty() {
            fields.push(field);
        }
    }
    fields.sort();
    fields.dedup();
    assert_eq!(
        fields,
        vec![
            "model_context".to_string(),
            "model_gpu_layers".to_string(),
            "model_threads".to_string(),
        ],
        "the model server's argv reads something other than its three numbers: {:?}",
        fields
    );
    let _: u32 = config.model_context;
    let _: i32 = config.model_gpu_layers;
    let _: u16 = config.model_threads;
}

#[test]
fn no_other_child_takes_an_argument_a_reader_could_supply() {
    // The window takes no argument at all, the agent takes one word, and the install child
    // takes the switch that means "install" plus the root. Those three lines, and the
    // absence of a URL in the four modules that build argv, are the whole of what this
    // process can hand to a child.
    let window_body = body_of(&shipped("window.rs"), "pub fn launch(")
        .expect("window::Window::launch exists");
    let window_code = code_only(&window_body);
    for spelling in [".arg(", ".args("] {
        assert!(
            !window_code.contains(spelling),
            "the window is started with an argument ({}):\n{}",
            spelling,
            window_code
        );
    }
    assert!(
        !window_code.contains("env::var"),
        "the window's argv comes from the environment:\n{}",
        window_code
    );

    let agent_body =
        body_of(&shipped("agent.rs"), "pub fn launch(").expect("agent::Agent::launch exists");
    let agent_code = code_only(&agent_body);
    assert_eq!(
        agent_code.matches(".arg(").count(),
        1,
        "the agent takes more than one argument:\n{}",
        agent_code
    );
    assert!(
        agent_code.contains(".arg(SERVE)"),
        "the agent's one argument is not `serve`:\n{}",
        agent_code
    );
    assert!(
        !agent_code.contains("env::var"),
        "the agent's argv comes from the environment:\n{}",
        agent_code
    );

    let setup_body = body_of(&shipped("setup.rs"), "fn launch(").expect("setup::launch exists");
    let setup_code = code_only(&setup_body);
    assert_eq!(
        setup_code.matches(".arg(").count(),
        3,
        "the install child takes a different number of arguments:\n{}",
        setup_code
    );
    for needle in [
        ".arg(INSTALL_ARG)",
        ".arg(\"--root\")",
        ".arg(&paths.root)",
    ] {
        assert!(
            setup_code.contains(needle),
            "the install child's argv changed ({} missing):\n{}",
            needle,
            setup_code
        );
    }
    assert!(
        !setup_code.contains("env::var"),
        "the install child's argv comes from the environment:\n{}",
        setup_code
    );

    // No URL in any module that builds a child's command line — not in code, not in a
    // comment. Measured when this was written: none of the four contains `://` at all.
    for name in ["llama.rs", "window.rs", "agent.rs", "setup.rs"] {
        let code = shipped(name);
        assert!(
            !code.contains("://"),
            "{} names a URL, and this is a module that builds a child's command line",
            name
        );
    }
}

// ---------------------------------------------------------------------------
// 4. the launcher never names wry or the webview module
// ---------------------------------------------------------------------------

#[test]
fn the_launcher_never_names_wry_or_a_webview() {
    let sources = shipped_all();
    assert!(!sources.is_empty(), "no source files were read");
    // The scan is a written-down fact, not an assumption: it would see the words.
    assert!(mentions("a hidden WebView window", "webview"));
    assert!(mentions("Wry at the edge", "wry"));
    assert!(!mentions("the reader view", "webview"));

    for (name, code) in &sources {
        for needle in ["wry", "webview", "webkit"] {
            assert!(
                !mentions(code, needle),
                "{} names {:?}: the webview belongs to the window, and this process only \
                 starts that binary",
                name,
                needle
            );
        }
    }

    // The manifest says the same from the other side: no webview crate, and no dependency
    // edge to the window's crate — it is a child process, not a library this links.
    let manifest = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .expect("the manifest is readable");
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        for needle in ["wry", "webview", "atlas-desktop"] {
            assert!(
                !line.contains(needle),
                "the launcher's manifest declares {:?}: {}",
                needle,
                line
            );
        }
    }
}
