//! What the install is doing, written where the window can read it.
//!
//! A 6.5 GB download takes a long time on a home connection, and a person watching a
//! dark screen has no way to tell "working" from "hung". So every phase of
//! `atlas-setup --install` is written to `data/setup.json`, and the window — which
//! reads `data/` and never writes it — can draw a real bar instead of a spinner:
//!
//! ```json
//! {"phase":"model","done":1234567,"total":8000000000,"message":"downloading the model","ok":null}
//! ```
//!
//! and, when it is over, one line that needs no phase:
//!
//! ```json
//! {"ok":true,"message":"ready — open the app with ATLAS.command"}
//! ```
//!
//! Three rules make the file safe to read while it is being written:
//!
//! * **Small.** One object, no history: a reader either sees the current state or the
//!   previous one, never half of both.
//! * **Replaced, not edited.** The bytes go to `setup.json.new` and are renamed over the
//!   old file, so a reader never opens a truncated file — the same rule the binaries in
//!   `bin/` follow.
//! * **Never fatal.** A progress write that fails (a read-only `data/`, a full disk) is
//!   reported once and then ignored: the install is the job, the bar is the courtesy.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Where the file lives, relative to the root — the contract with the window.
pub const RELATIVE_PATH: &str = "data/setup.json";

/// How often a byte counter is allowed to touch the disk.
///
/// The transfer calls back once per megabyte; a bar that is written 8 000 times for 8 GB
/// turns a courtesy into disk churn, and one that is written twice a second is smooth to
/// the eye. Phase and message changes are written immediately — those are the interesting
/// transitions.
pub const INTERVAL: Duration = Duration::from_millis(500);

/// The progress file at `<root>/data/setup.json`.
pub struct SetupFile {
    path: PathBuf,
    interval: Duration,
    last: Option<Instant>,
    last_phase: String,
    last_message: String,
}

impl SetupFile {
    /// The file for this root. Nothing is created until the first write.
    pub fn at(root: &Path) -> SetupFile {
        SetupFile {
            path: root.join("data").join("setup.json"),
            interval: INTERVAL,
            last: None,
            last_phase: String::new(),
            last_message: String::new(),
        }
    }

