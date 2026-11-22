//! Live AI activity and streaming reasoning, written atomically to `data/agent-status.json`.
//!
//! `atlas-desktop` is strictly read-only on the archive and cannot talk to `atlas-monitor`
//! over a socket. Instead, the background agent and research pipeline write one small,
//! private (`0600`) JSON snapshot beside the archive while they work, and the desktop
//! window polls it (just like `data/setup.json`).
//!
//! Nothing here ever writes article bodies or credentials. Reasoning and action summaries
//! are scrubbed of control/bidi characters and capped in length so the file stays tiny
//! (< 8 KB) and safe to read on every UI tick.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// The status file name under `<root>/data/`.
pub const STATUS_FILE: &str = "agent-status.json";

/// Maximum characters of live reasoning kept in the snapshot.
const MAX_REASONING_CHARS: usize = 4_000;

/// Maximum recent action log lines kept in the snapshot.
const MAX_LOG_LINES: usize = 20;

/// Minimum interval between disk flushes while streaming reasoning tokens.
const THINKING_FLUSH_EVERY: Duration = Duration::from_millis(350);

/// Serialises writes across the fast-lane topic poller and the slow research worker so
/// neither overwrites the other's log lines or active state.
static FILE_LOCK: Mutex<()> = Mutex::new(());

/// `<root>/data/agent-status.json`.
pub fn status_path_for_root(root: &Path) -> PathBuf {
    root.join("data").join(STATUS_FILE)
}

/// `agent-status.json` beside a file inside `data/` (e.g. `data/atlas.db` or `data/requests.db`).
pub fn status_path_beside(file: &Path) -> PathBuf {
    file.parent()
        .map(|dir| dir.join(STATUS_FILE))
        .unwrap_or_else(|| PathBuf::from(STATUS_FILE))
}

/// Append one timestamped line and update the queued count without disturbing any active
/// slow task currently streaming reasoning.
pub fn record_note(path: &Path, message: &str, queued: Option<usize>) {
    let Ok(_guard) = FILE_LOCK.lock() else {
        return;
    };
    let mut state = read_or_default(path);
    let line = stamped_line(message);
    push_log_line(&mut state.log, line);
    if let Some(q) = queued {
        state.queued = q;
    }
    state.updated_at = now_iso();
    let _ = write_snapshot(path, &state);
}

/// Mark the background agent idle while keeping the recent action log intact.
pub fn set_idle(path: &Path, queued: usize, note: Option<&str>) {
    let Ok(_guard) = FILE_LOCK.lock() else {
        return;
    };
    let mut state = read_or_default(path);
    state.active = false;
    state.kind.clear();
    state.topic.clear();
    state.phase = "idle".to_string();
    state.step = 0;
    state.max_steps = 0;
    state.reasoning.clear();
    state.queued = queued;
    if let Some(msg) = note {
        push_log_line(&mut state.log, stamped_line(msg));
    }
    state.updated_at = now_iso();
    let _ = write_snapshot(path, &state);
}

/// A live status handle for one active AI task (research, monitor run, briefing, enrich).
pub struct LiveStatus {
    path: PathBuf,
    kind: String,
    topic: String,
    phase: String,
    step: usize,
    max_steps: usize,
    reasoning: String,
    queued: Option<usize>,
    last_thinking_flush: Instant,
}

impl LiveStatus {
    /// Start tracking a task and immediately publish the initial snapshot.
    pub fn start(
        path: PathBuf,
        kind: &str,
        topic: &str,
        phase: &str,
        max_steps: usize,
        queued: Option<usize>,
    ) -> LiveStatus {
        let mut live = LiveStatus {
            path,
            kind: clean_line(kind, 40),
            topic: clean_line(topic, 120),
            phase: clean_line(phase, 140),
            step: 0,
            max_steps,
            reasoning: String::new(),
            queued,
            last_thinking_flush: Instant::now()
                .checked_sub(THINKING_FLUSH_EVERY)
                .unwrap_or_else(Instant::now),
        };
        let note = if live.topic.is_empty() {
            format!("{}: {}", live.kind, live.phase)
        } else {
            format!("{} \"{}\"", live.phase, live.topic)
        };
        live.flush(Some(&note));
        live
    }

