//! What first setup is doing, as the window reads it.
//!
//! The model is no longer shipped. It is downloaded once, by `atlas setup --install`, which
//! is another crate's job: it writes `<root>/data/setup.json` while it works and one last
//! time when it stops. This module is the *reader* — the window opens nothing for writing
//! here, and the disclosure rule that says so is `tests/disclosure.rs`'s.
//!
//! The file is a progress report, not a protocol: two shapes, both written by hand in the
//! installer's own JSON:
//!
//! ```text
//! {"phase":"model","done":1234567,"total":8000000000,"message":"downloading the model","ok":null}
//! {"ok":true,"message":"the model is installed"}
//! {"ok":false,"message":"the download stopped: the network went away"}
//! ```
//!
//! Anything else — an empty file, a half-written one, a phase this window has never heard
//! of with no byte count — is [`Setup::Nothing`]: a page may say "I do not know", and may
//! never invent a percentage. That is the rule this whole module exists to keep, because the
//! person reading it is not going to open the JSON.
//!
//! Three questions it answers, and nothing else:
//!
//! * is a download running, and how far (`Setup::Running`)?
//! * did it finish (`Setup::Done`)?
//! * did it stop without a model, and what did it say (`Setup::Failed`)?
//!
//! ## Why a poll and not a file watcher
//!
//! A watcher would be a dependency (or a thread per page) and the installer writes
//! continuously, so the file is *polled*: at most [`POLL`] apart, and only while the page
//! that would show it is open with nothing else to say. Two reads a second of a small file
//! is cheaper than the bar it draws.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The name the installer writes under `<root>/data/`.
pub const SETUP_FILE: &str = "setup.json";

/// How often the file is read while the page is watching it: twice a second.
///
/// Fast enough that a 8 GB download's bar moves in steps a reader cannot see, slow enough
/// that the window reads a file nobody is looking at twice a second and not once a frame.
pub const POLL: Duration = Duration::from_millis(500);

/// The command that downloads the model, as a reader types it. Named in the page when a
/// download failed and when a turn failed with no model: one string, so the two cannot drift.
pub const INSTALL_COMMAND: &str = "atlas setup --install";

/// The phase the installer's first download reports. Other phases are still reported with
/// their own words, so the page never has to know the list.
pub const MODEL_PHASE: &str = "model";

/// What setup is doing, or that nothing can be said about it.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Setup {
    /// No file, or one this window cannot read as a report: say nothing.
    #[default]
    Nothing,
    /// A download is running, with the bytes it has and the bytes it needs.
    Running(Download),
    /// It finished, and the model is there.
    Done,
    /// It stopped without a model, and this is the sentence why.
    Failed(String),
}

impl Setup {
    /// The download, if one is running.
    pub fn running(&self) -> Option<&Download> {
        match self {
            Setup::Running(download) => Some(download),
            _ => None,
        }
    }
}

/// A running download, as the installer last wrote it.
#[derive(Clone, Debug, PartialEq)]
pub struct Download {
    /// `"model"`, or whatever else the installer is fetching.
    pub phase: String,
    /// Bytes on disk so far.
    pub done: u64,
    /// Bytes the whole download needs; `0` when the installer has not said yet.
    pub total: u64,
    /// The installer's own words, used when the phase is not one this page knows.
    pub message: String,
}

impl Download {
    /// How far along, in `0.0 ..= 1.0`. A total of zero is zero, never a division by it.
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        (self.done as f64 / self.total as f64).clamp(0.0, 1.0) as f32
    }

    /// The bytes as a person reads them: `3.2 of 8.0 GB`. Empty when the installer has not
    /// said how big the download is yet.
    ///
    /// One ruler for both numbers, chosen by the *total*: `3.2 of 8000 MB` is the same fact
    /// written so that nobody can read it.
    pub fn bytes(&self) -> String {
        if self.total == 0 {
            return String::new();
        }
        let (unit, divisor) = ruler(self.total);
        format!(
            "{:.1} of {:.1} {unit}",
            self.done as f64 / divisor,
            self.total as f64 / divisor
        )
    }

    /// The one sentence the page shows: what is happening, and how far.
    pub fn sentence(&self) -> String {
        let happening = if self.phase == MODEL_PHASE {
            // "once" is the whole reassurance a first-run window can give, and it is true.
            "the model is being downloaded once".to_string()
        } else if self.message.trim().is_empty() {
            format!("{} is being downloaded", self.phase)
        } else {
            self.message.trim().trim_end_matches('.').to_string()
        };
        let bytes = self.bytes();
        if bytes.is_empty() {
            happening
        } else {
            format!("{happening} — {bytes}")
        }
    }
}

