//! `atlas-setup --install`: everything a fresh machine needs, in one command.
//!
//! # The order, and why it is that order
//!
//! 0. **the licence** - read before anything is fetched, because the app cannot ask later:
//!    the window never downloads anything at run time, so the person who accepts has to be
//!    the person whose command fetches. [`crate::license`] asks the same question the
//!    window's own screen asks (same document version), records the answer in
//!    `data/license.json`, and a "no" or a silent stream stops the run here - before the
//!    first byte, before the first file.
//!
//! The three pieces it then puts in place are the three a person would otherwise do by
//! hand:
//!
//! 1. **the Rust toolchain** - reported, never installed. It is needed to *build* ATLAS
//!    from source, not to run it, and a program that installs a compiler is a program
//!    nobody can audit. A machine without it still finishes this install.
//! 2. **the model server** - `bin/llama-server`, built from the copy of llama.cpp that
//!    ships in `tools/llama.cpp`. Nothing is downloaded for it: the tree is source, and
//!    cmake is the machine's own.
//! 3. **the model file** - the one download in ATLAS, from the URL the pin names into
//!    `models/`, resumable, and checked against the size the server reports (and against
//!    a fingerprint when the pin carries one).
//!
//! Every step **skips** when it is already satisfied, which is what makes a second run
//! instant, and a step that cannot run **does not stop the next one**: a machine without
//! cmake still gets its weights. What it never does is pretend: each failure is printed
//! with the exact command that closes it, and the last line says whether the app can be
//! started.
//!
//! ## A human starts it, and nothing listens
//!
//! This is the install step, started by a person double-clicking `ATLAS.command`
//! (macOS), `ATLAS.bat` (Windows) or `ATLAS.sh` (Linux), or by typing the command
//! themselves. The app itself never downloads anything at run time, and no process here
//! opens a port - the transfer is one outbound connection, not a listener.
//!
//! ## Progress
//!
//! Every phase is written to `data/setup.json` ([`crate::progress`]), so the window can
//! draw a bar while the 6.5 GB arrive instead of showing a frozen screen.

use std::path::Path;

use crate::progress::SetupFile;
use crate::{check, license, llama, model, Layout};

/// How the licence question may be answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Consent {
    /// Ask the person: the summary, then one line from them. A stream that has ended is a
    /// refusal, so a script that forgets this cannot download 6.5 GB by accident.
    Ask,
    /// Already answered by the flag `--accept-license`, for a run nobody is watching.
    Accept,
}

/// The sentence a finished install ends with: what to open, and the command for people
/// who prefer a terminal.
///
/// One sentence per platform, because "double-click" is a different file on each of the
/// three, and a first run that names the wrong one is a first run that ends in a
/// command-not-found.
pub fn ready_line(windows: bool, macos: bool) -> String {
    // The last line of the install is the only instruction a person reads after the wait,
    // so it carries three things: the file to open, the command for people who prefer a
    // terminal, and what the app will show them first. Without the third, the licence
    // screen looks like a second question they already answered.
    let open = if windows {
        "double-click ATLAS.bat (or run: rust\\target\\release\\atlas.exe start)"
    } else if macos {
        "double-click ATLAS.command (or run: rust/target/release/atlas start)"
    } else {
        "double-click ATLAS.sh (or run: rust/target/release/atlas start)"
    };
    format!(
        "ready - open the app: {open}. The app shows the licence in full before the \
         workspace opens: tick its line and you are in. Close its window to stop ATLAS"
    )
}

/// What a person is told about using a model of their own.
///
/// Two lines and both facts: the command that imports one, and the rule that is the whole
/// reason this crate exists - the app downloads nothing while it runs. The second line is
/// not decoration: a reader who has just watched 6.5 GB arrive is exactly the person who
/// will assume the window keeps doing it.
pub fn import_hint() -> Vec<String> {
    vec![
        "model: to serve a different model, run atlas-setup --import-model with the path of a \
         .gguf file (or an https address) - it copies it into models/ and names it in \
         data/config.json"
            .to_string(),
        "nothing is downloaded while the app runs: the window reads the files this install put \
         here, and an import is a step you start"
            .to_string(),
    ]
}

/// What a step that cannot be done says. It always carries the command that closes it,
/// because "it failed" is not a step a person can take.
pub fn failure_line(step: &str, why: &str, next: &str) -> String {
    format!("{step} could not be finished: {why} - {next}")
}

