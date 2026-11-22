//! Live AI activity and streaming reasoning, read-only from `data/agent-status.json`.
//!
//! `atlas-monitor` and `atlas-ingest` write `data/agent-status.json` atomically while
//! researching topics, checking monitors, and writing briefings. This module is strictly
//! the reader — it never creates, writes, or modifies any file.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::theme;

/// The file written beside `data/atlas.db`.
pub const STATUS_FILE: &str = "agent-status.json";

/// Maximum file size read into memory (64 KB safety cap).
const MAX_BYTES: u64 = 64 * 1024;

/// How often the window reads `agent-status.json` while polling.
pub const POLL: Duration = Duration::from_millis(400);

/// How often the window checks `atlas.db` / `atlas.db-wal` metadata for external updates.
pub const STAMP_POLL: Duration = Duration::from_millis(1_000);

/// Snapshot of what the background AI agent is currently doing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Activity {
    /// True once at least one valid `agent-status.json` snapshot has been read.
    pub present: bool,
    /// True while the background AI agent is actively running a task.
    pub active: bool,
    /// Task kind (`research`, `monitor_run`, `brief`, `enrich`, ...).
    pub kind: String,
    /// Human-readable topic or scope label.
    pub topic: String,
    /// Current step description (`searching news: "china"`, `thinking (step 2/14)`, ...).
    pub phase: String,
    /// Current agent step (1-based while running, 0 before step 1 or when idle).
    pub step: usize,
    /// Maximum steps for the current task.
    pub max_steps: usize,
    /// Live streamed reasoning tail from the local model.
    pub reasoning: String,
    /// Recent timestamped action lines.
    pub log: Vec<String>,
    /// Pending rows remaining in the background queue.
    pub queued: usize,
    /// ISO-8601 UTC timestamp of the last status update.
    pub updated_at: String,
}

impl Activity {
    /// One-line summary shown in the compact activity strip.
    pub fn summary(&self) -> String {
        if self.active {
            let mut parts: Vec<String> = Vec::new();
            if !self.phase.is_empty() {
                parts.push(self.phase.clone());
            } else if !self.kind.is_empty() {
                parts.push(self.kind.clone());
            } else {
                parts.push("working".to_string());
            }
            if !self.topic.is_empty() && !parts[0].contains(&self.topic) {
                parts.push(format!("\"{}\"", self.topic));
            }
            if self.step > 0 && self.max_steps > 0 && !parts[0].contains("step ") {
                parts.push(format!("step {}/{}", self.step, self.max_steps));
            }
            if self.queued > 0 {
                parts.push(format!("{} queued", self.queued));
            }
            format!("AI active · {}", parts.join(" · "))
        } else {
            let mut parts = vec!["AI idle".to_string()];
            if self.queued > 0 {
                parts.push(format!("{} queued", self.queued));
            }
            if let Some(last) = self.log.last() {
                parts.push(last.clone());
            }
            parts.join(" · ")
        }
    }
}

/// `<root>/data/agent-status.json` beside `archive`.
pub fn path(archive: &Path) -> PathBuf {
    archive
        .parent()
        .map(|data| data.join(STATUS_FILE))
        .unwrap_or_else(|| PathBuf::from(STATUS_FILE))
}

/// Read `agent-status.json` once. Missing, oversized, or malformed files return `Default`.
pub fn read(path: &Path) -> Activity {
    let Ok(meta) = std::fs::metadata(path) else {
        return Activity::default();
    };
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return Activity::default();
    }
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(_) => Activity::default(),
    }
}

