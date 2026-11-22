//! Tests that span modules: the command line, the file system, real children.
//!
//! Each of these is a behaviour the brief names, driven through the public surface
//! rather than a unit: `atlas doctor` on a root whose model is missing, a second
//! `atlas start` on one data directory, the shutdown order with real children, and
//! the model server's single restart with a real child and a real backoff.
//!
//! A test here may spawn a process, but only one it spawns itself, and only the ones
//! this file names: `/bin/echo` and `/bin/sleep` (children to stop), `/bin/ps` (the
//! process-table probe that tells a zombie from a reaped child), and — for the
//! "nothing is still listening" tests — this test binary itself. No shell, ever, and
//! every child is reaped before the test returns.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use crate::args::{Command as CliCommand, StartOptions};
use crate::entry::main_entry;
use crate::shutdown::{Shutdown, Step};
use crate::supervise::{self, ModelProcess, Outcome, Policy};

pub(crate) fn fresh_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "atlas-launcher-test-{}-{}-{}",
        tag,
        std::process::id(),
        atlas_store::now_iso().replace([':', '.'], "-")
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(dir.join("data"));
    let _ = std::fs::create_dir_all(dir.join("models"));
    dir
}

/// `ps(1)` where this platform keeps it: read-only, no shell, one pid at a time.
pub(crate) fn ps_program() -> Option<&'static str> {
    ["/bin/ps", "/usr/bin/ps"]
        .into_iter()
        .find(|program| Path::new(program).is_file())
}

/// The process-table state of a pid, as `ps` prints it: `Some("Z…")` for a zombie,
/// `None` when the operating system has no entry left for it.
///
/// This is the measurement a stopped child has to pass, and `kill -0` cannot make
/// it: `kill -0` *succeeds* on a zombie. A pid that was killed but never waited for
/// therefore looks alive to every probe but this one. Only a pid this crate's own
/// tests started is ever named here.
pub(crate) fn process_entry(pid: u32) -> Option<String> {
    let program = ps_program()?;
    let output = Command::new(program)
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .env_clear()
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Wait (briefly) for a pid to leave the process table, then report what is there.
pub(crate) fn wait_for_no_entry(pid: u32) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut state = process_entry(pid);
    while Instant::now() < deadline && state.is_some() {
        std::thread::sleep(Duration::from_millis(10));
        state = process_entry(pid);
    }
    state
}

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|word| word.to_string()).collect()
}

/// `atlas start --root <temp> --no-install`, as the process entry point sees it.
///
/// Every test that boots a temp root goes through here, and the `--no-install` is the
/// point: since `atlas start` provisions a machine that has no model, a test that booted
/// without the switch would start the *real* install child on any machine that happens to
/// have `atlas-setup` installed — a test suite that can download 6.5 GB. The provisioning
/// step has its own tests in `setup.rs`, against a stand-in binary inside the temp root.
fn start_here(root: &Path) -> i32 {
    main_entry(&words(&[
        "start",
        "--root",
        root.to_str().unwrap(),
        "--no-install",
    ]))
}

// ---------------------------------------------------------------------------
// the command line, through the process boundary
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// data/ is made owner-only by a start
// ---------------------------------------------------------------------------

/// The doctor's sentence — "run `atlas start` once — it hardens data/ to 0700 and
/// its files to 0600" — was advice nothing carried out: `harden_data_files` was
/// exported by `atlas-config` and called by nobody, so a `data/` left by an older
/// build (or created by hand) kept `0755`/`0644` and every log stayed readable by
/// any account on the machine. Measured before the fix: a `data/app.log` at `0644`
/// survived a whole boot at `0644` while `/api/security` published the mode ATLAS
/// *wanted*.
///
/// The boot runs to the model check, which this root fails (no model file) — that
/// is after the hardening step and before any child, so this asserts the step a
/// start performs without starting a model or opening a listener anyone can reach.
#[test]
fn a_start_makes_a_world_readable_log_owner_only() {
    let root = fresh_root("harden");
    let paths = atlas_config::Paths::at(&root);
    let log = paths.log_app.clone();
    std::fs::write(&log, b"a line from an older build\n").expect("an older log");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    }
    let code = start_here(&root);
    // The missing model is still a failed start; the hardening happened before it.
    assert_eq!(code, 1);
    if atlas_config::posix() {
        assert_eq!(
            atlas_platform::private::mode_of(&log),
            Some(atlas_config::FILE_MODE),
            "data/app.log was {:?} after a start, not 0o{:o}",
            atlas_platform::private::mode_of(&log),
            atlas_config::FILE_MODE
        );
        assert_eq!(
            atlas_platform::private::mode_of(&paths.data_dir),
            Some(atlas_config::DATA_MODE),
            "data/ was {:?} after a start, not 0o{:o}",
            atlas_platform::private::mode_of(&paths.data_dir),
            atlas_config::DATA_MODE
        );
    }
    // The contents are the older build's: hardening a mode is not truncating a log.
    assert_eq!(
        std::fs::read(&log).expect("the log is still there"),
        b"a line from an older build\n"
    );
    let _ = std::fs::remove_dir_all(&root);
}


