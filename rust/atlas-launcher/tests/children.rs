//! The children this launcher starts, measured from **outside** the crate.
//!
//! `atlas start` starts three children — the window (`atlas-desktop`), the agent
//! (`atlas-monitor serve`) and the model server (`llama-server`) — and, on an
//! unprovisioned machine, a fourth (`atlas-setup --install`). This file does not
//! re-test the modules' own units; it drives the *public* boundaries
//! (`window::Window::launch`, `agent::Agent::launch`, `llama::spawn`,
//! `supervise::{start,run}`, `start::plan`, `shutdown`, `setup::*`) with real
//! processes and measures what the operating system says about them.
//!
//! Four claims, each one a regression somebody could introduce by hand:
//!
//! 1. **A child that dies is reported once and is not respawned in a loop.** The
//!    supervisor's event stream and its spawn count are both measured, and the loop is
//!    watched *after* it returns, so "it stopped" is a fact and not an assumption.
//! 2. **A stop stops the children in [`shutdown::ORDER`]**: window, agent, model
//!    server. Measured with `ps`: what is still running at each step, and what is gone
//!    when the plan returns.
//! 3. **The provisioning child runs at most once per start, and never when the pieces
//!    are already present** — measured on disk (`setup::Present::scan`), in the plan a
//!    person reads (`plan::plan_lines`), and in the shipped source (one `launch` call
//!    in `setup::run`, called from one place in the boot).
//! 4. **No child inherits a listening socket.** The test process holds a loopback
//!    listener; every child it starts through the launcher's boundaries reports its own
//!    open descriptors with `lsof`, and the report must contain no socket. A control
//!    child opens a connection itself, so "no socket" cannot mean "the probe is blind".
//!
//! Everything a child here runs is a Rust program compiled at test time
//! ([`STAND_IN`]) or a plain POSIX tool (`ps`, `lsof`, `/bin/echo`, `/bin/sleep`). No
//! shell, no script, no network: the one socket in this file is a `127.0.0.1:0`
//! listener inside the test process, and the control connects to it and nothing else.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::{Duration, Instant};

use atlas_launcher::supervise::{self, Event, Outcome, Policy, SpawnedChild};
use atlas_launcher::{agent, llama, plan, report::Printer, setup, signals::Stop, start, window};

// ---------------------------------------------------------------------------
// the stand-in child: one Rust program, every boundary
// ---------------------------------------------------------------------------

/// The program every stand-in child runs.
///
/// It is written to the temp directory and compiled with the machine's own `rustc`, so
/// a child in this file is a real Rust process with a real descriptor table — not a
/// shell script, and not a copy of `/bin/echo` pretending to be a window. It does three
/// things, in this order, and then holds still until it is stopped:
///
/// * appends one line per run to `data/ran.txt` — the count of lines is the count of
///   children that started, which is how "at most once" is measured;
/// * writes its own open descriptors (`lsof -p <pid>`) and the internet sockets among
///   them (`lsof -p <pid> -a -i`) to `data/fds-<name>.txt`, so an inherited listener
///   would be *in the file* rather than in an argument;
/// * follows `data/standin-mode`: `sleep` (the default) holds still like a window or an
///   agent does, `connect <port>` opens one loopback connection first, which is the
///   control that proves the descriptor probe can see a socket at all.
const STAND_IN: &str = r####"
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut root = std::env::current_dir().unwrap_or_default();
    if let Some(at) = args.iter().position(|arg| arg == "--root") {
        if let Some(value) = args.get(at + 1) {
            root = std::path::PathBuf::from(value);
        }
    }
    let data = root.join("data");
    let _ = std::fs::create_dir_all(&data);
    let name = std::env::current_exe()
        .ok()
        .and_then(|path| path.file_name().map(|name| name.to_string_lossy().to_string()))
        .unwrap_or_else(|| "stand-in".to_string());
    let pid = std::process::id();

    if let Ok(mut runs) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data.join("ran.txt"))
    {
        // One `write_all` of a whole line: three stand-ins append to this file at the same
        // time, and `writeln!` would write the name, the pid and the newline separately, so
        // two children could interleave inside one line. With `O_APPEND`, one write of a
        // short buffer is one atomic append.
        let line = format!("{} {}\n", name, pid);
        let _ = runs.write_all(line.as_bytes());
    }

    let report = |suffix: &str| {
        let all = lsof(&["-p", &pid.to_string()]);
        let net = lsof(&["-p", &pid.to_string(), "-a", "-i"]);
        let _ = std::fs::write(
            data.join(format!("{}-{}.txt", suffix, name)),
            format!("ALL\n{}\nNET\n{}\n", all.trim_end(), net.trim_end()),
        );
    };
    report("fds");

    let mode_text = std::fs::read_to_string(data.join("standin-mode")).unwrap_or_default();
    let mut lines = mode_text.lines().map(str::trim);
    let mode = lines.next().unwrap_or("sleep").to_string();
    if let Some(port) = mode.strip_prefix("connect ") {
        let port: u16 = port.trim().parse().unwrap_or(0);
        if let Ok(stream) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            // The same report again, now with a socket this process opened itself.
            report("connected");
            std::thread::sleep(std::time::Duration::from_millis(1500));
            drop(stream);
        }
    }
    match mode.as_str() {
        // The stand-in install tool, doing what the mode file says: place the pieces the
        // following lines name, then exit 0 like a finished download.
        "install" => {
            for path in lines {
                if path.is_empty() {
                    continue;
                }
                let path = std::path::PathBuf::from(path);
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if path.file_name().map(|name| name.to_string_lossy().contains("llama-server"))
                    == Some(true)
                {
                    let _ = std::fs::copy("/bin/echo", &path);
                    set_executable(&path);
                } else {
                    let _ = std::fs::write(&path, vec![7u8; 4096]);
                }
            }
            return;
        }
        // A child that exits 0 and places nothing: the "installer said fine" case.
        "nothing" => return,
        // The default: hold still, like a window, an agent or a model server.
        _ => {}
    }

    // Hold still until the launcher stops us: that is what a window and an agent do,
    // and it is what lets the parent look at this process while it runs.
    std::thread::sleep(std::time::Duration::from_secs(60));
}