/// Does `cargo` exist on `PATH`? The toolchain report, without spawning anything yet.
///
/// The search is `atlas-platform`'s - `;` on Windows and `:` elsewhere, and `PATHEXT`
/// where the name has no extension - not a `cfg!(windows)` suffix written here. A local
/// `join("cargo.exe")` is a second answer to a question one module in this workspace
/// already answers, and it is the answer that goes stale: `PATH` may name a shim, and
/// the platform layer is what knows that.
fn cargo_present() -> bool {
    atlas_platform::executable::on_path("cargo").is_some()
}

/// Run the install. Returns the sentence to print (and to write as the final progress
/// line) or the failure that stopped it.
///
/// `answer` is called at most once, and only when the licence has not been accepted yet: it
/// returns the line the person typed, or `None` when there is nobody to ask.
pub fn run(
    root: &Path,
    consent: Consent,
    answer: &mut dyn FnMut() -> Option<String>,
    say: &mut dyn FnMut(&str),
) -> Result<String, String> {
    let layout = Layout::at(root)?;

    // --- 0. the licence -------------------------------------------------------------
    //
    // Before the progress file, before the toolchain report, before any network: this is
    // the step that decides whether the rest of the run happens at all.
    if !matches!(license::state(root), license::State::Accepted(_)) {
        let plan = model::pin(root)?;
        let (target, _) = model::paths(&plan, root);
        for line in license::summary() {
            say(&line);
        }
        say("");
        say(&format!(
            "Accepting means ATLAS may now download one file - {} - from {} into {}, and \
             build the model server from the copy of llama.cpp that ships beside this file. \
             Nothing else is fetched, and nothing here opens a port.",
            download_size(plan.bytes),
            plan.host(),
            target.display()
        ));
        say(&license::question());
    }
    match consent {
        // The scripted answer records the same document a person would have accepted; it
        // is not a way to skip the record, only a way to skip the typing.
        Consent::Accept => {
            if matches!(license::state(root), license::State::Missing) {
                license::accept(root, "atlas-setup --accept-license")?;
                say(&format!(
                    "licence: accepted ({}) because --accept-license was given; recorded in {}",
                    license::VERSION,
                    license::RELATIVE_PATH
                ));
            }
        }
        Consent::Ask => {
            // Already answered? Then there is nothing to ask, and the stream is not read.
            if matches!(license::state(root), license::State::Accepted(_)) {
                license::decide(root, "atlas-setup --install", None, say)?;
            } else {
                let typed = answer();
                license::decide(root, "atlas-setup --install", typed.as_deref(), say)?;
            }
        }
    }

    let mut progress = SetupFile::at(root);
    note(&mut progress, "start", 0, 0, "looking at what is already here");

    let mut failures: Vec<String> = Vec::new();

    // --- 1. the toolchain, reported -------------------------------------------------
    if cargo_present() {
        let mut version = String::from("cargo");
        if let Ok(output) = atlas_platform::child::command("cargo").arg("--version").output() {
            if output.status.success() {
                version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            }
        }
        say(&format!("rust: {version}"));
    } else {
        say(
            "rust: not installed - it is not needed to run ATLAS, only to build it from \
             source (https://rustup.rs installs it by hand)",
        );
    }

    // --- 2. the model server --------------------------------------------------------
    note(&mut progress, "model server", 0, 0, "checking the model server");
    if llama::server_present(root) {
        say(&format!(
            "model server: {} is already there",
            layout.model_server().display()
        ));
    } else if !llama::source_present(root) {
        let why = format!(
            "{} is missing, and the tree is part of this repository",
            layout.llama_source().display()
        );
        say(&failure_line(
            "the model server",
            &why,
            "restore tools/llama.cpp from the repository and run atlas-setup --install again",
        ));
        failures.push(why);
    } else if llama::cmake_available().is_err() {
        // Not fatal for the weights: a machine without cmake can still download the
        // model, and chat is the only view that needs the server.
        let why = llama::cmake_available().err().unwrap_or_default();
        say(&failure_line(
            "the model server",
            &why,
            "install cmake, then run atlas-setup llama --yes (the model download below \
             does not need it)",
        ));
        failures.push(why);
    } else {
        note(&mut progress, "model server", 0, 0, "building the model server");
        say("model server: building it from tools/llama.cpp - this takes a few minutes");
        match llama::build(root, say) {
            Ok(built) => say(&format!("model server: {} is in place", built.server.display())),
            Err(error) => {
                say(&failure_line(
                    "the model server",
                    &error,
                    "run atlas-setup llama --yes to try just this step again",
                ));
                failures.push(error);
            }
        }
    }

    // --- 3. the model file ----------------------------------------------------------
    let plan = model::pin(root)?;
    let (target, _) = model::paths(&plan, root);
    // The number comes from the library, the sentence is printed here: a library module
    // does not print, and the installer is the one thing that does.
    let present = model::weights_size(root).is_some();
    note(&mut progress, "model", 0, 0, "looking for the model file");
    // No free-space probe: asking the machine would mean starting a program (`df`, and on
    // Windows PowerShell running a formatted command line), and an installer that starts
    // programs is exactly what this workspace's audit rules forbid. The honest place to
    // learn that the disk is full is the write itself - `model.rs` turns ENOSPC into one
    // sentence that says how much arrived, keeps the part, and asks for room and a re-run.
    if present {
        say(&format!(
            "model: {} is already there, so there is nothing to download",
            target.display()
        ));
    } else {
        say(&format!(
            "model: about {} to download from {} into the models folder beside this file",
            display_size(plan.bytes),
            plan.host()
        ));
        say("model: this is the one download in ATLAS. It can be stopped and started again:");
        say("model: it carries on from where it stopped instead of starting over");
        say(&format!(
            "model: it needs {} free on this disk, and nothing on this machine is listening \
             while it arrives",
            display_size(model::needs(plan.bytes))
        ));
    }

    let mut on_bytes = |done: u64, total: u64| {
        let message = if done >= total { "model file is complete" } else { "downloading the model" };
        note(&mut progress, "model", done, total, message);
    };
    match model::ensure_with(&plan, root, false, false, &mut *say, &mut on_bytes) {
        Ok(fetched) => {
            if fetched.downloaded {
                say(&format!(
                    "model: in place at {} ({} bytes)",
                    fetched.path.display(),
                    fetched.bytes
                ));
            }
            note(&mut progress, "model", fetched.bytes, fetched.bytes, "model file is complete");
        }
        Err(error) => {
            say(&failure_line(
                "the model download",
                &error,
                "run atlas-setup --install again: the part already downloaded is kept and \
                 carried on from",
            ));
            failures.push(error);
        }
    }

    // --- 4. the verdict -------------------------------------------------------------
    if failures.is_empty() {
        for line in import_hint() {
            say(&line);
        }
        let ready = ready_line(cfg!(windows), cfg!(target_os = "macos"));
        say(&ready);
        let _ = progress.finish(true, &ready);
        return Ok(ready);
    }
    let message = format!(
        "not finished: {} step(s) could not be done - each line above says what it needs. \
         Run atlas-setup --install again after fixing them",
        failures.len()
    );
    say(&message);
    let _ = progress.finish(false, &message);
    Err(message)
}

