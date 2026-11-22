//! `atlas-setup` - provision a fresh machine, then say whether it is ready.
//!
//!     atlas-setup --install          everything a fresh machine needs: the licence, then
//!                                    the model server and the model file
//!     atlas-setup check              what is missing, one line each, no network
//!     atlas-setup model   --yes      fetch the pinned model file, verify it, put it in place
//!     atlas-setup --import-model P  use the model the reader chose: copy a local .gguf, or
//!                                    download an https address, then name it in
//!                                    data/config.json and say how to load it
//!     atlas-setup llama   --yes      build bin/llama-server from tools/llama.cpp
//!     atlas-setup all     --yes      the two above, in that order, then check
//!
//! Nothing that fetches or builds runs without `--yes`: without it the command prints
//! exactly what it would do (URL, argv, destination) and exits 3. That is the whole
//! consent model, and it is why this crate can exist in a project whose app never
//! downloads anything.
//!
//! Exit codes: 0 ready / done, 1 not ready or failed, 2 usage, 3 nothing done (needs
//! `--yes`).

use std::process::ExitCode;

use atlas_setup::install::Consent;
use atlas_setup::{check, import, install, llama, model, Layout};

const USAGE: &str = "\
atlas-setup - provision this machine for ATLAS

  atlas-setup --install [--root DIR]            do everything: the licence question, the
                                                model server, the model file, then say how to start
        --accept-license                        accept the licence without asking (scripts)
  atlas-setup check [--root DIR] [--verify]     report every piece; --verify hashes the model
  atlas-setup model [--root DIR] [--yes] [--fetch] [--url URL] [--sha256 HEX]
  atlas-setup --import-model PATH-OR-URL [--root DIR]
                                                use a model you chose: copies a local .gguf
                                                (never moves it) or downloads an https address,
                                                refuses a file that is not GGUF, and names it in
                                                data/config.json
  atlas-setup llama [--root DIR] [--yes]
  atlas-setup all   [--root DIR] [--yes]      both steps, then check (a failed step does not stop the next)

Nothing is fetched or built without --yes.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(Failure::Usage) => {
            println!("{USAGE}");
            ExitCode::from(2)
        }
        Err(Failure::Message(text)) => {
            println!("atlas-setup: {text}");
            ExitCode::from(1)
        }
    }
}

enum Failure {
    Usage,
    Message(String),
}

impl From<String> for Failure {
    fn from(text: String) -> Failure {
        Failure::Message(text)
    }
}

#[derive(Default)]
struct Flags {
    root: Option<String>,
    yes: bool,
    verify: bool,
    fetch: bool,
    accept_license: bool,
    url: Option<String>,
    sha256: Option<String>,
    import_model: Option<String>,
}

