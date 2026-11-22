//! `atlas start` — the whole boot, in the order the brief asks for it.
//!
//! 1. **A stop handler first.** Before anything is spawned: a `SIGTERM` while the
//!    weights are loading used to kill the process with the default disposition and
//!    leave a child behind. Now every wait in the boot watches
//!    [`crate::signals::Stop`].
//! 2. **One instance per data directory.** The lock is taken before anything is
//!    read or written, and a stale lock (a file nobody holds) is reported, not
//!    obeyed. Once the lock is ours, `data/` and the files in it are made
//!    owner-only ([`harden`]) — the step the doctor's advice names — so a
//!    `0644` log left by an older build is `0600` before this boot reads a
//!    setting, and the repaired count is part of what a start reports.
//! 3. **Settings** come from `atlas-config` (env > `data/config.json` > default),
//!    validated, and a bad value stops the start with the key and its origin.
//! 4. **The loopback invariant**: the one address this boot is responsible for is the
//!    model server's, and the invariant is that it is a loopback literal — it comes
//!    from `atlas-config`, is passed to `llama-server` as `--host 127.0.0.1`, and is
//!    checked here before a child exists. Nothing in this process listens: there is no
//!    listener, no `atlas-web`, no browser, and no port opened by ATLAS at all.
//! 5. **Provisioning** — [`crate::setup`] — runs before the model is resolved, because it
//!    is what *puts* the model there. When the model file or `bin/llama-server` is
//!    missing, `atlas-setup --install` runs as a supervised child, once, with its output
//!    inherited and its progress in `data/setup.json`; when both are present it starts
//!    nothing at all. It is the only step in the product that can reach the network, and
//!    `--no-install` is the switch that turns it off. A child that fails, or that exits 0
//!    and leaves a piece missing, is one sentence that says what is missing and that
//!    nothing else was started.
//! 6. **The model file, the window binary and the agent binary** must exist, once
//!    provisioning has had its chance. A piece that is still missing is a sentence with
//!    the exact path and the manual step. Both programs are resolved *before* the model is
//!    started, so a machine that never built one stops in a sentence instead of after four
//!    minutes of loading weights for a product that cannot work without it.
//! 7. **The model server** is started as a child of this process with the
//!    documented flags (`--host 127.0.0.1` among them). The supervisor attaches
//!    *immediately* — before the health wait — so a server that dies while loading
//!    is restarted once with a backoff instead of being waited on for four minutes.
//!    A server that never answers, or will not start at all, is reported and the
//!    window opens without it, which is the choice `run.sh` made.
//! 8. **The agent** — [`crate::agent`], the always-on half — is started as a child once
//!    the model server has been dealt with, because every action it claims drives a
//!    research pass through that model. It stays up for the whole run: it is what makes a
//!    click on "monitor this topic" happen, and what keeps a monitor's cadence while
//!    nobody is looking at the window.
//!
//! 9. **The window** — [`crate::window`], the product — is started as a child and this
//!    process waits for it. Closing the window is how `atlas start` ends.
//!
//! Then it shuts everything down in [`crate::shutdown::ORDER`] (the window, the agent,
//! then the model server) on Ctrl-C or `SIGTERM` — including when the stop arrives in the middle
//! of the boot, in which case no window is opened at all ([`Boot::StopDuringBoot`]) and
//! the child that was already started is still reaped by [`plan`]. What this module
//! does not decide: which flags (that is [`crate::llama`]), when to restart
//! ([`crate::supervise`]), how to stop ([`crate::shutdown`]), how to print
//! ([`crate::report`]), which root a command line names ([`crate::home`]), and whether
//! this machine needs provisioning at all ([`crate::setup`] — this module only asks it,
//! before the model is resolved, and stops the boot when it says a stop arrived). It is
//! the sequence.
//!
//! `--dry-run` prints exactly that sequence ([`crate::plan::plan_lines`]) and spawns nothing.
//!
//! What this module used to do, and no longer may: serve `atlas-web` in this process
//! and point the user's browser at the bound port. The product is the window, the
//! hard rule is that no port is ever opened, and `atlas-guard/tests/reachability.rs`
//! is the workspace-wide test that keeps the listener out of this binary's closure.

use std::sync::Arc;
use std::time::{Duration, Instant};

use atlas_config::{Config, Paths};

