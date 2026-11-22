//! `import_model` — bringing in a model the reader already has.
//!
//! The window can do nothing with a model file: its archive handle is read-only and it
//! starts no process. So the Settings page writes a row — `{"import_model": "<path or
//! https url>", "why": "…"}` with kind `settings_set` — and this module is the half that
//! acts on it. It starts `atlas-setup --import-model <value>` **once**, waits for it, and
//! finishes the row with what happened.
//!
//! Four rules, and each one is why this is a module and not ten lines in `serve`:
//!
//! * **Spawned, not linked.** `atlas-setup` owns every rule about what a model may be — a
//!   `.gguf`, an `https` link, a size, a digest — and this crate decides none of them: it
//!   passes the string the reader typed and reads the tool's own answer back. The install
//!   tool is not a dependency of this crate, which is what keeps "the app links nothing
//!   that can fetch" true with a second call site.
//! * **The boundary, not a `Command`.** The child is built by
//!   [`atlas_platform::child::command_with`] — `env_clear` plus the curated allowlist, no
//!   console window on Windows — with the project root as its working directory and its
//!   output inherited, because the person watching a multi-gigabyte copy is the one who can
//!   diagnose it.
//! * **One attempt.** No retry and no deadline: an import takes as long as a copy or a
//!   download takes, and a cap would abort a legitimate one. A failure finishes the row
//!   with a sentence, and the next press is a new row.
//! * **The answer is the tool's.** A refusal (not a `.gguf`, not `https`, a credential, a
//!   full disk) is written by `atlas-setup` to `data/setup.json` — the file the window
//!   already draws its progress from — and that sentence is what the row carries. "It
//!   exited 1" is the fallback, not the answer.
//!
//! It does not: pick a file, build a URL, check a digest, write `data/config.json`
//! (`atlas-setup` does that, atomically, `0600`), or decide whether the model is any good.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;

use crate::serve::Acted;
use crate::stop::Stop;

/// The tool that can name a model, as `cargo build -p atlas-setup` writes it.
pub const BINARY_NAME: &str = "atlas-setup";

/// The tool's own spelling for "take this file or link, verify it, put it in `models/`".
pub const IMPORT_ARG: &str = "--import-model";

/// How often the wait looks at the child and at the stop flag.
///
/// A quarter second, the same probe interval the loop uses between actions: a `SIGTERM`
/// during a long import is noticed within one tick, so the launcher's grace period for the
/// agent is a grace period rather than a bet.
pub const POLL: Duration = Duration::from_millis(250);

/// This workspace's release directory for a root, where the tool is looked for first.
pub fn release_dir(paths: &atlas_config::Paths) -> PathBuf {
    paths.root.join("rust").join("target").join("release")
}

/// The import tool: this workspace's build first, then `PATH`, like the window.
///
/// The same order as the launcher's resolver, so "which `atlas-setup` ran" has one answer
/// in both processes: the checkout the app was started from, and only then an installed
/// copy.
pub fn binary(paths: &atlas_config::Paths) -> Option<PathBuf> {
    let local = atlas_platform::host::app_binary(release_dir(paths), BINARY_NAME);
    if atlas_platform::executable::is_executable(&local) {
        return Some(local);
    }
    atlas_platform::executable::on_path(BINARY_NAME)
}

/// The sentence for "this machine has no import tool", with the path and the fix.
pub fn missing_line(paths: &atlas_config::Paths) -> String {
    format!(
        "{} was found neither at {} nor on PATH, so the model cannot be brought in and \
         data/config.json is unchanged. Build it with `cargo build --release -p {}` and send \
         the request again",
        BINARY_NAME,
        atlas_platform::host::app_binary(release_dir(paths), BINARY_NAME).display(),
        BINARY_NAME
    )
}

/// Handle one payload, or answer `None` when it is not about importing a model.
///
/// `None` is the common case: this arm sees every `settings_set` row, and only the ones
/// carrying `import_model` are its business.
pub fn apply(payload: &Value, root: &Path, stop: &Stop) -> Option<Acted> {
    let source = match payload.get("import_model") {
        None => return None,
        Some(Value::String(text)) => text.trim().to_string(),
        Some(other) => {
            return Some(refused(format!(
                "\"import_model\" has to be the path of a .gguf file or an https link, not {} \
                 — nothing was brought in",
                shape_of(other)
            )))
        }
    };
    if source.is_empty() {
        return Some(refused(
            "\"import_model\" is empty: paste the path of a .gguf file or an https link — \
             nothing was brought in",
        ));
    }
    Some(bring_in(&source, root, stop))
}