/// The unit a byte count is read in, and what to divide it by: gigabytes for a
/// gigabyte-sized download, megabytes below that, kilobytes below that, bytes at the bottom.
fn ruler(bytes: u64) -> (&'static str, f64) {
    if bytes >= 1_000_000_000 {
        ("GB", 1e9)
    } else if bytes >= 1_000_000 {
        ("MB", 1e6)
    } else if bytes >= 1_000 {
        ("KB", 1e3)
    } else {
        ("B", 1.0)
    }
}

/// The file beside the archive: `<root>/data/setup.json`.
pub fn path(archive: &Path) -> PathBuf {
    archive
        .parent()
        .map(|data| data.join(SETUP_FILE))
        .unwrap_or_else(|| PathBuf::from(SETUP_FILE))
}

/// Read it once. A missing file, an unreadable one, or one that is not a report is
/// [`Setup::Nothing`] — this function has no way to fail, because the page that calls it can
/// only draw.
pub fn read(path: &Path) -> Setup {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(_) => Setup::Nothing,
    }
}

/// Read the text the installer wrote.
pub fn parse(text: &str) -> Setup {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Setup::Nothing;
    };
    // The last word wins: `ok` is what the installer writes when it stops.
    match value.get("ok") {
        Some(serde_json::Value::Bool(true)) => return Setup::Done,
        Some(serde_json::Value::Bool(false)) => {
            return Setup::Failed(sentence_of(&value));
        }
        _ => {}
    }
    let phase = value
        .get("phase")
        .and_then(|phase| phase.as_str())
        .unwrap_or("")
        .trim();
    if phase.is_empty() {
        return Setup::Nothing;
    }
    let count = |key: &str| {
        value
            .get(key)
            .and_then(|number| number.as_u64())
            .unwrap_or(0)
    };
    Setup::Running(Download {
        phase: phase.to_string(),
        done: count("done"),
        total: count("total"),
        message: value
            .get("message")
            .and_then(|message| message.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

/// The installer's own sentence, or a plain one when it did not write any.
fn sentence_of(value: &serde_json::Value) -> String {
    let said = value
        .get("message")
        .and_then(|message| message.as_str())
        .unwrap_or("")
        .trim()
        .trim_end_matches('.');
    if said.is_empty() {
        "the download stopped before the model was in place".to_string()
    } else {
        said.to_string()
    }
}

/// The page's view of the file: what it last read, and when.
///
/// Reads at most once per [`POLL`], and only when the page asks (which it does while it is
/// open and nothing is answering). The clock is taken as an argument rather than read from
/// `Instant::now` so that "at most twice a second" is a test instead of a hope.
#[derive(Clone, Debug)]
pub struct Watch {
    file: PathBuf,
    state: Setup,
    at: Option<Instant>,
}

impl Default for Watch {
    /// A watch on the file beside the archive this window opens by default, so a page built
    /// before the app has said which window it shows still reads the right file
    /// (`ChatState::default`, and every test that never calls `set_window`).
    fn default() -> Watch {
        Watch::new(&crate::assets::archive())
    }
}

impl Watch {
    /// A watch on the file beside `archive`.
    pub fn new(archive: &Path) -> Watch {
        Watch {
            file: path(archive),
            state: Setup::Nothing,
            at: None,
        }
    }

    /// Point the watch at another archive's directory. A path it already watches is not
    /// re-seated (the app says which window it shows on every frame), and neither is what it
    /// has read: a moving window does not make a running download un-happen.
    pub fn point(&mut self, archive: &Path) {
        let file = path(archive);
        if file != self.file {
            self.file = file;
            self.at = None;
        }
    }

    /// The file this watch reads.
    pub fn file(&self) -> &Path {
        &self.file
    }

    /// What was last read.
    pub fn state(&self) -> &Setup {
        &self.state
    }

    /// Read the file if a [`POLL`] has gone by since the last read, and answer what the watch
    /// now holds.
    pub fn poll(&mut self, now: Instant) -> &Setup {
        let due = self.at.map_or(true, |at| {
            now.checked_duration_since(at)
                .map_or(true, |waited| waited >= POLL)
        });
        if due {
            self.state = read(&self.file);
            self.at = Some(now);
        }
        &self.state
    }

    /// Read now, whatever the clock says: the first frame of a page, and the tests that walk
    /// the four reports. Not used per frame — that is [`Watch::poll`]'s job.
    pub fn refresh(&mut self, now: Instant) -> &Setup {
        self.state = read(&self.file);
        self.at = Some(now);
        &self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A directory of our own under the system temp dir, removed when the test ends: the
    /// window's own `data/` is never read or written by a test.
    struct TempData {
        dir: PathBuf,
    }

    static NEXT: AtomicU64 = AtomicU64::new(0);

    impl TempData {
        fn new() -> TempData {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|spent| spent.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "atlas-setup-{}-{}-{}",
                std::process::id(),
                nanos,
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).expect("a temp directory");
            TempData { dir }
        }

        /// The archive the watch is pointed at; only its directory matters.
        fn archive(&self) -> PathBuf {
            self.dir.join("atlas.db")
        }

        fn write(&self, text: &str) {
            std::fs::write(self.dir.join(SETUP_FILE), text).expect("a written setup report");
        }
    }

    impl Drop for TempData {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn a_running_download_is_read_as_its_bytes() {
        let setup = parse(
            r#"{"phase":"model","done":3200000000,"total":8000000000,"message":"downloading the model","ok":null}"#,
        );
        let download = setup.running().expect("a running download");
        assert_eq!(download.phase, MODEL_PHASE);
        assert!((download.fraction() - 0.4).abs() < 1e-6, "{}", download.fraction());
        assert_eq!(download.bytes(), "3.2 of 8.0 GB");
        assert_eq!(
            download.sentence(),
            "the model is being downloaded once — 3.2 of 8.0 GB"
        );
    }

    #[test]
    fn the_end_of_a_download_is_a_verdict_not_a_progress() {
        assert_eq!(parse(r#"{"ok":true,"message":"the model is installed"}"#), Setup::Done);
        assert_eq!(
            parse(r#"{"ok":false,"message":"the download stopped: the network went away."}"#),
            Setup::Failed("the download stopped: the network went away".to_string())
        );
        // a failure with nothing to say still has a sentence, because the page must say one
        match parse(r#"{"ok":false}"#) {
            Setup::Failed(why) => assert!(why.contains("model"), "{why}"),
            other => panic!("a failure was read as {other:?}"),
        }
    }

    #[test]
    fn anything_that_is_not_a_report_says_nothing() {
        for text in [
            "",
            "   ",
            "not json",
            r#"{"phase":""}"#,
            r#"{"ok":null}"#,
            r#"{"done":10}"#,
            r#"{"phase":"model","total":0,"done":0,"message":""}"#,
            r#"{"phase":"model","done":"a lot"}"#,
        ] {
            // the last two *are* reports — a download that has not measured itself yet, or one
            // whose counts this window cannot read — so they may answer with a download, and
            // with no bytes on it: a phase is a fact, a byte count is a measurement
            let setup = parse(text);
            if text.contains("\"phase\":\"model\"") {
                let download = setup.running().expect("a download with no size yet");
                assert_eq!(download.bytes(), "", "no total, no bytes");
                assert_eq!(download.fraction(), 0.0, "no total is not a division by zero");
            } else {
                assert_eq!(setup, Setup::Nothing, "{text:?} was read as {setup:?}");
            }
        }
    }

    #[test]
    fn a_small_download_is_read_in_its_own_unit() {
        let download = Download {
            phase: "model".to_string(),
            done: 1_500_000,
            total: 3_000_000,
            message: String::new(),
        };
        assert_eq!(download.bytes(), "1.5 of 3.0 MB");
    }

    #[test]
    fn the_file_is_read_at_most_twice_a_second() {
        let data = TempData::new();
        data.write(r#"{"phase":"model","done":1,"total":2,"message":""}"#);
        let mut watch = Watch::new(&data.archive());
        assert_eq!(watch.file(), data.dir.join(SETUP_FILE));
        let start = Instant::now();

        let first = watch.poll(start);
        assert!(matches!(first, Setup::Running(_)), "{first:?}");

        // The installer writes again a tenth of a second later: the window does not look —
        // that is the poll rate, and it is what keeps a frame off the filesystem.
        data.write(r#"{"ok":true,"message":"installed"}"#);
        assert_eq!(
            watch.poll(start + Duration::from_millis(100)),
            &Setup::Running(Download {
                phase: MODEL_PHASE.to_string(),
                done: 1,
                total: 2,
                message: String::new()
            })
        );

        // Half a second on, it does look.
        assert_eq!(watch.poll(start + POLL), &Setup::Done);
        // and a missing file after that is nothing at all, not a stale sentence
        std::fs::remove_file(data.dir.join(SETUP_FILE)).expect("the file goes away");
        assert_eq!(watch.poll(start + POLL + POLL), &Setup::Nothing);
    }

    #[test]
    fn a_watch_moved_to_another_window_keeps_what_it_read() {
        let data = TempData::new();
        let mut watch = Watch::new(&data.archive());
        data.write(r#"{"ok":true}"#);
        assert_eq!(watch.refresh(Instant::now()), &Setup::Done);
        // the app says which window it shows on every frame: the same one changes nothing
        watch.point(&data.archive());
        assert_eq!(watch.state(), &Setup::Done);
        // another window's directory is not this one's report
        watch.point(&data.dir.join("elsewhere").join("atlas.db"));
        assert_eq!(watch.file(), data.dir.join("elsewhere").join(SETUP_FILE));
    }
}