#[test]
fn help_and_version_exit_zero_and_an_unknown_command_exits_two() {
    assert_eq!(main_entry(&[]), 0);
    assert_eq!(main_entry(&words(&["help"])), 0);
    assert_eq!(main_entry(&words(&["--version"])), 0);
    // Exit 2 is "not understood", distinct from 1 = "cannot do it".
    assert_eq!(main_entry(&words(&["startt"])), 2);
    assert_eq!(main_entry(&words(&["doctor", "--nope"])), 2);
    assert_eq!(main_entry(&words(&["start", "--root"])), 2);
}

#[test]
fn doctor_on_a_temporary_root_without_the_model_exits_one_and_names_the_file() {
    let root = fresh_root("doctor");
    let code = main_entry(&words(&["doctor", "--root", root.to_str().unwrap()]));
    assert_eq!(code, 1, "a missing model must fail the doctor");
    // The same root gains a model: the model check must clear and the code change.
    let paths = atlas_config::Paths::at(&root);
    std::fs::write(&paths.model_file, vec![0u8; 512]).unwrap();
    let report = crate::doctor::check(&paths);
    assert_eq!(report.find("model file").unwrap().status, crate::doctor::Status::Ok);
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// one instance per data directory
// ---------------------------------------------------------------------------

#[test]
fn a_second_start_on_the_same_data_directory_is_refused_before_anything_happens() {
    let root = fresh_root("twice");
    let paths = atlas_config::Paths::at(&root);
    // A first instance holds the lock (a real one: the file is open and locked).
    let first = crate::lock::acquire(&paths.data_dir, &crate::lock::this_process(&paths.root))
        .expect("the first instance takes the lock");
    let code = start_here(&root);
    assert_eq!(code, 1, "a second start must not run");
    // Exit 1 alone is not the promise: the refusal has to *say* what happened and
    // who has it. `start::run` returns that sentence, so it can be read here
    // without capturing a terminal — and the second attempt must not have touched
    // anything (it stopped at the lock, before the settings or the model).
    let message = crate::start::run(StartOptions {
        root: Some(root.clone()),
        dry_run: false,
        // A test must never start a real download: `--no-install` is the switch the
        // boot itself offers for a machine that provisions by hand, and it is what a
        // test about *anything else* has to pass. The provisioning step has its own
        // tests in `setup.rs`, against a stand-in binary in a temp root.
        no_install: true,
    })
    .expect_err("a held lock refuses the boot");
    assert!(
        message.contains("another ATLAS is already running"),
        "{}",
        message
    );
    assert!(
        message.contains(&format!("pid {}", std::process::id())),
        "the refusal names the instance that holds it: {}",
        message
    );
    assert!(message.contains("stop that instance first"), "{}", message);
    drop(first);
    // With the lock gone, the same command gets past the lock: the next thing it
    // reports is the missing model, which is what this root has.
    let code = start_here(&root);
    assert_eq!(code, 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_start_without_the_window_binary_stops_before_the_model_server_is_spawned() {
    // The window is the product, so a machine that never built it hears one sentence —
    // and hears it *before* four minutes of loading weights. The ordering is the claim:
    // the check sits above the model server in `start::run`, so no child exists yet.
    if atlas_platform::executable::on_path(crate::window::BINARY_NAME).is_some() {
        return; // this machine has a window on PATH: the boot would legitimately go on
    }
    let root = fresh_root("nowindow");
    let paths = atlas_config::Paths::at(&root);
    // A model file, so the model check passes and the window check is what stops it.
    std::fs::write(&paths.model_file, vec![7u8; 1024]).expect("a model file");
    let message = crate::start::run(StartOptions {
        root: Some(root.clone()),
        dry_run: false,
        // See the note in the lock test: a test never provisions. This one must also not
        // *ask* to provision, because `bin/llama-server` is absent here too.
        no_install: true,
    })
    .expect_err("a start without the window binary is refused");
    assert!(
        message.contains(crate::window::BINARY_NAME),
        "the refusal names the window binary: {}",
        message
    );
    assert!(
        message.contains("nothing is downloaded"),
        "the refusal keeps the no-download rule: {}",
        message
    );
    assert!(
        !paths.log_llama.exists(),
        "the model server was started for a window that cannot exist: {}",
        paths.log_llama.display()
    );
    // …and nothing was left holding the lock.
    let again = crate::lock::acquire(&paths.data_dir, &crate::lock::this_process(&paths.root));
    assert!(again.is_ok(), "the refused start must not leave the lock held");
    drop(again);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_start_without_the_model_file_stops_and_says_exactly_what_is_missing() {
    let root = fresh_root("nomodel");
    let paths = atlas_config::Paths::at(&root);
    let code = start_here(&root);
    assert_eq!(code, 1);
    // The start must not have started anything: no model server log was created,
    // and the lock it took is released again (a second attempt reaches the model
    // check rather than the lock check).
    assert!(!paths.log_llama.exists(), "no model server log should exist");
    let again = crate::lock::acquire(&paths.data_dir, &crate::lock::this_process(&paths.root));
    assert!(again.is_ok(), "the failed start must not leave the lock held");
    drop(again);
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// shutdown: children before exit
// ---------------------------------------------------------------------------

struct Tracked {
    name: &'static str,
    child: Child,
}

impl Tracked {
    fn spawn(name: &'static str) -> Option<Tracked> {
        if !Path::new("/bin/sleep").is_file() {
            return None;
        }
        let child = Command::new("/bin/sleep")
            .arg("30")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        Some(Tracked { name, child })
    }

    fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

#[test]
fn every_child_is_stopped_before_the_process_returns_and_in_the_declared_order() {
    // Three children: the window (`atlas-desktop`, the product), the monitoring agent
    // (`atlas-monitor serve`, the always-on half) and the model server. The listener and
    // the reader browser that used to be first in this list are gone from the product —
    // nothing in `atlas start` opens a port or starts Chrome — so the order under test is
    // window → agent → model.
    let (mut window, mut agent, mut model) = match (
        Tracked::spawn("window"),
        Tracked::spawn("agent"),
        Tracked::spawn("model"),
    ) {
        (Some(window), Some(agent), Some(model)) => (window, agent, model),
        _ => return, // a machine without /bin/sleep: nothing to prove here
    };
    // All are running *before* the shutdown, or "the child was stopped" below
    // would be a sentence about a process that had already finished on its own.
    assert!(
        !window.exited() && !agent.exited() && !model.exited(),
        "all three children are running"
    );
    let staged: Arc<Mutex<Vec<Tracked>>> = Arc::new(Mutex::new(vec![window, agent, model]));
    let order: Arc<Mutex<Vec<Step>>> = Arc::new(Mutex::new(Vec::new()));
    let pick = |name: &'static str| -> Option<Tracked> {
        let mut staged = staged.lock().unwrap();
        let index = staged.iter().position(|tracked| tracked.name == name)?;
        Some(staged.remove(index))
    };
    let window_tracked = pick("window");
    let agent_tracked = pick("agent");
    let model_tracked = pick("model");

    let mut shutdown = Shutdown::new();
    let model_tracked = Mutex::new(model_tracked);
    let agent_tracked = Mutex::new(agent_tracked);
    let window_tracked = Mutex::new(window_tracked);
    {
        let order = order.clone();
        shutdown = shutdown.add(Step::Agent, move || {
            if let Some(mut tracked) = agent_tracked.lock().unwrap().take() {
                crate::child::terminate(&mut tracked.child, Duration::from_secs(2));
            }
            order.lock().unwrap().push(Step::Agent);
        });
    }
    {
        let order = order.clone();
        shutdown = shutdown.add(Step::Model, move || {
            if let Some(mut tracked) = model_tracked.lock().unwrap().take() {
                let sentence = crate::child::terminate(&mut tracked.child, Duration::from_secs(2));
                assert!(sentence.contains("pid"), "{}", sentence);
            }
            order.lock().unwrap().push(Step::Model);
        });
    }
    {
        let order = order.clone();
        shutdown = shutdown.add(Step::Window, move || {
            if let Some(mut tracked) = window_tracked.lock().unwrap().take() {
                crate::child::terminate(&mut tracked.child, Duration::from_secs(2));
            }
            order.lock().unwrap().push(Step::Window);
        });
    }

    let performed = shutdown.run();
    assert_eq!(performed, crate::shutdown::ORDER.to_vec());
    assert_eq!(
        *order.lock().unwrap(),
        crate::shutdown::ORDER.to_vec(),
        "the stop order is the window, then the agent, then the model server"
    );
    assert!(staged.lock().unwrap().is_empty(), "every child was handed to a stop step");
}

// ---------------------------------------------------------------------------
// the model server: one restart, with the backoff actually waited for
// ---------------------------------------------------------------------------

fn echo_child() -> Option<Box<dyn ModelProcess>> {
    if !Path::new("/bin/echo").is_file() {
        return None;
    }
    let child = Command::new("/bin/echo")
        .arg("atlas")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    Some(Box::new(supervise::SpawnedChild::new(child)))
}

#[test]
fn a_real_child_that_dies_is_restarted_once_after_a_real_backoff() {
    let first = match echo_child() {
        Some(child) => child,
        None => return,
    };
    let spawns = Arc::new(AtomicU32::new(0));
    let counter = spawns.clone();
    let (_stop_sender, stop) = mpsc::channel::<()>();
    let mut events: Vec<supervise::Event> = Vec::new();
    let liveness = supervise::Liveness::new();
    let started = Instant::now();
    let outcome = supervise::run(
        first,
        &Policy {
            max_restarts: 1,
            backoff: Duration::from_millis(60),
            poll: Duration::from_millis(5),
        },
        Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            echo_child().ok_or_else(|| "nothing to spawn".to_string())
        }),
        &stop,
        &mut |event| events.push(event),
        &liveness,
    );
    let elapsed = started.elapsed();
    assert_eq!(outcome, Outcome::GaveUp { restarts: 1 });
    assert!(!liveness.alive(), "the boot sequence stops waiting for the model");
    assert_eq!(spawns.load(Ordering::SeqCst), 1);
    assert!(
        elapsed >= Duration::from_millis(60),
        "the backoff was not waited for: {:?}",
        elapsed
    );
    // The exit status of a real child reaches the report.
    let died = events
        .iter()
        .find(|event| matches!(event, supervise::Event::Died { .. }))
        .expect("the death was reported");
    if let supervise::Event::Died { status } = died {
        assert!(status.contains("exited"), "{}", status);
    }
}

// ---------------------------------------------------------------------------
// the arguments reach the modules they name
// ---------------------------------------------------------------------------

#[test]
fn the_parsed_start_options_carry_the_root_and_the_two_switches() {
    let root = fresh_root("options");
    let command = crate::args::parse(&words(&[
        "start",
        "--root",
        root.to_str().unwrap(),
        "--dry-run",
        "--no-install",
    ]))
    .unwrap();
    match command {
        CliCommand::Start(StartOptions {
            root: parsed,
            dry_run,
            no_install,
        }) => {
            assert_eq!(parsed.as_deref(), Some(root.as_path()));
            assert!(dry_run, "--dry-run reaches the boot");
            // The switch that can turn the one download off has to survive parsing: a
            // flag that stops at `args` is a flag that starts a download anyway.
            assert!(no_install, "--no-install reaches the boot");
        }
        other => panic!("expected Start, got {:?}", other),
    }
    // …and its absence is the double-click default: provision.
    let plain = crate::args::parse(&words(&["start"])).unwrap();
    match plain {
        CliCommand::Start(options) => assert!(!options.no_install && !options.dry_run),
        other => panic!("expected Start, got {:?}", other),
    }
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// the spawn boundary, audited in the shipped source
// ---------------------------------------------------------------------------

/// The shipped part of one `src/*.rs` file of this crate: everything above its
/// `#[cfg(test)]` blocks, with the test module's own literals left out.
///
/// Only files whose name is a Rust module name are read. A copy somebody left in
/// `src/` as `start 2.rs` is not compiled and is not this crate's code, and reading
/// it would make this audit answer a question about a file no build ever sees.
fn is_module_name(stem: &str) -> bool {
    !stem.is_empty()
        && stem
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

fn shipped_sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, prefix: &str, files: &mut Vec<(String, String)>) {
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
                // `tests/` is test code by its directory (§`atlas-guard::source`).
                if name != "tests" && is_module_name(&name) {
                    walk(&path, &format!("{}{}/", prefix, name), files);
                }
                continue;
            }
            let stem = match name.strip_suffix(".rs") {
                Some(stem) => stem,
                None => continue,
            };
            // `tests.rs`, `tests_*.rs`, `*_tests.rs` and `*_tests/` are test code by
            // their name, exactly as the workspace's own source scanner decides it.
            if !is_module_name(stem) || stem == "tests" || stem.ends_with("_tests") {
                continue;
            }
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(_) => continue,
            };
            let code = match text.find("#[cfg(test)]") {
                Some(at) => text[..at].to_string(),
                None => text,
            };
            files.push((format!("{}{}", prefix, name), code));
        }
    }
    let mut files: Vec<(String, String)> = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        "",
        &mut files,
    );
    files.sort();
    files
}

