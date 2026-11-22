//! The checks, one paragraph each.
//!
//! Every check is a pure function of a [`atlas_config::Paths`] (plus the settings
//! when it needs a port), so a test asks one question at a time and `doctor::check`
//! is only the list. The wording lives with the check: the detail is what the user
//! reads, and the fix is what they type.
//!
//! What none of them do: touch the network, write outside `data/`, or open a socket.
//! The SQLite probe is a throwaway file, removed again; the port check *connects* to the
//! model server's port (refused means free) and never binds it — the product's rule is
//! that no port is ever opened, and a probe that bound one would be a port opened.

use std::path::{Path, PathBuf};

use atlas_config::Paths;

use crate::report::one_line;
use crate::{health, llama, model, ports, toolchain};

use super::Check;

/// The toolchain: a `cargo` and a `rustc`, with the versions they report.
pub fn toolchain_check() -> Check {
    let toolchain = toolchain::detect();
    if toolchain.usable() {
        Check::ok("toolchain", toolchain.text())
    } else {
        Check::fail(
            "toolchain",
            toolchain.text(),
            "install Rust (rustup.rs). The build needs it once; ATLAS itself never downloads \
             anything at run time",
        )
    }
}

/// The window binary: the one program `atlas start` needs besides the model server.
///
/// This check used to be "the nine binaries the ingest pipeline calls", read out of
/// `atlas_web::native::REQUIRED` — the boot gate of the listener crate, which no shipped
/// binary links any more. The rule that ties the doctor to the app is unchanged and is
/// what makes the smaller list right: **the verdict has to be the answer `atlas start`
/// would give**, and `atlas start` now needs the window and the model server, nothing
/// else. A binary in `target/release` that nothing starts is not a thing to fail on.
pub fn window_check(paths: &Paths) -> Check {
    let release = crate::window::release_dir(paths);
    match crate::window::binary(paths) {
        Some(binary) => Check::ok("window", format!("{}", binary.display())),
        None => Check::fail(
            "window",
            crate::window::missing_line(paths),
            format!(
                "cargo build --release --offline --locked -p atlas-desktop (from {}); \
                 nothing is downloaded",
                release
                    .parent()
                    .and_then(|path| path.parent())
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| release.display().to_string())
            ),
        ),
    }
}

/// `atlas-config`'s settings, when they load at all.
pub(super) fn config_check(paths: &Paths) -> Option<atlas_config::Config> {
    atlas_config::Config::load_at(paths).ok()
}

/// The model file: present, and how big.
pub fn model_check(paths: &Paths, config: Option<&atlas_config::Config>) -> Check {
    let config = match config {
        Some(config) => config,
        None => {
            return Check::fail(
                "model file",
                "the configuration cannot be read, so the model path is unknown",
                format!("fix or remove {}", paths.config_file.display()),
            )
        }
    };
    match model::resolve(paths, config) {
        Ok(model) => Check::ok("model file", model.text()),
        // The fix names the file to put where: "the file is placed by hand" is not
        // a step a user can perform.
        Err(missing) => Check::fail(
            "model file",
            missing.sentence(),
            // The path, and the *command* that closes the line: "place the file" without
            // where it comes from is the step a person cannot perform. The verdict is the
            // answer `atlas start` gives, and since the product provisions itself that
            // answer is "the next start runs the install once" — not "fetch it yourself".
            format!(
                "run `atlas start` and let it provision (it runs `atlas-setup --install` once \
                 for {}), or run `atlas-setup model --yes` yourself; `atlas start --no-install` \
                 keeps a start off the network",
                missing.expected.display()
            ),
        ),
    }
}

/// The model server binary.
pub fn llama_check(paths: &Paths) -> Check {
    match llama::binary(paths) {
        Some(binary) => Check::ok(
            "model server",
            format!(
                "{} (the model is served on 127.0.0.1 only)",
                binary.display()
            ),
        ),
        None => Check::fail(
            "model server",
            format!(
                "{} is not in {} and not on PATH",
                llama::BINARY_NAME,
                paths.llama_local.display()
            ),
            "run `atlas-setup llama --yes` — it builds llama-server from tools/llama.cpp \
             into bin/ with the cmake this machine has, and nothing is downloaded; `atlas \
             start` runs that same install itself when the binary is missing",
        ),
    }
}

/// The loopback invariant: the model server's address, and no host-like setting.
///
/// This used to walk every port through `atlas_web::boot::bind_address`, because the
/// launcher *was* the listener. It is not any more: the only address in a start is the
/// model server's, it is the constant `atlas-config` publishes, and [`crate::llama`]
/// passes it as `--host 127.0.0.1`. So the check is the invariant itself — is the
/// constant loopback — plus the refusal of a config that carries a host of its own.
pub fn loopback_check(paths: &Paths, config: Option<&atlas_config::Config>) -> Check {
    let config = match config {
        Some(config) => config,
        None => {
            return Check::fail(
                "loopback invariant",
                "the configuration cannot be read, so the ports are unknown",
                format!("fix or remove {}", paths.config_file.display()),
            )
        }
    };
    if !atlas_config::is_loopback(atlas_config::BIND_HOST) {
        return Check::fail(
            "loopback invariant",
            format!(
                "{} is not judged loopback by atlas-guard",
                atlas_config::BIND_HOST
            ),
            "this is a bug, not a setting: rebuild the workspace",
        );
    }
    let problems = config.validate();
    if !problems.is_empty() {
        let lines: Vec<String> = problems.iter().map(|problem| problem.to_string()).collect();
        return Check::fail(
            "loopback invariant",
            format!("the configuration carries host-like keys: {}", lines.join("; ")),
            format!("remove them from {}", paths.config_file.display()),
        );
    }
    Check::ok(
        "loopback invariant",
        format!(
            "the model server is started on {} only; ATLAS itself opens no port and opens \
             no browser",
            atlas_config::BIND_HOST
        ),
    )
}