    /// The same file with a different throttle, so a test can pin the rule without a
    /// clock: `Duration::ZERO` writes every call, an hour writes only the first.
    pub fn with_interval(root: &Path, interval: Duration) -> SetupFile {
        let mut file = SetupFile::at(root);
        file.interval = interval;
        file
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write one snapshot, unconditionally.
    pub fn update(
        &mut self,
        phase: &str,
        done: u64,
        total: u64,
        message: &str,
    ) -> Result<(), String> {
        self.write(&serde_json::json!({
            "phase": phase,
            "done": done,
            "total": total,
            "message": message,
            "ok": serde_json::Value::Null,
        }))
    }

    /// [`update`](SetupFile::update), throttled: a new phase or a new message always
    /// lands, a byte count at most once per interval.
    pub fn tick(
        &mut self,
        phase: &str,
        done: u64,
        total: u64,
        message: &str,
    ) -> Result<(), String> {
        // A new phase or a new sentence is news and lands immediately; a byte count is
        // noise and waits for the interval (or for the last byte).
        let changed = phase != self.last_phase || message != self.last_message;
        let due = match self.last {
            None => true,
            Some(when) => when.elapsed() >= self.interval,
        };
        let finished = total > 0 && done >= total;
        if changed || due || finished {
            return self.update(phase, done, total, message);
        }
        Ok(())
    }

    /// The last line: what happened, in one sentence, with no phase.
    pub fn finish(&mut self, ok: bool, message: &str) -> Result<(), String> {
        self.write(&serde_json::json!({ "ok": ok, "message": message }))
    }

    /// The atomic replace: `.new` in the same directory, then a rename.
    fn write(&mut self, value: &serde_json::Value) -> Result<(), String> {
        let Some(dir) = self.path.parent() else {
            return Err(format!("{} has no directory", self.path.display()));
        };
        fs::create_dir_all(dir).map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
        let staged = staged(&self.path);
        let text = format!("{value}\n");
        if let Err(error) = fs::write(&staged, text) {
            let _ = fs::remove_file(&staged);
            return Err(format!("cannot write {}: {error}", staged.display()));
        }
        if let Err(error) = fs::rename(&staged, &self.path) {
            let _ = fs::remove_file(&staged);
            return Err(format!("cannot replace {}: {error}", self.path.display()));
        }
        self.last = Some(Instant::now());
        if let Some(phase) = value.get("phase").and_then(serde_json::Value::as_str) {
            self.last_phase = phase.to_string();
        }
        self.last_message = value
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        Ok(())
    }
}

/// `setup.json` → `setup.json.new`, beside it so the rename stays on one volume.
fn staged(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|name| name.to_os_string()).unwrap_or_default();
    name.push(".new");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "atlas-progress-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("data")).expect("a scratch root");
        dir
    }

    fn read(path: &Path) -> serde_json::Value {
        let text = fs::read_to_string(path).expect("the progress file");
        serde_json::from_str(&text).expect("valid JSON")
    }

    /// The running shape is the contract with the window: a bar needs a phase, a number
    /// done and a total, and `ok` must be *null* while it is still working — `ok:true`
    /// would tell the window the install had already finished.
    #[test]
    fn the_running_shape_names_the_phase_and_both_byte_counts() {
        let root = scratch("shape");
        let mut file = SetupFile::at(&root);
        assert_eq!(file.path(), root.join("data").join("setup.json"));
        file.update("model", 1_234_567, 8_000_000_000, "downloading the model")
            .expect("a write");

        let value = read(file.path());
        assert_eq!(value["phase"], "model");
        assert_eq!(value["done"], 1_234_567u64);
        assert_eq!(value["total"], 8_000_000_000u64);
        assert_eq!(value["message"], "downloading the model");
        assert!(value["ok"].is_null(), "still running, so ok is null: {value}");
        let _ = fs::remove_dir_all(&root);
    }

    /// The final shape carries the verdict and the sentence a person reads. No phase, no
    /// half-finished counts: a reader that sees `ok` set knows the run is over.
    #[test]
    fn the_final_shape_is_a_verdict_and_a_sentence() {
        let root = scratch("finish");
        let mut file = SetupFile::at(&root);
        file.update("model", 10, 10, "downloading the model").expect("a write");
        file.finish(true, "ready — open the app with ATLAS.command").expect("a finish");

        let value = read(file.path());
        assert_eq!(value["ok"], true);
        assert_eq!(value["message"], "ready — open the app with ATLAS.command");
        assert!(value.get("phase").is_none(), "the final line has no phase: {value}");

        file.finish(false, "the model could not be downloaded").expect("a failure line");
        let value = read(file.path());
        assert_eq!(value["ok"], false);
        assert!(value["message"].as_str().unwrap_or_default().contains("model"));
        let _ = fs::remove_dir_all(&root);
    }

    /// Atomic means: the file that exists is always a whole JSON object, and the staged
    /// name is never left behind for a reader (or a person) to find.
    #[test]
    fn a_write_replaces_the_file_and_leaves_no_staged_copy() {
        let root = scratch("atomic");
        let mut file = SetupFile::at(&root);
        let staged = root.join("data").join("setup.json.new");

        for (done, total) in [(0u64, 10u64), (5, 10), (10, 10)] {
            file.update("model", done, total, "downloading the model").expect("a write");
            assert_eq!(read(file.path())["done"], done);
            assert!(!staged.exists(), "the staged file is renamed, never left: {:?}", staged);
        }
        // And the directory holds exactly one file: no history, no `.new`.
        let names: Vec<String> = fs::read_dir(root.join("data"))
            .expect("the data dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["setup.json".to_string()], "{names:?}");
        let _ = fs::remove_dir_all(&root);
    }

    /// The throttle, pinned without a clock: a megabyte-by-megabyte stream must not
    /// rewrite the file every time, but a new phase or the last byte must land at once.
    #[test]
    fn a_byte_count_is_throttled_and_a_phase_change_is_not() {
        let root = scratch("throttle");
        let path = root.join("data").join("setup.json");

        let mut slow = SetupFile::with_interval(&root, Duration::from_secs(3600));
        slow.tick("model", 1, 10, "downloading the model").expect("the first write");
        assert_eq!(read(&path)["done"], 1, "the first tick always lands");
        slow.tick("model", 2, 10, "downloading the model").expect("a throttled tick");
        assert_eq!(read(&path)["done"], 1, "one interval has not passed");
        slow.tick("model", 3, 10, "checking the file").expect("a new message");
        assert_eq!(read(&path)["message"], "checking the file", "a new sentence is news");
        slow.tick("ready", 10, 10, "checking the file").expect("a new phase");
        assert_eq!(read(&path)["phase"], "ready", "a new phase is news");

        // `Duration::ZERO` is the other end: every call is written.
        let mut fast = SetupFile::with_interval(&root, Duration::ZERO);
        fast.tick("model", 7, 10, "downloading the model").expect("a write");
        fast.tick("model", 8, 10, "downloading the model").expect("a write");
        assert_eq!(read(&path)["done"], 8);
        let _ = fs::remove_dir_all(&root);
    }

    /// A progress file that cannot be written is a sentence, not a panic — an install
    /// must not die because a courtesy failed.
    #[test]
    fn an_unwritable_location_is_a_sentence() {
        let root = scratch("unwritable");
        // `data` as a *file*: create_dir_all and the write both fail.
        let data = root.join("data");
        let _ = fs::remove_dir_all(&data);
        fs::write(&data, b"not a directory").expect("a file where the directory goes");
        let mut file = SetupFile::at(&root);
        let error = file.update("model", 1, 2, "downloading the model").expect_err("a refusal");
        assert!(error.contains("data"), "{error}");
        let _ = fs::remove_dir_all(&root);
    }
}