/// Parse the JSON written by `atlas_ingest::live`.
pub fn parse(text: &str) -> Activity {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Activity::default();
    };
    if !value.is_object() {
        return Activity::default();
    }
    let str_of = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let usize_of = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as usize
    };
    let log = value
        .get("log")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Activity {
        present: true,
        active: value
            .get("active")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
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

/// Throttled reader for `data/agent-status.json`.
#[derive(Clone, Debug)]
pub struct Watch {
    file: PathBuf,
    state: Activity,
    at: Option<Instant>,
}

impl Watch {
    pub fn new(archive: &Path) -> Watch {
        Watch {
            file: path(archive),
            state: Activity::default(),
            at: None,
        }
    }

    /// Re-read the file at most once every [`POLL`].
    pub fn poll(&mut self, now: Instant) -> &Activity {
        let due = match self.at {
            None => true,
            Some(prev) => now.saturating_duration_since(prev) >= POLL,
        };
        if due {
            self.state = read(&self.file);
            self.at = Some(now);
        }
        &self.state
    }

    pub fn current(&self) -> &Activity {
        &self.state
    }
}

/// File modification stamp of `atlas.db` and its `-wal` sibling (read-only metadata).
///
/// Comparing two stamps tells the window when `atlas-monitor` has committed a new topic,
/// monitor, article, or briefing without opening an extra SQLite read.
pub fn archive_stamp(archive: &Path) -> Option<(Option<SystemTime>, u64, Option<SystemTime>, u64)> {
    let main = std::fs::metadata(archive).ok()?;
    let mut wal_os = archive.as_os_str().to_os_string();
    wal_os.push("-wal");
    let wal_path = PathBuf::from(wal_os);
    let (wal_mod, wal_len) = match std::fs::metadata(&wal_path) {
        Ok(meta) => (meta.modified().ok(), meta.len()),
        Err(_) => (None, 0),
    };
    Some((main.modified().ok(), main.len(), wal_mod, wal_len))
}

/// Draw the live AI activity strip and, when expanded, the Activity Log & Live Reasoning
/// drawer. Lives here (not in `app.rs`) so the shell owns zero scroll areas.
pub fn show(
    ui: &mut egui::Ui,
    act: &Activity,
    open: &mut bool,
    dismissed: &mut bool,
) {
    ui.horizontal(|ui| {
        let dot = if act.active { "●" } else { "○" };
        let dot_text = if act.active {
            theme::body(dot)
        } else {
            theme::dim(dot)
        };
        ui.label(dot_text);
        let summary = act.summary();
        let toggle_label = if *open {
            "Hide reasoning"
        } else {
            "Show reasoning & log"
        };
        let right_w = 160.0_f32;
        let avail = (ui.available_width() - right_w - theme::space::MD).max(120.0);
        ui.allocate_ui_with_layout(
            egui::vec2(avail, 20.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                let label = if act.active {
                    theme::body(&summary)
                } else {
                    theme::dim(&summary)
                };
                ui.add(egui::Label::new(label).truncate());
            },
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(theme::caption(toggle_label)).clicked() {
                *open = !*open;
                *dismissed = !*open;
            }
        });
    });

    if !*open {
        return;
    }

    ui.add_space(4.0);
    ui.separator();
    ui.add_space(6.0);

    let total_w = ui.available_width();
    let left_w = (total_w * 0.42).clamp(200.0, 420.0);
    let right_w = (total_w - left_w - theme::space::MD).max(180.0);
    let drawer_h = 168.0_f32;

    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(left_w, drawer_h),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.horizontal(|ui| {
                    ui.label(theme::section_label("AI activity"));
                    if act.active && act.max_steps > 0 {
                        ui.label(theme::dim(&format!(
                            "· step {}/{}",
                            act.step, act.max_steps
                        )));
                    }
                    if act.queued > 0 {
                        ui.label(theme::dim(&format!("· {} queued", act.queued)));
                    }
                });
                if !act.topic.is_empty() || !act.phase.is_empty() {
                    let line = if act.topic.is_empty() {
                        act.phase.clone()
                    } else if act.phase.is_empty() {
                        act.topic.clone()
                    } else {
                        format!("{} — {}", act.topic, act.phase)
                    };
                    ui.add(egui::Label::new(theme::body(&line)).truncate());
                }
                ui.add_space(4.0);
                egui::Frame::NONE
                    .fill(theme::RECESS)
                    .stroke(theme::line())
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("ai_activity_log")
                            .max_height(drawer_h - 46.0)
                            .auto_shrink([false, false])
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                if act.log.is_empty() {
                                    ui.label(theme::dim(
                                        "No actions recorded yet. Add a topic or click Research to watch the agent work.",
                                    ));
                                } else {
                                    for entry in &act.log {
                                        ui.label(theme::caption(entry));
                                    }
                                }
                            });
                    });
            },
        );

        ui.add_space(theme::space::MD);

        ui.allocate_ui_with_layout(
            egui::vec2(right_w, drawer_h),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.horizontal(|ui| {
                    ui.label(theme::section_label("Live reasoning"));
                    if act.active {
                        ui.label(theme::dim("· streaming"));
                    }
                });
                ui.add_space(4.0);
                egui::Frame::NONE
                    .fill(theme::RECESS)
                    .stroke(theme::line())
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("ai_live_reasoning")
                            .max_height(drawer_h - 30.0)
                            .auto_shrink([false, false])
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                if !act.reasoning.trim().is_empty() {
                                    ui.add(
                                        egui::Label::new(theme::caption(&act.reasoning)).wrap(),
                                    );
                                } else if act.active {
                                    ui.label(theme::dim(
                                        "Waiting for model reasoning tokens...",
                                    ));
                                } else {
                                    ui.label(theme::dim(
                                        "No active reasoning right now. Start a research pass or add a topic to watch the model think live.",
                                    ));
                                }
                            });
                    });
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_active_snapshot_with_reasoning_and_log() {
        let raw = r#"{
            "active": true,
            "kind": "monitor_run",
            "topic": "China",
            "phase": "searching news: \"china\"",
            "step": 2,
            "max_steps": 14,
            "reasoning": "Let me look for recent news articles on China.",
            "log": ["23:55:01Z · added topic \"China\" (queued research)", "23:55:03Z · find_news: \"china\""],
            "queued": 1,
            "updated_at": "2026-11-22T23:55:03Z"
        }"#;
        let act = parse(raw);
        assert!(act.present);
        assert!(act.active);
        assert_eq!(act.topic, "China");
        assert_eq!(act.step, 2);
        assert_eq!(act.max_steps, 14);
        assert_eq!(act.queued, 1);
        assert_eq!(act.log.len(), 2);
        assert!(act.reasoning.contains("recent news"));
        let summary = act.summary();
        assert!(summary.contains("AI active"));
        assert!(summary.contains("searching news"));
        assert!(summary.contains("1 queued"));
    }

    #[test]
    fn parse_invalid_or_empty_is_absent() {
        assert!(!parse("").present);
        assert!(!parse("not json").present);
        assert!(!parse("[]").present);
    }
}
