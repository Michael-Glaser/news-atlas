//! `llama-server`: which binary, and which flags.
//!
//! This is the port of everything `run.sh` and `ai.ensure_server` did around the
//! model process, up to the argv: [`binary`] finds the executable and [`invocation`]
//! writes the command line. What happens to the child's own output — and what to do
//! when the binary refuses `--log-disable` — is [`crate::silence`]. The log sink and
//! the spawn stay here, with [`Command`]: a spawn site is one reviewed file, and
//! `atlas-guard`'s audit names it.
//!
//! ## The flags, and why each one is there
//!
//! ```text
//! -m <model>  -c <model_context>  -ngl <model_gpu_layers>  -t <model_threads>
//! --host 127.0.0.1  --port <llama_port>  -np 1  --jinja
//! ```
//!
//! * `--host 127.0.0.1` is written out rather than relied on. llama.cpp's default
//!   has been loopback and could change; this app starts a listener, so the
//!   address is pinned by the caller, in the argv, every time.
//! * `-c`, `-ngl` and `-t` come from `data/config.json` (`model_context`,
//!   `model_gpu_layers`, `model_threads`), through [`argv`] — one builder, and the
//!   numbers are validated and clamped by `atlas-config` before they get here, so
//!   a value and the flag it becomes cannot disagree. `-ngl` is left out at -1 and
//!   `-t` at 0: those are llama.cpp's own choices, and passing `-ngl -1` would be
//!   this app overriding a decision it says it is not making.
//! * `-np 1` is one slot, and that is structural, not a setting. llama.cpp divides
//!   `-c` by `-np`: `-c 6144 -np 4` is 1 536 tokens per slot, and the system prompt
//!   plus the twenty-three tool schemas do not fit in that — chat would answer "the
//!   local model ran out of context" on its first turn. The window (`-c`) is the
//!   reader's; the number of slots is the agent loop's, and the loop has one model
//!   server and speaks to it one request at a time.
//! * `--jinja` is what makes tool calling work with this model's chat template.
//!
//! `--no-webui` and the MTP flag are *build dependent*, so they are asked for
//! first: `llama-server --help` is run once and its text is searched for the flag.
//! A build that has neither simply gets neither — no version guessing, no failing
//! start on an unknown option.
//!
//! `--log-disable` is **not** build dependent in that sense. It is passed always,
//! because a build whose `--help` probe failed is exactly the build that would
//! otherwise log the prompt into `data/llama.log`; a build that refuses the flag
//! says so within a second, and [`crate::silence::start`] starts it once more
//! without it and marks the result unsilenced so the boot can say so out loud.
//!
//! Nothing here reads a page, a feed or a prompt: the only strings that reach a
//! log are the binary path, the port and the model file name.
//!
//! ## The model path is one argument, and the log is one file
//!
//! The model path comes from a setting (`model_local`) or from a file name in
//! `models/`, so it is a string this crate did not author. Two rules hold for it:
//!
//! * **argv is a list, never a line.** `-m` and its value are two entries of a
//!   `Vec<String>`, so a path holding a space, a quote, a newline or a `--host` of
//!   its own cannot become a second flag: no shell parses it, and neither does
//!   `llama-server`'s own parser see anything but one string. `the_model_path_is_\
//!   one_argument` pins that. The value is printed shortened to its file name —
//!   and the printer strips control characters, so the name cannot forge a line
//!   (see `report`).
//! * **the log is opened as this file.** `data/llama.log` receives everything the
//!   child writes, and `--log-disable` is passed so that "everything" is not the
//!   prompt: the flag is in the argv on every start, and a build that refuses it is
//!   restarted once without it with the boot saying the model's own log is not
//!   silenced ([`crate::silence::start`]). The file is created `0600` from the first byte (not
//!   `chmod`ed afterwards, which left it world-readable for as long as the create
//!   took) and it is opened with `O_NOFOLLOW`: a symlink planted at that name would
//!   otherwise redirect the model's output into a file of somebody else's choosing.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use atlas_config::{Config, Paths};

use crate::child;
use crate::supervise::SpawnedChild;

/// Parallel slots. One: llama.cpp splits `-c` between them, and a split 8 192 is
/// too small for the tool schemas (`atlas_ai::agent::TOTAL_TOKEN_BUDGET`).
pub const SLOTS: &str = "1";