use crate::args::StartOptions;
use crate::report::Printer;
use crate::shutdown::{Shutdown, Step};
use crate::signals::Stop;
use crate::{agent, health, home, llama, lock, model, setup, signals, silence, supervise, window};

/// How often the waiting line is repeated while the model loads.
const PROGRESS_EVERY: u32 = 5;

/// How long a restarted model server is given to answer, on its own thread.
const RESTART_HEALTH_WAIT: Duration = Duration::from_secs(120);

/// The whole boot. `Ok(())` means "ATLAS ran and stopped when asked"; `Err` is the
/// sentence to print and exit 1 with.
pub fn run(options: StartOptions) -> Result<(), String> {
    let printer = Arc::new(Printer::new());
    // The root comes from `home`, so `--root .` is the absolute directory here and
    // in every message that names it (the lock record included).
    let paths = home::paths(&options.root);
    printer.say("ATLAS — fully local news intelligence (loopback only, nothing downloaded)");
    printer.note(format!("root {}", paths.root.display()));

    // A plan, not a boot. Every other way to ask "what will this do?" is a real
    // start, and a real start loads weights and opens a window.
    if options.dry_run {
        return crate::plan::dry_run(&paths, &printer, !options.no_install);
    }

    // 1. the runtime and the stop handler, before a socket or a child exists
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .thread_name("atlas-stop")
        .build()
        .map_err(|error| format!("cannot start the async runtime: {}", error))?;
    let stop = signals::install(runtime.handle());

    // 2. one instance per data directory
    let held = lock::acquire(&paths.data_dir, &lock::this_process(&paths.root))
        .map_err(|error| error.to_string())?;
    if let Some(previous) = held.previous() {
        printer.warn(format!(
            "took over a lock left by {} — that process is gone and nothing of it is running",
            previous.sentence()
        ));
    }

    // 2b. data/ is made owner-only before anything is read out of it or written
    // into it — the step the doctor has been telling users a start performs.
    harden(&printer, &paths);

    // 3. settings
    let config = atlas_config::load_managed_at(&paths).map_err(|error| {
        format!(
            "the configuration will not load ({}): {}. Fix the value or remove the line; \
             ATLAS does not repair a setting silently",
            paths.config_file.display(),
            error
        )
    })?;
    let problems = config.validate();
    if !problems.is_empty() {
        let lines: Vec<String> = problems.iter().map(|problem| problem.to_string()).collect();
        return Err(format!(
            "the configuration is not usable: {}",
            lines.join("; ")
        ));
    }
    printer.note(format!(
        "settings from {}: model port {} (ATLAS opens no port of its own)",
        paths.config_file.display(),
        config.llama_port
    ));

    // 4. the loopback invariant, before any child exists
    check_loopback(&config)?;
    printer.note(format!(
        "the model server is started on {} only, and nothing else in ATLAS listens",
        atlas_config::BIND_HOST
    ));

    // 5. provisioning, before the model is resolved and before any other child exists.
    // This is the only step that can reach the network, and it exists because the model
    // is several gigabytes: a person who double-clicks the app on a fresh machine must
    // end with a working window, not with a sentence about a manual download. On a
    // provisioned machine the step measures, finds everything present and starts nothing.
    if setup::run(&printer, &paths, &config, !options.no_install, &stop)? == setup::Outcome::Stopped {
        // Nothing was started, so there is nothing to stop: the lock is released by
        // dropping it on this return, and one line says ATLAS is over.
        printer.say("ATLAS stopped before anything was started.");
        drop(held);
        return Ok(());
    }

    // 6. where the model file and the two remaining programs are, before anything is spawned
    let model = model::resolve(&paths, &config).map_err(|missing| missing.sentence())?;
    printer.note(format!("model {}", model.text()));
    // Resolved here, spawned below: a machine that never built the window must hear
    // about it in one sentence, not after four minutes of loading weights for a window
    // that cannot appear.
    let window_binary = window::binary(&paths).ok_or_else(|| window::missing_line(&paths))?;
    printer.note(format!("window {}", window_binary.display()));
    // The agent is resolved here for the same reason: a start without it is a window
    // whose clicks are stored and never acted on, which is the hole this boot closes —
    // so a missing agent is a refusal with its own sentence, not a silent skip.
    let agent_binary = agent::binary(&paths).ok_or_else(|| agent::missing_line(&paths))?;
    printer.note(format!("agent {} {}", agent_binary.display(), agent::SERVE));

    // 7. the model server child, watched from the moment it exists
    let mut supervisor = start_model(&printer, &paths, &config, &model, &stop)?;

    // 7b. the monitoring agent, as a third child: started once the model server has
    // been dealt with (its every action is a research pass through that model) and kept
    // for the whole run, so a click on "monitor this topic" is acted on and a monitor's
    // cadence runs while the window is open or closed.
    let mut working: Option<agent::Agent> = match agent::Agent::launch(&paths, &printer) {
        Ok(handle) => Some(handle),
        // A resolved binary that will not spawn is the same refusal as a missing one:
        // the product would look started and monitor nothing.
        Err(message) => return Err(message),
    };

    // 8. the window, as a child of this process. Closing it is how ATLAS stops.
    let mut opened: Option<window::Window> = None;
    let failure = match boot_at(&stop) {
        Boot::Open => match window::Window::launch(&paths, &printer) {
            Ok(mut handle) => {
                printer.say("ATLAS is open. Close the window, or press Ctrl-C, to stop it.");
                match handle.wait(&stop) {
                    window::Ending::Closed(sentence) => printer.note(sentence),
                    window::Ending::Asked => {
                        printer.note("a stop was requested; the window is being closed")
                    }
                }
                opened = Some(handle);
                None
            }
            Err(message) => Some(message),
        },
        // A signal arrived during the boot: no window is opened, and everything
        // started above is stopped by the sequence below.
        Boot::StopDuringBoot => {
            printer.note("a stop arrived during the boot; the window was never opened");
            None
        }
    };

    // A window that will not start is the one failure worth printing before the
    // shutdown summary: it says why ATLAS is stopping. `main` prints the message when
    // it is non-empty, so the sentence appears exactly once (see `entry`). It is a
    // refusal, not a warning: the exit code is 1.
    if let Some(message) = &failure {
        printer.error(message.clone());
    }

    // 9. nothing outlives this process. The window is reaped first (it is usually
    // already closed by the user), then the model server; the runtime that carried the
    // stop flag goes with the process.
    let performed = plan(
        move || {
            // Taking the window is the step: a `Window` reaps its child when it is
            // dropped into this closure, and a boot that opened none still performs
            // the step so the order it reports is the same one.
            if let Some(mut window) = opened.take() {
                window.reap();
            }
        },
        printer.clone(),
        working.take(),
        supervisor.take(),
    )
    .run();
    let names: Vec<&str> = performed.iter().map(|step| step.name()).collect();
    printer.note(format!("stopped: {}", names.join(", ")));
    drop(held);
    printer.say("ATLAS stopped. Nothing it started is still running.");

    // Already printed above, in the right order; an empty message means so.
    match failure {
        Some(_) => Err(String::new()),
        None => Ok(()),
    }
}