fn run(args: &[String]) -> Result<ExitCode, Failure> {
    let Some(command) = args.first() else {
        return Err(Failure::Usage);
    };
    if command == "--help" || command == "-h" || command == "help" {
        return Err(Failure::Usage);
    }
    // `atlas-setup --install`, `atlas-setup install`, `atlas-setup setup --install`: one
    // command, three spellings. The install is what a person on a fresh machine types, so
    // every form they reach for has to work - the docs say `atlas-setup --install` (which
    // is the launcher's spelling of the same tool) and the scripts call this binary.
    let mut flags = Flags::default();
    let mut installing = matches!(command.as_str(), "--install" | "install" | "setup");
    // Which token the scan starts at. A flag-led invocation - `atlas-setup --import-model P`
    // or `atlas-setup --root DIR check` - names its first flag at position 0, so the scan
    // starts there; a subcommand-led one starts after the subcommand. Starting at 1 in both
    // cases was a real bug: `--root DIR --import-model file` read `DIR` as an unknown
    // argument, because `--root` had been taken for the command.
    let mut index = if command.starts_with("--") { 0 } else { 1 };
    // A subcommand written *after* the flags (`--root DIR check`) has to be found too: the
    // scan records the first token that is not a flag and not a flag's value, and the
    // command below is that token when there is one. Two tokens is a usage error, not a
    // guess.
    let mut positional: Option<String> = None;
    while index < args.len() {
        match args[index].as_str() {
            "--root" => {
                index += 1;
                flags.root = Some(args.get(index).cloned().ok_or(Failure::Usage)?);
            }
            "--url" => {
                index += 1;
                flags.url = Some(args.get(index).cloned().ok_or(Failure::Usage)?);
            }
            "--sha256" => {
                index += 1;
                flags.sha256 = Some(args.get(index).cloned().ok_or(Failure::Usage)?);
            }
            // `--import-model <path-or-url>`: the model a reader chose. It is a flag rather
            // than a subcommand because that is how it is written in the scripts and in
            // the README, where a person copies one line.
            "--import-model" => {
                index += 1;
                flags.import_model =
                    Some(args.get(index).cloned().ok_or(Failure::Usage)?);
            }
            "--install" => installing = true,
            "--accept-license" => {
                flags.accept_license = true;
                installing = true;
            }
            "--yes" => flags.yes = true,
            "--verify" => flags.verify = true,
            "--fetch" => flags.fetch = true,
            other if other.starts_with("--") => {
                return Err(Failure::Message(format!("unknown argument {other:?}\n\n{USAGE}")))
            }
            other => {
                if positional.is_some() {
                    return Err(Failure::Message(format!(
                        "unknown argument {other:?}\n\n{USAGE}"
                    )));
                }
                positional = Some(other.to_string());
            }
        }
        index += 1;
    }
    // The command is the subcommand as written, or the one a flag-first line carried.
    let command = positional.clone().unwrap_or(command.clone());
    let layout = match &flags.root {
        Some(root) => Layout::at(root)?,
        None => Layout::discover()?,
    };
    println!("ATLAS setup - {}", layout.root.display());

    // The import runs before anything else can: it is one step, it names its own file, and
    // it is what a reader with a model already on disk types.
    if let Some(text) = flags.import_model.clone() {
        return import_model(&layout, &text);
    }

    // The install asks nothing: `--yes` is what the command *is*, and a person who
    // double-clicked a file cannot answer a question. Everything it would print before
    // doing it is printed while it works, and `data/setup.json` carries the same news to
    // the window.
    if installing {
        let consent = if flags.accept_license { Consent::Accept } else { Consent::Ask };
        // One line from the person, read here so `install` stays a function a test can call
        // without a terminal: a closed stream is `None`, which is a refusal, not a hang.
        let mut answer = || {
            let mut line = String::new();
            match std::io::stdin().read_line(&mut line) {
                Ok(0) | Err(_) => None,
                Ok(_) => Some(line),
            }
        };
        let mut say = |line: &str| println!("{line}");
        return match install::run(&layout.root, consent, &mut answer, &mut say) {
            Ok(_) => Ok(ExitCode::SUCCESS),
            Err(error) => Err(Failure::Message(error)),
        };
    }

    // Every step reports `Progress`; only `check` answers with an exit code of its
    // own (0 ready, 1 not ready), which is the one command that is a question rather
    // than an action.
    let progress = |step: Result<Progress, Failure>| -> Result<ExitCode, Failure> {
        match step? {
            Progress::Done => Ok(ExitCode::SUCCESS),
            Progress::NeedsConsent => Ok(ExitCode::from(3)),
        }
    };
    match command.as_str() {
        "check" => check_now(&layout, flags.verify),
        "import-model" => match args.get(1) {
            Some(text) => import_model(&layout, text),
            None => Err(Failure::Message(import::usage_line())),
        },
        "model" => progress(fetch_model(&layout, &flags)),
        "llama" => progress(build_llama(&layout, &flags)),
        "all" => {
            // Each step either finishes or asks for consent; the first one that asks
            // stops the run, so a person always sees the next URL/argv before it runs.
            //
            // A step that *fails* does not stop the next one. On a fresh machine the
            // model server needs cmake and the weights do not, so a machine without
            // cmake would otherwise lose the 6.5 GB file it can actually get - and the
            // person would have to read the source to find out that `model` still
            // works. Both failures are printed, and the exit code says the run was not
            // a success.
            let mut failures: Vec<String> = Vec::new();
            for step in [build_llama, fetch_model] {
                match step(&layout, &flags) {
                    Ok(Progress::Done) => {}
                    Ok(Progress::NeedsConsent) => return Ok(ExitCode::from(3)),
                    Err(Failure::Message(text)) => failures.push(text),
                    Err(other) => return Err(other),
                }
            }
            if !failures.is_empty() {
                println!();
                for text in &failures {
                    println!("atlas-setup: {text}");
                }
                return Err(Failure::Message(format!(
                    "{} step(s) failed - each line above says what that step needed. \
                     `atlas-setup check` says which pieces are still missing; `atlas-setup \
                     model --yes` and `atlas-setup llama --yes` run one step on its own.",
                    failures.len()
                )));
            }
            check_now(&layout, true)
        }
        other => Err(Failure::Message(format!("unknown command {other:?}\n\n{USAGE}"))),
    }
}

/// Import the model the reader chose, and say what to do with it.
///
/// Two lines matter more than the rest: the file that is now served, and
/// [`import::next_line`]. The import itself is `atlas-setup`'s, the settings write is
/// `atlas-config`'s, and nothing here downloads anything unless the reader named an
/// address.
fn import_model(layout: &Layout, text: &str) -> Result<ExitCode, Failure> {
    let mut say = |line: &str| println!("{line}");
    // A byte counter for a long download: the same progress file the install writes, so
    // the window shows the same bar.
    let imported = import::import(&layout.root, text, &mut say, &mut |_, _| {})?;
    println!();
    println!(
        "{} is the model ATLAS serves ({} bytes, {}).",
        imported.path.display(),
        imported.bytes,
        if imported.copied { "copied from the file you named" } else { "downloaded" }
    );
    println!("{}", import::next_line());
    Ok(ExitCode::SUCCESS)
}