/// The loopback literal the model server must bind.
pub const HOST: &str = "127.0.0.1";

/// Where the model server binary is expected.
pub const BINARY_NAME: &str = "llama-server";

/// One `--help` probe may take this long before the probe is killed.
pub const HELP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// The flag that stops llama.cpp's own logging, prompt text included.
///
/// Passed always — see [`crate::llama::invocation`] — and dropped exactly once when
/// the binary refuses it (see [`start`]).
pub const LOG_DISABLE: &str = "--log-disable";


/// The binary: `bin/llama-server` first, then a `llama-server` on this machine.
///
/// The second path is not a download and not a build: it is the binary a user
/// already installed with their package manager or their own CMake run. Nothing is
/// started here — [`spawn`] does that, with the pinned flags.
pub fn binary(paths: &Paths) -> Option<PathBuf> {
    // The pinned path: `bin/llama-server[.exe]`, with the suffix from
    // `atlas-platform` rather than a `cfg!` that could disagree with the layout.
    let pinned = atlas_platform::host::app_binary(&paths.root.join("bin"), BINARY_NAME);
    for candidate in [&paths.llama_local, &pinned] {
        if atlas_platform::executable::is_executable(candidate) {
            return Some(candidate.clone());
        }
    }
    // Then the machine's own `PATH`: `;` or `:` and the `PATHEXT` rule are the
    // platform's, so `llama-server` becomes `llama-server.exe` where it must.
    atlas_platform::executable::on_path(BINARY_NAME)
}

/// The command line for one model file on one loopback port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub program: PathBuf,
    pub model: PathBuf,
    pub port: u16,
    pub args: Vec<String>,
}

impl Invocation {
    /// The argv line as a person reads it (the model path is shortened to its file
    /// name so the line stays one line).
    pub fn text(&self) -> String {
        let mut parts: Vec<String> = vec![
            self.program
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| self.program.display().to_string()),
        ];
        let model_name = self
            .model
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| self.model.display().to_string());
        let mut skip_model = false;
        for arg in &self.args {
            if skip_model {
                parts.push(model_name.clone());
                skip_model = false;
                continue;
            }
            if arg == "-m" {
                skip_model = true;
            }
            parts.push(arg.clone());
        }
        parts.join(" ")
    }

    /// Did this invocation ask for the MTP draft head?
    pub fn uses_mtp(&self) -> bool {
        self.args.iter().any(|arg| arg == "--mtp")
            || self
                .args
                .windows(2)
                .any(|pair| pair[0] == "--spec-type" && pair[1] == "draft-mtp")
    }
}

/// The model server's argv for one configuration, in the order a person reads it.
///
/// **One builder.** The window shows this command line before anything is started
/// (`atlas-desktop/src/views/settings.rs`), so the flags and their settings exist here
/// once: `-m`, `-c` and the two conditional numbers come from `data/config.json`
/// ([`atlas_config::Config`]), which is also what the window edits — a preview drawn from
/// a second copy is a preview that can disagree with the process it describes.
///
/// `help` is the model server's own `--help` text, `""` when it could not be asked. Only
/// `--no-webui` and the MTP flag depend on it; everything else is passed whatever the
/// probe answered (see the module doc). A caller that cannot spawn — the window — passes
/// `""` and gets exactly the flags this app always passes.
///
/// `-ngl` is absent at -1 and `-t` at 0: both mean "llama.cpp decides", and a flag that
/// says "you decide" in a number llama.cpp would have to interpret is this app pretending
/// to have an opinion it does not have.
pub fn argv(config: &Config, model: &Path, port: u16, help: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-m".to_string(),
        model.display().to_string(),
        "-c".to_string(),
        config.model_context.to_string(),
    ];
    if config.model_gpu_layers >= 0 {
        args.push("-ngl".to_string());
        args.push(config.model_gpu_layers.to_string());
    }
    if config.model_threads > 0 {
        args.push("-t".to_string());
        args.push(config.model_threads.to_string());
    }
    args.extend([
        "--host".to_string(),
        HOST.to_string(),
        "--port".to_string(),
        port.to_string(),
        // One slot: llama.cpp divides `-c` between slots, and the agent loop speaks to
        // this server one request at a time (see the module doc).
        "-np".to_string(),
        SLOTS.to_string(),
        "--jinja".to_string(),
    ]);
    // Always, whatever `--help` said. llama.cpp logs the prompt at its default
    // verbosity and the prompt carries page text, so a build that was never asked
    // is a build that may write an article into a file this app owns. The flag is
    // *asked for* rather than offered, and a build that does not know it is handled
    // where the refusal is observable — [`start`], which drops it once and says so.
    //
    // This used to be `if help.contains("--log-disable")`, which is silence only
    // for the builds whose `--help` probe answered: a probe that timed out or
    // exited oddly left the model logging prompts (THREAT-MODEL §4.9).
    args.push(LOG_DISABLE.to_string());
    if help.contains("--no-webui") {
        // The model server has its own browser UI. ATLAS has one UI; this one is
        // not opened, not reachable and not a second surface to keep patched.
        args.push("--no-webui".to_string());
    }
    if help.contains("draft-mtp") {
        args.push("--spec-type".to_string());
        args.push("draft-mtp".to_string());
    } else if help.contains("--mtp") {
        args.push("--mtp".to_string());
    }
    args
}