/// The name of the function a position sits in, so an allowed exception can name
/// the one place it is allowed.
fn enclosing_function(code: &str, at: usize) -> String {
    let mut name = String::from("?");
    for line in code[..at].lines() {
        let text = line.trim_start();
        if let Some(rest) = text
            .strip_prefix("pub fn ")
            .or_else(|| text.strip_prefix("fn "))
            .or_else(|| text.strip_prefix("pub(crate) fn "))
        {
            name = rest
                .split(|ch: char| ch == '(' || ch.is_whitespace())
                .next()
                .unwrap_or("?")
                .to_string();
        }
    }
    name
}

#[test]
fn no_child_is_started_through_a_shell_and_none_inherits_the_environment() {
    // Two promises this crate makes in prose (`child.rs`, `lib.rs`) that only hold
    // if the shipped source keeps them: argv is a list and never a line a shell
    // parses, and no child sees the caller's environment. Measured before this test:
    // the crate's only spawn sites were `child::capture`, `child::politely`,
    // `llama::spawn` and `opener::open`, all of them `env_clear` + allowlist. This
    // test is what keeps the next spawn site from being written without them.
    let sources = shipped_sources();
    assert!(!sources.is_empty(), "no source files were read");
    for (name, code) in &sources {
        for needle in [
            "sh -c",
            "/bin/sh",
            "/bin/bash",
            "bash -c",
            "cmd /c",
            "cmd.exe",
            "powershell",
            "ShellExecute",
        ] {
            assert!(
                !code.contains(needle),
                "{} names a shell ({:?}) — argv must be a list, not a line",
                name,
                needle
            );
        }
        let mut at = 0;
        while let Some(found) = code[at..].find("Command::new(") {
            let start = at + found;
            // One statement of context: `Command::new(...)` through the next `;`
            // that closes the builder chain, capped so the window stays local.
            let window: &str = &code[start..(start + 900).min(code.len())];
            let statement = match window.find(".spawn()") {
                Some(end) => &window[..end],
                None => window,
            };
            let function = enclosing_function(code, start);
            assert!(
                statement.contains(".env_clear()"),
                "{}::{} spawns without `env_clear` — a child must not inherit this process's \
                 environment:\n{}",
                name,
                function,
                statement
            );
            // The polite signal is the one exception, and it is a program that needs
            // no environment at all (`kill -TERM <pid>`, reaped immediately). Every
            // other spawn adds the allowlist.
            if function != "politely" {
                assert!(
                    statement.contains("spawn_env"),
                    "{}::{} spawns without `atlas_config::spawn_env` — the allowlist is the \
                     only environment a child may see:\n{}",
                    name,
                    function,
                    statement
                );
            }
            at = start + "Command::new(".len();
        }
        // The other shape: a child built by `atlas_platform::child`'s boundary. That
        // helper does `env_clear` itself (and, on Windows, `CREATE_NO_WINDOW`), so the
        // half this crate still decides is *which* environment it is handed — and the
        // answer has to be `atlas-config`'s allowlist, never `std::env::vars()`.
        for spelling in ["child::command(", "child::command_with("] {
            let mut at = 0;
            while let Some(found) = code[at..].find(spelling) {
                let start = at + found;
                let window: &str = &code[start..(start + 400).min(code.len())];
                let statement = match window.find(".spawn()") {
                    Some(end) => &window[..end],
                    None => window,
                };
                // `child::command(program)` carries no environment of its own: the
                // boundary supplies the platform allowlist, which is the whole point.
                if statement.contains("command_with(") {
                    assert!(
                        statement.contains("spawn_env")
                            || statement.contains("child::environment"),
                        "{}::{} builds a child with an environment that is not \
                         `atlas_config::spawn_env` — the allowlist is the only environment a \
                         child may see:\n{}",
                        name,
                        enclosing_function(code, start),
                        statement
                    );
                }
                at = start + spelling.len();
            }
        }
    }
}

