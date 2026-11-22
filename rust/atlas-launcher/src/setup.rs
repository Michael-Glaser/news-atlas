//! Provisioning: the model file and the model server, fetched once, before anything runs.
//!
//! The product is a window a person double-clicks, and the model is several gigabytes:
//! it cannot travel with the app. So the boot gets one step in front of everything else,
//! and it is the **only** place this binary can cause network traffic — when the model
//! file the app will load is missing, or `bin/llama-server` is, `atlas start` runs
//! `atlas-setup --install` as a child, waits for it, and only then resolves the model.
//!
//! Three rules, and each one is why this is a module and not three lines in `start`:
//!
//! * **The child does the work; this module decides nothing about it.** No URL, no file
//!   name, no size, no digest: `atlas-setup` holds the pin, prints what it does and
//!   verifies what arrived. This crate is not linked to it — it is *spawned*, like the
//!   window and the agent — which is what `atlas-guard`'s
//!   `the_install_tool_is_not_linked_into_the_app` pins.
//! * **Said out loud.** One line before it starts and one after. There is no progress
//!   bar here on purpose: `atlas-setup` writes its phases to `data/setup.json` and the
//!   window draws them, so a bar is never implemented twice and never disagrees with
//!   itself. The child inherits stdout and stderr, so a person at a terminal also sees
//!   its own lines.
//! * **Off on request.** `--no-install` provisions nothing, for a machine whose owner
//!   does it by hand; the boot then says so in one line and lets the existing
//!   no-model path report what is missing.
//!
//! What is *not* here: a retry (a download that failed says why, and the person decides),
//! a deadline (an install takes as long as the connection takes — a cap would abort a
//! legitimate first run), and any judgement about the model's contents.

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use atlas_config::{Config, Paths};

use crate::report::Printer;
use crate::signals::Stop;

/// The install tool's name, as `cargo build -p atlas-setup` writes it.
pub const BINARY_NAME: &str = "atlas-setup";

/// The one argument that means "do everything, no questions".
///
/// It is the tool's own spelling for the install (`atlas setup --install`, `atlas-setup
/// install` and `atlas-setup setup --install` are the same command there). Passing it is
/// the consent: the person started ATLAS, and the line printed before the child runs is
/// what they consented to.
pub const INSTALL_ARG: &str = "--install";

/// How often the wait looks at the child and at the stop flag.
///
/// A quarter second, like the window's wait: a Ctrl-C during a twenty-minute download is
/// noticed within one tick, and the child is asked to stop rather than killed.
pub const POLL: Duration = Duration::from_millis(250);

/// How long the install child gets to answer a stop before it is killed.
pub const STOP_GRACE: Duration = Duration::from_secs(10);

/// One piece of a working install, as a name a sentence can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Piece {
    /// The GGUF file the app will load.
    Model,
    /// `bin/llama-server`, the model server program.
    ModelServer,
}

impl Piece {
    /// The name used in every line about it.
    pub fn name(self) -> &'static str {
        match self {
            Piece::Model => "the model file",
            Piece::ModelServer => "bin/llama-server",
        }
    }

    /// Where the piece is expected, so a line can carry the path and not just the noun.
    pub fn path(self, paths: &Paths) -> PathBuf {
        match self {
            Piece::Model => paths.model_file.clone(),
            Piece::ModelServer => paths.llama_local.clone(),
        }
    }
}

/// What is on disk right now, measured — the whole input of the decision.
///
/// A value rather than a `fn(&Paths) -> bool` pair so the decision below is a pure
/// function a test can drive through all four cases without a file system, an install
/// tool, or a `PATH` that differs from machine to machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Present {
    /// Is there a model file this boot would load?
    pub model: bool,
    /// Is there a `bin/llama-server` (or one on `PATH`)?
    pub server: bool,
}

impl Present {
    /// Look at the root. Both answers come from the resolvers the boot itself uses, so
    /// "provisioning is not needed" cannot mean anything the model step disagrees with.
    pub fn scan(paths: &Paths, config: &Config) -> Present {
        Present {
            model: crate::model::resolve(paths, config).is_ok(),
            server: crate::llama::binary(paths).is_some(),
        }
    }

    /// The pieces that are missing, in the order a person reads them.
    pub fn missing(&self) -> Vec<Piece> {
        let mut out: Vec<Piece> = Vec::new();
        if !self.model {
            out.push(Piece::Model);
        }
        if !self.server {
            out.push(Piece::ModelServer);
        }
        out
    }

