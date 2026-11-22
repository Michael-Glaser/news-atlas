//! The first screen: what this software is, what it is not, and the license.
//!
//! # Why it exists
//!
//! ATLAS reads news for a reader and shows what a model made of it. That is a claim with
//! consequences, so before the workspace opens for the first time the reader is shown three
//! things in plain English: what the app does with their machine, that the model can be
//! wrong, and the terms — the Apache License 2.0 in full, the scope of that license, and the
//! absence of any warranty.
//!
//! # What the screen deliberately is not
//!
//! Not a modal, not a dialog over a blurred workspace, not a scroll of legalese with a
//! disabled button. It is a *document*: a wordmark, a title, three short paragraphs, the
//! whole license in a bordered column the reader can scroll, and one line at the end that has
//! to be ticked before the door opens. The license is the full text — a link would be a
//! promise this app cannot keep, because it has no network of its own.
//!
//! # Scope
//!
//! The license covers **this** code. The crates it is built on, the vendored llama.cpp tree
//! and the model weights keep their own terms, and the screen says so in one sentence: a
//! reader must not be able to think they are relicensing someone else's work by pressing a
//! button here.
//!
//! # The decision is remembered
//!
//! [`VERSION`] is written beside the archive when the reader accepts, in the same small
//! database the window already keeps for its requests (`crate::requests`). A later change to
//! the text — a new sentence, a new scope — bumps the version and the screen comes back:
//! consent to a document is consent to *that* document, not to any later one.

use eframe::egui;

use crate::theme::{self, role, space};

/// The full Apache License 2.0, embedded at build time.
///
/// `include_str!` and not a file read: this screen is the one thing that must appear on a
/// machine where nothing has been set up, and a screen that opened a file to draw itself could
/// fail on the day the file is missing. It is the same text as `LICENSE` in the repository
/// root, 201 lines.
pub const TEXT: &str = include_str!("../../assets/APACHE-2.0.txt");

/// What the reader accepted, and which version they accepted.
///
/// Stored as the key of one row, so the record says both: a new [`VERSION`] is a new key and
/// the old row cannot answer for it.
pub const VERSION: &str = "apache-2.0+notice-2";

/// The widest this document gets. Under 80 characters at [`theme::size::BODY`], which is the
/// line length a page of prose wants.
const COLUMN: f32 = 640.0;

/// What the reader did with this screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Accepted: the workspace may open.
    Accept,
    /// Asked to leave.
    Quit,
}

/// The screen's own state: how far the license was read, and whether the line was ticked.
#[derive(Debug, Default)]
pub struct Gate {
    /// The reader reached the end of the license text at least once.
    pub reached_end: bool,
    /// The acceptance line is ticked.
    pub checked: bool,
    /// A failed write, in the interface's own words, shown beside the button.
    pub failure: Option<String>,
}

impl Gate {
    /// Is the door allowed to open?
    pub fn ready(&self) -> bool {
        self.checked
    }
}