/// Is a window still wanted once everything before it has run?
///
/// A `SIGTERM` that arrives while the weights are loading must not be followed by a
/// window on screen: the boot goes straight to the shutdown order, which is the only
/// thing that reaps the child it already started. One function, because the rule has
/// to be visible and testable — an `if stop.requested()` in the middle of a 120-line
/// sequence is neither.
fn boot_at(stop: &Stop) -> Boot {
    if stop.requested() {
        Boot::StopDuringBoot
    } else {
        Boot::Open
    }
}

/// What the boot does once the model server has been dealt with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boot {
    /// Open the window and wait for it.
    Open,
    /// A stop arrived during the boot: open nothing, stop what was started.
    StopDuringBoot,
}

/// Make `data/` and the files ATLAS owns owner-only, and report what it repaired.
///
/// The doctor's advice has always been "run `atlas start` once — it hardens data/
/// to 0700 and its files to 0600", and nothing performed it: `harden_data_files`
/// was exported by `atlas-config` and called by nobody, so a `data/` created by an
/// older build (or by hand) kept its `0755`/`0644` modes and every log — the model
/// log included — stayed readable by any account on the machine while
/// `/api/security` reported the mode ATLAS *wanted*.
///
/// The rule itself lives in `atlas-config` (one 0700/0600 decision, one place that
/// refuses to `chmod` through a symlink); what lives here is *when*: after the lock
/// (two instances cannot race each other through the same chmod) and before the
/// settings are read (the file is private before this boot looks at it). The count
/// is printed because "hardened" without a number is a claim this launcher cannot
/// back — the doctor's sentence now has something behind it.
fn harden(printer: &Arc<Printer>, paths: &Paths) {
    let repaired = atlas_config::harden_data_files(paths, &[]);
    if !atlas_config::posix() {
        // No mode to set: privacy is the ACL inherited from the user profile, and
        // `atlas-platform` explains that rather than pretending a chmod happened.
        printer.note(format!(
            "data/ privacy: {} — no mode to set on this platform, so nothing was repaired",
            atlas_config::permission_basis(atlas_config::platform())
        ));
        return;
    }
    printer.note(format!(
        "data/ is owner-only (0o{:o}, files 0o{:o}): {} path(s) repaired",
        atlas_config::DATA_MODE,
        atlas_config::FILE_MODE,
        repaired.len()
    ));
}

