//! The `atlas-text` / `atlas-dates` seam, and the process implementation.
//!
//! Four of the rules this crate applies are not its own, and it does not repeat
//! them:
//!
//! | Question | Owner |
//! | --- | --- |
//! | are these two links the same page | `atlas-text norm_url` |
//! | what outlet is this link from | `atlas-text publisher` |
//! | what is this headline without the site tail | `atlas-text clean_title` |
//! | what timestamp is written here | `atlas-dates parse` |
//! | is that timestamp inside the window | `atlas-dates window` |
//!
//! Both binaries have no library target, so this crate speaks the workspace's
//! stdin/stdout JSON contract to them — the same one `server/textcore.py` speaks,
//! with the same one-object-in/one-object-out shape. The [`Rules`] trait is the
//! seam: the server embeds an implementation it already has (and shares its own
//! cache), and a test runs the whole pipeline without spawning five processes per
//! page. There is no Python fallback and no "assume now" anywhere: an engine
//! that is not built is reported, once, at startup.
//!
//! Answers are memoised ([`Cache`]). A result page repeats itself — forty cards
//! say "2 hours ago" and every card prints its own URL twice — so the memo is
//! what keeps one search at tens of milliseconds of engine time instead of a
//! second. It caches the *engine's* answer; it never caches a decision.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

/// How long one engine question may take before the child is killed. The engines
/// are pure local binaries that answer in microseconds; three seconds is the
/// bridge contract's number, kept identical here.
pub const ENGINE_TIMEOUT: Duration = Duration::from_secs(3);

/// One payload may not be larger than this.
const MAX_PAYLOAD: usize = 4 << 20;

/// Where the engine binaries live, if the environment says so.
pub const BIN_DIR_ENV: &str = "ATLAS_BIN_DIR";

/// The engines this crate calls.
pub const ENGINES: &[&str] = &["atlas-text", "atlas-dates"];

/// One freshness verdict, as `atlas-dates window` answers it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Window {
    /// Inside the window, per `atlas-dates`.
    pub fresh: bool,
    /// `fresh`, `stale`, `skew` or `unparsed`.
    pub reason: String,
    /// The timestamp, ISO-8601 UTC.
    pub iso: String,
    pub age_s: i64,
}

impl Window {
    /// The engine could not read a timestamp here at all.
    pub fn unparsed(&self) -> bool {
        self.reason == "unparsed"
    }
}

/// The rules this crate applies but does not own.
///
/// `Send + Sync` so the server can share one implementation across the process.
/// A method returns an empty string (or `None`) when the engine could not answer;
/// the caller then leaves the field out rather than inventing a value.
pub trait Rules: Send + Sync {
    /// `atlas-text norm_url`: the canonical form used as a duplicate key.
    fn norm_url(&self, url: &str) -> String;
    /// `atlas-text publisher`: a presentable outlet name.
    fn publisher(&self, url: &str) -> String;
    /// `atlas-text clean_title`: the headline without the outlet tail.
    fn clean_title(&self, title: &str, publisher: &str) -> String;
    /// `atlas-dates parse`: the timestamp as ISO-8601 UTC, or `None`.
    fn date_iso(&self, text: &str, now: i64) -> Option<String>;
    /// `atlas-dates window`: is this timestamp inside `max_age_s` before `now`?
    fn window(&self, text: &str, now: i64, max_age_s: i64) -> Option<Window>;
}

/// The engine's answers, kept for the length of one process.
#[derive(Debug, Default)]
struct Cache {
    norm: HashMap<String, String>,
    publisher: HashMap<String, String>,
    title: HashMap<String, String>,
    date: HashMap<String, Option<Window>>,
    iso: HashMap<String, Option<String>>,
}

/// The real thing: `atlas-text` and `atlas-dates` from the workspace build.
#[derive(Debug)]
pub struct Engines {
    bin_dir: PathBuf,
    cache: Mutex<Cache>,
}

impl Engines {
    /// Bind to one directory of binaries (the workspace's `rust/target/release`).
    pub fn with_bin_dir(dir: impl Into<PathBuf>) -> Engines {
        Engines {
            bin_dir: dir.into(),
            cache: Mutex::new(Cache::default()),
        }
    }

    /// Find the binaries: `ATLAS_BIN_DIR`, else the directory this executable
    /// sits in (a binary in `rust/target/release` finds its siblings).
    pub fn discover() -> Result<Engines, String> {
        let dir = match std::env::var(BIN_DIR_ENV) {
            Ok(value) if !value.trim().is_empty() => PathBuf::from(value.trim()),
            _ => match std::env::current_exe() {
                Ok(exe) => exe
                    .parent()
                    .map(|dir| dir.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from(".")),
                Err(_) => PathBuf::from("."),
            },
        };
        let engines = Engines::with_bin_dir(dir);
        engines.check()?;
        Ok(engines)
    }