/// Draw the screen. Returns what the reader chose, if anything.
///
/// The acceptance line is ticked by a click or by Space (it is a real checkbox, so it is in
/// the tab order and answers the keyboard like every other control here); the button then
/// enables and answers Enter. "Read to the end" is not a gate — a reader may accept from the
/// first page — but the hint under the license stops asking once they have scrolled through
/// it, and the note says what the license does and does not cover.
pub fn show(ui: &mut egui::Ui, gate: &mut Gate) -> Option<Choice> {
    let mut choice = None;
    let panel = ui.available_rect_before_wrap();
    let width = panel.width().min(COLUMN);
    let pad = space::XXL;
    let left = panel.left() + ((panel.width() - width) / 2.0).max(0.0);
    let rect = egui::Rect::from_min_size(
        egui::pos2(left, panel.top() + pad),
        egui::vec2(
            (width - pad).max(0.0),
            (panel.height() - pad * 2.0).max(0.0),
        ),
    );
    let mut column = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    let ui = &mut column;

    // The lockup, in the same language as the rail: the mark, the name, and what it is.
    ui.horizontal(|ui| {
        let (mark, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
        crate::logo::paint(ui.painter(), mark, theme::INK);
        ui.add_space(space::SM);
        ui.label(role::CARD_TITLE.text("ATLAS"));
        ui.add_space(space::SM);
        ui.label(role::CAPTION.text("local news intelligence"));
    });

    ui.add_space(space::XXL);
    ui.label(role::PAGE_TITLE.text("Read this before you start."));
    ui.add_space(space::LG);

    for paragraph in DISCLAIMER {
        ui.label(role::BODY.text(*paragraph));
        ui.add_space(space::LG);
    }

    ui.add_space(space::SM);
    ui.separator();
    ui.add_space(space::MD);
    ui.horizontal(|ui| {
        ui.label(role::SECTION_LABEL.text("Apache License 2.0"));
        ui.add_space(space::SM);
        ui.label(role::CAPTION.text("version 2.0, January 2004 — the whole text"));
    });
    ui.add_space(space::MD);

    // The license column: its own scroll area, so the document around it stays put while a
    // person reads 201 lines. Its height is what is left after the acceptance row, measured
    // rather than typed, so a small window scrolls the text further and a tall one shows more
    // of it — and the text is the frame's one non-ladder size, because a license is set in a
    // monospaced column so its indentation survives.
    let acceptance = 120.0;
    let room = (ui.available_height() - acceptance).max(140.0);
    let scroll = egui::ScrollArea::vertical()
        .id_salt("license-text")
        .max_height(room)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for line in TEXT.lines() {
                let text = if line.trim().is_empty() {
                    " "
                } else {
                    line
                };
                // The document role: 11 px monospace, dim ink. It is a named role (and
                // not a `FontId` literal) because the design audit reads a painted line's
                // (size, face) pair and asks the role table whether the interface produces
                // it — the one page whose text is a legal document is not an excuse for a
                // size no role knows.
                ui.label(theme::role::DOCUMENT.text(text));
            }
        });
    // "Read to the end" is measured from the scroll area's own state: the tallest offset it
    // allows, against where the reader is. Nothing here runs on a timer.
    let offset = scroll.state.clamped_offset().y;
    let furthest = (scroll.content_size.y - scroll.inner_rect.height()).max(0.0);
    if furthest <= 0.0 || offset >= furthest - 4.0 {
        gate.reached_end = true;
    }
    if !gate.reached_end {
        ui.add_space(space::SM);
        ui.label(role::CAPTION.text("scroll to the end to read the rest"));
    }

    ui.add_space(space::MD);
    ui.checkbox(
        &mut gate.checked,
        role::BODY.text(
            "I have read and accept the Apache License 2.0, the scope in the notice, and \
             the disclaimer above.",
        ),
    )
    .on_hover_text(
        "Covers this app's own source. The libraries, the model server and the model keep \
         their own licenses.",
    );
    if let Some(failure) = &gate.failure {
        ui.add_space(space::SM);
        ui.label(role::CAPTION.text(failure));
    }

    // The one thing a reader may want before ticking: the app can serve a model of their
    // own, and the command is the whole answer (the Settings page does the same thing
    // through the queue). Read-only text, so it costs no write and no frame read. It names
    // no path and no file extension: `tests/disclosure.rs` keeps those out of every view,
    // and the Settings page already shows the file in force.
    ui.add_space(space::SM);
    ui.label(role::CAPTION.text(
        "Using a model of your own: run atlas-setup --import-model with the file or a link, \
         then restart ATLAS. Settings shows the model in use and the llama.cpp flags it gets.",
    ));

    ui.add_space(space::MD);
    ui.horizontal(|ui| {
        // Not `add_enabled_ui`: a disabled egui widget is drawn by multiplying alpha, which
        // is the one thing this window never paints (measured: fill rgb(8,8,8) and stroke
        // rgb(18,18,18), both at alpha 128 — two colours no palette can name). Before the
        // tick, the same place holds the sentence that says what is missing.
        if gate.ready() {
            let button = egui::Button::new(role::CARD_TITLE.text("Accept and open ATLAS"))
                .min_size(egui::vec2(0.0, theme::space::XL + theme::space::SM))
                .corner_radius(theme::RADIUS);
            if ui.add(button).clicked() {
                choice = Some(Choice::Accept);
            }
        } else {
            ui.label(role::CAPTION.text("Tick the box to accept, then this button appears."));
        }
        ui.add_space(space::MD);
        // Framed on purpose, and the frame is load-bearing: egui paints keyboard focus *as* the
        // widget's frame (`style.rs:1386` — `response.has_focus()` → `WidgetVisuals::active`,
        // whose `bg_stroke` is `theme::focus_stroke`), and `Button::frame(false)` paints the text
        // and no frame at all. Measured: drawn `frame(false)`, tabbing through this screen put the
        // ring on the tick and on "Accept and open ATLAS" and nowhere over "Quit", so a keyboard
        // user could not see where they were. The same rule, in the same words, is at
        // `views::topics::table::request_link`; this is that rule applied to the one control it
        // was missing from. The frame is the theme's hairline, not a new ornament.
        let quit = egui::Button::new(role::BODY.text("Quit"))
            .min_size(egui::vec2(0.0, theme::space::XL + theme::space::SM))
            .corner_radius(theme::RADIUS);
        if ui.add(quit).clicked() {
            choice = Some(Choice::Quit);
        }
        ui.add_space(space::MD);
        ui.label(role::CAPTION.text(
            "No warranty. You are responsible for how you use what this app shows you.",
        ));
    });
    choice
}