/// The one address this boot is responsible for must be the loopback literal.
///
/// Before this it walked three ports through `atlas_web::boot::bind_address`, because
/// the launcher served a listener and started a browser. Neither exists in this binary
/// any more: the window dials the model server outbound and listens for nothing, so the
/// only address left is the model server's, and it is a constant that
/// [`crate::llama`] puts in the child's argv.
fn check_loopback(config: &Config) -> Result<(), String> {
    if !atlas_config::is_loopback(atlas_config::BIND_HOST) {
        return Err(format!(
            "the loopback invariant does not hold: {} is not judged loopback by atlas-guard. \
             ATLAS cannot start",
            atlas_config::BIND_HOST
        ));
    }
    // An address-shaped setting is refused here as well: `llama_port` is a port, and a
    // config that carries a host of its own must not reach a child's argv.
    if let Some(problem) = config
        .validate()
        .iter()
        .find(|problem| problem.key.contains("host"))
    {
        return Err(format!(
            "the configuration names a host ({}): ATLAS starts the model server on {} only",
            problem,
            atlas_config::BIND_HOST
        ));
    }
    Ok(())
}

/// The sentence for "this machine has no `llama-server`", with the paths in it.
///
/// Split out of [`start_model`] so the wording is testable without a boot. It is the
/// whole of what a user gets for the commonest reason a model cannot start — the
/// binary was never built or never installed — so it has to name the file, the
/// directory it was looked for in, and say that nothing is downloaded: a user's next
/// move otherwise is to look for a downloader this app does not have.
///
/// One line, no newline: it goes through `report::write_line`, which refuses a second
/// line and every control character in a path.
pub(crate) fn missing_binary_line(paths: &Paths) -> String {
    format!(
        "{} was not found in {} or on PATH. Build llama.cpp with CMake and put the binary \
         in bin/; nothing is downloaded. ATLAS opens without its local model and the \
         window's status row says so",
        llama::BINARY_NAME,
        paths.llama_local.display()
    )
}