/// [`argv`], with the program and the port the caller resolved — the shape the boot
/// spawns and the supervisor restarts with.
///
/// The flags are not decided here: this is the two facts a caller owns (which binary, which
/// port) joined to the one builder, so a restart re-uses the same command line it was
/// started with.
pub fn invocation(config: &Config, program: &Path, model: &Path, port: u16, help: &str) -> Invocation {
    Invocation {
        program: program.to_path_buf(),
        model: model.to_path_buf(),
        port,
        args: argv(config, model, port, help),
    }
}

/// The binary's own help text, or `""` when it will not answer.
///
/// A probe failure is not fatal: the flags that are certain are still passed.
pub fn help_text(program: &Path) -> String {
    match child::capture(program, &["--help".to_string()], HELP_TIMEOUT) {
        Ok(text) => text,
        Err(_) => String::new(),
    }
}

/// The file the child's stdout and stderr go to: `data/llama.log`, opened as
/// *this* file, `0600` from the first byte.
///
/// Three decisions, in one place because they are one decision — the child's
/// output is a file this app owns and nobody else may read or redirect:
///
/// * `atlas_platform::private::create_options` sets the mode at creation, so there
///   is no window in which the file exists `0644` (the default `umask`) while the
///   prompt text is already being appended — the old code opened it and then called
///   `ensure_file` to `chmod` it, and ignored the result.
/// * `no_follow_flag` is `O_NOFOLLOW`: `data/` is ours and `0700`, but a link at
///   this name would still send everything the model writes into a file of the
///   planter's choosing.
/// * `append`, so a restart adds to the log instead of erasing the reason the first
///   one died.
///
/// And one more, which used to be missing: **the log is bounded across runs.**
/// `append` means every start adds to the file, and nothing ever removed a byte of
/// it — a model that cannot load is a restart loop, so the file grew by one full
/// load per attempt, on the same disk as the archive. [`bound_child_log`] rotates a
/// file that has reached the cap to `llama.log.1` *before* the handle is opened,
/// which is the only moment this process can act on a file the child writes to.
/// It rotates a regular file only: a link planted at this name is left for the
/// `O_NOFOLLOW` open below to refuse, because moving an attack out of the way is
/// how an attack goes unnoticed.
pub fn log_sink(log: &Path) -> Result<std::fs::File, String> {
    if let Some(parent) = log.parent() {
        atlas_config::ensure_dir(parent);
    }
    // Before the handle exists. A log that cannot be rotated is appended to as
    // before: `bound_child_log` answers `false` rather than failing a start.
    let _ = crate::childlog::bound_child_log(log);
    let mut options: std::fs::OpenOptions = atlas_platform::private::create_options();
    options.append(true);
    let flag = atlas_platform::private::no_follow_flag();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(flag);
    }
    options.open(log).map_err(|error| {
        format!(
            "cannot open {}: {}. It must be a regular file inside data/ that this account can \
             write (a symbolic link is refused: it would send the model's output somewhere \
             else)",
            log.display(),
            error
        )
    })
}