#[test]
fn the_window_the_agent_and_the_install_child_go_through_the_platform_boundary() {
    // The environment rule survived either way, so nothing noticed when these three built
    // their `Command` by hand: `env_clear` + `atlas_config::spawn_env` still kept the
    // shell out. What they skipped is `atlas_platform::child`'s `hide_console` — the
    // `CREATE_NO_WINDOW` flag — and this is the one process in ATLAS that is started from
    // a GUI (the double-clicked launcher), so on Windows each of its children flashed a
    // console window for a moment. `child.rs` (atlas-platform) says every spawn site
    // builds its command there; this test is what makes that true for the sites that
    // start a *long-lived* child, and it names the failure a reader can act on.
    let sources = shipped_sources();
    for name in ["window.rs", "agent.rs", "setup.rs"] {
        let code = sources
            .iter()
            .find(|(file, _)| file == name)
            .map(|(_, code)| code.clone())
            .unwrap_or_else(|| panic!("{} is read", name));
        assert!(
            code.contains("atlas_platform::child::command"),
            "{} starts a child without the platform boundary — it must use \
             `atlas_platform::child::command` (or `command_with`), which is the only place \
             `env_clear` and the Windows `CREATE_NO_WINDOW` flag live",
            name
        );
        // The needle is built at run time on purpose: this file is the audit, and a
        // literal `Command::new(` here would be counted as a spawn site by the scan
        // below (`shipped_sources` reads this file's source, not its intent).
        let hand_built = ["Command", "::new("].concat();
        assert!(
            !code.contains(&hand_built),
            "{} builds its own `Command` again: the environment would still be curated, but \
             a GUI parent's child would flash a console window on Windows",
            name
        );
    }
}