/// Bring up the model server, or explain why the app runs without one.
///
/// Four cases, all reported, and **none of them fails the boot**: a server already
/// answering on the port (nothing is started, and this launcher does not adopt a
/// process it did not spawn), no binary (a manual step), a binary that will not
/// spawn (the log file or the port — the same "run without a model" answer as the
/// other two, because ATLAS can serve the archive without inference), and a child
/// of our own — which is then watched and waited for.
fn start_model(
    printer: &Arc<Printer>,
    paths: &Paths,
    config: &Config,
    model: &model::ModelFile,
    stop: &Stop,
) -> Result<Option<supervise::Handle>, String> {
    let port = config.llama_port;
    let already = health::probe(port);
    if already.is_up() {
        printer.note(format!(
            "a model server is already answering on 127.0.0.1:{} — ATLAS will use it and will not \
             stop it (a process this launcher did not start is not its child)",
            port
        ));
        return Ok(None);
    }
    let binary = match llama::binary(paths) {
        Some(binary) => binary,
        None => {
            printer.warn(missing_binary_line(paths));
            return Ok(None);
        }
    };
    let help = llama::help_text(&binary);
    let invocation = llama::invocation(config, &binary, &model.path, port, &help);
    printer.note(format!("model server {}", invocation.text()));
    let started = match silence::start(&invocation, &paths.root, &paths.log_llama) {
        Ok(started) => started,
        // Not a failed boot: every other "no model server" answer above continues,
        // and `run`'s contract is that ATLAS serves without inference. A `?` here
        // stopped the whole app for one child that could not be started.
        Err(error) => {
            printer.warn(format!(
                "the model server will not start: {}. ATLAS keeps going without its local model \
                 — the window's status row says so, and {} explains the child",
                error,
                paths.log_llama.display()
            ));
            return Ok(None);
        }
    };
    // The one answer a user needs from `--log-disable`: this build would not take
    // it. Without this line the flag would look applied and llama.cpp's own log —
    // prompt text included — would sit in `data/llama.log` unsilenced and unspoken.
    if !started.silenced {
        printer.warn(format!(
            "this {} build refuses --log-disable, so the model server was started again without \
             it: the model's own log is NOT silenced, and llama.cpp may write prompt text into {}. \
             A llama.cpp built from a recent source accepts the flag",
            llama::BINARY_NAME,
            paths.log_llama.display()
        ));
    }
    let child = started.child;
    let pid = child.pid;
    printer.note(format!(
        "model server pid {} — waiting up to {} for http://127.0.0.1:{}/health",
        pid,
        crate::report::span(health::BOOT_DEADLINE),
        port
    ));

    // Watched before it is waited for: a death during the load is restarted, not
    // hidden behind a four-minute wait.
    let respawn = respawner(
        // The argv that is actually running: a restart that asked for the flag
        // this build refused would die the same way, once, for nothing.
        started.invocation.clone(),
        paths.root.clone(),
        paths.log_llama.clone(),
        printer.clone(),
        stop.clone(),
    );
    let (handle, liveness) = supervise::start(
        Box::new(child),
        supervise::Policy::default(),
        respawn,
        printer.clone(),
    );
    let ready = wait_ready(printer, port, health::BOOT_DEADLINE, stop, &liveness);
    report_ready(printer, ready, &paths.log_llama);
    Ok(Some(handle))
}

/// How the wait for `/health` ended. Decided by [`wait_ready`], worded by
/// [`report_ready`], so a test can ask *what happened* without a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ready {
    /// `/health` answered `200`; the model is loaded.
    Answering(Duration),
    /// A stop was requested while the boot was waiting. Nothing to say: the
    /// shutdown order reports what it stopped.
    Stopped,
    /// The supervisor gave up, so no model server is running any more.
    Gone { state: String },
    /// The deadline passed with the port still not answering.
    Silent { state: String, elapsed: Duration },
}

/// Wait for `/health`, printing progress the way a person can follow, and stop the
/// moment a signal arrives or the supervisor gives up.
///
/// `deadline` is a parameter, not the constant, so a test can drive the
/// "never answers" ending in milliseconds instead of four minutes.
fn wait_ready(
    printer: &Arc<Printer>,
    port: u16,
    deadline: Duration,
    stop: &Stop,
    liveness: &supervise::Liveness,
) -> Ready {
    let started = Instant::now();
    let stop_watch = stop.clone();
    let alive = liveness.clone();
    let outcome = health::wait(
        port,
        deadline,
        health::BOOT_INTERVAL,
        &mut health::probe,
        &mut |attempt, state| {
            let quiet = attempt == 1 || attempt % PROGRESS_EVERY == 0 || state.is_up();
            if !quiet {
                return;
            }
            match state {
                health::State::Up => printer.note(format!(
                    "the model server is answering after {}",
                    crate::report::span(started.elapsed())
                )),
                _ => printer.note(format!(
                    "waiting for the model server: {} ({})",
                    state.text(),
                    crate::report::span(started.elapsed())
                )),
            }
        },
        &mut || stop_watch.requested() || !alive.alive(),
    );
    let elapsed = started.elapsed();
    match outcome {
        Ok(_) => Ready::Answering(elapsed),
        // A stop is not a failure; the shutdown sequence says what happened.
        Err(_) if stop.requested() => Ready::Stopped,
        // The supervisor gave up: the child is gone and will not come back, so the
        // wait must say that instead of waiting for a port nothing owns.
        Err(state) if !liveness.alive() => Ready::Gone {
            state: state.text(),
        },
        Err(state) => Ready::Silent {
            state: state.text(),
            elapsed,
        },
    }
}

