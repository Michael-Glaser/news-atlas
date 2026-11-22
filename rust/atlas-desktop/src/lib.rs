//! ATLAS as a native window: the library half.
//!
//! Everything the window is lives here; `main.rs` is the six lines that open it. The
//! split exists because a binary crate cannot be linked by its own tests — before this
//! file, every integration test reached into `src/` with `#[path = "../src/data.rs"]`,
//! which meant a test could not see a `pub(crate)` item, a duplicated copy of a module
//! had to be kept in `tests/support/`, and one file's `mod tests;` resolved against the
//! *test's* directory. All three are symptoms of the same missing boundary.
//!
//! The modules, and what each decides:
//!
//! * [`app`] — the frame: a rail, a page, a status row, and *when* the archive is read.
//! * [`views`] — what a page looks like, one module per page.
//! * [`data`] — every query, over one read-only SQLite connection.
//! * [`assets`] — where the project root and the map's outlines are.
//! * [`geo`] — the map's projection, zoom and level of detail, with no UI types in it.
//! * [`gesture`] — the pointer arithmetic behind a smooth pinch and drag.
//! * [`theme`] — the palette, the type ladder, the one corner radius.
//! * [`logo`] — the mark, drawn with the painter and rasterised for the dock.
//! * [`link`] — the one place a stored address may become an action.
//! * [`http`] — the one socket: a POST to the local model on a `127.0.0.1` literal.
//! * [`requests`] — the one file the window writes: an intent for the supervised agent.
//! * [`setup`] — what first setup is doing, read from `data/setup.json` and never written.
//!
//! Nothing here listens, spawns, writes the archive, or downloads. The single write is the
//! request queue, and it is a *request*: a row the agent may claim, never an action the
//! window takes.

pub mod agent;
pub mod app;
pub mod assets;
pub mod data;
pub mod geo;
pub mod gesture;
pub mod http;
pub mod link;
pub mod logo;
pub mod requests;
pub mod setup;
pub mod theme;
pub mod tools;
pub mod webview;
pub mod views;