#[test]
fn the_audit_itself_can_see_the_spawn_sites_it_judges() {
    // A scanner that reads nothing passes every assertion above. This pins that it
    // sees the real spawn sites — in both shapes this crate uses: `Command::new`
    // directly, and `atlas_platform::child::command`, the workspace's boundary — and
    // the functions they are in.
    let sources = shipped_sources();
    let mut found: Vec<String> = Vec::new();
    for (name, code) in &sources {
        for needle in ["Command::new(", "atlas_platform::child::command"] {
            let mut at = 0;
            while let Some(site) = code[at..].find(needle) {
                let start = at + site;
                found.push(format!("{}::{}", name, enclosing_function(code, start)));
                at = start + 1;
            }
        }
    }
    for expected in [
        "child.rs::capture",
        "child.rs::politely",
        "llama.rs::spawn",
        "window.rs::launch",
        "agent.rs::launch",
        "setup.rs::launch",
    ] {
        assert!(
            found.iter().any(|site| site == expected),
            "the audit did not find {}: {:?}",
            expected,
            found
        );
    }
    // The walk reaches the modules below `src/` too (`doctor/`), and it does not
    // read a stray copy that no build compiles — a file named `start 2.rs` in `src/`
    // is somebody's artefact, not this crate's code.
    for expected in [
        "lock.rs",
        "supervise.rs",
        "silence.rs",
        "doctor/mod.rs",
        "doctor/checks.rs",
    ] {
        assert!(
            sources.iter().any(|(name, _)| name == expected),
            "the audit did not read {}: {:?}",
            expected,
            sources.iter().map(|(name, _)| name).collect::<Vec<_>>()
        );
    }
    assert!(
        !sources.iter().any(|(name, _)| name.contains(' ')),
        "the audit read a file whose name is not a module name"
    );
}

