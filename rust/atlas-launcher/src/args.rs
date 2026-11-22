//! The command line: `atlas start`, `atlas doctor`, `atlas help`, `atlas --version`.
//!
//! Parsing is pure — a vector of arguments in, a [`Command`] out — so every shape
//! a shell can produce is a test and not a walkthrough of `main`. Nothing here
//! reads the environment, files or a clock; [`Command::Start`] carries the whole
//! decision a command line can make (`root`, and whether this is a `--dry-run`),
//! so a boot has no hidden switch to fall back on.
//!
//! An unknown word is refused, never guessed at: `atlas strat` must not silently
//! start something. `atlas` with no argument prints the help and exits 0.

use std::path::PathBuf;

/// The product version, in the one form a support report quotes.
///
/// `atlas-web` publishes the same string (`SERVER_VERSION`, atlas-web/src/lib.rs:97) and
/// this crate no longer links that crate: it is the *product's* name and number, not the
/// listener's, so it is spelled here rather than pulled in with a server. A bump moves
/// both — the same way `docs/THREAT-MODEL.md` names the version.
pub const PRODUCT: &str = "ATLAS/1.0";

/// The version line, built from the product and this crate's own numbers.
pub fn version_line() -> String {
    format!("{} — atlas-launcher {}", PRODUCT, env!("CARGO_PKG_VERSION"))
}

/// The help text. Monochrome, English, and it names every command that exists.
pub const HELP: &str = "\
ATLAS — fully local news intelligence. One process starts the app.

  atlas start [--root DIR] [--dry-run] [--no-install]
        Load data/config.json, check that the model server would listen on 127.0.0.1
        only, then provision if this machine has never run ATLAS: when the model file
        or bin/llama-server is missing, atlas-setup --install runs as a child once —
        a multi-gigabyte download, the only time this app touches the network — and
        writes its progress to data/setup.json for the window to draw. Then the model
        file and the window binary are checked, llama-server is started as a child on
        127.0.0.1, waited for, then ATLAS itself opens: the native window
        (rust/target/release/atlas-desktop), which reads data/atlas.db directly.
        ATLAS opens no port of its own and starts no browser. Closing the window —
        or Ctrl-C — stops the model server this command started.
        --dry-run prints that sequence and exits without starting anything.
        --no-install skips the provisioning step, for a machine whose owner places
        the model file and bin/llama-server by hand.
        To serve a model of your own: atlas-setup --import-model /path/model.gguf
        names it in data/config.json; the window then shows the exact argv it will use.

  atlas doctor [--root DIR]
        Print, in plain English, what this machine has and what it is missing:
        toolchain, the window binary, the model file and its size, the loopback
        invariant, a writable SQLite, the model server's port, data permissions.
        Exit 0 when ATLAS can start. No network call is made.

  atlas help            this text
  atlas --version       the version

Options:
  --root DIR    use DIR as the project root instead of the one this binary is in
  --dry-run     print what `atlas start` would do, and start nothing
  --no-install  never run the provisioning step; a missing piece is then reported
                by name and path instead of being fetched

Exit codes:
  0  it worked (doctor: nothing is missing)
  1  ATLAS cannot do what was asked; the sentence above says why
  2  the command line was not understood

One download, once.  A fresh machine has neither the model file nor the model server,
and the model is several gigabytes, so it cannot travel with the app: the first
`atlas start` runs `atlas-setup --install` as a child whose output you can read, and
that child verifies the size and digest of what it fetched. A second start finds both
pieces in place and touches no network at all.
";

/// What the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Start the app.
    Start(StartOptions),
    /// Check the machine.
    Doctor(RootOption),
    /// Print the help.
    Help,
    /// Print the version.
    Version,
}

/// `atlas start` options.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StartOptions {
    pub root: RootOption,
    /// Print the plan and start nothing.
    ///
    /// There used to be an `open_browser` decision here (`--open` / `--no-open`, folded
    /// from `ATLAS_OPEN`) because the boot ended by pointing the user's browser at a
    /// loopback port. The product is the native window and no port is opened, so there is
    /// nothing left to open: the flag, the environment switch and `src/opener.rs` are
    /// gone rather than kept as a switch that switches nothing.
    pub dry_run: bool,
    /// Never run the provisioning step (`--no-install`).
    ///
    /// The default is the double-click: a person who starts the app with no config, no
    /// model and no `bin/llama-server` must get a working app, not a paragraph about how
    /// to download 6.5 GB by hand. The switch exists for the machine whose owner already
    /// provisions it, and for a start that must never touch the network — it turns the
    /// one download off, and the missing pieces are then reported by name and path.
    pub no_install: bool,
}

/// An optional explicit project root (`--root DIR`).
pub type RootOption = Option<PathBuf>;

/// A command line that cannot be honoured, with the sentence to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError {
    pub message: String,
}