fn set_executable(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(path) {
            let mut mode = metadata.permissions();
            mode.set_mode(0o755);
            let _ = std::fs::set_permissions(path, mode);
        }
    }
}

fn lsof(args: &[&str]) -> String {
    for program in ["/usr/sbin/lsof", "/usr/bin/lsof", "/sbin/lsof"] {
        if !std::path::Path::new(program).is_file() {
            continue;
        }
        let output = std::process::Command::new(program)
            .args(args)
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output();
        if let Ok(output) = output {
            return String::from_utf8_lossy(&output.stdout).to_string();
        }
    }
    "lsof-missing".to_string()
}
"####;

/// The compiled stand-in, once per test process: `rustc` here, then copied into each
/// test's own root under the name that boundary resolves.
fn stand_in() -> Option<PathBuf> {
    static BUILT: OnceLock<Option<PathBuf>> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let rustc = atlas_launcher::toolchain::locate("RUSTC", "rustc")?;
            let dir = std::env::temp_dir().join(format!("atlas-stand-in-{}", std::process::id()));
            std::fs::create_dir_all(&dir).ok()?;
            let source = dir.join("standin.rs");
            std::fs::write(&source, STAND_IN).ok()?;
            let program = dir.join("standin");
            // The environment is *not* cleared here, unlike a child of the product:
            // `rustc` is this test's own tool and needs `PATH` for the linker and the
            // platform's SDK variables to build anything at all.
            let built = Command::new(rustc)
                .arg(&source)
                .arg("-o")
                .arg(&program)
                .arg("--edition=2021")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .ok()?;
            if !built.status.success() || !program.is_file() {
                eprintln!(
                    "the stand-in did not compile:\n{}",
                    String::from_utf8_lossy(&built.stderr)
                );
                return None;
            }
            Some(program)
        })
        .clone()
}

// ---------------------------------------------------------------------------
// small things: a temp root, `ps`, a place to put a child
// ---------------------------------------------------------------------------