// ---------------------------------------------------------------------------
// a stop during the boot: nothing is served, the child is still reaped
// ---------------------------------------------------------------------------

/// A child that never exits on its own, and counts how often it was stopped.
struct Watched {
    stops: Arc<AtomicU32>,
}

impl ModelProcess for Watched {
    fn try_wait(&mut self) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn stop(&mut self) -> String {
        self.stops.fetch_add(1, Ordering::SeqCst);
        "stopped".to_string()
    }
}

#[test]
fn a_stop_during_the_boot_stops_the_model_child_in_the_declared_order() {
    // The `SIGTERM`-while-the-weights-load case, through the shutdown the binary
    // really installs: the window first (nothing of it may be left on screen), then the
    // model child this process spawned. The model step is the one that must never be
    // skipped — a skipped step is a child left behind.
    let stops = Arc::new(AtomicU32::new(0));
    let (handle, liveness) = supervise::start(
        Box::new(Watched {
            stops: stops.clone(),
        }),
        Policy {
            max_restarts: 1,
            backoff: Duration::from_millis(10),
            poll: Duration::from_millis(2),
        },
        Box::new(|| Err("never".to_string())),
        Arc::new(crate::report::Printer::new()),
    );
    assert!(liveness.alive(), "a running child is alive");

    let order: Arc<Mutex<Vec<Step>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = |step: Step| {
        let order = order.clone();
        move || order.lock().unwrap().push(step)
    };
    let performed = crate::start::plan(
        recorded(Step::Window),
        Arc::new(crate::report::Printer::new()),
        None,
        Some(handle),
    )
    .run();

    assert_eq!(performed, crate::shutdown::ORDER.to_vec());
    assert_eq!(
        *order.lock().unwrap(),
        vec![Step::Window],
        "the window closes before the agent and the model; the model step is the real one, \
         so it records itself in the stop counter below instead"
    );
    assert_eq!(stops.load(Ordering::SeqCst), 1, "the model child was stopped once");
    assert!(!liveness.alive(), "the supervisor ends when it is stopped");
}

#[test]
fn a_plan_without_a_model_child_is_still_ordered_and_stops_nothing() {
    // The "no model server" boots (missing binary, adopted server): the plan must still
    // perform both steps, in order, and asking it to stop a child it does not have is
    // not a failure.
    let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let opened = order.clone();
    let performed = crate::start::plan(
        move || opened.lock().unwrap().push("window"),
        Arc::new(crate::report::Printer::new()),
        None,
        None,
    )
    .run();
    assert_eq!(performed, crate::shutdown::ORDER.to_vec());
    assert_eq!(*order.lock().unwrap(), vec!["window"]);
}

// ---------------------------------------------------------------------------
// the root: found from the binary, and absolute once it is known
// ---------------------------------------------------------------------------

#[test]
fn the_project_root_comes_from_the_binary_not_from_the_working_directory() {
    // `atlas start` with no `--root` has to find the project it was built into,
    // even when it is started from somewhere else (a shortcut, `atlas` on PATH,
    // another project's shell). The rule lives in `atlas-platform` and is found
    // from the running binary; this pins that the launcher asks it and does not
    // fall back to whatever directory the user happened to be in.
    let discovered = crate::home::paths(&None);
    assert_eq!(discovered.root, atlas_platform::root::app_root());
    assert!(
        discovered.root.join("rust").join("Cargo.toml").is_file(),
        "the root discovery lost the workspace: {}",
        discovered.root.display()
    );
    assert!(
        atlas_platform::root::looks_like_app_root(&discovered.root),
        "{} does not look like the project",
        discovered.root.display()
    );
    // And a relative `--root` becomes a directory a message can print.
    let named = crate::home::paths(&Some(PathBuf::from("some/where")));
    assert!(named.root.is_absolute(), "{:?}", named.root);
    assert_eq!(named.data_dir, named.root.join("data"));
}