/// The three paragraphs above the license.
///
/// Written for a person who has just double-clicked something: what it does, what it cannot
/// do, and what the terms are. No marketing, no promises, and no word that needs a glossary.
/// The third paragraph is the scope the reader must not be able to misread: the license
/// covers what this project can license, and nothing else.
pub const DISCLAIMER: &[&str] = &[
    "ATLAS runs on this machine. A local model decides what to search for, opens those pages, \
     reads them, and files what it finds in a local database.",
    "The searches leave this machine: the query goes to one news search engine (built in: Bing \
     News) and the pages it points at are fetched. Nothing else is sent, there is no account and \
     no telemetry, and nothing is collected while you are not using it. Settings can point the \
     search at an engine you choose.",
    "It can be wrong. A model summarizes, classifies and connects; it does not verify. Treat \
     what you read here as a first pass over the reporting rather than the reporting itself, \
     and open the original article before you rely on a number, a quote or a name.",
    "The software is licensed under the Apache License 2.0. That license covers this app's own \
     source only: the libraries it is built on, the model server and the model itself keep \
     their own licenses and are not relicensed here. There is no warranty, and the authors \
     accept no liability for what the app shows you or for what you do with it. You are \
     responsible for your use of it, and for the terms of the pages it reads.",
];


// ------------------------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    /// What one frame painted that this file's question is about: the rectangles (with the
    /// stroke each was outlined by) and the runs of text (with the box each occupies).
    #[derive(Default)]
    struct Painted {
        rects: Vec<(egui::Rect, egui::Stroke)>,
        text: Vec<(String, egui::Rect)>,
    }

    impl Painted {
        /// Where a label was painted, by its words.
        fn at(&self, words: &str) -> egui::Rect {
            self.text
                .iter()
                .find(|(text, _)| text.trim() == words)
                .map(|(_, rect)| *rect)
                .unwrap_or_else(|| {
                    panic!(
                        "nothing painted {words:?}; painted:\n{}",
                        self.text
                            .iter()
                            .map(|(text, _)| text.as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    )
                })
        }

        /// Is one of the painted outlines the theme's keyboard focus ring?
        fn is_focus_ring(stroke: egui::Stroke) -> bool {
            let ring = theme::focus_stroke();
            stroke.color == ring.color && stroke.width >= ring.width
        }

        /// The focus ring over a point, if there is one.
        fn ring_over(&self, at: egui::Pos2) -> Option<egui::Rect> {
            self.rects
                .iter()
                .find(|(rect, stroke)| Self::is_focus_ring(*stroke) && rect.contains(at))
                .map(|(rect, _)| *rect)
        }

        fn any_ring(&self) -> bool {
            self.rects
                .iter()
                .any(|(_, stroke)| Self::is_focus_ring(*stroke))
        }
    }

    /// A shape tree flattened: a control's frame is nested inside the panel's own `Shape::Vec`s,
    /// and a walker that stopped at the first level would see no button at all.
    fn collect(shape: &egui::Shape, out: &mut Painted) {
        match shape {
            egui::Shape::Vec(items) => {
                for item in items {
                    collect(item, out);
                }
            }
            egui::Shape::Rect(rect) => out.rects.push((rect.rect, rect.stroke)),
            egui::Shape::Text(text) => out.text.push((
                text.galley.text().to_string(),
                egui::Rect::from_min_size(text.pos, text.galley.size()),
            )),
            _ => {}
        }
    }

    fn fresh() -> egui::Context {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        ctx
    }

    /// One frame of the screen at a window tall enough for the acceptance row to be on it.
    ///
    /// Two passes with the events on the first: egui lays the screen out on the pass it is
    /// given and paints its final size on the next, and a key replayed on both passes is a
    /// second key press the widget would act on twice.
    fn frame(ctx: &egui::Context, gate: &mut Gate, events: Vec<egui::Event>) -> Painted {
        let mut painted = Painted::default();
        for pass in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 1100.0),
                )),
                events: if pass == 0 { events.clone() } else { Vec::new() },
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    let _ = show(ui, gate);
                });
            });
            painted = Painted::default();
            for clipped in &output.shapes {
                collect(&clipped.shape, &mut painted);
            }
            // egui 0.37 asserts in debug when a `FullOutput` is dropped with texture deltas in
            // it; a headless frame has nothing to upload them to.
            output.drop_without_applying_deltas();
        }
        painted
    }

    fn tab() -> egui::Event {
        egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// The Quit button is in the tab order and, when the keyboard is on it, it wears the theme's
    /// focus ring — the same rule `views::topics::table::request_link` states in words: egui
    /// paints focus *as* the widget's frame (`WidgetVisuals::active`), so a button drawn
    /// `Button::frame(false)` has no visible keyboard focus at all. Measured before the fix:
    /// the ring was on the checkbox and on "Accept and open ATLAS", and nowhere over "Quit".
    #[test]
    fn the_quit_button_wears_the_focus_ring_when_the_keyboard_reaches_it() {
        let ctx = fresh();
        let mut gate = Gate {
            checked: true,
            ..Gate::default()
        };
        let first = frame(&ctx, &mut gate, Vec::new());
        let quit = first.at("Quit");
        let quit_at = quit.center();

        // Walk the tab order. Four controls can hold the keyboard on this screen (the tick, the
        // accept button, the quit button, and egui's own scroll area), so six presses is more
        // than enough and the test does not pin *how many* there are — only where they land.
        let mut ring_seen_somewhere = false;
        for _ in 0..6 {
            frame(&ctx, &mut gate, vec![tab()]);
            let painted = frame(&ctx, &mut gate, Vec::new());
            ring_seen_somewhere |= painted.any_ring();
            if painted.ring_over(quit_at).is_some() {
                return;
            }
        }
        assert!(
            ring_seen_somewhere,
            "no control on this screen paints a focus ring, so this frame proves nothing about \
             the Quit button — the harness or the theme is the defect here"
        );
        panic!(
            "the keyboard reaches the other controls, but the Quit button paints no focus ring: a \
             keyboard user cannot see where they are. It is drawn `.frame(false)`, which throws \
             away the state (`WidgetVisuals::active`) egui paints the ring from \
             (theme.rs:38-44, topics.rs:861-867). Quit's box was {quit:?}."
        );
    }
}