/// A temp project root with the directories the boot expects; removed on drop.
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
            "atlas-children-{}-{}-{}",
            tag,
            std::process::id(),
            stamp
        ));
        let _ = std::fs::remove_dir_all(&path);
        for sub in ["data", "models", "bin", "rust/target/release"] {
            let _ = std::fs::create_dir_all(path.join(sub));
        }
        Root { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn release(&self) -> PathBuf {
        self.path.join("rust").join("target").join("release")
    }

    /// Put the stand-in where a boundary resolves it: the root's own release directory
    /// under the name the product looks for.
    fn stand_in_as(&self, name: &str) -> Option<PathBuf> {
        let from = stand_in()?;
        let to = atlas_platform_binary(&self.release(), name);
        std::fs::create_dir_all(self.release()).ok()?;
        std::fs::copy(&from, &to).ok()?;
        make_executable(&to)?;
        Some(to)
    }

    /// What this root's stand-ins do when they start.
    fn mode(&self, mode: &str) {
        std::fs::write(self.path.join("data").join("standin-mode"), mode).expect("the mode");
    }

    /// Wait until `expected` stand-ins have written their run line.
    ///
    /// A child that is `fork`ed is not yet a child that has run its first statement, and a
    /// test that stops it in that window measures the race, not the shutdown. Everything a
    /// stop-order test asserts waits for this first.
    fn wait_for_runs(&self, expected: usize, within: Duration) -> Vec<String> {
        let deadline = Instant::now() + within;
        loop {
            let runs = self.runs();
            if runs.len() >= expected || Instant::now() >= deadline {
                return runs;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// One line per stand-in run: how many children actually started.
    fn runs(&self) -> Vec<String> {
        std::fs::read_to_string(self.path.join("data").join("ran.txt"))
            .map(|text| {
                text.lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn atlas_platform_binary(dir: &Path, name: &str) -> PathBuf {
    if cfg!(windows) {
        dir.join(format!("{}.exe", name))
    } else {
        dir.join(name)
    }
}

fn make_executable(path: &Path) -> Option<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut mode = std::fs::metadata(path).ok()?.permissions();
        mode.set_mode(0o755);
        std::fs::set_permissions(path, mode).ok()?;
    }
    Some(())
}

/// `ps(1)`, where this platform keeps it.
fn ps_program() -> Option<&'static str> {
    ["/bin/ps", "/usr/bin/ps"]
        .into_iter()
        .find(|program| Path::new(program).is_file())
}

/// The process-table state of a pid, as `ps` prints it.
///
/// `Some("Z…")` is a zombie (signalled, not reaped); `Some("S…")` is running or
/// sleeping; `None` is "the operating system has no entry left", which is what a stopped
/// child must look like. Only pids this file started are ever named.
fn state(pid: u32) -> Option<String> {
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

fn running(pid: u32) -> bool {
    matches!(state(pid), Some(text) if !text.starts_with('Z'))
}

fn wait_gone(pid: u32, within: Duration) -> Option<String> {
    let deadline = Instant::now() + within;
    let mut last = state(pid);
    while Instant::now() < deadline && last.is_some() {
        std::thread::sleep(Duration::from_millis(10));
        last = state(pid);
    }
    last
}

fn wait_running(pid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if running(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

/// Wait for one of the stand-in's report files and return its text.
///
/// The panic names the file and lists the directory it should be in: "the child wrote no
/// report" without the listing is a failure nobody can act on.
fn wait_for_report(path: &Path, within: Duration) -> Option<String> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(path) {
            if text.contains("NET") {
                return Some(text);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let listing: Vec<String> = path
        .parent()
        .and_then(|dir| std::fs::read_dir(dir).ok())
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| format!("{:?}", entry.file_name()))
                .collect()
        })
        .unwrap_or_default();
    panic!("no report at {:?}; the directory holds {:?}", path, listing);
}

/// A port on loopback nothing is using: the kernel hands one out and it is released.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("a free loopback port")
        .local_addr()
        .expect("its own address")
        .port()
}

/// A real child that exits at once, for the supervisor's "it died" cases.
fn exits_at_once() -> Option<Child> {
    let program = Path::new("/bin/echo");
    if !program.is_file() {
        return None;
    }
    Command::new(program)
        .arg("a model server that could not load its weights")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

// ---------------------------------------------------------------------------
// (1) a death is reported once, and there is no respawn loop
// ---------------------------------------------------------------------------

#[test]
fn a_crash_loop_is_reported_once_per_event_and_restarted_exactly_once() {
    // The model server that dies on every start: the shape of a crash loop. Measured
    // here are the two things a loop would break — the number of children started and
    // the number of lines a person reads — and then the loop is watched *after* it
    // returned, because "it stopped" has to be a fact about the process, not a reading
    // of the code.
    let first = exits_at_once().expect("this machine has /bin/echo and can start it");
    let (sender, receiver) = mpsc::channel::<()>();
    let spawns = Arc::new(AtomicU32::new(0));
    let counter = spawns.clone();
    let policy = Policy {
        max_restarts: 1,
        backoff: Duration::from_millis(120),
        poll: Duration::from_millis(5),
    };
    let mut events: Vec<Event> = Vec::new();
    let started = Instant::now();
    let outcome = supervise::run(
        Box::new(SpawnedChild::new(first)),
        &policy,
        Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            let child = exits_at_once().ok_or_else(|| "no /bin/echo on this machine".to_string())?;
            Ok(Box::new(SpawnedChild::new(child)) as Box<dyn supervise::ModelProcess>)
        }),
        &receiver,
        &mut |event| events.push(event),
        &supervise::Liveness::new(),
    );
    let elapsed = started.elapsed();
    // The sender stays alive for the whole run: a dropped stop channel reads as "stop
    // now", which would end the loop before the second death and prove nothing.
    drop(sender);

    assert_eq!(
        outcome,
        Outcome::GaveUp { restarts: 1 },
        "two deaths, one restart, then the supervisor stops: {:?}",
        events
    );
    assert_eq!(
        spawns.load(Ordering::SeqCst),
        1,
        "a crash loop must cost one restart, not a flock: {:?}",
        events
    );
    let count = |wanted: fn(&Event) -> bool| events.iter().filter(|event| wanted(event)).count();
    assert_eq!(
        count(|event| matches!(event, Event::Died { .. })),
        1,
        "the first death is reported once, not once per poll: {:?}",
        events
    );
    assert_eq!(
        count(|event| matches!(event, Event::Restarting { .. })),
        1,
        "{:?}",
        events
    );
    assert_eq!(
        count(|event| matches!(event, Event::Restarted { .. })),
        1,
        "{:?}",
        events
    );
    assert_eq!(
        count(|event| matches!(event, Event::GaveUp { .. })),
        1,
        "the give-up is reported once: {:?}",
        events
    );
    assert_eq!(events.len(), 4, "four lines for two deaths: {:?}", events);
    println!(
        "measured: {} death line, {} restart, {} death line and {} give-up; children started: {}",
        count(|event| matches!(event, Event::Died { .. })),
        spawns.load(Ordering::SeqCst),
        count(|event| matches!(event, Event::GaveUp { .. })),
        count(|event| matches!(event, Event::Restarting { .. })),
        spawns.load(Ordering::SeqCst)
    );
    assert!(
        elapsed >= policy.backoff,
        "the restart happened without the backoff: {:?}",
        elapsed
    );

    // The measurement a busy loop would fail: five backoffs of wall clock later, the
    // loop that was supposed to be over is still over.
    std::thread::sleep(policy.backoff * 5);
    assert_eq!(
        spawns.load(Ordering::SeqCst),
        1,
        "something restarted the child after the supervisor gave up"
    );
    assert_eq!(events.len(), 4, "events kept arriving after the give-up");
}

#[test]
fn one_death_is_one_restart_and_the_restarted_child_is_stopped_and_reaped() {
    // The other half: a child that dies once and comes back. The respawned child is
    // real and stays up (`/bin/sleep`), and the stop path has to leave no entry behind —
    // a restarted model server is still a child of this process.
    let first = exits_at_once().expect("this machine has /bin/echo and can start it");
    assert!(
        Path::new("/bin/sleep").is_file(),
        "this machine has /bin/sleep, and the restarted child is a real one"
    );
    let (sender, receiver) = mpsc::channel::<()>();
    let spawns = Arc::new(AtomicU32::new(0));
    let counter = spawns.clone();
    let pids = Arc::new(std::sync::Mutex::new(Vec::<u32>::new()));
    let recorded = pids.clone();
    let policy = Policy {
        max_restarts: 1,
        backoff: Duration::from_millis(60),
        poll: Duration::from_millis(5),
    };
    let mut events: Vec<Event> = Vec::new();
    // The supervisor runs on this thread and blocks in the poll; the stop is sent from
    // another one, like a Ctrl-C arriving while the model serves.
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        let _ = sender.send(());
    });
    let outcome = supervise::run(
        Box::new(SpawnedChild::new(first)),
        &policy,
        Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            let child = Command::new("/bin/sleep")
                .arg("30")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|error| error.to_string())?;
            recorded.lock().expect("the pid list").push(child.id());
            Ok(Box::new(SpawnedChild::new(child)) as Box<dyn supervise::ModelProcess>)
        }),
        &receiver,
        &mut |event| events.push(event),
        &supervise::Liveness::new(),
    );
    stopper.join().expect("the stop thread");

    assert_eq!(outcome, Outcome::Stopped, "{:?}", events);
    assert_eq!(spawns.load(Ordering::SeqCst), 1, "{:?}", events);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Died { .. }))
            .count(),
        1,
        "one death, one line: {:?}",
        events
    );
    let restarted = pids.lock().expect("the pid list").clone();
    assert_eq!(restarted.len(), 1, "one restart, one child");
    if ps_program().is_some() {
        assert_eq!(
            wait_gone(restarted[0], Duration::from_secs(5)),
            None,
            "pid {} is still in the process table after the stop",
            restarted[0]
        );
    }
}

