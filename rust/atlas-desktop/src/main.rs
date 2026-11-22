//! ATLAS as a native window — the binary.
//!
//! One process, one window: it draws the archive with egui, reads `data/atlas.db`
//! through the same SQLite library the rest of the workspace uses, and opens no listener
//! at all. There is no HTML, no CSS, no wasm bundle, no page and no socket for a page to
//! talk to — a window, a GPU surface and a database.
//!
//! The one socket it does open is outbound: [`atlas_desktop::http`] dials the local model
//! server on a `127.0.0.1` literal, for the Chat page only, and only when the reader
//! sends a message. Nothing listens, nothing is spawned, and the archive is opened
//! `READ_ONLY` — the window is a reader of the tables the collection side writes.
//!
//! What it deliberately does not do: start the model server. Chat is the one view that
//! needs `llama-server`; it says so in the status row instead of guessing.
//!
//! Everything else lives in the library (`src/lib.rs`), so the integration tests can link
//! this crate instead of reaching into its files with `#[path]`.

// A shipped Windows build is a window, not a window plus a console. Debug builds keep the
// console, because that is where a panic and a `println!` land while working.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use atlas_desktop::{app, logo};

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ATLAS")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([860.0, 560.0])
            // The dock/taskbar icon is drawn here, not shipped as a file: a PNG in the
            // tree is one more thing that can be missing on a fresh machine, and this way
            // the icon cannot drift away from the mark the rail draws.
            .with_icon(logo::icon(256))
            .with_app_id("atlas"),
        ..Default::default()
    };
    eframe::run_native(
        "ATLAS",
        options,
        Box::new(|cc| Ok(Box::new(app::Atlas::new(cc)))),
    )
}