/// Start the model server as a child of this process.
///
/// The environment is [`atlas_config::spawn_env`] and nothing else, the working
/// directory is the project root, stdin is closed, and stdout+stderr go to
/// `data/llama.log` — the file the app has always pointed a user at. No shell.
pub fn spawn(invocation: &Invocation, root: &Path, log: &Path) -> Result<SpawnedChild, String> {
    let sink = log_sink(log)?;
    let second = sink
        .try_clone()
        .map_err(|error| format!("cannot share {}: {}", log.display(), error))?;
    let mut command = Command::new(&invocation.program);
    command
        .args(&invocation.args)
        .current_dir(root)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::from(sink))
        .stderr(Stdio::from(second));
    for (name, value) in atlas_config::spawn_env(&[]) {
        command.env(name, value);
    }
    let child = command
        .spawn()
        .map_err(|error| format!("cannot start {}: {}", invocation.program.display(), error))?;
    Ok(SpawnedChild::new(child))
}

#[cfg(test)]
mod tests {
    use super::*;
    // The two helpers the silence module owns; this test is about the *argv*, so it
    // asks them rather than re-writing the rule.
    use crate::silence::{asks_for_silence, without_log_disable};
    use crate::tests::fresh_root;

    fn program() -> PathBuf {
        PathBuf::from("/opt/atlas/bin/llama-server")
    }

    fn model() -> PathBuf {
        PathBuf::from("/opt/atlas/models/qwen.gguf")
    }

    /// The settings the argv is built from, at their defaults — `Config::build` is pure, so
    /// this touches no file and reads no environment.
    fn config() -> Config {
        let paths = Paths::at(std::env::temp_dir().join("atlas-llama-argv"));
        // `read_stored` on a directory that does not exist answers an empty map, so this
        // is the pure `build` with no file and no environment: the defaults, exactly.
        let stored = atlas_config::read_stored(&paths).expect("no file, no problem");
        Config::build(&paths, &[], &stored)
    }

    /// The argv a default configuration produces — the flags, nothing else.
    fn default_args() -> Vec<String> {
        invocation(&config(), &program(), &model(), 8080, "").args
    }

    const HELP_WITH_EVERYTHING: &str = "\
usage: llama-server [options]
  --host HOST                 or -h
  --port PORT
  --no-webui                  disable the Web UI
  --log-disable               log disable
  --spec-type TYPE            drafting type: none, draft-mtp, ...
";

    #[test]
    fn the_documented_flags_are_always_passed_and_bound_to_loopback() {
        let invocation = invocation(&config(), &program(), &model(), 8080, "");
        let joined = invocation.args.join(" ");
        assert!(joined.starts_with("-m /opt/atlas/models/qwen.gguf -c 32768"), "{}", joined);
        assert!(joined.contains("--host 127.0.0.1"), "{}", joined);
        assert!(joined.contains("--port 8080"), "{}", joined);
        assert!(joined.contains("-np 1"), "{}", joined);
        assert!(joined.contains("--jinja"), "{}", joined);
        // Without a help text, a build-dependent flag is not guessed at. This
        // assertion used to name `--log-disable` too, which was the obsolete half
        // of the expectation: a flag that is only passed when the probe answers is
        // a prompt in `data/llama.log` for every build that could not be asked (see
        // `the_model_server_cannot_be_told_to_log_a_prompt`). It is passed always
        // now, and the refusal is handled on the child's exit, so it is asserted
        // *present* here.
        assert!(!joined.contains("--no-webui"), "{}", joined);
        assert!(joined.contains(LOG_DISABLE), "{}", joined);
        assert!(!joined.contains("mtp"), "{}", joined);
        assert!(!invocation.uses_mtp());
        assert_eq!(invocation.port, 8080);
    }