/// The size of the download, in the words the announcement uses: "about 6.5 GB".
///
/// "About" because the pin's byte count is exact and a person wants a number they can hold
/// against their disk, not a count of bytes; the exact size is printed by `--install` when
/// the transfer starts.
fn download_size(bytes: u64) -> String {
    if bytes == 0 {
        return "a file of the size the server reports".to_string();
    }
    format!("about {}", display_size(bytes))
}

/// Bytes as a person says them: `8.0 GB`, `512 MB`.
fn display_size(bytes: u64) -> String {
    if bytes == 0 {
        return "an unknown number of".to_string();
    }
    let gb = bytes as f64 / 1_000_000_000.0;
    if gb >= 1.0 {
        return format!("{gb:.1} GB");
    }
    let mb = bytes as f64 / 1_000_000.0;
    format!("{mb:.0} MB")
}

/// A progress write that is not allowed to stop the install.
fn note(progress: &mut SetupFile, phase: &str, done: u64, total: u64, message: &str) {
    let _ = progress.tick(phase, done, total, message);
}

/// The toolchain line, for the tests: what a machine without `cargo` is told.
pub fn toolchain_line(present: bool, version: &str) -> String {
    if present {
        format!("rust: {version}")
    } else {
        "rust: not installed - it is not needed to run ATLAS, only to build it from source \
         (https://rustup.rs installs it by hand)"
            .to_string()
    }
}

/// Is the model file already usable? The fast path, as a function so a test can pin it.
pub fn already_present(size: Option<u64>) -> bool {
    matches!(size, Some(bytes) if bytes > 0)
}

