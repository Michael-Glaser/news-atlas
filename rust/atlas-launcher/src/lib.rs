//! `atlas` — the single entry point: one binary that runs the app.
//!
//! Before this crate the app was started by `run.sh`/`run.ps1` plus `server/app.py`,
//! and `llama-server` was started by the Python. Now one Rust binary does the whole
//! boot, and the pieces it starts are the crates:
//!
//! | Module | What it decides |
//! | --- | --- |
//! | [`args`] | the command line: `start`, `doctor`, `help`, `--version` |
//! | [`home`] | which project root a command line names, made absolute |
//! | [`start`] | the boot sequence, in the one order that is safe |
//! | [`plan`] | the same sequence as the lines `--dry-run` prints, spawning nothing |
//! | [`lock`] | one instance per data directory, and the stale lock it may find |
//! | [`model`] | which GGUF file is loaded, and the exact sentence when it is missing |
//! | [`setup`] | provisioning: when the model or the model server is missing, the one install child |
//! | [`llama`] | the model server's binary and its flags |
//! | [`silence`] | is the model server's own log silenced, and what to do when it refuses |
//! | [`health`] | is `/health` answering, and how long we wait |
//! | [`supervise`] | one restart with a backoff, then a plain report |
//! | [`shutdown`] | the stop order: the window, the agent, then the model server |
//! | [`signals`] | Ctrl-C and `SIGTERM` as one future |
//! | [`window`] | the product, `atlas-desktop`, as a child this process waits for |
//! | [`agent`] | the always-on half, `atlas-monitor serve`, as a child that claims the queue |
//! | [`childlog`] | the model server's log, bounded across restarts |
//! | [`doctor`] | what a fresh machine is missing, in plain English, with an exit code |
//! | [`toolchain`] | cargo and rustc, found and named |
//! | [`ports`] | is a loopback port free (asked of the kernel, once) |
//! | [`child`] | spawn, read and stop a child — the one implementation |
//! | [`report`] | the lines a person reads: monochrome, English, no numbers that were not measured |
//! | [`entry`] | argv, stdout and the exit code |
//!
//! # The rules this crate keeps
//!
//! * **No port is ever opened.** This process binds nothing and serves nothing: it
//!   starts the model server (whose own loopback listener is in `llama-server`'s argv,
//!   not here), starts the monitoring agent and the native window as children, and
//!   waits. The window reads
//!   `data/atlas.db` itself and dials the model outbound. The only sockets this crate
//!   touches are outbound: the model server's `/health` probe and the doctor's port
//!   probe, which connects and hangs up instead of binding.
//! * **Nothing is downloaded, except once.** A missing model or binary is a sentence with
//!   the exact path and the manual step, and `atlas doctor` makes no network call at all.
//!   The one exception is the first run of `atlas start` on a machine that has never been
//!   provisioned: the model is several gigabytes and cannot ship with the app, so
//!   [`setup`] runs `atlas-setup --install` as a *child* — this crate links no HTTP
//!   client — once, says so before and after, and never again once the pieces are in
//!   place. `--no-install` turns that off, and then the old rule holds unchanged.
//! * **No child is left behind.** The model server, the monitoring agent and the window
//!   are children of this process and are stopped and reaped before it exits, in
//!   [`shutdown::ORDER`] — the window first, then the agent that is using it, then the
//!   model. A process this launcher did not start is never signalled, and never stopped.
//! * **One instance.** An OS advisory lock on the `data/` directory itself (so the
//!   name `data/atlas.lock` cannot be swapped for a fresh file under a running
//!   instance) plus one on `data/atlas.lock`, which is the record of who holds it.
//!   A second `atlas start` refuses instead of writing to the same database; a lock
//!   nobody holds is reported as stale and taken over, and nothing in the record
//!   decides that — only the kernel does.
//! * **No `unsafe`.** `#![forbid(unsafe_code)]`, and no `libc`: the polite signal
//!   goes through `kill(1)`, which is reaped like any other child.
//! * **Nothing from a page or a model is printed.** The launcher prints paths,
//!   ports, pids, exit statuses and counts; never text it did not produce.

#![forbid(unsafe_code)]

pub mod agent;
pub mod args;
pub mod child;
pub mod childlog;
pub mod doctor;
pub mod entry;
pub mod health;
pub mod home;
pub mod llama;
pub mod lock;
pub mod model;
pub mod silence;
pub mod plan;
pub mod ports;
pub mod report;
pub mod setup;
pub mod shutdown;
pub mod signals;
pub mod start;
pub mod supervise;
pub mod toolchain;
pub mod window;

#[cfg(test)]
mod tests;

pub use args::{Command, StartOptions};
pub use entry::main_entry;
pub use report::Printer;
pub use shutdown::{Shutdown, Step};