/// The sentence a [`Ready`] earns — or `None` when the ending needs no line. One
/// place, so the boot and the supervisor's own lines cannot say different things
/// about the same model server.
///
/// `log` is the launcher's `data/llama.log`, named in full: a reader of the
/// message may not be standing in the project directory.
fn ready_line(ready: &Ready, log: &std::path::Path) -> Option<String> {
    match ready {
        // The progress line already said "answering after Ns".
        Ready::Answering(_) => None,
        // A stop is not a failure; the shutdown order reports what it stopped.
        Ready::Stopped => None,
        Ready::Gone { state } => Some(format!(
            "the model server is not running ({}); see {}. ATLAS keeps going without its local \
             model",
            state,
            log.display()
        )),
        Ready::Silent { state, elapsed } => Some(format!(
            "the model server did not answer within {} ({}); see {}. ATLAS keeps going without it",
            crate::report::span(*elapsed),
            state,
            log.display()
        )),
    }
}

/// Print what a [`Ready`] ended with. Warning, not note: the app runs, with less
/// than it could.
fn report_ready(printer: &Arc<Printer>, ready: Ready, log: &std::path::Path) {
    if let Some(line) = ready_line(&ready, log) {
        printer.warn(line);
    }
}

/// The one restart: rebuild the argv and spawn.
///
/// The health wait is **not** done here, and that is deliberate: this closure runs
/// on the supervisor's thread, and blocking it on a port is how a model server that
/// dies twice keeps ATLAS waiting for four minutes instead of reporting the second
/// death. A watcher thread announces the moment the restarted server answers; the
/// window's status row is what says "offline" until then.
fn respawner(
    invocation: llama::Invocation,
    root: std::path::PathBuf,
    log: std::path::PathBuf,
    printer: Arc<Printer>,
    stop: Stop,
) -> Box<dyn FnMut() -> Result<Box<dyn supervise::ModelProcess>, String> + Send> {
    Box::new(move || {
        let child = llama::spawn(&invocation, &root, &log)?;
        printer.note(format!(
            "model server restarted as pid {} on 127.0.0.1:{}",
            child.pid, invocation.port
        ));
        announce_when_ready(
            printer.clone(),
            invocation.port,
            RESTART_HEALTH_WAIT,
            stop.clone(),
        );
        Ok(Box::new(child))
    })
}

/// Watch `/health` from a thread of its own and say one line when it answers.
///
/// Nothing waits on this thread and it never starts anything: it is the difference
/// between "restarted" and "restarted and answering", and it must not hold up the
/// supervisor.
///
/// It watches the stop flag because it outlives the restart: without that it could
/// announce "the restarted model server is answering after 4s" *after* ATLAS had
/// printed "ATLAS stopped. Nothing it started is still running." — a line
/// contradicting the sentence the user just read, about a child that this very
/// process had already killed. The handle is returned so a test can wait for the
/// thread instead of leaving it behind.
fn announce_when_ready(
    printer: Arc<Printer>,
    port: u16,
    deadline: Duration,
    stop: Stop,
) -> Option<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("atlas-model-ready".to_string())
        .spawn(move || {
            let started = Instant::now();
            let outcome = health::wait(
                port,
                deadline,
                health::BOOT_INTERVAL,
                &mut health::probe,
                &mut |_, _| {},
                &mut || stop.requested(),
            );
            if outcome.is_ok() {
                printer.note(format!(
                    "the restarted model server is answering after {}",
                    crate::report::span(started.elapsed())
                ));
            }
        })
        .ok()
}

