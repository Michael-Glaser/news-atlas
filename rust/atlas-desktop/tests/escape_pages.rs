//! Esc on the pages that keep a detail open: it closes that detail and leaves the page — and
//! everything else on it — exactly where the reader was.
//!
//! One test per page, one frame per keystroke, at the size the window is: Articles (the
//! reading pane), Topics (a topic's terms and its story band) and Monitors (a monitor's
//! query). Chat's own detail — the working-out under an answer — is pinned inside
//! `src/views/chat.rs`, because a transcript line's fold state is that module's private
//! business and its test module already has the painting helpers to reach it.
//!
//! **Why the key cannot be read behind `has_focus()`.** egui *surrenders* the focused widget
//! on the Escape key in `begin_pass`, before any widget is laid out
//! (`egui-0.37.0/src/memory/mod.rs:597` clears `focused_widget`), so a page whose only gate is
//! `response.has_focus()` never runs its handler: the key reads as "nothing happened" and the
//! detail stays open. Each of these tests therefore presses Escape as the only event of a
//! frame and demands the detail close — the state that was measured as inert on the running
//! window.
//!
//! Nothing here opens an archive or a socket: every fixture is written by hand, and each page
//! is drawn straight into a frame.

use egui::{Pos2, Rect, Shape, Vec2};

use atlas_desktop::data::{Article, Monitor, Topic};
use atlas_desktop::theme;
use atlas_desktop::views::articles::{self, ArticlesView};
use atlas_desktop::views::monitors::{self, MonitorStories};
use atlas_desktop::views::topics::{self, Stories};

/// The screen every frame is told about: the app's own default window, so a page lays out as
/// it does in the window (the rail on the left, the status row at the bottom).
const SCREEN: Vec2 = Vec2::new(1440.0, 900.0);

/// The window the app reads its stories in.
const HOURS: i64 = 72;

/// What one frame painted, as text, and what the page answered.
#[derive(Default)]
struct Painted {
    text: String,
}

impl Painted {
    fn has(&self, needle: &str) -> bool {
        self.text.contains(needle)
    }
}

/// One frame of a page, in the box the window gives it: the rail and the status row are
/// drawn as empty panels so the page's own box is the app's, and the page itself is the only
/// thing that paints text (so a string found here is the page's, not the shell's).
fn frame<T>(
    ctx: &egui::Context,
    index: usize,
    events: Vec<egui::Event>,
    mut page: impl FnMut(&mut egui::Ui) -> T,
) -> (Painted, T) {
    let mut painted = Painted::default();
    let mut answer: Option<T> = None;
    let output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
            // a real clock, so `stable_dt` is a sixtieth and not zero
            time: Some(index as f64 / 60.0),
            events,
            ..Default::default()
        },
        |ui| {
            egui::Panel::left("rail")
                .exact_size(188.0)
                .resizable(false)
                .show(ui, |_| {});
            egui::Panel::bottom("status")
                .exact_size(38.0)
                .frame(egui::Frame::NONE)
                .show(ui, |_| {});
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::NONE
                        .fill(theme::PAGE)
                        .inner_margin(egui::Margin::symmetric(20, 16)),
                )
                .show(ui, |ui| {
                    answer = Some(page(ui));
                });
        },
    );
    for clipped in &output.shapes {
        walk(&clipped.shape, &mut painted);
    }
    output.drop_without_applying_deltas();
    (painted, answer.expect("the page was drawn"))
}

fn walk(shape: &Shape, painted: &mut Painted) {
    match shape {
        Shape::Vec(items) => {
            for item in items {
                walk(item, painted);
            }
        }
        Shape::Text(text) => painted.text.push_str(text.galley.text()),
        _ => {}
    }
}

/// The Escape key as the platform sends it: pressed, no modifiers.
fn escape() -> Vec<egui::Event> {
    vec![egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    }]
}

fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    ctx
}

// ------------------------------------------------------------------------ the fixtures

fn story(id: i64, title: &str, body: &str) -> Article {
    Article {
        id,
        title: title.to_string(),
        source: "Madrid Desk".to_string(),
        published: "2024-05-01T09:00:00Z".to_string(),
        summary: "A summary.".to_string(),
        content: body.to_string(),
        country: "Chile".to_string(),
        url: "https://example.invalid/story".to_string(),
        full_text: true,
    }
}