/// A real SQLite file in `data/`: created, written, read back, removed.
///
/// A probe instead of the archive, because the doctor must not touch the data the
/// app is going to use — and a probe still proves the two things that fail on a
/// fresh machine: the directory is writable and the bundled SQLite can open a file
/// there (no system library, no missing WAL support).
pub fn sqlite_check(paths: &Paths) -> Check {
    atlas_config::ensure_dir(&paths.data_dir);
    let probe = paths.data_dir.join(".atlas-doctor.db");
    let _ = remove_db(&probe);
    let opened = atlas_store::Store::open(&probe).and_then(|store| {
        store.exec("CREATE TABLE IF NOT EXISTS doctor_probe(x INTEGER)", &[])?;
        store.exec("INSERT INTO doctor_probe(x) VALUES (1)", &[])?;
        let rows = store.query("SELECT x FROM doctor_probe", &[])?;
        Ok(rows.len())
    });
    let removed = remove_db(&probe);
    match opened {
        Ok(1) if removed => Check::ok(
            "sqlite",
            format!(
                "a database in {} opens, writes and reads back (bundled SQLite, file removed again)",
                paths.data_dir.display()
            ),
        ),
        Ok(rows) => Check::fail(
            "sqlite",
            format!("the probe wrote and read back {} rows instead of 1", rows),
            "this is a bug in the store, not a setting",
        ),
        // One line: a SQLite error arrives with newlines in it, and the report
        // promises one line per check (see `report::entry`).
        Err(error) => Check::fail(
            "sqlite",
            one_line(&error.to_string()),
            format!(
                "make {} writable by you; ATLAS keeps its archive there",
                paths.data_dir.display()
            ),
        ),
    }
}

fn remove_db(path: &Path) -> bool {
    let mut ok = true;
    for candidate in [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ] {
        if candidate.exists() && std::fs::remove_file(&candidate).is_err() {
            ok = false;
        }
    }
    ok
}

/// The model server's port, asked of the kernel.
///
/// A port the model server cannot have is a warning, never a failure: `atlas start`
/// adopts a `llama-server` that is already answering on it, and a held port only means
/// the window opens without its local model. The web port that used to be the one
/// failure here is gone with the listener.
pub fn ports_check(paths: &Paths, config: Option<&atlas_config::Config>) -> Check {
    let config = match config {
        Some(config) => config,
        None => {
            return Check::fail(
                "ports",
                "the configuration cannot be read, so the port is unknown",
                format!("fix or remove {}", paths.config_file.display()),
            )
        }
    };
    let model = ports::model(config);
    // Asked once: a probe is a connection, and asking three times (state, then the
    // sentence, then the fix) described a port that had changed between two of them.
    let state = ports::state(model.port);
    match state {
        ports::Port::Free => Check::ok("ports", format!("{} {} is free", model.name, model.port)),
        // A model server already answering is the *good* case: the next `atlas
        // start` reuses it and starts no second one.
        ports::Port::Taken if health::probe(model.port).is_up() => Check::ok(
            "ports",
            format!(
                "a model server is already answering on {} — `atlas start` will use it",
                model.port
            ),
        ),
        ports::Port::Taken | ports::Port::Unusable => Check::warn(
            "ports",
            format!(
                "something else holds {} {} ({}), so ATLAS opens without its local model",
                model.name,
                model.port,
                way_of(state)
            ),
            format!(
                "stop what holds port {}, or set {} in {} to a free port",
                model.port,
                model.key,
                paths.config_file.display()
            ),
        ),
    }
}

/// What a port's state is, as a sentence fragment.
fn way_of(state: ports::Port) -> &'static str {
    match state {
        ports::Port::Free => "it is free",
        ports::Port::Taken => "it is already in use",
        ports::Port::Unusable => "the connection failed for a reason that is neither a refusal \
             nor an answer",
    }
}

/// `data/` is 0700 and what ATLAS writes there is 0600.
pub fn permissions_check(paths: &Paths) -> Check {
    let dir = paths.data_dir.display().to_string();
    let mut parts: Vec<String> = Vec::new();
    let mut exposure: Vec<String> = Vec::new();
    let mut entries: Vec<String> = vec![dir.clone()];
    for extra in [&paths.config_file, &paths.db, &paths.log_app, &paths.log_llama] {
        if extra.exists() {
            entries.push(extra.display().to_string());
        }
    }
    for entry in &entries {
        let info = atlas_config::permissions(entry);
        if !info.exists {
            continue;
        }
        parts.push(format!(
            "{} {}",
            Path::new(entry)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| entry.clone()),
            info.mode
        ));
        if !info.ok {
            exposure.push(entry.clone());
        }
    }
    let writable = probe_write(&paths.data_dir);
    if !writable {
        return Check::fail(
            "data permissions",
            format!("{} is not writable by this user", paths.data_dir.display()),
            "fix the ownership of data/, or start ATLAS as the user who owns it",
        );
    }
    if !exposure.is_empty() {
        return Check::warn(
            "data permissions",
            format!(
                "{} is readable beyond your account: {}",
                paths.data_dir.display(),
                exposure.join(", ")
            ),
            "run `atlas start` once — it hardens data/ to 0700 and its files to 0600",
        );
    }
    Check::ok(
        "data permissions",
        format!("writable, {}", parts.join(", ")),
    )
}

fn probe_write(dir: &Path) -> bool {
    atlas_config::ensure_dir(dir);
    let probe = dir.join(".atlas-doctor-write");
    let written = std::fs::write(&probe, b"atlas\n").is_ok();
    let removed = std::fs::remove_file(&probe).is_ok();
    written && removed
}