    #[test]
    fn each_model_flag_is_the_setting_it_comes_from() {
        // The argv is built from `data/config.json` — the same numbers the window offers —
        // so each flag is pinned to its field here. A hardcoded `-c 32768` is exactly what
        // this replaced: the window could say 65536 and the server would start with 32768.
        let mut settings = config();
        settings.model_context = 65_536;
        settings.model_gpu_layers = 48;
        settings.model_threads = 16;
        let built = invocation(&settings, &program(), &model(), 8080, "").args;
        let joined = built.join(" ");
        assert!(joined.starts_with("-m /opt/atlas/models/qwen.gguf -c 65536"), "{}", joined);
        assert!(joined.contains("-ngl 48"), "{}", joined);
        assert!(joined.contains("-t 16"), "{}", joined);
        // In the order the window prints them, right after `-c`: the preview it shows and
        // the process this builds are one command line, and a reader compares them.
        let c = built.iter().position(|arg| arg == "-c").expect("-c");
        let ngl = built.iter().position(|arg| arg == "-ngl").expect("-ngl");
        let t = built.iter().position(|arg| arg == "-t").expect("-t");
        let host = built.iter().position(|arg| arg == "--host").expect("--host");
        assert!(c < ngl && ngl < t && t < host, "{:?}", built);
        // The two token budgets are ATLAS's, not llama.cpp's: they must not reach the argv.
        assert!(!joined.contains("8192"), "a budget leaked into the argv: {}", joined);
    }

    #[test]
    fn an_unset_layer_count_and_thread_count_add_no_flag() {
        // -1 and 0 both mean "llama.cpp decides". Passing `-ngl -1` or `-t 0` would be this
        // app answering the question it says it is leaving open — and on a build that
        // validates the range, a refusal at start.
        let settings = config();
        assert_eq!(settings.model_gpu_layers, -1, "the default is llama.cpp's choice");
        assert_eq!(settings.model_threads, 0, "the default is llama.cpp's choice");
        let built = default_args();
        assert!(
            !built.iter().any(|arg| arg == "-ngl"),
            "the default argv passes -ngl: {:?}",
            built
        );
        assert!(
            !built.iter().any(|arg| arg == "-t"),
            "the default argv passes -t: {:?}",
            built
        );
        // 0 layers is a value ("CPU only"), not an absence: it is passed.
        let mut cpu = config();
        cpu.model_gpu_layers = 0;
        assert!(invocation(&cpu, &program(), &model(), 8080, "")
            .args
            .join(" ")
            .contains("-ngl 0"));
    }

    #[test]
    fn the_context_is_one_number_and_the_slots_are_one_slot() {
        // The rule the module doc states: the window (`-c`) is the reader's number, the
        // number of slots is structural. A second `-c` or a `-np` other than 1 would mean
        // the agent loop was given a server that splits its context.
        let mut settings = config();
        settings.model_context = 131_072;
        let built = invocation(&settings, &program(), &model(), 8080, "").args;
        assert_eq!(built.iter().filter(|arg| *arg == "-c").count(), 1, "{:?}", built);
        let np = built.iter().position(|arg| arg == "-np").expect("-np");
        assert_eq!(built[np + 1], "1", "one slot: {:?}", built);
        assert_eq!(SLOTS, "1");
    }

    #[test]
    fn a_build_dependent_flag_is_passed_only_when_the_binary_offers_it() {
        let full = invocation(&config(), &program(), &model(), 8080, HELP_WITH_EVERYTHING);
        assert!(full.args.iter().any(|arg| arg == "--no-webui"));
        assert!(
            full.args.iter().any(|arg| arg == "--log-disable"),
            "the prompt must not reach the model's own log: {:?}",
            full.args
        );
        assert!(full.uses_mtp());
        assert_eq!(full.args[full.args.len() - 2..], ["--spec-type", "draft-mtp"]);
        // A build with only `--mtp` gets `--mtp`.
        let old = invocation(&config(), &program(), &model(), 8080, "  --mtp   enable MTP\n");
        assert!(old.args.iter().any(|arg| arg == "--mtp"));
        assert!(old.uses_mtp());
        assert!(!old.args.iter().any(|arg| arg == "--no-webui"));
    }

    #[test]
    fn the_printed_argv_shows_the_model_by_name_and_stays_short() {
        let invocation = invocation(&config(), &program(), &model(), 8080, HELP_WITH_EVERYTHING);
        let text = invocation.text();
        assert!(text.starts_with("llama-server -m qwen.gguf -c 32768"), "{}", text);
        assert!(!text.contains("/opt/atlas/models"), "{}", text);
        assert!(text.contains("--host 127.0.0.1"), "{}", text);
    }