/// **Articles.** Esc closes the reading pane and the list stays: the reader is still on the
/// page, one keystroke away from the story they had open (Tab puts the ring back on the first
/// row — `egui-0.37.0/src/memory/mod.rs:677`).
#[test]
fn escape_closes_the_open_story_and_keeps_the_articles_page() {
    let ctx = context();
    let stories = vec![
        story(1, "First story", "The body of the first story."),
        story(2, "Second story", "The body of the second story."),
    ];
    let mut view = ArticlesView {
        needle: String::new(),
        selected: Some(2),
    };
    let (open, _) = frame(&ctx, 0, Vec::new(), |ui| {
        let _ = articles::show(ui, &mut view, &stories, HOURS);
    });
    assert!(
        open.has("The body of the second story."),
        "the reading pane was not open to begin with:\n{}",
        open.text
    );

    let (shut, answered) = frame(&ctx, 1, escape(), |ui| {
        articles::show(ui, &mut view, &stories, HOURS)
    });
    assert_eq!(view.selected, None, "Esc left the story open");
    assert_eq!(answered, None, "Esc reported a story to open");
    assert!(
        !shut.has("The body of the second story."),
        "the reading pane is still painted after Esc:\n{}",
        shut.text
    );
    assert!(
        shut.has("Choose a story on the left to read it here."),
        "the pane did not fall back to its own sentence:\n{}",
        shut.text
    );
    // the page, and the list on it
    assert!(
        shut.has("First story") && shut.has("Second story"),
        "Esc took rows out of the list:\n{}",
        shut.text
    );
    assert!(
        shut.has("stories in the last 72 h"),
        "Esc left the page:\n{}",
        shut.text
    );
}

fn topic(id: i64, name: &str, keywords: &str) -> Topic {
    Topic {
        id,
        name: name.to_string(),
        keywords: keywords.to_string(),
        active: true,
        hits: 3,
        last_hit: "2024-05-01T09:00:00Z".to_string(),
    }
}

/// **Topics.** Esc shuts the open topic: its terms and its story band go, the table stays.
#[test]
fn escape_closes_the_open_topic_and_keeps_the_topics_page() {
    let ctx = context();
    let topics = vec![
        topic(7, "Lithium brine", "lithium; brine; salar"),
        topic(8, "Copper", "copper; smelter"),
    ];
    topics::remember_open(&ctx, Some(7));
    let stories: [Article; 0] = [];

    let (open, _) = frame(&ctx, 0, Vec::new(), |ui| {
        let _ = topics::show_with(ui, &topics, Stories { topic: None, rows: &stories });
    });
    assert_eq!(
        topics::open_topic(&ctx),
        Some(7),
        "the topic was not open to begin with"
    );
    assert!(
        open.has("lithium; brine; salar"),
        "the open topic's terms are not on the page:\n{}",
        open.text
    );

    let (shut, _) = frame(&ctx, 1, escape(), |ui| {
        let _ = topics::show_with(ui, &topics, Stories { topic: None, rows: &stories });
    });
    assert_eq!(topics::open_topic(&ctx), None, "Esc left the topic open");
    // The state closes on the frame the key arrives; the terms are painted from the state the
    // page read at the top of the frame, so the *picture* is one frame behind — and the page
    // asks for that frame itself (`topics.rs`: `if open != was_open { request_repaint() }`).
    // A reader sees the terms go on the next 16 ms; this asserts on it.
    let (after, _) = frame(&ctx, 2, Vec::new(), |ui| {
        let _ = topics::show_with(ui, &topics, Stories { topic: None, rows: &stories });
    });
    assert!(
        !after.has("lithium; brine; salar"),
        "the terms are still painted after Esc:\n{}",
        after.text
    );
    let _ = shut;
    assert!(
        after.has("Lithium brine") && after.has("Copper"),
        "Esc took rows out of the table:\n{}",
        after.text
    );
    assert!(after.has("Topics"), "Esc left the page:\n{}", after.text);
}

fn monitor(id: i64, name: &str, query: &str) -> Monitor {
    Monitor {
        id,
        name: name.to_string(),
        query: query.to_string(),
        active: true,
        cadence: 60,
        last_run: "2024-05-01T09:00:00Z".to_string(),
        hits: 2,
    }
}

/// **Monitors.** Esc shuts the open monitor: its query goes, the table stays.
#[test]
fn escape_closes_the_open_monitor_and_keeps_the_monitors_page() {
    let ctx = context();
    let monitors = vec![
        monitor(3, "Brine watch", "salar OR brine"),
        monitor(4, "Copper watch", "smelter"),
    ];
    monitors::remember_open(&ctx, Some(3));

    let (open, _) = frame(&ctx, 0, Vec::new(), |ui| {
        let _ = monitors::show_with(ui, &monitors, MonitorStories::none());
    });
    assert_eq!(
        monitors::open_monitor(&ctx),
        Some(3),
        "the monitor was not open to begin with"
    );
    assert!(
        open.has("salar OR brine"),
        "the open monitor's query is not on the page:\n{}",
        open.text
    );

    let (shut, _) = frame(&ctx, 1, escape(), |ui| {
        let _ = monitors::show_with(ui, &monitors, MonitorStories::none());
    });
    assert_eq!(monitors::open_monitor(&ctx), None, "Esc left the monitor open");
    // the picture is one frame behind the state, as on Topics, and the page asks for it
    let (after, _) = frame(&ctx, 2, Vec::new(), |ui| {
        let _ = monitors::show_with(ui, &monitors, MonitorStories::none());
    });
    assert!(
        !after.has("salar OR brine"),
        "the query is still painted after Esc:\n{}",
        after.text
    );
    let _ = shut;
    assert!(
        after.has("Brine watch") && after.has("Copper watch"),
        "Esc took rows out of the table:\n{}",
        after.text
    );
    assert!(after.has("Monitors"), "Esc left the page:\n{}", after.text);
}
