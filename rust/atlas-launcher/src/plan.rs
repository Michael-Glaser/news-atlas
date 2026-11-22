//! What `atlas start --dry-run` prints: the boot as lines, with nothing started.
//!
//! It lives beside [`crate::start`] rather than inside it because the two questions are
//! different: the boot performs a sequence, the plan describes it. Both read the same
//! resolvers ([`crate::window::binary`], [`crate::agent::binary`],
//! [`crate::llama::binary`], [`crate::model::resolve`]), so a plan cannot drift from the
//! boot without taking a resolver with it.
//!
//! The three children are one line each, in the order the boot reaches them: the model
//! file it resolves first (so a missing model is the first thing a reader sees), the
//! window and the agent binaries it resolves before anything is spawned, and last the
//! model server — the child that is started first (start.rs:180) and stopped last
//! (shutdown::ORDER). "What will this do?" is answered for all three before a single
//! process exists.
//!
//! `--dry-run` exists because every other way to ask "what will this do?" is a real
//! start: it loads weights and opens a window. The command takes no lock, hardens
//! nothing and writes no default into `data/config.json` — it reads settings
//! (`Config::load_at`, never `load_managed_at`) and looks at files, which is all the
//! answer needs.
//!
//! The two claims a reader came for are in the lines by name — "no listener" and the stop
//! order — so a regression that quietly reintroduces a server has to delete a line a test
//! asserts on (`tests/no_listener.rs::the_dry_run_prints_a_plan_with_no_listener_and_no_browser`).

use atlas_config::{Config, Paths};

use crate::report::Printer;
use crate::{agent, llama, model, setup, start, window};

/// The boot as the lines a person reads, with nothing spawned and nothing touched.
///
/// `--dry-run` prints this and exits. It is a pure function of the root: it looks at
/// files (`std::fs::metadata` through the resolvers) and reads settings, and it does not
/// spawn the model server, does not spawn the install child, does not open a window, does
/// not take the lock and does not harden anything. The two claims a reader came for are
/// in it by name — "no listener" and the stop order — so a regression that quietly
/// reintroduces a server has to delete a line this test asserts on.
///
/// The lines are in the order the boot does them: provisioning first — it is the step that
/// decides whether this start touches the network at all, so it is the first thing a reader
/// has to be able to see — then the model file, then the window and the agent binaries the
/// boot resolves before anything is spawned (`start.rs:171`, `:176`), then the model server
/// it starts (`start.rs:180`). The three child lines used to print window, agent, model
/// server: that is the *stop* order, and under this sentence it was a plan that disagreed
/// with its own boot. `install` is `--no-install` inverted — a plan for a machine whose
/// owner provisions by hand must not promise a download.
pub fn plan_lines(paths: &Paths, config: Option<&Config>, install: bool) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("root {}", paths.root.display()));
    // One decision, read twice: the provisioning line and the two lines below it must
    // agree. A plan that prints "the install fetches the model" and then "nothing is
    // downloaded at run time — place the file there yourself" would be two plans.
    let provision = config.map(|config| {
        setup::decide(setup::Present::scan(paths, config), install)
    });
    lines.push(provision_line(paths, config, provision.as_ref()));
    // "The install will fetch it" is a claim about a child that will run, so it is only
    // made when the tool exists; without it the boot refuses above and these two lines
    // say that instead of promising a download that cannot happen.
    let installs = provision
        .as_ref()
        .is_some_and(|provision| provision.spawns());
    let fetching = installs && setup::binary(paths).is_some();
    let blocked = installs && !fetching;
    match config {
        Some(config) => match model::resolve(paths, config) {
            Ok(model) => lines.push(format!("model file {}", model.text())),
            // The manual sentence says "nothing is downloaded at run time and here is
            // where to put the file yourself", which is the truth for a machine that
            // provisions by hand and a contradiction next to a provisioning line that
            // promises the install. When the install is the plan, the plan says so.
            Err(missing) if fetching => lines.push(format!(
                "model file {} does not exist yet: the provisioning step above fetches it",
                missing.expected.display()
            )),
            Err(missing) if blocked => lines.push(format!(
                "model file {} does not exist yet, and the provisioning step above cannot run \
                 on this machine, so `atlas start` stops there",
                missing.expected.display()
            )),
            Err(missing) => lines.push(format!("model file {}", missing.sentence())),
        },
        None => lines.push(format!(
            "model file unknown: {} will not load",
            paths.config_file.display()
        )),
    }
    match window::binary(paths) {
        Some(binary) => lines.push(format!("window {} (found)", binary.display())),
        None => lines.push(format!("window {}", window::missing_line(paths))),
    }
    match agent::binary(paths) {
        Some(binary) => lines.push(format!(
            "agent {} {} (found; one JSON line per action on stdout)",
            binary.display(),
            agent::SERVE
        )),
        None => lines.push(format!("agent {}", agent::missing_line(paths))),
    }
    match llama::binary(paths) {
        Some(binary) => {
            let port = config.map(|config| config.llama_port);
            match port {
                Some(port) => lines.push(format!(
                    "model server {} on {}:{} (its own argv, and the only socket this start \
                     causes)",
                    binary.display(),
                    atlas_config::BIND_HOST,
                    port
                )),
                None => lines.push(format!("model server {}", binary.display())),
            }
        }
        None if fetching => lines.push(format!(
            "model server {} does not exist yet: the provisioning step above builds it from \
             the vendored llama.cpp tree",
            paths.llama_local.display()
        )),
        None if blocked => lines.push(format!(
            "model server {} does not exist yet, and the provisioning step above cannot run \
             on this machine, so `atlas start` stops there",
            paths.llama_local.display()
        )),
        None => lines.push(format!("model server {}", start::missing_binary_line(paths))),
    }
    lines.push(
        "no listener: no HTTP server, no browser, no port opened by ATLAS — the window reads \
         data/atlas.db itself"
            .to_string(),
    );
    lines.push("stop order: window, then agent, then model server".to_string());
    lines
}