    /// Is everything the install would provide already here?
    pub fn complete(&self) -> bool {
        self.model && self.server
    }
}

/// What this start will do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provision {
    /// Nothing is missing: no child, no network, no build.
    Ready,
    /// These pieces are missing and the install tool will run for them.
    Install(Vec<Piece>),
    /// These pieces are missing and `--no-install` says the owner provisions by hand.
    ByHand(Vec<Piece>),
}

impl Provision {
    /// Does this start spawn an install child?
    ///
    /// The one question the boot, the plan and the tests all ask, so "no network at all
    /// on a second run" is one expression instead of three that can drift.
    pub fn spawns(&self) -> bool {
        matches!(self, Provision::Install(_))
    }

    /// The pieces this decision is about, or none when there is nothing to do.
    pub fn missing(&self) -> &[Piece] {
        match self {
            Provision::Ready => &[],
            Provision::Install(pieces) | Provision::ByHand(pieces) => pieces,
        }
    }
}

/// The decision: what is measured, and whether the owner allowed the install.
///
/// Pure, and separate from [`run`] because it is the part worth pinning: a missing model,
/// a missing server, both present, and the refused case are four answers, and the boot's
/// only choices about them are "run the child" or "do not".
pub fn decide(present: Present, install: bool) -> Provision {
    let missing = present.missing();
    if missing.is_empty() {
        return Provision::Ready;
    }
    if install {
        Provision::Install(missing)
    } else {
        Provision::ByHand(missing)
    }
}

/// Where the workspace puts its release binaries — the install tool's first home.
pub fn release_dir(paths: &Paths) -> PathBuf {
    paths.root.join("rust").join("target").join("release")
}

/// The install tool: this workspace's build first, then `PATH`, like the window.
pub fn binary(paths: &Paths) -> Option<PathBuf> {
    let local = atlas_platform::host::app_binary(release_dir(paths), BINARY_NAME);
    if atlas_platform::executable::is_executable(&local) {
        return Some(local);
    }
    atlas_platform::executable::on_path(BINARY_NAME)
}

/// `the model file (/…/models/….gguf) and bin/llama-server (/…/bin/llama-server)`.
///
/// Paths in full: a person reading the line may not be standing in the project directory,
/// and "the model is missing" without a path is the generic failure this project keeps
/// replacing.
pub fn pieces_text(pieces: &[Piece], paths: &Paths) -> String {
    let parts: Vec<String> = pieces
        .iter()
        .map(|piece| format!("{} ({})", piece.name(), piece.path(paths).display()))
        .collect();
    match parts.len() {
        0 => "nothing".to_string(),
        1 => parts[0].clone(),
        _ => {
            let (last, head) = parts.split_last().expect("two or more");
            format!("{} and {}", head.join(", "), last)
        }
    }
}

/// The line printed before the child starts.
pub fn before_line(missing: &[Piece], paths: &Paths) -> String {
    let verb = if missing.len() > 1 { "are" } else { "is" };
    format!(
        "{} {verb} not here yet: {} {} runs once — a multi-gigabyte download, the one time \
         this machine touches the network — and writes its progress to {}, which the window \
         reads",
        pieces_text(missing, paths),
        BINARY_NAME,
        INSTALL_ARG,
        setup_json(paths).display()
    )
}

/// The line printed once the child has succeeded and the pieces are measured again.
pub fn after_line(present: Present, paths: &Paths) -> String {
    format!(
        "{} {} in place now, so this step does not run on the next start",
        pieces_text(&present.installed(), paths),
        if present.installed().len() > 1 {
            "are"
        } else {
            "is"
        }
    )
}

impl Present {
    /// The pieces that are here — what an "after" line may claim.
    fn installed(&self) -> Vec<Piece> {
        let mut out: Vec<Piece> = Vec::new();
        if self.model {
            out.push(Piece::Model);
        }
        if self.server {
            out.push(Piece::ModelServer);
        }
        out
    }
}

/// Where the install tool says what it is doing.
pub fn setup_json(paths: &Paths) -> PathBuf {
    paths.data_dir.join("setup.json")
}