/// Run the tool once for one value and report what it left behind.
fn bring_in(source: &str, root: &Path, stop: &Stop) -> Acted {
    let paths = atlas_config::Paths::at(root);
    let program = match binary(&paths) {
        Some(program) => program,
        None => return refused(missing_line(&paths)),
    };
    let before = config_bytes(&paths);
    let setup_before = std::fs::read(setup_file(&paths)).ok();
    let mut command = atlas_platform::child::command_with(&program, &atlas_config::spawn_env(&[]));
    command
        .arg(IMPORT_ARG)
        .arg(source)
        .arg("--root")
        .arg(&paths.root)
        .current_dir(&paths.root)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return refused(format!(
                "{} {} {} will not start ({}), so the model stays as it was",
                program.display(),
                IMPORT_ARG,
                source,
                error
            ))
        }
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if stop.requested() {
                    // No `kill(1)` here on purpose: this crate starts exactly one program,
                    // and asking a copier to stop politely would mean starting a second.
                    // The cost is a partial file, which `atlas-setup` resumes.
                    let _ = child.kill();
                    let _ = child.wait();
                    return refused(format!(
                        "a stop was requested while {} was bringing in {}, so it was ended and \
                         data/config.json is unchanged",
                        BINARY_NAME, source
                    ));
                }
                std::thread::sleep(POLL);
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return refused(format!(
                    "the exit status of {} cannot be read ({}), so ATLAS cannot tell whether \
                     {} was brought in",
                    BINARY_NAME, error, source
                ));
            }
        }
    };
    if !status.success() {
        // The tool's own sentence, when it left one: a refusal names the rule it refused
        // by, and that is the only answer a reader can act on. The file's state is checked
        // rather than assumed, because "unchanged" is the promise a refusal makes.
        let state = if config_bytes(&paths) == before {
            "data/config.json is unchanged"
        } else {
            "data/config.json changed anyway, which a refusal must not do"
        };
        let said = refusal_in_setup_file(&paths, setup_before.as_deref());
        return refused(match said {
            Some(sentence) => format!("{} ({})", sentence, state),
            None => format!(
                "{} {} {} ended with {} and {}; its own lines in this terminal say which rule \
                 refused it",
                BINARY_NAME,
                IMPORT_ARG,
                source,
                describe(status),
                state
            ),
        });
    }
    // Exited 0: read the file back instead of trusting the exit code. A tool that says
    // "done" and names nothing has not done the thing the reader asked for.
    match named_model(&config_bytes(&paths)) {
        Some(_) => Acted {
            stored: 1,
            error: None,
            ..Acted::default()
        },
        None => refused(format!(
            "{} {} {} exited 0 but data/config.json names no model, so the Settings page still \
             has none to show",
            BINARY_NAME, IMPORT_ARG, source
        )),
    }
}

/// `data/config.json` as it is on disk, or empty when it does not exist.
fn config_bytes(paths: &atlas_config::Paths) -> Vec<u8> {
    std::fs::read(&paths.config_file).unwrap_or_default()
}

/// The model file `data/config.json` names, when it names one.
fn named_model(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let parsed: Value = serde_json::from_str(text).ok()?;
    let model = parsed.get("model_local")?.as_str()?.trim().to_string();
    if model.is_empty() {
        None
    } else {
        Some(model)
    }
}

/// The progress file `atlas-setup` writes — the channel its sentences travel on.
fn setup_file(paths: &atlas_config::Paths) -> PathBuf {
    paths.data_dir.join("setup.json")
}

/// The tool's refusal sentence, when this run wrote one.
///
/// Guarded by "the file changed", so last week's failure is never this row's answer: a
/// reader would be told about a model they replaced since.
fn refusal_in_setup_file(paths: &atlas_config::Paths, before: Option<&[u8]>) -> Option<String> {
    let after = std::fs::read(setup_file(paths)).ok()?;
    if before == Some(after.as_slice()) {
        return None;
    }
    let parsed: Value = serde_json::from_slice(&after).ok()?;
    if parsed.get("ok").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    let message = parsed.get("message")?.as_str()?.trim().to_string();
    if message.is_empty() {
        None
    } else {
        Some(message)
    }
}

/// `ended with exit status 1`, or `a signal`.
fn describe(status: std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit status {}", code),
        None => "a signal".to_string(),
    }
}

/// The kind of a JSON value, for a refusal that has to name what it got.
fn shape_of(value: &Value) -> String {
    match value {
        Value::Null => "nothing (null)".to_string(),
        Value::Bool(_) => "true/false".to_string(),
        Value::Number(_) => "a number".to_string(),
        Value::String(_) => "a string".to_string(),
        Value::Array(_) => "a list".to_string(),
        Value::Object(_) => "an object".to_string(),
    }
}

/// One finished row that refused, with the sentence a reader can act on.
fn refused(sentence: impl Into<String>) -> Acted {
    Acted {
        error: Some(sentence.into()),
        ..Acted::default()
    }
}