/// The stop order this process performs, in one place.
///
/// The window step is a parameter (the caller hands in the child it owns), so a test can
/// run the *production* plan with a recorder instead of a window and prove two things:
/// the order is window → model, and the model child this process spawned is stopped
/// exactly once — including when the stop arrived during the boot and no window was ever
/// opened.
pub fn plan(
    close_window: impl FnMut() + Send + 'static,
    printer: Arc<Printer>,
    agent: Option<agent::Agent>,
    mut supervisor: Option<supervise::Handle>,
) -> Shutdown {
    let agent = Arc::new(std::sync::Mutex::new(agent));
    let printer_for_agent = printer.clone();
    Shutdown::new()
        .add(Step::Window, close_window)
        // Between the window and the model: the agent's work goes *through* the model,
        // so it is asked to stop while the model can still answer it.
        .add(Step::Agent, move || {
            let held = match agent.lock() {
                Ok(mut held) => held.take(),
                Err(poisoned) => poisoned.into_inner().take(),
            };
            if let Some(mut handle) = held {
                let sentence = handle.stop();
                if sentence.contains("had already exited") {
                    printer_for_agent.warn(sentence);
                } else {
                    printer_for_agent.note(sentence);
                }
            }
        })
        // `take()`: the step runs once, but a step is an `FnMut` — a handle left in
        // the option cannot be moved out of a closure that may be called twice.
        .add(Step::Model, move || {
            if let Some(mut handle) = supervisor.take() {
                match handle.stop() {
                    Some(supervise::Outcome::Stopped) => {
                        printer.note("the model server was stopped")
                    }
                    // The supervisor thread unwound before the stop. Its child was
                    // still stopped (the guard in `supervise::run` is what makes
                    // that true), so the shutdown is complete either way — but a
                    // silent exit here is how a future leak would look, so it says
                    // so out loud instead.
                    Some(supervise::Outcome::Failed(detail)) => printer.warn(format!(
                        "the model server supervisor {} — its child was stopped with it",
                        detail
                    )),
                    _ => {}
                }
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;    use crate::supervise::Liveness;

    /// A temp project root with `data/`, `models/` and `bin/`.
    fn fresh_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "atlas-start-{}-{}-{}",
            tag,
            std::process::id(),
            atlas_store::now_iso().replace([':', '.'], "-")
        ));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["data", "models", "bin"] {
            let _ = std::fs::create_dir_all(dir.join(sub));
        }
        dir
    }

    /// A loopback port nothing holds: the kernel hands one out and it is released.
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("a free loopback port")
            .local_addr()
            .unwrap()
            .port()
    }

    #[test]
    fn the_boot_opens_the_window_only_when_no_stop_arrived() {
        assert_eq!(boot_at(&Stop::idle()), Boot::Open);
        // The `SIGTERM`-during-the-boot case: no window is opened, straight to the stop
        // order, which reaps the model child that was already started.
        assert_eq!(boot_at(&Stop::fired()), Boot::StopDuringBoot);
    }

    #[test]
    fn the_health_wait_returns_at_once_when_the_supervisor_has_given_up() {
        // Port 9 on loopback: nothing listens, so every probe is refused at once.
        let printer = Arc::new(Printer::new());
        let started = Instant::now();
        let ready = wait_ready(
            &printer,
            9,
            Duration::from_secs(240),
            &Stop::idle(),
            &Liveness::gone(),
        );
        let elapsed = started.elapsed();
        assert!(
            matches!(ready, Ready::Gone { .. }),
            "expected the give-up ending, got {:?}",
            ready
        );
        // The whole point: it does not wait out the four-minute deadline for a
        // child that will never answer.
        assert!(elapsed < Duration::from_secs(5), "{:?}", elapsed);
    }

    #[test]
    fn the_health_wait_reports_a_silent_port_after_its_deadline() {
        let printer = Arc::new(Printer::new());
        let started = Instant::now();
        let ready = wait_ready(
            &printer,
            9,
            Duration::from_millis(80),
            &Stop::idle(),
            &Liveness::new(),
        );
        let elapsed = started.elapsed();
        assert!(
            matches!(ready, Ready::Silent { .. }),
            "expected the deadline ending, got {:?}",
            ready
        );
        assert!(elapsed >= Duration::from_millis(80), "{:?}", elapsed);
        assert!(elapsed < Duration::from_secs(5), "the wait was not bounded");
    }

    #[test]
    fn a_requested_stop_ends_the_health_wait_as_a_stop() {
        let printer = Arc::new(Printer::new());
        let started = Instant::now();
        let ready = wait_ready(
            &printer,
            9,
            Duration::from_secs(240),
            &Stop::fired(),
            &Liveness::new(),
        );
        assert_eq!(ready, Ready::Stopped, "a stop is not a model failure");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn the_ready_line_names_the_log_file_in_full() {
        // A reader may not be standing in the project directory, so the message
        // carries the path it was given, not a hard-coded `data/llama.log`.
        let log = std::path::Path::new("/tmp/some root/data/llama.log");
        // The two quiet endings say nothing at all.
        assert_eq!(ready_line(&Ready::Answering(Duration::from_secs(1)), log), None);
        assert_eq!(ready_line(&Ready::Stopped, log), None);
        let silent = ready_line(
            &Ready::Silent {
                state: "no answer (connection refused)".to_string(),
                elapsed: Duration::from_secs(200),
            },
            log,
        )
        .expect("the deadline ending carries a sentence");
        assert!(silent.contains("/tmp/some root/data/llama.log"), "{}", silent);
        assert!(silent.contains("3m 20s"), "{}", silent);
        assert!(silent.contains("keeps going without it"), "{}", silent);
        let gone = ready_line(
            &Ready::Gone {
                state: "exited with exit status: 1".to_string(),
            },
            log,
        )
        .expect("the give-up ending carries a sentence");
        assert!(gone.contains("is not running"), "{}", gone);
        assert!(gone.contains("exit status: 1"), "{}", gone);
        assert!(gone.contains("/tmp/some root/data/llama.log"), "{}", gone);
    }

    #[test]
    fn the_ready_announcer_stops_probing_when_a_stop_was_requested() {
        // The restarted-server announcer outlives the restart. Without the stop
        // watch it could print "answering" after "ATLAS stopped".
        let printer = Arc::new(Printer::new());
        let started = Instant::now();
        let handle = announce_when_ready(
            printer,
            9,
            RESTART_HEALTH_WAIT,
            Stop::fired(),
        )
        .expect("the announcer thread starts");
        handle.join().expect("the announcer thread ends");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the announcer kept probing for {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_model_server_that_will_not_spawn_is_a_warning_and_not_a_failed_boot() {
        // Every other "no model server" answer (missing binary, port already
        // serving) lets the app run; a spawn failure used to stop the whole start
        // with `?`. The log path is a directory here, so `llama::spawn` cannot even
        // open its output — the cheapest real spawn failure there is.
        let root = fresh_root("nospawn");
        let paths = Paths::at(&root);
        let mut config = Config::load_at(&paths).expect("a config can always be built");
        config.llama_port = free_port();
        std::fs::write(&paths.model_file, vec![7u8; 1024]).unwrap();
        // An executable `bin/llama-server`, so the failure is the log, not the binary.
        if !std::path::Path::new("/bin/echo").is_file() {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        std::fs::copy("/bin/echo", &paths.llama_local).expect("bin/llama-server is created");
        std::fs::create_dir_all(&paths.log_llama).expect("the log path is a directory");
        let model = model::resolve(&paths, &config).expect("the model file is there");
        let printer = Arc::new(Printer::new());
        let outcome = start_model(&printer, &paths, &config, &model, &Stop::idle());
        assert!(
            matches!(outcome, Ok(None)),
            "a model server that will not start must not stop the boot: {:?}",
            outcome.map(|handle| handle.map(|_| "a supervisor"))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_binary_is_one_calm_sentence_and_no_spawn_attempt() {
        // The third "no model server" answer, and the one a fresh checkout hits: the
        // binary was never built. It must be a named sentence — file, directory,
        // "nothing is downloaded" — and it must be the *end* of the model step: no
        // process is started, and no log file appears (an attempt to spawn creates
        // one through `llama::log_sink`, so the absence of the file is the measure
        // of "no spawn was attempted").
        let root = fresh_root("nobinary");
        let paths = Paths::at(&root);
        let mut config = Config::load_at(&paths).expect("a config can always be built");
        config.llama_port = free_port();
        std::fs::write(&paths.model_file, vec![7u8; 1024]).unwrap();
        let model = model::resolve(&paths, &config).expect("the model file is there");

        let sentence = missing_binary_line(&paths);
        assert!(sentence.contains(llama::BINARY_NAME), "{}", sentence);
        assert!(
            sentence.contains(&paths.llama_local.display().to_string()),
            "the sentence must name where the binary was looked for: {}",
            sentence
        );
        assert!(
            sentence.contains("nothing is downloaded"),
            "the sentence must say the app does not fetch a binary: {}",
            sentence
        );
        assert!(
            !sentence.contains('\n') && !sentence.contains('\r'),
            "one line: {:?}",
            sentence
        );
        assert!(!paths.log_llama.exists(), "the root starts with no model log");

        // A machine that already has a `llama-server` on PATH would legitimately
        // start it here, and this test must not start a model: the sentence is all
        // that is asserted in that case.
        if atlas_platform::executable::on_path(llama::BINARY_NAME).is_some() {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let printer = Arc::new(Printer::new());
        let outcome = start_model(&printer, &paths, &config, &model, &Stop::idle());
        assert!(
            matches!(outcome, Ok(None)),
            "a missing binary is a warning, not a failed boot: {:?}",
            outcome.map(|handle| handle.map(|_| "a supervisor"))
        );
        assert!(
            !paths.log_llama.exists(),
            "a spawn was attempted: {} exists",
            paths.log_llama.display()
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