/// The refusal when the install tool itself is not on this machine.
///
/// One sentence, no newline: it goes through `report::write_line`, which refuses a second
/// line and every control character in a path. It names the file, where it was looked for,
/// the build command, what is missing, and the switch for a machine that provisions by
/// hand — because that switch is the only way out of this state without a build.
pub fn missing_line(paths: &Paths, missing: &[Piece]) -> String {
    format!(
        "{} was not found in {} or on PATH, so {} cannot be provisioned. Build it with \
         `cargo build --release --offline --locked -p {}` in rust/, or run `atlas start \
         --no-install` and place the pieces yourself; nothing is fetched by this launcher, \
         so `atlas start` stops here",
        BINARY_NAME,
        release_dir(paths).display(),
        pieces_text(missing, paths),
        BINARY_NAME
    )
}

/// The refusal when the child ran and the pieces are still missing.
///
/// A child that exits 0 and leaves the model absent is the same answer as a child that
/// exits 1: the install did not happen, and this boot must not go on to open a window that
/// cannot think. The status is carried so the sentence says which of the two it was.
pub fn failed_line(status: &ExitStatus, still: &[Piece], paths: &Paths) -> String {
    format!(
        "{} ended with {status} and {} {} still missing, so nothing else was started — no \
         model server, no agent, no window. Its own output above names the step it stopped \
         on",
        BINARY_NAME,
        pieces_text(still, paths),
        if still.len() > 1 { "are" } else { "is" }
    )
}

/// How waiting for the install child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// It exited; the status decides whether the boot goes on.
    Done(ExitStatus),
    /// A stop arrived and the child was asked to stop, then killed if it would not.
    Stopped,
}

/// What the boot should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Everything was already there (or the install was not needed).
    Ready,
    /// The child ran and the pieces are here now.
    Installed,
    /// The person stopped ATLAS while it was provisioning; nothing else was started.
    Stopped,
}

/// Provision this root, or refuse in one sentence. The boot's step, in one place.
///
/// The order is the promise: measure, decide, say what will happen, run the child, measure
/// again, and only claim success for a piece that is on disk *now*. `Err` always means the
/// same thing: nothing else was started.
pub fn run(
    printer: &Printer,
    paths: &Paths,
    config: &Config,
    install: bool,
    stop: &Stop,
) -> Result<Outcome, String> {
    let present = Present::scan(paths, config);
    let provision = decide(present, install);
    if provision.spawns() {
        printer.note(before_line(provision.missing(), paths));
        match launch(printer, paths, provision.missing(), stop)? {
            Ending::Stopped => return Ok(Outcome::Stopped),
            Ending::Done(status) => {
                // Measured again, never assumed: an installer that exits 0 and a file
                // that is not there is exactly the failure this step exists to catch.
                let after = Present::scan(paths, config);
                if !after.complete() {
                    return Err(failed_line(&status, &after.missing(), paths));
                }
                printer.note(after_line(after, paths));
                return Ok(Outcome::Installed);
            }
        }
    }
    // The refused case is said out loud rather than passed over: a person who typed
    // `--no-install` gets one line naming what the next steps will report, and the boot
    // keeps its old behaviour for a hand-provisioned machine (a missing model is the
    // model step's sentence, a missing server is the model server's warning).
    if let Provision::ByHand(missing) = &provision {
        printer.note(format!(
            "provisioning is off (--no-install): {} {} missing, and the steps below say what \
             each one needs",
            pieces_text(missing, paths),
            if missing.len() > 1 { "are" } else { "is" }
        ));
    }
    Ok(Outcome::Ready)
}