// ---------------------------------------------------------------------------
// (1) the launch sites: enumerated, pinned, and bound to loopback
// ---------------------------------------------------------------------------

/// The shipped code of a file with its comment lines dropped.
///
/// A doc comment that *names* a wildcard address ("never `0.0.0.0`") is prose, not a
/// bind, and the audit below must judge what the code does.
fn code_without_comments(code: &str) -> String {
    code.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<&str>>()
        .join("\n")
}

#[test]
fn the_processes_this_crate_starts_are_exactly_these_and_none_binds_a_wildcard() {
    // The brief's first question — "enumerate the process-launch sites in shipped
    // code" — answered by a test, so the answer cannot rot. Measured when this was
    // written: four sites, all of them in this crate, all spawned through a
    // `Command` (never a shell), all `env_clear` + the `atlas_config::spawn_env`
    // allowlist (pinned by `no_child_is_started_through_a_shell_and_none_inherits_
    // the_environment` above).
    //
    // * `child::capture`       — a `--version`/`--help` probe, bounded by a deadline
    // * `child::politely`      — `kill -TERM <pid>` for a pid from our own `Child`
    // * `llama::spawn`         — `llama-server`, the model server
    // * `window::Window::launch` — `atlas-desktop`, the product's own window
    // * `agent::Agent::launch`  — `atlas-monitor serve`, the always-on half
    // * `setup::launch`        — `atlas-setup --install`, the one-time provisioning
    //
    // The fourth site used to be `opener::open`, which asked the desktop to open a
    // loopback URL in the user's browser. The product is the window and no address is
    // promised, so that module is gone and the window replaced it: a different
    // program, and the only one this crate starts that shows the user anything.
    //
    // The last two sites build their command with `atlas_platform::child::command`,
    // the workspace's own boundary (`env_clear` + the allowlist), instead of spelling
    // `Command::new` here — so this scan looks for both shapes. A new spawn site is a
    // decision somebody has to make here, in this list, with the loopback and
    // environment rules next to it. The workspace-wide rule that a new spawn anywhere
    // is a fetch is `atlas-guard`'s (`audit_downloads.rs`).
    let sources = shipped_sources();
    let mut sites: Vec<String> = Vec::new();
    for (name, code) in &sources {
        for needle in ["Command::new(", "atlas_platform::child::command"] {
            let mut at = 0;
            while let Some(site) = code[at..].find(needle) {
                let start = at + site;
                sites.push(format!("{}::{}", name, enclosing_function(code, start)));
                at = start + 1;
            }
        }
    }
    sites.sort();
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
        "the set of process-launch sites changed: {:?}",
        sites
    );

    for (name, code) in &sources {
        let code = code_without_comments(code);
        // A child told to bind a wildcard address is a listener on every interface:
        // the model server must be on the loopback literal, always.
        for needle in ["0.0.0.0", "[::]:", "::0"] {
            assert!(
                !code.contains(needle),
                "{} names a wildcard address ({:?})",
                name,
                needle
            );
        }
        // And no binary is fetched at run time: the launcher has no downloader, and
        // `llama::binary` is the only chooser (a pinned `bin/`, then PATH).
        for needle in ["curl", "wget", "reqwest", "ureq", "rustls"] {
            assert!(
                !code.contains(needle),
                "{} names a downloader ({:?}) — the model server is built or installed, \
                 never fetched",
                name,
                needle
            );
        }
    }

    // The model server's program is the *resolved* binary and its argv is the
    // invocation: nothing from a config value reaches the program name, and the host
    // is the constant, not a setting.
    let llama = sources
        .iter()
        .find(|(name, _)| name == "llama.rs")
        .map(|(_, code)| code.clone())
        .expect("llama.rs is read");
    let llama_code = code_without_comments(&llama);
    assert!(
        llama_code.contains("Command::new(&invocation.program)"),
        "the model server must be started from the invocation's resolved program"
    );
    assert!(
        llama_code.contains("pub const HOST: &str = \"127.0.0.1\";"),
        "the loopback literal is a constant this audit can find"
    );
}

// ---------------------------------------------------------------------------
// (3) nothing is still listening after the model server is stopped
// ---------------------------------------------------------------------------

/// The marker line the child prints once it is bound.
const LISTENING_ON: &str = "ATLAS-LISTENING-ON ";