    /// THREAT-MODEL §4.9: "`data/llama.log` receives the model server's stdout and
    /// stderr … so a prompt can be written there by llama.cpp's own logging".
    ///
    /// The trade this closes with, stated so a reader can disagree with it:
    /// `--log-disable` is in the argv on **every** start, whatever the `--help`
    /// probe answered — a probe that fails is the case that mattered, because a
    /// build nobody could ask is the build that logs the prompt. A build that does
    /// not know the flag refuses the argv in milliseconds, and [`start`] answers
    /// that by starting the child **once more without the flag** and marking the
    /// result [`Started::silenced`]` == false`; `crate::start` prints "the model's
    /// own log is not silenced" with it. Nothing is dropped to make this pass: the
    /// model still starts either way, and the second path is still silenced.
    ///
    /// This test used to be `#[ignore]`d: with the flag conditional, no answer could
    /// satisfy both "a build that lacks the flag must still start" and "a prompt must
    /// never be logged". Asking always and falling back once satisfies both, so the
    /// ignore is gone.
    #[test]
    fn the_model_server_cannot_be_told_to_log_a_prompt() {
        for help in ["", "  --mtp   enable MTP\n", HELP_WITH_EVERYTHING] {
            let invocation = invocation(&config(), &program(), &model(), 8080, help);
            assert!(
                invocation.args.iter().any(|arg| arg == LOG_DISABLE),
                "with --help output {:?} the argv is {:?}: llama.cpp logs at its default \
                 verbosity and the child's stderr is data/llama.log",
                help,
                invocation.args
            );
            assert!(asks_for_silence(&invocation), "{:?}", invocation.args);
        }
        // And the fallback is the same command line minus that one entry, so a
        // restart without it is still the documented invocation.
        let full = invocation(&config(), &program(), &model(), 8080, HELP_WITH_EVERYTHING);
        let quiet = without_log_disable(&full);
        assert!(!asks_for_silence(&quiet), "{:?}", quiet.args);
        assert_eq!(quiet.args.len(), full.args.len() - 1);
        assert_eq!(quiet.model, full.model);
        assert_eq!(quiet.port, full.port);
        assert!(quiet.args.iter().any(|arg| arg == "--jinja"));
        assert!(quiet.args.iter().any(|arg| arg == HOST));
    }


    #[test]
    fn a_missing_binary_is_none_rather_than_a_guess() {
        let paths = atlas_config::Paths::at(std::env::temp_dir().join("atlas-llama-none"));
        // With no bin/ and no llama-server on PATH this must be None; if the
        // machine has one on PATH, the answer must still be an executable file.
        if let Some(found) = binary(&paths) {
            assert!(found.is_file(), "{}", found.display());
        }
    }