/// Spawn `atlas-setup --install`, let it talk to this terminal, and wait for it.
///
/// The spawn boundary is `atlas-platform`'s (`env_clear` plus the curated allowlist, no
/// console window on Windows); the working directory is the root, so the child's `--root`
/// and its cwd agree; stdout and stderr are inherited, because the human watching the
/// download is the one who can diagnose it.
fn launch(
    printer: &Printer,
    paths: &Paths,
    missing: &[Piece],
    stop: &Stop,
) -> Result<Ending, String> {
    let program = binary(paths).ok_or_else(|| missing_line(paths, missing))?;
    if let Some(ending) = already_requested(stop, printer, &program) {
        return Ok(ending);
    }
    let mut command = atlas_platform::child::command_with(&program, &atlas_config::spawn_env(&[]));
    command
        .arg(INSTALL_ARG)
        .arg("--root")
        .arg(&paths.root)
        .current_dir(&paths.root)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut child = command.spawn().map_err(|error| {
        format!(
            "{} will not start ({}), so {} cannot be provisioned and nothing else was \
             started. Check that the file is an executable for this platform, or run \
             `atlas start --no-install` and place the pieces yourself",
            program.display(),
            error,
            pieces_text(missing, paths)
        )
    })?;
    let pid = child.id();
    printer.note(format!(
        "{} {} --root {} (pid {}) — its own output follows",
        program.display(),
        INSTALL_ARG,
        paths.root.display(),
        pid
    ));
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Ending::Done(status)),
            Ok(None) => {
                if stop.requested() {
                    // Asked to stop, not killed on arrival: the child is finishing a
                    // partial file it knows how to resume from, and `terminate` gives it
                    // the grace before the kill.
                    let sentence = crate::child::terminate(&mut child, STOP_GRACE);
                    printer.warn(format!(
                        "a stop was requested, so provisioning was ended ({}); the next \
                         start picks it up where it stopped",
                        sentence
                    ));
                    return Ok(Ending::Stopped);
                }
                std::thread::sleep(POLL);
            }
            Err(error) => {
                return Err(format!(
                    "the exit status of {} (pid {}) cannot be read ({}), so ATLAS cannot \
                     tell whether {} was installed; nothing else was started",
                    BINARY_NAME,
                    pid,
                    error,
                    pieces_text(missing, paths)
                ))
            }
        }
    }
}

