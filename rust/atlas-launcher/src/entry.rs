//! The process boundary: arguments in, lines out, an exit code.
//!
//! `main.rs` is four lines long because everything here is testable: [`main_entry`]
//! takes the arguments after the program name and returns the exit code, so a test
//! can ask "what does `atlas doctor` on an empty directory do?" without a terminal.
//!
//! Exit codes, once, for the whole app:
//!
//! * `0` — the command did what it said;
//! * `1` — ATLAS cannot do it, and the sentence printed above says why;
//! * `2` — the command line was not understood.

use crate::args::{self, Command};
use crate::doctor;
use crate::home;
use crate::report::Printer;
use crate::start;

/// Run one command line and answer with the exit code.
pub fn main_entry(argv: &[String]) -> i32 {
    let printer = Printer::new();
    let command = match args::parse(argv) {
        Ok(command) => command,
        // A command line that was not understood is a refusal (exit 2), so the
        // line is not prefixed with "warning:" — nothing continues after it.
        Err(error) => {
            printer.error(error.message);
            printer.say("Try `atlas help`.");
            return 2;
        }
    };
    match command {
        Command::Help => {
            // The only deliberately multi-line text ATLAS prints; `block` keeps its
            // lines while `Printer::say`'s rule (no control character on a terminal)
            // still holds for each of them.
            printer.block(args::HELP);
            0
        }
        Command::Version => {
            printer.say(args::version_line());
            0
        }
        Command::Doctor(root) => {
            // The same root rule as `start`, from one place: `--root .` names the
            // absolute directory in the header, not a dot.
            let report = doctor::check(&home::paths(&root));
            report.print(&printer);
            report.exit_code()
        }
        // `start` prints the sentence itself when the failure has to appear before
        // the shutdown summary; an empty message means "already said, just exit 1".
        Command::Start(options) => match start::run(options) {
            Ok(()) => 0,
            Err(message) => {
                if !message.trim().is_empty() {
                    printer.error(message);
                }
                1
            }
        },
    }
}
