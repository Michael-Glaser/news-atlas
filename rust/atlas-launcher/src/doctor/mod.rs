//! `atlas doctor` — one command, plain English, an exit code.
//!
//! Everything a fresh machine needs, in the order a person would check it, with
//! no network call anywhere: the toolchain, the window binary, the model file and its
//! size, the loopback invariant, a writable SQLite, the model server's port, and the
//! permissions on `data/`. There is no browser check and no web-asset check because the
//! product has neither: one window, the local model, no listener.
//!
//! One rule ties the checks to the app: **the doctor's verdict has to be the
//! answer `atlas start` would give.** A check fails only when the boot would stop;
//! anything the boot works around (no browser, a model server that is not there, a
//! model port already served by a `llama-server` ATLAS will reuse) is a warning or
//! an `ok` with the caveat in the detail.
//!
//! Three levels, and only one of them is fatal:
//!
//! * `ok`    — the check passed;
//! * `warn`  — ATLAS starts, with less than the machine could give it (no model
//!   server: the window opens without the local model);
//! * `FAIL`  — ATLAS cannot start until this is fixed.
//!
//! The exit code is 1 when anything failed and 0 otherwise, so a script can use
//! it. Every failure carries the exact command or path that fixes it, and the
//! model failure names the file and says the link is printed, never opened.

use std::path::PathBuf;

use atlas_config::Paths;

use crate::report::{self, Printer};

mod checks;

#[cfg(test)]
mod tests;

pub use checks::{
    llama_check, loopback_check, model_check, permissions_check, ports_check, sqlite_check,
    toolchain_check, window_check,
};

use checks::config_check;

/// How one check ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    /// The word in the column. Not colour: a log has no terminal.
    pub fn word(&self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "FAIL",
        }
    }
}

/// One check's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub detail: String,
    /// What to do about it, when the detail is not already an instruction.
    pub fix: Option<String>,
}

impl Check {
    pub fn ok(name: &str, detail: impl Into<String>) -> Check {
        Check {
            name: name.to_string(),
            status: Status::Ok,
            detail: detail.into(),
            fix: None,
        }
    }

    pub fn warn(name: &str, detail: impl Into<String>, fix: impl Into<String>) -> Check {
        Check {
            name: name.to_string(),
            status: Status::Warn,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    pub fn fail(name: &str, detail: impl Into<String>, fix: impl Into<String>) -> Check {
        Check {
            name: name.to_string(),
            status: Status::Fail,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    /// The line a person reads, then the fix on its own line when there is one.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![report::entry(&self.name, self.status.word(), &self.detail)];
        if let Some(fix) = &self.fix {
            lines.push(format!("  {}-> {}", " ".repeat(report::NAME_WIDTH), fix));
        }
        lines
    }
}

/// Every check, for one root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub root: PathBuf,
    pub checks: Vec<Check>,
}

impl Report {
    pub fn failed(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.status == Status::Fail)
            .count()
    }

    pub fn warned(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.status == Status::Warn)
            .count()
    }

    /// 1 when ATLAS cannot start, 0 when it can.
    pub fn exit_code(&self) -> i32 {
        if self.failed() > 0 {
            1
        } else {
            0
        }
    }

    /// One check by name, for a caller that wants to assert on it.
    pub fn find(&self, name: &str) -> Option<&Check> {
        self.checks.iter().find(|check| check.name == name)
    }

    /// The whole report, monochrome, English.
    pub fn print(&self, printer: &Printer) {
        printer.say(format!("ATLAS doctor — {}", self.root.display()));
        printer.say("");
        for check in &self.checks {
            for line in check.lines() {
                printer.say(line);
            }
        }
        printer.say("");
        let failed = self.failed();
        let warned = self.warned();
        if failed == 0 && warned == 0 {
            printer.say(format!(
                "{} checks, all clear — `atlas start` will run.",
                self.checks.len()
            ));
        } else if failed == 0 {
            printer.say(format!(
                "{} checks, {} warning{} — `atlas start` will run, with the limits above.",
                self.checks.len(),
                warned,
                if warned == 1 { "" } else { "s" }
            ));
        } else {
            printer.say(format!(
                "{} checks, {} failed, {} warning{} — fix the FAIL lines above; `atlas start` \
                 would stop at the first of them.",
                self.checks.len(),
                failed,
                warned,
                if warned == 1 { "" } else { "s" }
            ));
        }
    }
}

/// Run every check for one project root. Nothing is written outside `data/` (three
/// probe files, created and removed), no socket is bound (the port check connects and
/// hangs up), and nothing leaves the machine.
pub fn check(paths: &Paths) -> Report {
    let mut checks: Vec<Check> = Vec::new();
    checks.push(checks::toolchain_check());
    checks.push(checks::window_check(paths));
    let config = config_check(paths);
    let config_value = config.clone();
    checks.push(checks::model_check(paths, config_value.as_ref()));
    checks.push(checks::llama_check(paths));
    checks.push(checks::loopback_check(paths, config_value.as_ref()));
    checks.push(checks::sqlite_check(paths));
    checks.push(checks::ports_check(paths, config_value.as_ref()));
    checks.push(checks::permissions_check(paths));
    Report {
        root: paths.root.clone(),
        checks,
    }
}