/// The check helper the install reuses, so "what is missing" has one answer.
pub fn missing_lines(root: &Path) -> Vec<crate::Line> {
    check::run(&Layout { root: root.to_path_buf() }, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "atlas-install-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch root");
        dir
    }

    /// `cargo` is looked for through `atlas-platform`, which knows `;` from `:` and
    /// `PATHEXT` from a bare name. These tests are run by a cargo on this machine, so
    /// the answer on this machine is yes - and the function has to agree with the
    /// platform layer rather than with a `cfg!(windows)` suffix of its own.
    #[test]
    fn cargo_is_found_the_way_the_platform_layer_finds_it() {
        assert_eq!(
            cargo_present(),
            atlas_platform::executable::on_path("cargo").is_some()
        );
        assert!(cargo_present(), "these tests are run by a cargo on this machine");
    }

    /// The last line is the one a person acts on: it names the file to double-click on
    /// their platform *and* the command a terminal user would type.
    #[test]
    /// The import hint names the command, names the file it writes, and says the run-time
    /// rule in the same breath as the download it just did.
    #[test]
    fn the_import_hint_names_the_command_and_the_run_time_rule() {
        let lines = import_hint();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("atlas-setup --import-model"), "{}", lines[0]);
        assert!(lines[0].contains(".gguf"), "{}", lines[0]);
        assert!(lines[0].contains("data/config.json"), "{}", lines[0]);
        assert!(
            lines[1].contains("nothing is downloaded while the app runs"),
            "{}",
            lines[1]
        );
        for line in &lines {
            assert!(line.is_ascii(), "not ASCII: {line}");
        }
    }

    fn the_ready_line_names_the_platform_file_and_the_command() {
        let mac = ready_line(false, true);
        assert!(mac.contains("ATLAS.command"), "{mac}");
        assert!(mac.contains("rust/target/release/atlas start"), "{mac}");
        assert!(mac.starts_with("ready"), "{mac}");
        // And what happens next, so the app's own licence screen is not a surprise.
        assert!(mac.contains("shows the licence in full"), "{mac}");
        assert!(mac.contains("tick its line"), "{mac}");
        assert!(mac.is_ascii(), "ASCII, so no console renders it wrong: {mac}");

        let linux = ready_line(false, false);
        assert!(linux.contains("ATLAS.sh"), "{linux}");
        assert!(!linux.contains("ATLAS.command"), "{linux}");
        assert!(linux.contains("rust/target/release/atlas start"), "{linux}");

        let windows = ready_line(true, false);
        assert!(windows.contains("ATLAS.bat"), "{windows}");
        assert!(windows.contains("rust\\target\\release\\atlas.exe start"), "{windows}");
    }

    /// A failure ends with a command, not with an apology: the person reading it has to
    /// know what to type next.
    #[test]
    fn a_failure_names_the_next_command() {
        let line = failure_line(
            "the model download",
            "the transfer stopped after 10 bytes",
            "run atlas-setup --install again",
        );
        assert!(line.contains("the model download"), "{line}");
        assert!(line.contains("the transfer stopped"), "{line}");
        assert!(line.contains("atlas-setup --install"), "{line}");
    }

    /// The toolchain is reported, never installed, and a machine without it is told that
    /// plainly instead of being blocked.
    #[test]
    fn a_machine_without_rust_is_told_it_can_still_run() {
        let without = toolchain_line(false, "");
        assert!(without.contains("not needed to run ATLAS"), "{without}");
        assert!(without.contains("rustup.rs"), "the one address, by hand: {without}");
        assert!(!without.contains("installing"), "ATLAS installs nothing here: {without}");
        assert_eq!(toolchain_line(true, "cargo 1.90.0"), "rust: cargo 1.90.0");
    }

    /// The fast path is a rule, not a coincidence: any non-empty file at the pinned name
    /// is the model, and nothing (not even an empty file) pretends to be.
    #[test]
    fn an_existing_file_is_the_fast_path_and_an_empty_one_is_not() {
        assert!(already_present(Some(6_526_302_432)));
        assert!(!already_present(Some(0)));
        assert!(!already_present(None));
    }

    /// Sizes read the way a person says them, including "we do not know yet".
    #[test]
    fn sizes_are_spoken_in_gigabytes() {
        assert_eq!(display_size(8_000_000_000), "8.0 GB");
        assert_eq!(display_size(512_000_000), "512 MB");
        assert_eq!(display_size(0), "an unknown number of");
    }

    /// `--accept-license` is a scripted *answer*, not a way past the record: a run with it
    /// must leave `data/license.json` behind exactly as an answered question would. This
    /// test walks the whole of `run` on a root whose model is already in place, so no byte
    /// is fetched and no socket is opened.
    #[test]
    fn the_scripted_flag_records_the_licence_and_skips_the_question() {
        let root = scratch("scripted-run");
        std::fs::create_dir_all(root.join("rust")).expect("rust");
        std::fs::create_dir_all(root.join("models")).expect("models");
        std::fs::write(root.join("rust").join("Cargo.toml"), b"[workspace]\n").expect("a manifest");
        // A pinned file that is already here: the fast path, so the model step never
        // reaches the network.
        let body = b"tiny model";
        std::fs::write(
            root.join("models").join("model.json"),
            format!(
                r#"{{"name":"m.gguf","repo":"a/b","url":"https://example.invalid/m.gguf","bytes":{},"sha256":"{}","license":"x"}}"#,
                body.len(),
                crate::sha::sha256(body)
            ),
        )
        .expect("a manifest");
        std::fs::write(root.join("models").join("m.gguf"), body).expect("the weights");

        let mut said: Vec<String> = Vec::new();
        // The model server is not in this scratch root, so the run ends with a failure —
        // what this test is about is the record the run leaves behind.
        let _ = run(&root, Consent::Accept, &mut || panic!("the question must not be asked"), &mut |line| {
            said.push(line.to_string())
        });

        let record = crate::license::state(&root);
        match record {
            crate::license::State::Accepted(record) => {
                assert_eq!(record.by, "atlas-setup --accept-license");
                assert_eq!(record.version, crate::license::VERSION);
            }
            crate::license::State::Missing => panic!("--accept-license recorded nothing"),
        }
        assert!(
            said.iter().any(|line| line.contains("recorded in data/license.json")),
            "{said:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The first-run contract, as a test: with no answer to the licence question, the run
    /// stops **before** the progress file, before the toolchain report, before any byte -
    /// and it says what it would have downloaded, from where, and how big.
    #[test]
    fn without_an_answer_nothing_is_written_and_nothing_is_downloaded() {
        let root = scratch("consent");
        std::fs::create_dir_all(root.join("rust")).expect("rust");
        std::fs::write(root.join("rust").join("Cargo.toml"), b"[workspace]\n").expect("a manifest");

        let mut said: Vec<String> = Vec::new();
        let error = run(&root, Consent::Ask, &mut || None, &mut |line| said.push(line.to_string()))
            .expect_err("a refusal without an answer");

        assert!(error.contains("not been accepted"), "{error}");
        assert!(error.contains("--accept-license"), "the scripted answer is named: {error}");
        assert!(!root.join("data").join("setup.json").exists(), "nothing started, so no progress file");
        assert!(!root.join("data").join("license.json").exists(), "and no acceptance was recorded");
        assert!(
            !root.join("models").join("Qwen3.5-9B-Defiant-Fable-NEO-MAX-MTP-IQ4_XS.gguf").exists(),
            "nothing was downloaded"
        );

        // What the person was told before the question: the terms, the amount, the place.
        let all = said.join("\n");
        assert!(all.contains("Apache License 2.0"), "{all}");
        assert!(all.contains("keep their own terms"), "{all}");
        assert!(all.contains("no warranty"), "{all}");
        assert!(all.contains("about 6.5 GB"), "how much: {all}");
        assert!(all.contains("huggingface.co"), "from where: {all}");
        assert!(all.contains("Qwen3.5-9B-Defiant-Fable-NEO-MAX-MTP-IQ4_XS.gguf"), "into which file: {all}");
        assert!(all.contains("Type yes"), "and the question itself: {all}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `--accept-license` (the scripted answer) records the same document the question
    /// would have recorded, so a machine provisioned by a script is in the same state as
    /// one provisioned by a person.
    #[test]
    fn the_scripted_answer_records_the_same_document() {
        let root = scratch("scripted");
        std::fs::create_dir_all(root.join("rust")).expect("rust");
        std::fs::write(root.join("rust").join("Cargo.toml"), b"[workspace]\n").expect("a manifest");

        // The gate itself is what this test is about, not what comes after it.
        let record = crate::license::decide(
            &root,
            "atlas-setup --accept-license",
            Some("yes"),
            &mut |_| {},
        )
        .expect("an acceptance");
        assert_eq!(record.version, crate::license::VERSION);
        assert_eq!(record.by, "atlas-setup --accept-license");
        assert!(matches!(crate::license::state(&root), crate::license::State::Accepted(_)));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The install refuses a root that is not one, with the sentence the rest of the
    /// crate uses, and writes nothing outside it.
    #[test]
    fn a_root_that_is_not_atlas_is_refused_before_anything_is_written() {
        let dir = std::env::temp_dir().join(format!("atlas-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch dir");
        let error = run(&dir, Consent::Accept, &mut || None, &mut |_| {}).expect_err("a refusal");
        assert!(error.contains("not an ATLAS root"), "{error}");
        assert!(!dir.join("data").join("setup.json").exists(), "nothing was written");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