/// The pre-flight: every piece, then one sentence and an exit code.
fn check_now(layout: &Layout, verify: bool) -> Result<ExitCode, Failure> {
    let lines = check::run(layout, verify);
    println!();
    for line in &lines {
        println!("{}", line.text());
    }
    println!();
    if check::all_ok(&lines) {
        println!("ready: the window can run on this machine.");
        Ok(ExitCode::SUCCESS)
    } else {
        let missing: Vec<&str> =
            lines.iter().filter(|line| !line.ok).map(|line| line.piece).collect();
        println!("not ready: {} piece(s) missing - {}", missing.len(), missing.join(", "));
        // Not every row can be closed by ATLAS: the archive is collected, not built, and
        // two pieces are files that ship with the repository. The sentence says what is
        // true of all of them - the line above each one says what to do.
        println!("each line above says what to do about the piece it names.");
        Ok(ExitCode::from(1))
    }
}

/// What a provisioning step did: finished, or stopped for consent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Progress {
    Done,
    NeedsConsent,
}

/// The model file: consent, then one transfer with the URL printed.
fn fetch_model(layout: &Layout, flags: &Flags) -> Result<Progress, Failure> {
    let mut plan = model::load_manifest(&layout.root)?;
    if let Some(url) = &flags.url {
        plan.url = url.clone();
    }
    if let Some(sha) = &flags.sha256 {
        plan.sha256 = sha.to_ascii_lowercase();
    }
    let (target, _) = model::paths(&plan, &layout.root);
    println!();
    println!("model    {} ({} bytes)", plan.name, plan.bytes);
    println!("repo     {}", plan.repo);
    println!("url      {}", plan.url);
    println!("sha256   {}", plan.sha256);
    println!("into     {}", target.display());
    println!("licence  {}", plan.licence);

    let present = target.is_file() && std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0) == plan.bytes;
    if present && !flags.fetch {
        println!();
        println!("the file is already there at the pinned size; nothing to fetch.");
        if flags.verify {
            let (bytes, digest) = model::verify_file(&target)?;
            if digest != plan.sha256 {
                return Err(Failure::Message(format!(
                    "{} is {bytes} bytes but does not match the pin (sha256 {digest}) - \
                     re-run with --fetch to replace it",
                    target.display()
                )));
            }
            println!("sha256 verified: {digest}");
        }
        return Ok(Progress::Done);
    }

    if !flags.yes {
        println!();
        println!("nothing was fetched. This is the one download in ATLAS: 6.5 GB from the");
        println!("host above, over https, verified against the sha256 above before it is used.");
        println!("re-run with --yes to fetch it.");
        return Ok(Progress::NeedsConsent);
    }

    println!();
    let mut say = |line: &str| println!("{line}");
    let fetched = model::ensure(&plan, &layout.root, flags.verify, flags.fetch, &mut say)?;
    println!();
    println!(
        "model    {} - {} bytes, sha256 {}{}",
        fetched.path.display(),
        fetched.bytes,
        fetched.sha256,
        if fetched.downloaded { " (downloaded)" } else { " (already present)" }
    );
    Ok(Progress::Done)
}

/// The model server, from the vendored tree.
fn build_llama(layout: &Layout, flags: &Flags) -> Result<Progress, Failure> {
    if llama::server_present(&layout.root) {
        println!();
        println!("model server: {} is already built; nothing to do.", layout.model_server().display());
        return Ok(Progress::Done);
    }
    if !llama::source_present(&layout.root) {
        return Err(Failure::Message(format!(
            "{} is missing - the model server is built from the vendored llama.cpp tree, and \
             nothing here fetches it (it is part of this repository)",
            layout.llama_source().display()
        )));
    }
    let cmake = llama::cmake_available()?;
    println!();
    println!("model server: {}", layout.model_server().display());
    println!("source       {}", layout.llama_source().display());
    println!("toolchain    {cmake}");
    println!("build        cmake -S ... -B .../build -DCMAKE_BUILD_TYPE=Release, then");
    println!("             cmake --build .../build --target llama-server -j <cores>");
    if !flags.yes {
        println!();
        println!("nothing was built. re-run with --yes to build it (this takes a few minutes).");
        return Ok(Progress::NeedsConsent);
    }
    println!();
    let mut say = |line: &str| println!("{line}");
    let built = llama::build(&layout.root, &mut say)?;
    println!();
    println!("model server {}", built.server.display());
    for path in &built.copied {
        println!("copied       {}", path.display());
    }
    Ok(Progress::Done)
}
