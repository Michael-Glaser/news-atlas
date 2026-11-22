//! What ATLAS says on a terminal: one line at a time, no colour, no progress bars.
//!
//! The launcher is the only part of the app a user reads before the UI exists, and
//! two threads write to it (the boot sequence and the supervisor). [`Printer`]
//! serialises a whole line so a restart notice can never land inside a "waiting
//! for the model" line.
//!
//! Three kinds of line, and the words say which one it is: [`Printer::note`] is the
//! app speaking, [`Printer::warn`] is something degraded *and the app is still
//! running*, [`Printer::error`] is a refusal the process stops on. Formatting is
//! pure ([`note_line`], [`warn_line`], [`error_line`]) so the three can be tested
//! without capturing a terminal, and [`one_line`] is what keeps an operating-system
//! message from breaking the doctor's columns.
//!
//! ## The one place a line is made safe to print
//!
//! Everything ATLAS prints goes through [`Printer`]: paths, pids, exit statuses,
//! port numbers, the model's file name, the owner a lock record names. Three of
//! those are strings this process did not author — a file name in `models/`, a
//! `--root`, the contents of `data/atlas.lock` — and a name holding a newline or an
//! escape sequence used to reach the terminal verbatim. On a terminal that is a line
//! ATLAS never wrote (`ATLAS: nothing it started is still running` under a headline
//! that says otherwise) and a screen an attacker can clear and repaint; on a log file
//! it is a forged entry. So [`printable`] is applied to every line here, once, at the
//! boundary: control characters become `?`, and the line is capped. It is the same
//! rule `atlas-web`'s static handler and `atlas-guard`'s `clean_text` keep for their
//! own inputs, narrowed to what a terminal is allowed to receive.
//!
//! It decides nothing else: no exit codes, no wording of a check, no ANSI.
//! Monochrome English, as the product rule says.

use std::io::Write;
use std::sync::Mutex;
use std::time::Duration;

/// A line-at-a-time writer for stdout and stderr.
///
/// `say` is the app speaking, `warn` is the app telling the user that something
/// degraded but the app is still running. Nothing here colours, animates or wraps
/// with Unicode: a terminal in a log file has to stay readable.
pub struct Printer {
    out: Mutex<()>,
    err: Mutex<()>,
}

impl Default for Printer {
    fn default() -> Printer {
        Printer::new()
    }
}

/// The prefix every line ATLAS speaks carries, so a line in a log file is
/// attributable to one program.
pub const PREFIX: &str = "ATLAS: ";

/// The word that marks a line as "degraded, but running". Only [`warn`] uses it:
/// a fatal refusal is not a warning, and saying "warning:" above an exit code of
/// 1 is how a user learns not to read the warnings.
pub const WARNING: &str = "warning: ";

/// The longest line ATLAS prints. A path is at most a few kilobytes; a line
/// longer than this is a value that is not a path.
pub const MAX_LINE: usize = 4_000;

/// One line of text that cannot be read as two, and cannot carry a terminal
/// instruction.
///
/// Control characters (`\n`, `\r`, `\t`, `ESC`, `DEL`, C1) become `?`, and
/// everything after [`MAX_LINE`] characters is cut. A value that legitimately
/// holds a space — a path — keeps it: this is not `clean_text`, which decides what
/// may be *stored*; it decides what a *terminal* may receive.
pub fn printable(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_LINE));
    let mut taken = 0;
    for ch in text.chars() {
        if taken >= MAX_LINE {
            out.push('…');
            break;
        }
        out.push(if ch.is_control() { '?' } else { ch });
        taken += 1;
    }
    out
}

/// A line the app speaks: `ATLAS: root /x`.
pub fn note_line(text: &str) -> String {
    format!("{}{}", PREFIX, text)
}

/// A line that says something degraded but the app is still running.
pub fn warn_line(text: &str) -> String {
    format!("{}{}{}", PREFIX, WARNING, text)
}