impl UsageError {
    fn new(message: impl Into<String>) -> UsageError {
        UsageError {
            message: message.into(),
        }
    }
}

/// Parse the arguments after the program name.
///
/// `atlas`, `atlas help`, `atlas --help` and `atlas -h` are the help; `--version`
/// and `-V` are the version. `start` and `doctor` accept `--root DIR` (both
/// spellings, `--root=DIR` included) and `start` also accepts `--no-open`.
pub fn parse(args: &[String]) -> Result<Command, UsageError> {
    let mut rest = args.iter();
    let first = match rest.next() {
        Some(word) => word.as_str(),
        None => return Ok(Command::Help),
    };
    match first {
        "help" | "--help" | "-h" => Ok(Command::Help),
        "--version" | "-V" | "version" => Ok(Command::Version),
        "start" => {
            let mut options = StartOptions {
                dry_run: false,
                no_install: false,
                root: None,
            };
            let words: Vec<&String> = rest.collect();
            let mut index = 0;
            while index < words.len() {
                let flag = words[index].as_str();
                match flag {
                    "--dry-run" => options.dry_run = true,
                    // Said out loud, never inferred: this is the switch that keeps a
                    // start away from the network, and a typo must not become a download.
                    "--no-install" => options.no_install = true,
                    "--root" => {
                        index += 1;
                        let value = words.get(index).ok_or_else(|| {
                            UsageError::new("--root needs a directory after it")
                        })?;
                        options.root = Some(PathBuf::from(value));
                    }
                    other => match other.strip_prefix("--root=") {
                        Some(value) if !value.is_empty() => {
                            options.root = Some(PathBuf::from(value))
                        }
                        _ => {
                            return Err(UsageError::new(format!(
                                "`atlas start` does not know the option {} — see `atlas help`",
                                other
                            )));
                        }
                    },
                }
                index += 1;
            }
            Ok(Command::Start(options))
        }
        "doctor" => {
            let mut root: RootOption = None;
            let words: Vec<&String> = rest.collect();
            let mut index = 0;
            while index < words.len() {
                let flag = words[index].as_str();
                match flag {
                    "--root" => {
                        index += 1;
                        let value = words.get(index).ok_or_else(|| {
                            UsageError::new("--root needs a directory after it")
                        })?;
                        root = Some(PathBuf::from(value));
                    }
                    other => match other.strip_prefix("--root=") {
                        Some(value) if !value.is_empty() => root = Some(PathBuf::from(value)),
                        _ => {
                            return Err(UsageError::new(format!(
                                "`atlas doctor` does not know the option {} — see `atlas help`",
                                other
                            )))
                        }
                    },
                }
                index += 1;
            }
            Ok(Command::Doctor(root))
        }
        other => Err(UsageError::new(format!(
            "`{}` is not an ATLAS command — try `atlas start`, `atlas doctor` or `atlas help`",
            other
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn no_argument_is_the_help() {
        assert_eq!(parse(&[]).unwrap(), Command::Help);
    }

    #[test]
    fn help_has_three_spellings_and_version_has_two() {
        for word in ["help", "--help", "-h"] {
            assert_eq!(parse(&args(&[word])).unwrap(), Command::Help, "{}", word);
        }
        for word in ["--version", "-V", "version"] {
            assert_eq!(parse(&args(&[word])).unwrap(), Command::Version, "{}", word);
        }
    }

    #[test]
    fn start_defaults_to_a_real_boot_and_dry_run_is_the_only_switch() {
        // The default is "boot": no flag is needed to start the app, because the app is
        // the window and nothing is opened on the user's behalf. `--dry-run` is the one
        // switch, and it only prints.
        assert_eq!(
            parse(&args(&["start"])).unwrap(),
            Command::Start(StartOptions {
                root: None,
                dry_run: false,
                no_install: false,
            })
        );
        let expected = |root: Option<&str>, dry: bool| {
            Command::Start(StartOptions {
                root: root.map(PathBuf::from),
                dry_run: dry,
                no_install: false,
            })
        };
        assert_eq!(
            parse(&args(&["start", "--dry-run", "--root", "/tmp/x"])).unwrap(),
            expected(Some("/tmp/x"), true)
        );
        assert_eq!(
            parse(&args(&["start", "--root=/tmp/x", "--dry-run"])).unwrap(),
            expected(Some("/tmp/x"), true)
        );
        // The browser switch is gone with the browser. It is *refused*, not ignored: a
        // flag that silently does nothing is the lie this change removed.
        assert!(parse(&args(&["start", "--no-open"])).is_err());
        assert!(parse(&args(&["start", "--open"])).is_err());
    }

    #[test]
    fn no_install_is_its_own_switch_and_its_absence_means_the_first_run_provisions() {
        // The default is "provision": a double-clicked app on a machine with no model
        // must end with a working window, and the only way to that is the install child.
        // The switch is spelled, parsed and carried — never inferred from a tty, an
        // environment variable or a file, so "did this start use the network?" is
        // answered by the command line and nothing else.
        let options = match parse(&args(&["start", "--no-install"])).unwrap() {
            Command::Start(options) => options,
            other => panic!("expected a start, got {:?}", other),
        };
        assert!(options.no_install);
        assert!(!options.dry_run);
        // Both switches together, in either order, is a plan that says the download is
        // off — the two do not cancel each other, and neither drops the other.
        for words in [
            ["start", "--dry-run", "--no-install"],
            ["start", "--no-install", "--dry-run"],
        ] {
            match parse(&args(&words)).unwrap() {
                Command::Start(options) => {
                    assert!(options.dry_run, "{:?}", words);
                    assert!(options.no_install, "{:?}", words);
                }
                other => panic!("expected a start for {:?}, got {:?}", words, other),
            }
        }
        // A near-miss is refused like any other unknown word: `--no-instal` must not
        // start a download by being ignored.
        let error = parse(&args(&["start", "--no-instal"])).unwrap_err();
        assert!(error.message.contains("--no-instal"), "{}", error.message);
    }

    #[test]
    fn doctor_accepts_a_root_and_nothing_else() {
        assert_eq!(parse(&args(&["doctor"])).unwrap(), Command::Doctor(None));
        assert_eq!(
            parse(&args(&["doctor", "--root", "/tmp/r"])).unwrap(),
            Command::Doctor(Some(PathBuf::from("/tmp/r")))
        );
        assert!(parse(&args(&["doctor", "--fix"])).is_err());
    }

    #[test]
    fn a_missing_root_value_is_an_error_not_an_empty_root() {
        let error = parse(&args(&["doctor", "--root"])).unwrap_err();
        assert!(error.message.contains("--root needs a directory"), "{}", error.message);
        let error = parse(&args(&["start", "--root="])).unwrap_err();
        assert!(error.message.contains("does not know the option"), "{}", error.message);
    }

    #[test]
    fn an_unknown_command_is_refused_and_named() {
        let error = parse(&args(&["strat"])).unwrap_err();
        assert!(error.message.contains("strat"), "{}", error.message);
        assert!(error.message.contains("atlas start"), "{}", error.message);
        // A near-miss must not be treated as a prefix of a real command.
        assert!(parse(&args(&["starts"])).is_err());
        assert!(parse(&args(&["docto"])).is_err());
    }

    #[test]
    fn the_help_names_every_command_and_the_one_download() {
        for needle in ["atlas start", "atlas doctor", "atlas help", "atlas --version"] {
            assert!(HELP.contains(needle), "help does not name {}", needle);
        }
        // The help used to promise "Nothing is downloaded at run time". That sentence is
        // no longer true of the product: the first start fetches the model, on purpose,
        // because 6.5 GB cannot travel with a double-clicked app. What the help must say
        // instead is *when* — once, by name, and with a switch that turns it off — so a
        // reader is never surprised by traffic they did not ask for.
        assert!(
            !HELP.contains("Nothing is downloaded at run time"),
            "the help still claims nothing is fetched: {}",
            HELP
        );
        for needed in [
            "--no-install",
            "atlas-setup --install",
            "data/setup.json",
            "touches no network",
            // Serving a model the reader already has: the install child is the only thing
            // that can name one, and the help has to say so — otherwise the only documented
            // way to a local model is the download.
            "atlas-setup --import-model",
        ] {
            assert!(
                HELP.contains(needed),
                "the help does not say {:?}: {}",
                needed,
                HELP
            );
        }
    }

    #[test]
    fn the_help_promises_no_web_address_and_names_the_window() {
        // What the help used to say: "serve the collection API on the loopback port …
        // needs no browser", and a `--no-open` option for a window it would point at an
        // address. The product is the window, the rule is that no port is opened, and a
        // help that offers an address sends the user looking for a server.
        for stale in ["http://", "127.0.0.1:", "--no-open", "ATLAS_OPEN", "loopback port"] {
            assert!(
                !HELP.contains(stale),
                "the help still promises {:?}: {}",
                stale,
                HELP
            );
        }
        for wanted in [
            "atlas-desktop",
            "no port of its own and starts no browser",
            "--dry-run",
            "data/atlas.db",
        ] {
            assert!(HELP.contains(wanted), "the help does not say {:?}", wanted);
        }
    }

    #[test]
    fn the_version_line_carries_the_product_and_the_launcher_version() {
        // `atlas-web`'s `SERVER_VERSION` used to supply the first field; this crate no
        // longer links the listener, so the product string is declared here and the test
        // keeps the two halves of the line apart.
        let line = version_line();
        assert!(line.starts_with("ATLAS/1.0"), "{}", line);
        assert!(line.contains("atlas-launcher"), "{}", line);
        assert!(line.contains(env!("CARGO_PKG_VERSION")), "{}", line);
        assert_eq!(PRODUCT, "ATLAS/1.0");
    }
}