    #[test]
    fn a_file_that_is_not_executable_is_never_the_binary() {
        // The fourth "no model server" answer needs the binary to be *chosen* first:
        // a `bin/llama-server` that is a plain file (`0644`, a half-finished copy, a
        // symlink to nowhere) is not a program, and choosing it would be a spawn that
        // can only fail — or worse, a spawn of whatever the exec bit says.
        if !atlas_config::posix() {
            return;
        }
        let root = fresh_root("noexec");
        let paths = atlas_config::Paths::at(&root);
        std::fs::create_dir_all(paths.llama_local.parent().unwrap()).unwrap();
        std::fs::write(&paths.llama_local, b"not a program\n").expect("the file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&paths.llama_local, std::fs::Permissions::from_mode(0o644))
                .expect("chmod");
        }
        assert_ne!(
            binary(&paths).as_deref(),
            Some(paths.llama_local.as_path()),
            "a file with no exec bit was chosen as the model server"
        );
        // A *directory* of that name is the same answer: `is_file` is not
        // `is_executable`, and an exec attempt on a directory is not a start.
        std::fs::remove_file(&paths.llama_local).unwrap();
        std::fs::create_dir(&paths.llama_local).unwrap();
        assert_ne!(
            binary(&paths).as_deref(),
            Some(paths.llama_local.as_path()),
            "a directory was chosen as the model server"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_port_is_the_caller_and_the_host_is_never() {
        // The port is a setting (`llama_port`, a `u16`); the host is not a setting at
        // all. Both halves, on the ports an odd config could hold.
        for port in [1u16, 8080, 8087, 65535] {
            let built = invocation(&config(), &program(), &model(), port, "");
            assert_eq!(
                built.args.iter().filter(|arg| *arg == "--port").count(),
                1,
                "one --port on {}: {:?}",
                port,
                built.args
            );
            assert_eq!(
                built.args.iter().filter(|arg| *arg == "--host").count(),
                1,
                "one --host on {}: {:?}",
                port,
                built.args
            );
            let host = built.args.iter().position(|arg| arg == "--host").unwrap();
            assert_eq!(built.args[host + 1], HOST, "{:?}", built.args);
            let at = built.args.iter().position(|arg| arg == "--port").unwrap();
            assert_eq!(built.args[at + 1], port.to_string(), "{:?}", built.args);
            assert_eq!(built.port, port);
        }
        assert_eq!(HOST, "127.0.0.1");
    }

    #[test]
    fn the_model_path_is_one_argument_whatever_it_holds() {
        // A model file name is chosen by whoever put the file in `models/`, and
        // `model_local` is a setting. Neither may become a second flag: with spaces,
        // a quote, a newline or a whole `--host 0.0.0.0` inside it, the value must
        // still be exactly one entry of argv and the pinned flags must be unchanged.
        let hostile = PathBuf::from("/tmp/m/aa b\"c\n--host 0.0.0.0 -m other.gguf");
        let built = invocation(&config(), &program(), &hostile, 8080, HELP_WITH_EVERYTHING);
        assert_eq!(
            built.args.iter().filter(|arg| *arg == "-m").count(),
            1,
            "one -m: {:?}",
            built.args
        );
        assert!(
            built
                .args
                .iter()
                .any(|arg| arg.as_str() == "/tmp/m/aa b\"c\n--host 0.0.0.0 -m other.gguf"),
            "the whole path is one argument: {:?}",
            built.args
        );
        assert_eq!(
            built.args.iter().filter(|arg| *arg == "--host").count(),
            1,
            "{:?}",
            built.args
        );
        let host = built.args.iter().position(|arg| arg == "--host").unwrap();
        assert_eq!(built.args[host + 1], HOST, "{:?}", built.args);
        assert_eq!(built.port, 8080);
        // And the same for a path that *is* a flag on its own.
        let flag_like = invocation(&config(), &program(), Path::new("--help"), 8080, "");
        assert_eq!(flag_like.args[0], "-m");
        assert_eq!(flag_like.args[1], "--help");
        assert_eq!(flag_like.port, 8080);
    }

    #[test]
    fn the_model_log_is_bounded_across_restarts() {
        // Measured before this: `log_sink` appended, and nothing ever removed a
        // byte, so two 3 MiB appends left a 6 MiB `data/llama.log` and no
        // `llama.log.1` — a restart loop is how a model that cannot load turns
        // into a full disk under the archive. The rotation is at the sink, before
        // the handle the child inherits, because that is the only moment this
        // process can act on the file.
        let root = fresh_root("logbound");
        let log = root.join("data").join("llama.log");
        std::fs::create_dir_all(log.parent().expect("data/")).expect("data/");
        let cap = crate::childlog::CHILD_LOG_MAX_BYTES;
        // A sparse file of exactly the cap: the size decides, so this needs no
        // cap-sized write.
        let big = std::fs::File::create(&log).expect("create");
        big.set_len(cap).expect("set the length");
        drop(big);
        drop(log_sink(&log).expect("the sink opens"));
        let rotated = log.with_file_name("llama.log.1");
        assert!(
            rotated.exists(),
            "the model log was not rotated at the cap: {:?} stays {} bytes",
            log,
            std::fs::metadata(&log).map(|meta| meta.len()).unwrap_or(0)
        );
        assert!(
            std::fs::metadata(&log).map(|meta| meta.len()).unwrap_or(u64::MAX) < cap,
            "the current log is still at or over the cap"
        );
        // One generation, not a pile: the second rotation *replaces* `.1` with the
        // file it rotated, so a marker written into the first generation is gone and
        // no `llama.log.2` appears.
        std::fs::write(&rotated, b"the first generation\n").expect("mark the generation");
        let big = std::fs::File::create(&log).expect("create");
        big.set_len(cap).expect("set the length");
        drop(big);
        drop(log_sink(&log).expect("the sink opens"));
        let second = std::fs::read(&rotated).expect("the generation");
        assert!(
            !String::from_utf8_lossy(&second).contains("the first generation"),
            "the rotated generation is kept and appended to instead of replaced"
        );
        assert!(
            !log.with_file_name("llama.log.2").exists(),
            "a second generation was created: the bound is two files, not a chain"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_link_is_refused_and_not_rotated_out_of_the_way() {
        // The rotation must not become the fix for the attack the `O_NOFOLLOW`
        // open already refuses: a link at the log's name is left where it is, so
        // the refusal still happens (and still says why).
        let root = fresh_root("loglinkbound");
        let victim = root.join("victim.txt");
        std::fs::write(&victim, b"before\n").expect("the victim");
        let log = root.join("data").join("llama.log");
        std::fs::create_dir_all(log.parent().expect("data/")).expect("data/");
        std::os::unix::fs::symlink(&victim, &log).expect("plant the link");
        assert!(
            log_sink(&log).is_err(),
            "a symlinked model log was accepted because the rotation moved it"
        );
        assert_eq!(
            std::fs::read(&victim).expect("the victim still there"),
            b"before\n",
            "the model's log was written through the link"
        );
        assert!(
            std::fs::symlink_metadata(&log)
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(false),
            "the planted link was renamed out of the way instead of being refused"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_model_log_is_private_from_the_first_byte() {
        // The log carries prompt text whenever the binary lacks `--log-disable`, and
        // it used to be created with the default `umask` (0644) and `chmod`ed after
        // the fact — a window, and a `chmod` whose failure was ignored.
        let root = fresh_root("logmode");
        let log = root.join("data").join("llama.log");
        let sink = log_sink(&log).expect("the log is created");
        drop(sink);
        if atlas_platform::private::modes_apply() {
            assert_eq!(
                atlas_platform::private::mode_of(&log),
                Some(atlas_platform::private::FILE_MODE),
                "the model log must be 0600, not {:?}",
                atlas_platform::private::mode_of(&log)
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_model_log_is_refused_and_its_target_untouched() {
        // The attack: plant `data/llama.log` as a symlink, then let the model server
        // append its own output — prompt text included — into a file of the
        // planter's choosing. Measured before the fix: the file below received the
        // child's argv line.
        let root = fresh_root("loglink");
        let victim = root.join("victim.txt");
        std::fs::write(&victim, b"before\n").unwrap();
        let log = root.join("data").join("llama.log");
        std::os::unix::fs::symlink(&victim, &log).unwrap();

        let built = invocation(&config(), Path::new("/bin/echo"), &model(), 8080, "");
        let refused = spawn(&built, &root, &log);
        let message = match refused {
            Err(message) => message,
            Ok(child) => {
                let mut child = child;
                let _ = crate::supervise::ModelProcess::try_wait(&mut child);
                panic!("a symlinked model log was followed");
            }
        };
        assert!(message.contains("cannot open"), "{}", message);
        assert!(message.contains("symbolic link"), "{}", message);
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"before\n",
            "the model's output was written through the link"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_real_child_writes_into_the_log_and_the_environment_is_cleared() {
        // The end-to-end version, so the sink is not only a mode: a real `/bin/echo`
        // stands in for `llama-server`, receives the argv this module builds, and its
        // output lands in the log file.
        if !Path::new("/bin/echo").is_file() {
            return;
        }
        let root = fresh_root("logwrite");
        let log = root.join("data").join("llama.log");
        let built = invocation(&config(), Path::new("/bin/echo"), &model(), 8080, "");
        let mut child = spawn(&built, &root, &log).expect("the child starts");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if matches!(
                crate::supervise::ModelProcess::try_wait(&mut child),
                Ok(Some(_))
            ) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let written = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(written.contains("-m"), "the child's output reached the log: {:?}", written);
        assert!(written.contains("--host 127.0.0.1"), "{:?}", written);
        let _ = std::fs::remove_dir_all(&root);
    }
}