/// A line that says ATLAS will not do what was asked (the exit code is 1 or 2).
/// Deliberately not "warning": nothing continues.
pub fn error_line(text: &str) -> String {
    format!("{}{}", PREFIX, text)
}

/// Write one line, made safe to print, into a handle.
///
/// The single function [`Printer::say`], [`Printer::warn`] and [`Printer::error`]
/// all write through, so "what a terminal receives" is one code path — and one a
/// test can drive with a `Vec<u8>` instead of a terminal it would have to capture.
pub fn write_line(out: &mut impl Write, line: &str) -> std::io::Result<()> {
    writeln!(out, "{}", printable(line))
}

impl Printer {
    pub fn new() -> Printer {
        Printer {
            out: Mutex::new(()),
            err: Mutex::new(()),
        }
    }

    /// One line on stdout.
    ///
    /// [`write_line`] runs here, so no caller can print a second line or an escape
    /// sequence by accident.
    pub fn say(&self, line: impl AsRef<str>) {
        if let Ok(_guard) = self.out.lock() {
            let mut out = std::io::stdout().lock();
            let _ = write_line(&mut out, line.as_ref());
            let _ = out.flush();
        }
    }

    /// One line on stdout, with `ATLAS: ` in front.
    pub fn note(&self, line: impl AsRef<str>) {
        self.say(note_line(line.as_ref()));
    }

    /// One line on stderr, with a prefix that says it is not fatal.
    pub fn warn(&self, line: impl AsRef<str>) {
        if let Ok(_guard) = self.err.lock() {
            let mut err = std::io::stderr().lock();
            let _ = write_line(&mut err, &warn_line(line.as_ref()));
            let _ = err.flush();
        }
    }

    /// One line on stderr for a refusal: the sentence that explains why ATLAS
    /// stopped, and what to do about it. No "warning:" — nothing continues.
    pub fn error(&self, line: impl AsRef<str>) {
        if let Ok(_guard) = self.err.lock() {
            let mut err = std::io::stderr().lock();
            let _ = write_line(&mut err, &error_line(line.as_ref()));
            let _ = err.flush();
        }
    }

    /// Several lines that ATLAS itself wrote — `atlas help`.
    ///
    /// The one entry point where a newline is intended. It still goes through
    /// [`printable`] per line, so the rule ("a terminal receives no control
    /// character") has no exception a later caller could pick: the difference
    /// between this and [`Printer::say`] is the *line break*, not the characters.
    pub fn block(&self, text: impl AsRef<str>) {
        for line in text.as_ref().lines() {
            self.say(line);
        }
    }
}

/// A byte count as a person reads it: `412 MB`, `5.4 GB`.
///
/// Decimal units, because a GGUF file is advertised in decimal gigabytes by the
/// place it came from, and a mismatch would look like a truncated download.
pub fn size(bytes: u64) -> String {
    const KB: f64 = 1_000.0;
    const MB: f64 = 1_000_000.0;
    const GB: f64 = 1_000_000_000.0;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.1} GB", value / GB)
    } else if value >= MB {
        format!("{:.0} MB", value / MB)
    } else if value >= KB {
        format!("{:.0} kB", value / KB)
    } else {
        format!("{} B", bytes)
    }
}

/// A wait as a person reads it: `0.4s`, `12s`, `3m 20s`, `4m`.
pub fn span(duration: Duration) -> String {
    let secs = duration.as_secs_f64();
    if secs < 1.0 {
        format!("{:.1}s", secs)
    } else if secs < 60.0 {
        format!("{:.0}s", secs)
    } else {
        let whole = secs as u64;
        // A whole number of minutes is not followed by "0s": the boot prints this
        // for its own deadline, and "waiting up to 4m 0s" reads like a bug.
        if whole % 60 == 0 {
            format!("{}m", whole / 60)
        } else {
            format!("{}m {}s", whole / 60, whole % 60)
        }
    }
}