    /// Every engine built? The answer names the file and the command that fixes
    /// it, because a rule that quietly answers differently is worse than a stop.
    pub fn check(&self) -> Result<(), String> {
        let missing: Vec<String> = ENGINES
            .iter()
            .map(|engine| self.path(engine))
            .filter(|path| !is_executable(path))
            .map(|path| path.display().to_string())
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        Err(format!(
            "search needs {} engine binary/binaries that are not built: {} - run: cargo build --release --manifest-path rust/Cargo.toml",
            missing.len(),
            missing.join(", ")
        ))
    }

    /// The directory the engines are looked for in.
    pub fn bin_dir(&self) -> &Path {
        &self.bin_dir
    }

    fn path(&self, engine: &str) -> PathBuf {
        atlas_platform::host::app_binary(&self.bin_dir, engine)
    }

    /// Ask one engine one question: `None` when it cannot answer.
    fn ask(&self, engine: &str, op: &str, payload: &Value) -> Option<Value> {
        let path = self.path(engine);
        if !is_executable(&path) {
            return None;
        }
        let blob = payload.to_string();
        if blob.len() > MAX_PAYLOAD {
            return None;
        }
        let text = run(&path, op, blob)?;
        let value: Value = serde_json::from_str(&text).ok()?;
        if value.get("error").is_some() {
            return None;
        }
        Some(value)
    }

    fn text_op(&self, op: &str, payload: Value, key: &str) -> String {
        match self.ask("atlas-text", op, &payload) {
            Some(answer) => answer
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            None => String::new(),
        }
    }
}

impl Rules for Engines {
    fn norm_url(&self, url: &str) -> String {
        if url.trim().is_empty() {
            return String::new();
        }
        if let Some(hit) = self.lock().norm.get(url) {
            return hit.clone();
        }
        let answer = self.text_op("norm_url", serde_json::json!({ "url": url }), "url");
        self.lock().norm.insert(url.to_string(), answer.clone());
        answer
    }

    fn publisher(&self, url: &str) -> String {
        if url.trim().is_empty() {
            return String::new();
        }
        if let Some(hit) = self.lock().publisher.get(url) {
            return hit.clone();
        }
        let answer = self.text_op("publisher", serde_json::json!({ "url": url }), "publisher");
        self.lock().publisher.insert(url.to_string(), answer.clone());
        answer
    }

    fn clean_title(&self, title: &str, publisher: &str) -> String {
        // The only assumption: `clean_title` without a separator is the title
        // back. Asking the engine anyway would cost one process per result for
        // an answer that is already sitting in the string.
        if !has_separator(title) {
            return title.trim().to_string();
        }
        let key = format!("{}\u{1}{}", publisher, title);
        if let Some(hit) = self.lock().title.get(&key) {
            return hit.clone();
        }
        let answer = self.text_op(
            "clean_title",
            serde_json::json!({ "title": title, "publisher": publisher }),
            "title",
        );
        self.lock().title.insert(key, answer.clone());
        answer
    }

    fn date_iso(&self, text: &str, now: i64) -> Option<String> {
        let key = format!("{}\u{1}{}", now, text);
        if let Some(cached) = self.lock().iso.get(&key) {
            return cached.clone();
        }
        let answer = self.ask(
            "atlas-dates",
            "parse",
            &serde_json::json!({ "ts": text, "hint": "", "now": now }),
        );
        let iso = answer.and_then(|answer| {
            if answer.get("ok").and_then(Value::as_bool) != Some(true) {
                return None;
            }
            let iso = answer.get("iso").and_then(Value::as_str)?.to_string();
            if iso.is_empty() {
                None
            } else {
                Some(iso)
            }
        });
        self.lock().iso.insert(key, iso.clone());
        iso
    }

    fn window(&self, text: &str, now: i64, max_age_s: i64) -> Option<Window> {
        let key = format!("{}\u{1}{}\u{1}{}", now, max_age_s, text);
        if let Some(cached) = self.lock().date.get(&key) {
            return cached.clone();
        }
        let answer = self.ask(
            "atlas-dates",
            "window",
            &serde_json::json!({ "ts": text, "now": now, "max_age_s": max_age_s }),
        );
        let verdict = answer.map(|answer| Window {
            fresh: answer.get("fresh").and_then(Value::as_bool).unwrap_or(false),
            reason: answer
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unparsed")
                .to_string(),
            iso: answer
                .get("iso")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            age_s: answer.get("age_s").and_then(Value::as_i64).unwrap_or(0),
        });
        self.lock().date.insert(key, verdict.clone());
        verdict
    }
}