// ---------------------------------------------------------------------------
// (2) a stop stops the children in the documented order
// ---------------------------------------------------------------------------

#[test]
fn a_sigterm_stops_the_window_then_the_agent_then_the_model_server() {
    // The three children the boot starts, at the three boundaries it starts them
    // through: `window::Window::launch`, `agent::Agent::launch` and `llama::spawn` (so
    // the model child is the model server's own spawn path, log sink and all). Each step
    // measures the *others* before it stops its own child, so the order is a fact about
    // running processes rather than about the constant.
    assert!(
        stand_in().is_some(),
        "the stand-in child must compile: cargo built this test with a rustc already"
    );
    assert!(
        ps_program().is_some(),
        "there is no ps(1) on this machine, so the order cannot be measured"
    );
    let root = Root::new("order");
    root.mode("sleep");
    let paths = atlas_config::Paths::at(root.path());
    let printer = Printer::new();
    assert!(
        root.stand_in_as("atlas-desktop").is_some(),
        "the window stand-in"
    );
    assert!(
        root.stand_in_as("atlas-monitor").is_some(),
        "the agent stand-in"
    );
    let model_program = root
        .stand_in_as("llama-server")
        .expect("the model server stand-in");

    let mut window = window::Window::launch(&paths, &printer).expect("the window child starts");
    let agent_child = agent::Agent::launch(&paths, &printer).expect("the agent child starts");
    let agent_pid = agent_child.pid();
    // In a slot, the way the production plan holds it: a step is an `FnMut`, so the child
    // cannot be moved out of the closure's capture, only taken out of a cell.
    let agent = std::sync::Arc::new(std::sync::Mutex::new(Some(agent_child)));
    let invocation = llama::Invocation {
        program: model_program,
        model: paths.model_file.clone(),
        port: free_port(),
        args: vec!["-m".to_string()],
    };
    let model = llama::spawn(&invocation, root.path(), &paths.log_llama)
        .expect("the model child starts");

    let window_pid = window.pid();
    let model_pid = model.pid;
    for pid in [window_pid, agent_pid, model_pid] {
        assert!(
            wait_running(pid, Duration::from_secs(5)),
            "pid {} never started",
            pid
        );
    }
    // A forked child is not yet a child that has run its first statement: everything the
    // stop order is measured on waits until all three have written their run line.
    let runs = root.wait_for_runs(3, Duration::from_secs(5));
    assert_eq!(
        runs.len(),
        3,
        "only {} of the three children started: {:?}",
        runs.len(),
        runs
    );

    // What each step saw, in the order the steps ran: `(step, still running)`.
    let seen: Arc<std::sync::Mutex<Vec<(&'static str, [bool; 3])>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let record = |step: &'static str| {
        let seen = seen.clone();
        move || {
            let snapshot = [running(window_pid), running(agent_pid), running(model_pid)];
            seen.lock().expect("the record").push((step, snapshot));
        }
    };
    // What each stop said it did: `SIGTERM` is the polite signal, and the sentence says
    // which one removed the child.
    let said: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sentence_window = said.clone();
    let sentence_agent = said.clone();
    // The production step bodies, wrapped so each one also records who was still up.
    let before_window = record("window");
    let before_agent = record("agent");
    let before_model = record("model");
    let (handle, liveness) = supervise::start(
        Box::new(model),
        Policy::default(),
        Box::new(|| Err("not restarted in this test".to_string())),
        Arc::new(Printer::new()),
    );
    assert!(liveness.alive(), "the model supervisor is running");
    // `take()` in the step, like the production plan: a step is an `FnMut`, so a handle
    // left in place cannot be moved out of it.
    let mut supervisor = Some(handle);
    let shutdown_started = Instant::now();
    let performed = atlas_launcher::Shutdown::new()
        .add(atlas_launcher::Step::Model, move || {
            before_model();
            if let Some(mut held) = supervisor.take() {
                // The real stop: SIGTERM, the grace, then the kill, and the child reaped.
                let _ = held.stop();
            }
        })
        .add(atlas_launcher::Step::Window, move || {
            before_window();
            let sentence = window.reap();
            sentence_window.lock().expect("the sentences").push(sentence);
        })
        .add(atlas_launcher::Step::Agent, move || {
            before_agent();
            let held = match agent.lock() {
                Ok(mut held) => held.take(),
                Err(poisoned) => poisoned.into_inner().take(),
            };
            if let Some(mut child) = held {
                let sentence = child.stop();
                sentence_agent.lock().expect("the sentences").push(sentence);
            }
        })
        .run();
    let shutdown_span = shutdown_started.elapsed();

    // Three boundaries, three children: no boundary started a second one, and the stop
    // above did not cost a restart.
    assert_eq!(
        root.runs().len(),
        3,
        "a boundary started more than one child: {:?}",
        root.runs()
    );

    // The polite signal, measured: the window and the agent were removed by `SIGTERM`
    // rather than by the kill that follows the grace.
    let said = said.lock().expect("the sentences").clone();
    assert_eq!(said.len(), 2, "two sentences: {:?}", said);
    for sentence in &said {
        assert!(
            sentence.contains("after SIGTERM"),
            "a child was not stopped by SIGTERM: {}",
            sentence
        );
    }
    // And the whole stop was shorter than one grace: had a child ignored `SIGTERM`, its
    // step would have waited the grace out before killing it.
    assert!(
        shutdown_span < Duration::from_secs(3),
        "the stop took {:?}, which is at least the SIGKILL grace",
        shutdown_span
    );

    // The documented order, as performed.
    assert_eq!(
        performed,
        vec![
            atlas_launcher::Step::Window,
            atlas_launcher::Step::Agent,
            atlas_launcher::Step::Model
        ],
        "the shutdown performed a different order"
    );
    let seen = seen.lock().expect("the record").clone();
    println!(
        "measured stop order: {:?}; running before each step (window, agent, model): {:?}",
        seen.iter().map(|(step, _)| *step).collect::<Vec<_>>(),
        seen.iter().map(|(_, live)| *live).collect::<Vec<_>>()
    );
    assert_eq!(
        seen.iter().map(|(step, _)| *step).collect::<Vec<_>>(),
        vec!["window", "agent", "model"],
        "the steps ran in a different order: {:?}",
        seen
    );
    // At the window's step all three were up; at the agent's step the window was gone and
    // the model was still serving; at the model's step only the model was left.
    assert_eq!(
        seen[0].1,
        [true, true, true],
        "the window was stopped after another child: {:?}",
        seen
    );
    assert_eq!(
        seen[1].1,
        [false, true, true],
        "the agent's step did not come after the window and before the model: {:?}",
        seen
    );
    assert_eq!(
        seen[2].1,
        [false, false, true],
        "the model was stopped before the agent: {:?}",
        seen
    );

    // And nothing is left: no window, no agent, no model, and no zombie of any of them.
    for (name, pid) in [
        ("window", window_pid),
        ("agent", agent_pid),
        ("model", model_pid),
    ] {
        assert_eq!(
            wait_gone(pid, Duration::from_secs(10)),
            None,
            "the {} (pid {}) is still in the process table",
            name,
            pid
        );
    }
}

#[test]
fn the_production_plan_stops_the_children_it_was_given() {
    // The plan the boot itself builds (`start::plan`), with the three real children: it
    // has to perform every registered step in `ORDER`, and afterwards the process table
    // and the plan agree that nothing ATLAS started is still running.
    assert!(
        stand_in().is_some(),
        "the stand-in child must compile: cargo built this test with a rustc already"
    );
    assert!(ps_program().is_some(), "there is no ps(1) on this machine");
    let root = Root::new("plan");
    root.mode("sleep");
    let paths = atlas_config::Paths::at(root.path());
    assert!(root.stand_in_as("atlas-desktop").is_some());
    assert!(root.stand_in_as("atlas-monitor").is_some());
    let model_program = root.stand_in_as("llama-server").expect("the model stand-in");

    let mut window = window::Window::launch(&paths, &Printer::new()).expect("the window child");
    let agent = agent::Agent::launch(&paths, &Printer::new()).expect("the agent child");
    let invocation = llama::Invocation {
        program: model_program,
        model: paths.model_file.clone(),
        port: free_port(),
        args: vec!["-m".to_string()],
    };
    let model = llama::spawn(&invocation, root.path(), &paths.log_llama).expect("the model child");
    let pids = [window.pid(), agent.pid(), model.pid];
    for pid in pids {
        assert!(wait_running(pid, Duration::from_secs(5)), "pid {}", pid);
    }
    let runs = root.wait_for_runs(3, Duration::from_secs(5));
    assert_eq!(runs.len(), 3, "only {} children started: {:?}", runs.len(), runs);
    let (handle, _liveness) = supervise::start(
        Box::new(model),
        Policy::default(),
        Box::new(|| Err("not restarted in this test".to_string())),
        Arc::new(Printer::new()),
    );
    let performed = start::plan(
        move || {
            let _ = window.reap();
        },
        Arc::new(Printer::new()),
        Some(agent),
        Some(handle),
    )
    .run();
    assert_eq!(
        performed,
        vec![
            atlas_launcher::Step::Window,
            atlas_launcher::Step::Agent,
            atlas_launcher::Step::Model
        ],
        "the production plan skipped a child or reordered the stop"
    );
    println!("measured production plan: {:?}", performed);
    for pid in pids {
        assert_eq!(
            wait_gone(pid, Duration::from_secs(10)),
            None,
            "pid {} survived the production plan",
            pid
        );
    }
}

// ---------------------------------------------------------------------------
// (3) the provisioning child: at most once per start, never on a ready root
// ---------------------------------------------------------------------------

/// The shipped `setup.rs`, without its test module: what the *product* does, as opposed
/// to what its units would like it to do.
fn setup_source() -> Option<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("setup.rs");
    let text = std::fs::read_to_string(path).ok()?;
    let cut = text.find("#[cfg(test)]").unwrap_or(text.len());
    Some(text[..cut].to_string())
}