    /// Update the current phase and step counter and flush immediately.
    pub fn set_phase(&mut self, phase: &str, step: usize, max_steps: usize, note: Option<&str>) {
        self.phase = clean_line(phase, 140);
        self.step = step;
        if max_steps > 0 {
            self.max_steps = max_steps;
        }
        self.flush(note);
    }

    /// Append a streamed reasoning token and flush at most every [`THINKING_FLUSH_EVERY`].
    pub fn push_thinking(&mut self, step: usize, delta: &str) {
        if delta.is_empty() {
            return;
        }
        let step_changed = step != self.step && step > 0;
        if step_changed {
            self.step = step;
            self.phase = if self.max_steps > 0 {
                format!("thinking (step {}/{})", step, self.max_steps)
            } else {
                format!("thinking (step {step})")
            };
            if !self.reasoning.is_empty() {
                self.reasoning.push_str("\n---\n");
            }
        } else if self.phase.is_empty() || self.phase == "starting" {
            self.phase = "thinking".to_string();
        }
        for ch in delta.chars() {
            if ch == '\n' || ch == '\t' {
                self.reasoning.push(ch);
            } else if ch.is_control() || atlas_guard::cleantext::is_invisible(ch) {
                continue;
            } else {
                self.reasoning.push(ch);
            }
        }
        trim_reasoning_head(&mut self.reasoning, MAX_REASONING_CHARS);
        if step_changed || self.last_thinking_flush.elapsed() >= THINKING_FLUSH_EVERY {
            self.last_thinking_flush = Instant::now();
            self.flush(None);
        }
    }

    /// Append a human-readable log entry without changing the current phase, and flush.
    pub fn log(&mut self, message: &str) {
        self.flush(Some(message));
    }

    /// Write the current state to disk, merging with any log lines added by another thread.
    fn flush(&mut self, note: Option<&str>) {
        let Ok(_guard) = FILE_LOCK.lock() else {
            return;
        };
        let mut state = read_or_default(&self.path);
        state.active = true;
        state.kind = self.kind.clone();
        state.topic = self.topic.clone();
        state.phase = self.phase.clone();
        state.step = self.step;
        state.max_steps = self.max_steps;
        state.reasoning = self.reasoning.clone();
        if let Some(q) = self.queued.take() {
            state.queued = q;
        }
        if let Some(msg) = note {
            push_log_line(&mut state.log, stamped_line(msg));
        }
        state.updated_at = now_iso();
        let _ = write_snapshot(&self.path, &state);
    }
}

/// On-disk snapshot structure.
#[derive(Default)]
struct Snapshot {
    active: bool,
    kind: String,
    topic: String,
    phase: String,
    step: usize,
    max_steps: usize,
    reasoning: String,
    log: Vec<String>,
    queued: usize,
    updated_at: String,
}

fn read_or_default(path: &Path) -> Snapshot {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Snapshot::default();
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return Snapshot::default();
    };
    let str_of = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let usize_of = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize
    };
    let log = value
        .get("log")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Snapshot {
        active: value.get("active").and_then(Value::as_bool).unwrap_or(false),
        kind: str_of("kind"),
        topic: str_of("topic"),
        phase: str_of("phase"),
        step: usize_of("step"),
        max_steps: usize_of("max_steps"),
        reasoning: str_of("reasoning"),
        log,
        queued: usize_of("queued"),
        updated_at: str_of("updated_at"),
    }
}