impl Engines {
    fn lock(&self) -> std::sync::MutexGuard<'_, Cache> {
        // A poisoned cache only means some other thread panicked mid-write; the
        // map is still a valid map, so the answer is not lost.
        self.cache.lock().unwrap_or_else(|error| error.into_inner())
    }
}

/// The separators `atlas-text clean_title` looks for. Knowing *these* is not a
/// second implementation of the rule: they only decide whether asking is worth a
/// process. The engine still decides what to drop.
fn has_separator(title: &str) -> bool {
    [" | ", " – ", " — ", " - ", " :: ", " · "]
        .iter()
        .any(|separator| title.contains(separator))
}

/// The command one engine question is asked with.
///
/// The child gets the payload on stdin, its answer on stdout, and nothing else:
/// no shell, no arguments beyond the operation, and **no inherited environment**.
/// The last one is [`atlas_platform::child::command`] and not a `Command::new`
/// here: an engine spawned with the caller's environment inherits every
/// `*_API_KEY`, proxy credential and `LD_PRELOAD`/`DYLD_*` this process holds,
/// which is the one thing THREAT-MODEL §4.9 says cannot happen to a child of
/// ATLAS. `env_clear` plus the curated allowlist is the workspace's answer and it
/// is asked for, never re-implemented.
fn engine(path: &Path, op: &str) -> Command {
    let mut command = atlas_platform::child::command(path);
    command
        .arg(op)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

/// One engine question over the workspace's process contract.
///
/// See [`engine`] for what the child is given. It is polled, so a wedged engine
/// is killed after [`ENGINE_TIMEOUT`] — this crate spawns the process, so killing
/// it is its own business and nobody else's.
fn run(path: &Path, op: &str, payload: String) -> Option<String> {
    let mut child = engine(path, op).spawn().ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        // The payload is a few hundred bytes, far below a pipe buffer: this
        // cannot block, so no writer thread is needed.
        let _ = stdin.write_all(payload.as_bytes());
        let _ = stdin.flush();
    }
    let deadline = Instant::now() + ENGINE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = Vec::with_capacity(4096);
    child.stdout.take()?.read_to_end(&mut out).ok()?;
    String::from_utf8(out).ok()
}

/// Is this a regular executable file?
pub fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THREAT-MODEL §4.9, measured before the fix: `run` built its child with a
    /// bare `Command::new(path)`, so an engine inherited this process's whole
    /// environment. `/usr/bin/env -0` stands in for an engine here — it prints
    /// the environment it was started with (NUL-separated, exit 0) and takes no
    /// command to run — so what reached the child is readable instead of argued.
    ///
    /// The variable is planted in this process on purpose: a value the parent
    /// holds is exactly what a child must not inherit, and the name is this
    /// crate's alone so no other test in this binary reads it.
    #[cfg(unix)]
    #[test]
    fn a_hostile_variable_does_not_reach_the_engine() {
        let env_bin = Path::new("/usr/bin/env");
        if !is_executable(env_bin) {
            println!("skipped: /usr/bin/env is not here to print a child's environment");
            return;
        }
        const HOSTILE: &str = "ATLAS_SEARCH_TEST_HOSTILE_LEAK";
        std::env::set_var(HOSTILE, "sk-live-do-not-inherit");
        let seen = run(env_bin, "-0", String::new());
        std::env::remove_var(HOSTILE);
        let seen = seen.expect("the stand-in engine prints its environment");
        // The control first: a child that printed nothing would make the
        // assertion below pass by accident.
        assert!(
            seen.contains("PATH="),
            "the child printed no environment at all: {:?}",
            seen
        );
        assert!(
            !seen.contains(HOSTILE) && !seen.contains("sk-live-do-not-inherit"),
            "the engine inherited this process's environment: {:?}",
            seen
        );
    }

    /// The same promise without a process: every variable the command carries is
    /// on the platform's allowlist...
    #[test]
    fn the_engine_child_is_given_the_allowlist_and_nothing_else() {
        let compiled = engine(Path::new("atlas-text"), "norm_url");
        let allowlist = atlas_platform::child::allowed(atlas_platform::host::os());
        let mut seen = 0;
        for (name, value) in compiled.get_envs() {
            let name = name.to_string_lossy().to_string();
            assert!(
                allowlist.contains(&name.as_str()),
                "{} is not on the allowlist",
                name
            );
            assert!(value.is_some(), "{} must carry a value", name);
            seen += 1;
        }
        assert!(seen > 0, "the curated environment must be applied");
        // ...and the argv is the operation alone, never a line with flags in it.
        let args: Vec<String> = compiled
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();
        assert_eq!(args, vec!["norm_url".to_string()]);
    }
}