/// A stop that arrived before the child existed: spawn nothing, say so.
///
/// Without this, a Ctrl-C during the settings step would still start an install and then
/// have to end it: a download begun for a boot that is already over.
fn already_requested(stop: &Stop, printer: &Printer, program: &Path) -> Option<Ending> {
    if !stop.requested() {
        return None;
    }
    printer.warn(format!(
        "a stop was requested before provisioning began, so {} was not started",
        program.display()
    ));
    Some(Ending::Stopped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A temp project root with `data/`, `models/` and `bin/`.
    ///
    /// Nothing here ever resolves the *real* install tool: the temp root's own
    /// `target/release` is where a stand-in is written, so a test can never reach the
    /// binary that would download 6.5 GB.
    fn fresh_root(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "atlas-setup-{}-{}-{}",
            tag,
            std::process::id(),
            stamp
        ));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["data", "models", "bin"] {
            let _ = std::fs::create_dir_all(dir.join(sub));
        }
        dir
    }

    fn config_at(paths: &Paths) -> Config {
        Config::load_at(paths).expect("a config can always be built")
    }

    /// A model file the resolver accepts (it only asks for a non-empty `.gguf`) and an
    /// executable `bin/llama-server`, so "everything is present" means present to the
    /// same resolvers the boot uses.
    fn provision_by_hand(paths: &Paths) {
        std::fs::write(&paths.model_file, vec![7u8; 4096]).expect("a model file");
        let server = paths.llama_local.clone();
        std::fs::write(&server, b"#!/bin/sh\nexit 0\n").expect("a model server");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut mode = std::fs::metadata(&server).expect("mode").permissions();
            mode.set_mode(0o755);
            std::fs::set_permissions(&server, mode).expect("chmod");
        }
    }

    /// A stand-in install tool in the root's own `target/release`, doing what the test
    /// says: writing a marker every time it runs, so "how many children started?" is a
    /// line count and not an assumption.
    fn stand_in(paths: &Paths, body: &str) -> PathBuf {
        let dir = release_dir(paths);
        std::fs::create_dir_all(&dir).expect("release dir");
        let program = atlas_platform::host::app_binary(&dir, BINARY_NAME);
        std::fs::write(&program, format!("#!/bin/sh\n{}\n", body)).expect("the stand-in");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut mode = std::fs::metadata(&program).expect("mode").permissions();
            mode.set_mode(0o755);
            std::fs::set_permissions(&program, mode).expect("chmod");
        }
        program
    }

    fn marker(paths: &Paths) -> PathBuf {
        paths.data_dir.join("install-ran.txt")
    }

    fn runs(paths: &Paths) -> usize {
        std::fs::read_to_string(marker(paths))
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    // ------------------------------------------------------------------
    // the decision: four answers, no file system, no child
    // ------------------------------------------------------------------

    #[test]
    fn a_missing_model_is_an_install_and_so_is_a_missing_model_server() {
        // Both missing, model missing, server missing: three states, one answer each,
        // and the pieces are named so the sentence can carry the paths.
        assert_eq!(
            decide(Present { model: false, server: false }, true),
            Provision::Install(vec![Piece::Model, Piece::ModelServer])
        );
        assert_eq!(
            decide(Present { model: false, server: true }, true),
            Provision::Install(vec![Piece::Model])
        );
        assert_eq!(
            decide(Present { model: true, server: false }, true),
            Provision::Install(vec![Piece::ModelServer])
        );
        // The order is the line's order, and the model is first because it is the piece
        // that takes the time.
        assert_eq!(Piece::Model.name(), "the model file");
        assert_eq!(Piece::ModelServer.name(), "bin/llama-server");
    }

    #[test]
    fn both_pieces_present_provisions_nothing() {
        // Measured today: a second start on a provisioned root has nothing to do, so it
        // must decide `Ready` — which is the whole of "no network on a second run".
        let provision = decide(Present { model: true, server: true }, true);
        assert_eq!(provision, Provision::Ready);
        assert!(!provision.spawns(), "a complete root must not start a child");
        assert!(provision.missing().is_empty());
        assert!(Present { model: true, server: true }.complete());
        assert!(!Present { model: true, server: false }.complete());
    }

    #[test]
    fn a_refused_install_names_the_pieces_and_starts_nothing() {
        // `--no-install`: the pieces are still named (the boot says in one line what the
        // steps below will report), and no child is started — the switch is the promise.
        let provision = decide(Present { model: false, server: false }, false);
        assert_eq!(
            provision,
            Provision::ByHand(vec![Piece::Model, Piece::ModelServer])
        );
        assert!(!provision.spawns(), "--no-install must not spawn anything");
        assert_eq!(provision.missing().len(), 2);
        // Refusing when nothing is missing is still "nothing to do": a person who passes
        // --no-install on a provisioned machine must not be told something is missing.
        assert_eq!(decide(Present { model: true, server: true }, false), Provision::Ready);
    }

    // ------------------------------------------------------------------
    // the step itself, against a stand-in child
    // ------------------------------------------------------------------

    #[test]
    fn a_complete_root_starts_no_install_child_at_all() {
        // The requirement, as a measurement: a root with both pieces present runs *zero*
        // provisioning children. The stand-in would write a marker and exit 0; the only
        // thing this test has to prove is that it never ran — and that it *would* have
        // been found, so "not started" is not "not resolved".
        let root = fresh_root("complete");
        let paths = Paths::at(&root);
        let config = config_at(&paths);
        provision_by_hand(&paths);
        stand_in(&paths, &format!("echo ran >> {}", marker(&paths).display()));
        assert!(
            binary(&paths).is_some(),
            "the stand-in must be resolvable, or this test proves nothing about spawning"
        );
        assert!(Present::scan(&paths, &config).complete(), "the root is complete");

        let printer = Printer::new();
        let outcome = run(&printer, &paths, &config, true, &Stop::idle()).unwrap();
        assert_eq!(outcome, Outcome::Ready);
        assert_eq!(runs(&paths), 0, "an install child was started for a complete root");
        drop(printer);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_piece_runs_the_child_once_and_a_piece_it_did_not_place_is_a_refusal() {
        // The child exits 0 and places nothing: exactly the "installer said fine, the
        // file is not there" case. It must be a refusal that says what is missing and
        // that nothing else was started — never a boot that goes on to a window which
        // cannot think.
        let root = fresh_root("noop");
        let paths = Paths::at(&root);
        let config = config_at(&paths);
        stand_in(&paths, &format!("echo ran >> {}", marker(&paths).display()));
        let printer = Printer::new();
        let error = run(&printer, &paths, &config, true, &Stop::idle())
            .expect_err("a child that installed nothing is a refusal");
        assert_eq!(runs(&paths), 1, "the child must run exactly once");
        assert!(error.contains(BINARY_NAME), "{}", error);
        assert!(error.contains("still missing"), "{}", error);
        assert!(
            error.contains("nothing else was started"),
            "the refusal has to say the rest did not happen: {}",
            error
        );
        assert!(
            error.contains(&paths.model_file.display().to_string()),
            "the refusal names the file by path: {}",
            error
        );
        assert!(!error.contains('\n'), "one sentence: {:?}", error);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_install_that_places_the_pieces_is_measured_and_not_assumed() {
        // The child creates both pieces and exits 0: the step must say `Installed` — and
        // it only can because it measured the root again *after* the child, which is the
        // difference between "the installer claims" and "the file is there".
        let root = fresh_root("places");
        let paths = Paths::at(&root);
        let config = config_at(&paths);
        let model = paths.model_file.display().to_string();
        let server = paths.llama_local.display().to_string();
        stand_in(
            &paths,
            &format!(
                "echo ran >> {}\nprintf 'x' > '{model}'\nprintf '#!/bin/sh\\n' > '{server}'\nchmod 755 '{server}'",
                marker(&paths).display()
            ),
        );
        let printer = Printer::new();
        let outcome = run(&printer, &paths, &config, true, &Stop::idle()).unwrap();
        assert_eq!(outcome, Outcome::Installed);
        assert_eq!(runs(&paths), 1);
        assert!(Present::scan(&paths, &config).complete());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stop_before_the_child_exists_starts_nothing() {
        // A Ctrl-C during the settings step must not begin a twenty-minute download for
        // a boot that is already over.
        let root = fresh_root("stopped");
        let paths = Paths::at(&root);
        let config = config_at(&paths);
        stand_in(&paths, &format!("echo ran >> {}", marker(&paths).display()));
        let printer = Printer::new();
        let outcome = run(&printer, &paths, &config, true, &Stop::fired()).unwrap();
        assert_eq!(outcome, Outcome::Stopped);
        assert_eq!(runs(&paths), 0, "a stop must not start the install");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_refused_install_runs_nothing_and_leaves_the_missing_piece_to_its_own_step() {
        // `--no-install` on an unprovisioned root: no child, and `Outcome::Ready` so the
        // boot continues to the model step, whose sentence already names the file, the
        // URL and the manual step.
        let root = fresh_root("byhand");
        let paths = Paths::at(&root);
        let config = config_at(&paths);
        stand_in(&paths, &format!("echo ran >> {}", marker(&paths).display()));
        let printer = Printer::new();
        let outcome = run(&printer, &paths, &config, false, &Stop::idle()).unwrap();
        assert_eq!(outcome, Outcome::Ready);
        assert_eq!(runs(&paths), 0, "--no-install started the child");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_install_tool_is_one_sentence_with_the_path_and_the_switch() {
        let root = fresh_root("notool");
        let paths = Paths::at(&root);
        let missing = [Piece::Model, Piece::ModelServer];
        // Only meaningful when this machine has no install tool on PATH; the resolver's
        // own fallback is what would find one.
        let sentence = missing_line(&paths, &missing);
        assert!(sentence.contains(BINARY_NAME), "{}", sentence);
        assert!(
            sentence.contains(&release_dir(&paths).display().to_string()),
            "the sentence names where it looked: {}",
            sentence
        );
        assert!(sentence.contains("--no-install"), "{}", sentence);
        assert!(
            sentence.contains(&paths.model_file.display().to_string()),
            "the sentence names the missing piece: {}",
            sentence
        );
        assert!(!sentence.contains('\n') && !sentence.contains('\r'), "{:?}", sentence);

        // And the boot's answer for that state: a refusal, before anything is started.
        let printer = Printer::new();
        if binary(&paths).is_none() {
            let error = run(&printer, &paths, &config_at(&paths), true, &Stop::idle())
                .expect_err("no install tool and nothing installed is a refusal");
            assert!(error.contains(BINARY_NAME), "{}", error);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_lines_name_the_pieces_their_paths_and_the_progress_file() {
        let root = fresh_root("lines");
        let paths = Paths::at(&root);
        let missing = [Piece::Model, Piece::ModelServer];
        let before = before_line(&missing, &paths);
        assert!(before.contains(&paths.model_file.display().to_string()), "{}", before);
        assert!(before.contains(&paths.llama_local.display().to_string()), "{}", before);
        assert!(before.contains(INSTALL_ARG), "{}", before);
        assert!(
            before.contains(&setup_json(&paths).display().to_string()),
            "the line says where the progress goes: {}",
            before
        );
        assert!(before.contains("the network"), "{}", before);
        assert!(!before.contains('\n'), "{:?}", before);

        // The after line claims only what was measured: one piece here, and it says so.
        let after = after_line(Present { model: true, server: false }, &paths);
        assert!(after.contains("is in place"), "{}", after);
        assert!(!after.contains("bin/llama-server"), "{}", after);
        let both = after_line(Present { model: true, server: true }, &paths);
        assert!(both.contains("are in place"), "{}", both);
        let _ = std::fs::remove_dir_all(&root);
    }
}