/// The child that listens: this test binary, run again with one test selected.
///
/// A stand-in for `llama-server` that needs no model, no build and no download, and
/// that really holds a loopback socket — the only way to prove "nothing is listening
/// after the stop" from the outside. It is `#[ignore]`d so a plain `cargo test` never
/// runs it: the tests below start it, with `--ignored --exact`.
#[test]
#[ignore = "the listening child is started by the tests below, never on its own"]
fn the_listening_child() {
    // Only when a test started this binary for that purpose. `cargo test
    // --include-ignored` would otherwise leave a two-minute child holding a port,
    // which is exactly the thing these tests exist to rule out.
    if std::env::var("ATLAS_TEST_LISTENER").is_err() {
        return;
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its own address").port();
    // Printed only after the bind: a parent that reads the line knows the socket is
    // held, so "the port is free afterwards" cannot pass for a child that never bound.
    println!("{}{}", LISTENING_ON, port);
    let _ = std::io::Write::flush(&mut std::io::stdout());
    // Hold it until the supervisor stops us.
    std::thread::sleep(Duration::from_secs(120));
}

/// A listening child, with a guard that stops it however the test ends.
struct Listening {
    child: Option<Child>,
    port: u16,
}

impl Listening {
    /// Take the child out, so it can be handed to the supervisor under test.
    fn hand_over(&mut self) -> Child {
        self.child.take().expect("the child is still here")
    }
}

impl Drop for Listening {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Start the listening child and read the port it holds.
fn listening_child() -> Option<Listening> {
    let exe = std::env::current_exe().ok()?;
    let mut command = Command::new(&exe);
    command
        .args([
            "--exact",
            "tests::the_listening_child",
            "--ignored",
            "--nocapture",
        ])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (name, value) in atlas_config::spawn_env(&[(
        "ATLAS_TEST_LISTENER".to_string(),
        "1".to_string(),
    )]) {
        command.env(name, value);
    }
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let mut reader = std::io::BufReader::new(stdout);
    let mut port: Option<u16> = None;
    for _ in 0..50 {
        let mut line = String::new();
        if std::io::BufRead::read_line(&mut reader, &mut line).ok()? == 0 {
            break;
        }
        if let Some(rest) = line.trim().strip_prefix(LISTENING_ON) {
            port = rest.trim().parse::<u16>().ok();
            break;
        }
    }
    let port = match port {
        Some(port) => port,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    Some(Listening {
        child: Some(child),
        port,
    })
}

/// One attempt: is something holding this loopback port right now?
///
/// The positive control of the two tests below. It is a single attempt on purpose —
/// a retry loop here would turn "still listening" into "still listening after five
/// seconds", which is not the same claim.
fn taken(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_err()
}

/// Wait (briefly) for a port to be bindable: is anything still listening on it?
fn bindable(port: u16) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match std::net::TcpListener::bind(("127.0.0.1", port)) {
            Ok(_) => return true,
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return false,
        }
    }
}

#[test]
fn nothing_listens_on_the_models_port_after_the_supervisor_stops_it() {
    // The brief's third question, measured with a real listener: a child that holds
    // `127.0.0.1:<port>` is watched by the shipped supervisor, stopped the way a
    // closing window stops it, and the port is bindable again — and the process is
    // gone from the table, not a zombie.
    let Some(mut listening) = listening_child() else {
        return;
    };
    let port = listening.port;
    // The positive control: while it runs, the port is taken. Without this the rest
    // would also pass for a child that never bound anything.
    assert!(
        taken(port),
        "nothing was listening on 127.0.0.1:{} while the child ran",
        port
    );

    let child = listening.hand_over();
    let pid = child.id();
    let (mut handle, liveness) = supervise::start(
        Box::new(supervise::SpawnedChild::new(child)),
        Policy::default(),
        Box::new(|| Err("the listener is not restarted here".to_string())),
        Arc::new(crate::report::Printer::new()),
    );
    assert!(liveness.alive(), "a running child is alive");
    assert_eq!(handle.stop(), Some(Outcome::Stopped));

    assert!(
        bindable(port),
        "something is still listening on 127.0.0.1:{} after the stop",
        port
    );
    if ps_program().is_some() {
        assert_eq!(
            wait_for_no_entry(pid),
            None,
            "pid {} is still in the process table after the stop",
            pid
        );
    }
}

#[test]
fn dropping_the_handle_stops_the_child_so_a_closed_window_leaves_nothing_listening() {
    // The path that has no explicit stop: the handle is dropped (the window closed,
    // the boot's `?` took an early exit, an unwind). `Drop for Handle` stops the
    // child, so a listener cannot outlive the app that started it.
    let Some(mut listening) = listening_child() else {
        return;
    };
    let port = listening.port;
    assert!(taken(port), "nothing was listening on 127.0.0.1:{}", port);
    let child = listening.hand_over();
    let pid = child.id();
    {
        let (handle, _liveness) = supervise::start(
            Box::new(supervise::SpawnedChild::new(child)),
            Policy::default(),
            Box::new(|| Err("the listener is not restarted here".to_string())),
            Arc::new(crate::report::Printer::new()),
        );
        assert!(handle.running());
        drop(handle);
    }
    assert!(
        bindable(port),
        "something is still listening on 127.0.0.1:{} after the handle was dropped",
        port
    );
    if ps_program().is_some() {
        assert_eq!(wait_for_no_entry(pid), None, "pid {} survived the drop", pid);
    }
}