/// The provisioning line: the one step that can touch the network, stated as a fact.
///
/// Three answers, one per [`setup::Provision`], and each says what the *boot* would do —
/// not what the files are. A plan that printed "run the install" for a machine the owner
/// said to provision by hand (`--no-install`) would be the kind of plan this crate
/// refuses to print. The decision is passed in rather than made here, so the two lines
/// below can read the same answer.
fn provision_line(
    paths: &Paths,
    config: Option<&Config>,
    provision: Option<&setup::Provision>,
) -> String {
    let (Some(_config), Some(provision)) = (config, provision) else {
        // Without a config the model cannot be resolved, so what is missing cannot be
        // decided. Saying that is better than guessing a download.
        return format!(
            "provisioning unknown: {} will not load, so what is missing cannot be decided \
             here — the boot reports it as a problem instead",
            paths.config_file.display()
        );
    };
    match provision {
        setup::Provision::Ready => format!(
            "provisioning nothing: {} and {} are in place, so {} is not run and this start \
             touches no network",
            setup::Piece::Model.name(),
            setup::Piece::ModelServer.name(),
            setup::BINARY_NAME
        ),
        setup::Provision::ByHand(missing) => format!(
            "provisioning off (--no-install): {} missing, so the steps below report it and \
             nothing is fetched",
            setup::pieces_text(missing, paths)
        ),
        setup::Provision::Install(missing) => match setup::binary(paths) {
            // The names in full, then the tool: a reader of a plan for a fresh machine
            // is being told what will be downloaded and by which program.
            Some(binary) => format!(
                "provisioning {} {} --root {} ({} missing: the one time this start touches \
                 the network)",
                binary.display(),
                setup::INSTALL_ARG,
                paths.root.display(),
                setup::pieces_text(missing, paths)
            ),
            None => format!("provisioning {}", setup::missing_line(paths, missing)),
        },
    }
}

/// Print the plan. The only thing `--dry-run` does.
pub fn dry_run(paths: &Paths, printer: &Printer, install: bool) -> Result<(), String> {
    // `load_at`, not `load_managed_at`: a plan must not write a default into
    // `data/config.json` or harden a directory. A config that will not load is not a
    // failure of the plan — it is one of the lines.
    let config = atlas_config::Config::load_at(paths).ok();
    printer.say("ATLAS — the plan for `atlas start` (nothing was started)");
    for line in plan_lines(paths, config.as_ref(), install) {
        printer.note(line);
    }
    printer.say("Nothing was spawned, nothing was bound, no window was opened.");
    Ok(())
}