fn write_snapshot(path: &Path, state: &Snapshot) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    // Never create directories out of thin air in unit tests where `data/` does not exist.
    if !parent.is_dir() {
        return Ok(());
    }
    if atlas_platform::private::is_link(path) {
        return Ok(());
    }
    let tmp = path.with_extension("json.tmp");
    if atlas_platform::private::is_link(&tmp) {
        return Ok(());
    }
    let body = json!({
        "active": state.active,
        "kind": state.kind,
        "topic": state.topic,
        "phase": state.phase,
        "step": state.step,
        "max_steps": state.max_steps,
        "reasoning": state.reasoning,
        "log": state.log,
        "queued": state.queued,
        "updated_at": state.updated_at,
    })
    .to_string();
    let mut options = atlas_platform::private::create_options();
    options.truncate(true);
    let mut file = options.open(&tmp)?;
    file.write_all(body.as_bytes())?;
    file.flush()?;
    drop(file);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn push_log_line(log: &mut Vec<String>, line: String) {
    if line.is_empty() {
        return;
    }
    log.push(line);
    if log.len() > MAX_LOG_LINES {
        let drop_n = log.len() - MAX_LOG_LINES;
        log.drain(0..drop_n);
    }
}

fn stamped_line(message: &str) -> String {
    let clean = clean_line(message, 180);
    if clean.is_empty() {
        return String::new();
    }
    let iso = now_iso();
    // Extract HH:MM:SS from YYYY-MM-DDTHH:MM:SSZ.
    let hms = iso.get(11..19).unwrap_or(&iso);
    format!("{hms}Z · {clean}")
}

fn clean_line(text: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max_chars));
    for ch in text.chars() {
        if out.chars().count() >= max_chars {
            break;
        }
        if ch.is_control() || atlas_guard::cleantext::is_invisible(ch) {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out.split_whitespace().collect::<Vec<&str>>().join(" ")
}

fn trim_reasoning_head(buf: &mut String, max_chars: usize) {
    let count = buf.chars().count();
    if count <= max_chars {
        return;
    }
    let skip = count - max_chars;
    if let Some((byte_idx, _)) = buf.char_indices().nth(skip) {
        buf.drain(0..byte_idx);
    }
}

fn now_iso() -> String {
    atlas_store::clock::iso_from_epoch(atlas_store::clock::epoch_secs_now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_data_dir(label: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "atlas-live-{}-{}-{}",
            label,
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn live_status_streams_reasoning_and_preserves_fast_lane_notes() {
        let dir = temp_data_dir("stream");
        let path = dir.join(STATUS_FILE);

        let mut live = LiveStatus::start(
            path.clone(),
            "research",
            "China chips",
            "starting research",
            14,
            Some(1),
        );
        live.push_thinking(1, "Let me search recent news on China chips.");
        // Simulate fast-lane thread adding a topic while research is running.
        record_note(&path, "added topic \"Energy\" (queued research)", Some(2));
        live.set_phase("searching news: \"China chips\"", 1, 14, Some("find_news: \"China chips\""));

        let text = std::fs::read_to_string(&path).expect("read status");
        let value: Value = serde_json::from_str(&text).expect("parse status");
        assert_eq!(value["active"], true);
        assert_eq!(value["topic"], "China chips");
        assert_eq!(value["step"], 1);
        assert!(value["reasoning"]
            .as_str()
            .unwrap_or("")
            .contains("search recent news"));
        let log = value["log"].as_array().expect("log array");
        assert!(
            log.iter().any(|entry| entry.as_str().unwrap_or("").contains("Energy")),
            "fast-lane note must be preserved: {log:?}"
        );
        assert!(
            log.iter().any(|entry| entry.as_str().unwrap_or("").contains("find_news")),
            "tool call note must be recorded: {log:?}"
        );

        set_idle(&path, 0, Some("finished"));
        let after: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read idle"))
                .expect("parse idle");
        assert_eq!(after["active"], false);
        assert_eq!(after["phase"], "idle");
        assert!(after["log"].as_array().unwrap().len() >= 3);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