/// One function body from a source text: from `fn <name>` to the line that opens at the
/// function's own indent level and closes it.
fn body_of(source: &str, header: &str) -> Option<String> {
    let start = source.find(header)?;
    let rest = &source[start..];
    let mut depth = 0i32;
    let mut body = String::new();
    for line in rest.lines() {
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

#[test]
fn provisioning_runs_nothing_when_the_pieces_are_present() {
    // "Never when the model is already present", measured three ways: on disk with the
    // resolvers the boot itself uses, in the plan a person reads, and — for the case that
    // matters most — on a root where the install tool does not exist at all, where a
    // spawn attempt could only be an error.
    let root = Root::new("ready");
    let paths = atlas_config::Paths::at(root.path());
    let config = atlas_config::Config::load_at(&paths).expect("a config can always be built");

    // Nothing here yet: both pieces are missing and the install is what would provide
    // them, so this root is the control for the two below.
    let empty = setup::Present::scan(&paths, &config);
    assert!(
        !empty.model && !empty.server,
        "an empty root claims a piece: {:?}",
        empty
    );
    assert_eq!(
        setup::decide(empty, true),
        setup::Provision::Install(vec![setup::Piece::Model, setup::Piece::ModelServer]),
        "a missing model and a missing model server are an install"
    );

    // Now the pieces are in place, by hand: an executable `bin/llama-server` and the
    // model file the resolver accepts.
    std::fs::write(&paths.model_file, vec![7u8; 4096]).expect("the model file");
    std::fs::copy("/bin/echo", &paths.llama_local).expect("the model server");
    make_executable(&paths.llama_local).expect("chmod");
    let ready = setup::Present::scan(&paths, &config);
    assert!(ready.complete(), "the root is not complete: {:?}", ready);
    let decision = setup::decide(ready, true);
    assert_eq!(decision, setup::Provision::Ready);
    assert!(
        !decision.spawns(),
        "a provisioned root must not start an install child"
    );
    // And with the switch that refuses anyway, the answer is the same: nothing to do.
    assert_eq!(setup::decide(ready, false), setup::Provision::Ready);

    // The line a person reads before anything starts says the same thing, and says it
    // about this root: the plan is what `--dry-run` prints.
    let lines = plan::plan_lines(&paths, Some(&config), true);
    let provision = lines
        .iter()
        .find(|line| line.trim_start().starts_with("provisioning"))
        .unwrap_or_else(|| panic!("the plan has no provisioning line: {:#?}", lines));
    assert!(
        provision.contains("provisioning nothing"),
        "the plan promises an install on a ready root: {}",
        provision
    );
    assert!(
        provision.contains("not run"),
        "the plan does not say the install child is skipped: {}",
        provision
    );
    assert!(
        !provision.contains(setup::INSTALL_ARG),
        "the plan promises an install on a ready root: {}",
        provision
    );

    // The strongest form: delete the install tool's home and ask again. A spawn attempt
    // could not even resolve a program, so a `Ready` here is a measurement of "no child
    // was started", not of a child that happened to fail quietly.
    let _ = std::fs::remove_dir_all(setup::release_dir(&paths));
    let still_ready = setup::decide(setup::Present::scan(&paths, &config), true);
    assert_eq!(
        still_ready,
        setup::Provision::Ready,
        "removing the install tool changed the decision for a ready root"
    );
}

#[test]
fn the_install_child_has_one_call_site_and_no_retry_in_the_shipped_source() {
    // "At most once per start" as a property of the code that ships: the boot asks
    // `setup::run` once, and `run` calls `launch` once, with no loop and no second
    // attempt. This is the half a test outside the crate can check without a `Stop`
    // (see the note at the end of this file).
    let source = setup_source().expect("setup.rs is readable");
    let run_body = body_of(&source, "pub fn run(").expect("setup::run exists");
    assert_eq!(
        run_body.matches("launch(").count(),
        1,
        "setup::run calls launch {} times: {:?}",
        run_body.matches("launch(").count(),
        run_body
    );
    // Comment lines are prose ("for a hand-provisioned machine"); what matters is the
    // code, so the scan drops them the way the crate's own audits do.
    let code: String = run_body
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<&str>>()
        .join("\n");
    for keyword in ["loop {", "for ", "while ", ".launch("] {
        assert!(
            !code.contains(keyword),
            "setup::run contains `{}` — an install child in a loop is a download loop: \
             {:?}",
            keyword,
            run_body
        );
    }

    let boot = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("start.rs"),
    )
    .expect("start.rs is readable");
    let boot = &boot[..boot.find("#[cfg(test)]").unwrap_or(boot.len())];
    assert_eq!(
        boot.matches("setup::run(").count(),
        1,
        "the boot asks for provisioning more than once"
    );
}

// ---------------------------------------------------------------------------
// (4) no child inherits a listening socket
// ---------------------------------------------------------------------------

/// The internet-socket section of a stand-in's report, as `lsof` printed it.
///
/// Empty means the child held no socket at all — inherited or its own. The control in
/// the test below is what makes an empty answer mean something.
fn net_section(report: &str) -> String {
    match report.split_once("\nNET\n") {
        Some((_, net)) => net.trim().to_string(),
        None => panic!("the report has no NET section: {:?}", report),
    }
}

#[test]
fn no_child_inherits_a_listening_socket() {
    // The listener is this test process's, bound to `127.0.0.1:0` and never accepted on.
    // Every child is started through one of the launcher's real boundaries; each one
    // writes down its own descriptor table, and none of them may contain a socket.
    assert!(
        stand_in().is_some(),
        "the stand-in child must compile: cargo built this test with a rustc already"
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let port = listener
        .local_addr()
        .expect("its own address")
        .port();
    let root = Root::new("fds");
    root.mode("sleep");
    let paths = atlas_config::Paths::at(root.path());
    assert!(root.stand_in_as("atlas-desktop").is_some());
    assert!(root.stand_in_as("atlas-monitor").is_some());
    let model_program = root.stand_in_as("llama-server").expect("the model stand-in");

    let mut window = window::Window::launch(&paths, &Printer::new()).expect("the window child");
    let mut agent = agent::Agent::launch(&paths, &Printer::new()).expect("the agent child");
    let invocation = llama::Invocation {
        program: model_program,
        model: paths.model_file.clone(),
        port,
        args: vec!["-m".to_string()],
    };
    let mut model = llama::spawn(&invocation, root.path(), &paths.log_llama).expect("the model child");

    let mut reports: Vec<(&str, String)> = Vec::new();
    // The name in the report is the executable's, which is what each boundary resolved.
    for (name, program, pid) in [
        ("window", "atlas-desktop", window.pid()),
        ("agent", "atlas-monitor", agent.pid()),
        ("model", "llama-server", model.pid),
    ] {
        assert!(
            wait_running(pid, Duration::from_secs(5)),
            "the {} child never started",
            name
        );
        let file = root
            .path()
            .join("data")
            .join(format!("fds-{}.txt", program));
        let report = wait_for_report(&file, Duration::from_secs(5))
            .unwrap_or_else(|| panic!("the {} child wrote no report", name));
        assert!(
            !report.contains("ALL\nlsof-missing"),
            "there is no lsof on this machine, so this child's descriptors cannot be read"
        );
        reports.push((name, report));
    }
    // One line per child that started: each boundary started exactly one.
    assert_eq!(
        root.runs().len(),
        3,
        "the boundaries started a different number of children: {:?}",
        root.runs()
    );

    // What the probe says about a child that started through each boundary.
    for (name, report) in &reports {
        let net = net_section(report);
        assert!(
            net.is_empty(),
            "the {} child holds an internet socket although this process opened the only \
             listener:\n{}",
            name,
            report
        );
    }

    // The control: a stand-in that opens one loopback connection to the listener above.
    // If the probe cannot see *that*, the empty sections prove nothing.
    let control_root = Root::new("fds-control");
    control_root.mode(&format!("connect {}", port));
    let control_program = control_root
        .stand_in_as("llama-server")
        .expect("the control stand-in");
    let control_invocation = llama::Invocation {
        program: control_program,
        model: paths.model_file.clone(),
        port,
        args: vec!["-m".to_string()],
    };
    let mut control = llama::spawn(
        &control_invocation,
        control_root.path(),
        &control_root.path().join("data").join("llama.log"),
    )
    .expect("the control child");
    let control_file = control_root
        .path()
        .join("data")
        .join("connected-llama-server.txt");
    let control_report = wait_for_report(&control_file, Duration::from_secs(10))
        .expect("the control child connected and reported");
    let control_net = net_section(&control_report);
    assert!(
        control_net.contains("TCP") && control_net.contains(&port.to_string()),
        "the descriptor probe cannot see a socket, so the empty reports above prove \
         nothing:\n{}",
        control_report
    );

    // Leave nothing behind: every child here is this test's own.
    let _ = window.reap();
    let _ = agent.stop();
    let _ = supervise::ModelProcess::stop(&mut model);
    let _ = supervise::ModelProcess::stop(&mut control);
    drop(listener);
}

#[test]
fn the_install_child_runs_once_per_start_and_a_second_start_runs_none() {
    // The provisioning child, counted rather than described: a real `atlas-setup --install`
    // user-land stand-in (compiled Rust) that appends one line per run, so "how many
    // children did this start?" is a line count on disk. `setup::run` is driven six ways:
    // a first start that must install, a second start on the same (now complete) root that
    // must start nothing, a child that exits 0 and places nothing, a root where the install
    // tool does not exist at all, `--no-install`, and a stop that arrived before any child
    // existed.
    //
    // The flag is `Stop::idle()` — the crate's API for exactly this question ("what does the
    // boot do when nothing asked it to stop?"), so no runtime and no signal handler is built
    // here. It used to be unreachable from `tests/`, which is why this test once stood a
    // never-driven tokio runtime up to obtain a `Stop`.
    assert!(
        stand_in().is_some(),
        "the stand-in install tool must compile: cargo built this test with a rustc already"
    );
    let stop = Stop::idle();

    // 1. Nothing provisioned: one child, which places both pieces.
    let root = Root::new("install-once");
    let paths = atlas_config::Paths::at(root.path());
    let config = atlas_config::Config::load_at(&paths).expect("a config");
    assert!(root.stand_in_as(setup::BINARY_NAME).is_some(), "the install stand-in");
    root.mode(&format!(
        "install\n{}\n{}",
        paths.model_file.display(),
        paths.llama_local.display()
    ));
    let printer = Printer::new();
    let outcome = setup::run(&printer, &paths, &config, true, &stop)
        .expect("an install that places the pieces is a success");
    assert_eq!(outcome, setup::Outcome::Installed);
    assert_eq!(
        root.runs().len(),
        1,
        "the first start ran {} install children: {:?}",
        root.runs().len(),
        root.runs()
    );
    assert!(
        setup::Present::scan(&paths, &config).complete(),
        "the pieces are not on disk after the install"
    );
    // The install child, like every other, holds no socket — even though this test process
    // was holding a loopback listener when it ran.
    let report_file = root.path().join("data").join("fds-atlas-setup.txt");
    if let Ok(report) = std::fs::read_to_string(&report_file) {
        assert!(
            !report.contains("ALL\nlsof-missing"),
            "there is no lsof here, so the install child's descriptors cannot be read"
        );
        assert!(
            net_section(&report).is_empty(),
            "the install child holds an internet socket:\n{}",
            report
        );
    }

    // 2. The same root, a second start: the pieces are there, so nothing runs.
    let outcome = setup::run(&printer, &paths, &config, true, &stop)
        .expect("a complete root is not a failure");
    assert_eq!(outcome, setup::Outcome::Ready);
    assert_eq!(
        root.runs().len(),
        1,
        "a second start ran the install child again: {:?}",
        root.runs()
    );

    // 3. A child that exits 0 and places nothing: one child, one refusal, no retry.
    let nothing = Root::new("install-nothing");
    let nothing_paths = atlas_config::Paths::at(nothing.path());
    let nothing_config = atlas_config::Config::load_at(&nothing_paths).expect("a config");
    assert!(nothing.stand_in_as(setup::BINARY_NAME).is_some());
    nothing.mode("nothing");
    let error = setup::run(&printer, &nothing_paths, &nothing_config, true, &stop)
        .expect_err("a child that installed nothing is a refusal");
    assert_eq!(
        nothing.runs().len(),
        1,
        "a failing install child was retried: {:?}",
        nothing.runs()
    );
    assert!(error.contains("still missing"), "{}", error);

    // 4. No install tool and nothing installed: a refusal, and zero children.
    let bare = Root::new("install-notool");
    let bare_paths = atlas_config::Paths::at(bare.path());
    let bare_config = atlas_config::Config::load_at(&bare_paths).expect("a config");
    let error = setup::run(&printer, &bare_paths, &bare_config, true, &stop)
        .expect_err("no install tool and nothing installed is a refusal");
    assert!(
        error.contains(setup::BINARY_NAME),
        "the refusal does not name the tool: {}",
        error
    );
    assert!(
        bare.runs().is_empty(),
        "something ran without an install tool: {:?}",
        bare.runs()
    );

    // 5. `--no-install`: the switch that promises no network and no child.
    let refused = Root::new("install-refused");
    let refused_paths = atlas_config::Paths::at(refused.path());
    let refused_config = atlas_config::Config::load_at(&refused_paths).expect("a config");
    assert!(refused.stand_in_as(setup::BINARY_NAME).is_some());
    refused.mode("nothing");
    let outcome = setup::run(&printer, &refused_paths, &refused_config, false, &stop)
        .expect("--no-install is not a failure");
    assert_eq!(outcome, setup::Outcome::Ready);
    assert!(
        refused.runs().is_empty(),
        "--no-install started the install child: {:?}",
        refused.runs()
    );

    // 6. A stop that arrived before the child existed: no child at all, and the boot says
    // it was stopped rather than continuing to a window that cannot think. `Stop::fired()`
    // is the state a `SIGTERM` during the settings step produces.
    let stopped = Root::new("install-stopped");
    let stopped_paths = atlas_config::Paths::at(stopped.path());
    let stopped_config = atlas_config::Config::load_at(&stopped_paths).expect("a config");
    assert!(
        stopped.stand_in_as(setup::BINARY_NAME).is_some(),
        "an install tool that would have been started, so \"not started\" is a fact"
    );
    stopped.mode("nothing");
    let outcome = setup::run(&printer, &stopped_paths, &stopped_config, true, &Stop::fired())
        .expect("a stop before provisioning is not a failure");
    assert_eq!(outcome, setup::Outcome::Stopped);
    assert!(
        stopped.runs().is_empty(),
        "a stop before provisioning started the install child: {:?}",
        stopped.runs()
    );
}

// ---------------------------------------------------------------------------
// what the two `Stop` constructors are for, and what this file asks them
// ---------------------------------------------------------------------------
//
// `signals::Stop::idle()` and `signals::Stop::fired()` are part of the crate's API
// (`atlas-launcher/src/signals.rs`) precisely so a test outside the crate can drive the
// boot when *nothing* has asked it to stop, and when a `SIGTERM` arrived before the
// children were up. Production code never calls them: `signals::install` is the only
// constructor the boot uses, so the two states a test can build are states a signal
// handler can also produce, and no test needs a runtime or a real signal.
//
// The provisioning test above uses both: `idle()` for the ordinary starts, `fired()` for
// the stop that arrives before an install child exists (which must spawn nothing).