/// One line out of text that may carry several — the first non-empty one.
///
/// Every value the doctor prints goes through [`entry`], which promises one line
/// per check. An operating-system or SQLite error arrives with newlines in it; a
/// raw copy would push the status column of every following check out of
/// alignment and make the table unreadable.
pub fn one_line(text: &str) -> String {
    text.lines()
        .map(|line| line.trim())
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_string()
}

/// One aligned entry: `  toolchain      ok    cargo 1.100.0`.
///
/// `name` is padded to [`NAME_WIDTH`] characters so a column of checks scans
/// straight down, and a name that is longer keeps its own line rather than
/// pushing the status out of alignment.
pub const NAME_WIDTH: usize = 20;

pub fn entry(name: &str, status: &str, detail: &str) -> String {
    let padded = if name.chars().count() < NAME_WIDTH {
        format!("{}{}", name, " ".repeat(NAME_WIDTH - name.chars().count()))
    } else {
        name.to_string()
    };
    if detail.is_empty() {
        format!("  {} {}", padded, status)
    } else {
        format!("  {} {}  {}", padded, status, detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_byte_count_reads_as_a_person_would_say_it() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(999), "999 B");
        assert_eq!(size(1_400), "1 kB");
        assert_eq!(size(412_000_000), "412 MB");
        assert_eq!(size(5_412_000_000), "5.4 GB");
    }

    #[test]
    fn a_wait_reads_in_seconds_and_minutes() {
        assert_eq!(span(Duration::from_millis(400)), "0.4s");
        assert_eq!(span(Duration::from_secs(12)), "12s");
        assert_eq!(span(Duration::from_secs(200)), "3m 20s");
        // A whole minute is not "4m 0s": this is the boot's own deadline line.
        assert_eq!(span(Duration::from_secs(240)), "4m");
        assert_eq!(span(Duration::from_secs(3600)), "60m");
    }

    #[test]
    fn a_fatal_line_is_not_called_a_warning() {
        let fatal = error_line("another ATLAS is already running");
        assert_eq!(fatal, "ATLAS: another ATLAS is already running");
        assert!(!fatal.contains("warning"), "{}", fatal);
        // The degraded case still says so, and both are attributable to ATLAS.
        let degraded = warn_line("the local model is not answering");
        assert!(degraded.starts_with(PREFIX), "{}", degraded);
        assert!(degraded.contains(WARNING), "{}", degraded);
        assert_eq!(note_line("root /x"), "ATLAS: root /x");
    }

    #[test]
    fn a_multi_line_detail_is_reduced_to_one_line() {
        // What a SQLite or io error looks like when it is printed raw.
        let raw = "\n  database is locked: cannot write\n  second line\n";
        assert_eq!(one_line(raw), "database is locked: cannot write");
        assert_eq!(one_line(""), "");
        let line = entry("sqlite", "FAIL", &one_line(raw));
        assert!(!line.contains('\n'), "{:?}", line);
    }

    #[test]
    fn what_reaches_a_terminal_is_one_line_with_no_control_character() {
        // The production path, byte for byte: `Printer::say`/`warn`/`error` write
        // through `write_line`, so this is what a terminal (or `data/app.log`, or a
        // terminal multiplexer's scrollback) receives for a hostile value.
        let hostile = note_line(&format!(
            "model {}",
            "evil.gguf\nATLAS: nothing it started is still running\u{1b}[2J"
        ));
        let mut sink: Vec<u8> = Vec::new();
        write_line(&mut sink, &hostile).expect("a Vec<u8> accepts a line");
        let written = String::from_utf8(sink).expect("the line is UTF-8");
        assert_eq!(
            written.matches('\n').count(),
            1,
            "exactly one line break, the one writeln adds: {:?}",
            written
        );
        assert!(written.ends_with('\n'), "{:?}", written);
        assert!(
            !written.chars().any(|ch| ch.is_control() && ch != '\n'),
            "a control character reached the terminal: {:?}",
            written
        );
        assert!(written.contains("evil.gguf"), "{:?}", written);
        // The warning and error paths use the same function; a value cannot pick
        // which one it lands in.
        let mut sink: Vec<u8> = Vec::new();
        write_line(&mut sink, &warn_line("x\u{1b}[1A")).unwrap();
        let written = String::from_utf8(sink).unwrap();
        assert!(!written.contains('\u{1b}'), "{:?}", written);
    }

    #[test]
    fn a_hostile_file_name_cannot_become_a_second_line_or_a_screen_instruction() {
        // The attack: a model file named `evil.gguf<newline>ATLAS: nothing it started
        // is still running<ESC>[2J` is printed by `start` as `model <name>`. Before
        // [`printable`] every one of those characters reached the terminal: one fake
        // line that contradicts the real one, and an escape sequence that clears the
        // screen. On a log file it is a forged entry, on a terminal a repainted one.
        let name = "evil.gguf\nATLAS: nothing it started is still running\u{1b}[2J\r\u{7}";
        let line = printable(&note_line(&format!("model {}", name)));
        assert!(!line.contains('\n'), "{:?}", line);
        assert!(!line.contains('\r'), "{:?}", line);
        assert!(!line.contains('\u{1b}'), "{:?}", line);
        assert!(
            !line.chars().any(|ch| ch.is_control()),
            "a control character survived: {:?}",
            line
        );
        // The visible words are still there: this is a sanitiser, not a filter that
        // hides where a value came from.
        assert!(line.contains("evil.gguf"), "{:?}", line);
        assert!(line.contains("nothing it started is still running"), "{:?}", line);
        // A path with spaces and quotes keeps them, because a path must be readable.
        assert_eq!(
            printable("/tmp/a b/\"quoted\" model.gguf"),
            "/tmp/a b/\"quoted\" model.gguf"
        );
    }

    #[test]
    fn a_line_that_is_too_long_for_a_terminal_is_cut() {
        let long = "A".repeat(MAX_LINE * 2);
        let line = printable(&long);
        assert!(line.chars().count() <= MAX_LINE + 1, "{}", line.chars().count());
        assert!(line.ends_with('…'), "{:?}", &line[line.len() - 8..]);
    }

    #[test]
    fn the_help_block_keeps_its_lines_and_loses_its_control_characters() {
        // `atlas help` is the one thing ATLAS prints that is deliberately several
        // lines: the boundary must not join it into one, and must still remove
        // anything a terminal would obey.
        let block = "usage: atlas start\n  --root DIR\n\t--no-open\u{1b}[1m";
        let lines: Vec<String> = block.lines().map(printable).collect();
        assert_eq!(lines.len(), 3, "{:?}", lines);
        assert_eq!(lines[0], "usage: atlas start");
        assert!(!lines[2].contains('\u{1b}'), "{:?}", lines[2]);
        // A printed block is printed once: the same rule, through the printer.
        let printer = Printer::new();
        printer.block(block);
    }

    #[test]
    fn an_entry_lines_up_and_tolerates_a_long_name() {
        let line = entry("toolchain", "ok", "cargo 1.100.0");
        assert!(line.starts_with("  toolchain            ok  cargo 1.100.0"), "{}", line);
        let long = entry("a name that is far longer than the column", "fail", "x");
        assert!(long.contains("far longer than the column fail"), "{}", long);
    }

    #[test]
    fn printing_writes_whole_lines_and_can_be_shared_between_threads() {
        let printer = std::sync::Arc::new(Printer::new());
        let mut handles = Vec::new();
        for index in 0..8 {
            let printer = printer.clone();
            handles.push(std::thread::spawn(move || {
                for step in 0..25 {
                    printer.say(format!("thread {} step {}", index, step));
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
    }
}
