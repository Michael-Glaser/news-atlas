//! The doctor's tests: a temporary root with and without a model, a broken config,
//! and the two permission answers a fresh `data/` can give.

use std::path::{Path, PathBuf};

use super::checks::{model_check, ports_check, permissions_check, sqlite_check};
use super::{check, Status};
use crate::report::Printer;
use atlas_config::Paths;



fn fresh_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "atlas-doctor-{}-{}-{}",
        tag,
        std::process::id(),
        atlas_store::now_iso().replace([':', '.'], "-")
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(dir.join("data"));
    let _ = std::fs::create_dir_all(dir.join("models"));
    dir
}

#[test]
fn a_root_with_no_model_file_fails_the_model_check_and_the_exit_code() {
    let root = fresh_root("nomodel");
    let paths = Paths::at(&root);
    let report = check(&paths);
    let model_check = report.find("model file").expect("the model check ran");
    assert_eq!(model_check.status, Status::Fail);
    assert!(
        model_check.detail.contains(&paths.model_file.display().to_string()),
        "{}",
        model_check.detail
    );
    // The detail is the model step's own sentence, and that sentence says what the next
    // start does — it used to promise "nothing is downloaded", which stopped being true
    // when the app learned to provision itself.
    assert!(
        model_check.detail.contains("provisions"),
        "{}",
        model_check.detail
    );
    assert_eq!(report.exit_code(), 1);
    assert!(report.failed() >= 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_report_names_every_check_the_brief_asks_for() {
    let root = fresh_root("all");
    let paths = Paths::at(&root);
    let report = check(&paths);
    for name in [
        "toolchain",
        "window",
        "model file",
        "model server",
        "loopback invariant",
        "sqlite",
        "ports",
        "data permissions",
    ] {
        assert!(report.find(name).is_some(), "no check named {}", name);
    }
    // Eight, not nine: the "native binaries" check read `atlas_web::native::REQUIRED`
    // (the listener crate's boot gate), and the "browser" check looked for Chrome. No
    // shipped binary links either now, and the product has no browser, so both are gone
    // — and `window` replaced the first, because the window is the one program a start
    // needs besides the model server.
    assert_eq!(report.checks.len(), 8);
    for gone in ["native binaries", "browser"] {
        assert!(
            report.find(gone).is_none(),
            "the report still checks {:?}, which the product no longer uses",
            gone
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[test]
fn a_writable_data_directory_passes_the_sqlite_and_permission_checks() {
    let root = fresh_root("writable");
    let paths = Paths::at(&root);
    assert_eq!(sqlite_check(&paths).status, Status::Ok);
    // Nothing of the probe is left behind.
    assert!(!paths.data_dir.join(".atlas-doctor.db").exists());
    assert!(!paths.data_dir.join(".atlas-doctor.db-wal").exists());
    assert!(!paths.data_dir.join(".atlas-doctor-write").exists());
    // The fresh directory was made with the process umask; the check has to
    // notice that, and say `ok` once it is 0700.
    #[cfg(unix)]
    {
        let before = permissions_check(&paths);
        assert!(
            before.status == Status::Warn || before.status == Status::Ok,
            "{:?}",
            before
        );
        set_mode(&paths.data_dir, 0o700);
    }
    assert_eq!(permissions_check(&paths).status, Status::Ok);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_data_directory_readable_beyond_its_owner_is_a_warning_not_a_failure() {
    let root = fresh_root("exposed");
    let paths = Paths::at(&root);
    #[cfg(unix)]
    set_mode(&paths.data_dir, 0o755);
    let check = permissions_check(&paths);
    assert_eq!(check.status, Status::Warn, "{:?}", check);
    assert!(check.detail.contains("readable beyond"), "{}", check.detail);
    assert!(check.fix.is_some(), "a warning carries the fix");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_full_doctor_run_with_a_model_present_can_be_clean() {
    let root = fresh_root("clean");
    let paths = Paths::at(&root);
    std::fs::write(&paths.model_file, vec![0u8; 2048]).unwrap();
    let report = check(&paths);
    let model_check = report.find("model file").unwrap();
    assert_eq!(model_check.status, Status::Ok, "{}", model_check.detail);
    assert!(model_check.detail.contains("2 kB"), "{}", model_check.detail);
    // The loopback invariant holds on every machine this app runs on.
    assert_eq!(report.find("loopback invariant").unwrap().status, Status::Ok);
    assert_eq!(report.find("sqlite").unwrap().status, Status::Ok);
    // The toolchain is present where these tests run.
    assert_eq!(report.find("toolchain").unwrap().status, Status::Ok);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_broken_config_file_is_a_failure_and_not_a_crash() {
    let root = fresh_root("badconfig");
    let paths = Paths::at(&root);
    std::fs::write(&paths.config_file, b"{ this is not json").unwrap();
    let report = check(&paths);
    assert_eq!(report.find("model file").unwrap().status, Status::Fail);
    assert_eq!(report.find("ports").unwrap().status, Status::Fail);
    assert_eq!(report.exit_code(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_report_prints_one_aligned_line_per_check_and_says_what_is_missing() {
    let root = fresh_root("print");
    let paths = Paths::at(&root);
    let report = check(&paths);
    let printer = Printer::new();
    report.print(&printer);
    assert!(report.exit_code() == 1);
    // `lines()` is the shape a terminal shows.
    let lines = report.find("model file").unwrap().lines();
    assert!(lines[0].starts_with("  model file"), "{}", lines[0]);
    assert!(lines[0].contains("FAIL"), "{}", lines[0]);
    assert!(lines.len() >= 2, "a failure carries a fix line");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_status_word_is_plain_ascii_and_the_failure_stands_out() {
    assert_eq!(Status::Ok.word(), "ok");
    assert_eq!(Status::Warn.word(), "warn");
    assert_eq!(Status::Fail.word(), "FAIL");
}

// ---------------------------------------------------------------------------
// the doctor's verdict is the answer `atlas start` would give
// ---------------------------------------------------------------------------

/// One lock for the four port tests: each binds an ephemeral loopback port and
/// drops it again, and the kernel may hand the same number to two of them at
/// once. Serialising them is the difference between a test and a coin toss.
static PORTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    PORTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A loopback port this test **holds**, so nothing can take it in the meantime.
fn held_port() -> (std::net::TcpListener, u16) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free loopback port");
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

/// One port that is free by the time this returns.
///
/// Bound, released, then confirmed with the same probe the check uses. A bind
/// elsewhere on the machine can take it in between; that is retried rather than
/// asserted away.
fn free_port_now() -> u16 {
    for _ in 0..5 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free loopback port");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        if crate::ports::state(port).is_free() {
            return port;
        }
    }
    panic!("no loopback port stayed free for the check");
}

/// A config that fixes the model port in the file, so `atlas start` honours it as
/// written.
fn fixed_config(paths: &Paths, llama: u16) -> atlas_config::Config {
    std::fs::write(
        &paths.config_file,
        format!("{{\"llama_port\": {}}}", llama),
    )
    .expect("the config file is written");
    atlas_config::Config::load_at(paths).expect("the written config loads")
}

#[test]
fn a_held_model_port_is_a_warning_because_the_window_opens_anyway() {
    // `atlas start` adopts a llama-server that is already answering and runs without
    // one that is not, so the doctor must not tell the user ATLAS cannot start. The web
    // port that used to be the one FAIL here is gone with the listener.
    let _serial = serial();
    let root = fresh_root("heldmodel");
    let paths = Paths::at(&root);
    let (held, llama) = held_port();
    let config = fixed_config(&paths, llama);
    let check = ports_check(&paths, Some(&config));
    assert_ne!(check.status, Status::Fail, "{:?}", check);
    assert_eq!(check.status, Status::Warn, "{:?}", check);
    assert!(check.detail.contains("without its local model"), "{}", check.detail);
    assert!(check.fix.as_deref().unwrap_or_default().contains("llama_port"));
    drop(held);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_answering_model_server_on_the_model_port_is_ok() {
    // The other half of the same question, and the one that needs a real answer: a
    // `llama-server` that replies `200` to `/health` is *reused* by `atlas start`, which
    // is the good case. The stand-in is this test's own loopback socket, held for the
    // length of one check and answered from a thread — the only way to test the branch
    // without a model.
    let _serial = serial();
    let root = fresh_root("answering");
    let paths = Paths::at(&root);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        use std::io::Write;
        // Two connections arrive: the port probe (`ports::state`) connects and hangs up
        // without a word, then the health probe asks for `/health`. Answering both is what
        // a real `llama-server` does.
        for _ in 0..2 {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}");
            let _ = stream.flush();
        }
    });
    let config = fixed_config(&paths, port);
    let check = ports_check(&paths, Some(&config));
    let _ = server.join();
    assert_eq!(check.status, Status::Ok, "{:?}", check);
    assert!(
        check.detail.contains("already answering"),
        "{}",
        check.detail
    );
    assert!(check.detail.contains(&port.to_string()), "{}", check.detail);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_free_model_port_is_reported_by_name() {
    let _serial = serial();
    let root = fresh_root("freeports");
    let paths = Paths::at(&root);
    let llama = free_port_now();
    let config = fixed_config(&paths, llama);
    let check = ports_check(&paths, Some(&config));
    assert_eq!(check.status, Status::Ok, "{:?}", check);
    assert!(check.detail.contains("is free"), "{}", check.detail);
    assert!(check.detail.contains(&llama.to_string()), "{}", check.detail);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn every_failure_carries_the_command_or_step_that_fixes_it() {
    // The promise of the whole command: a FAIL line is followed by something to do.
    let root = fresh_root("fixes");
    let paths = Paths::at(&root);
    // A broken config and no model: as many failures as this root can produce.
    std::fs::write(&paths.config_file, b"{ this is not json").unwrap();
    let report = check(&paths);
    assert!(report.failed() >= 1);
    for check in &report.checks {
        if check.status == Status::Fail {
            let fix = check.fix.as_deref().unwrap_or("");
            assert!(!fix.trim().is_empty(), "the {} check has no fix", check.name);
            assert!(fix.chars().count() > 10, "the {} fix says nothing", check.name);
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_failing_sqlite_line_stays_on_one_line() {
    // The report prints one line per check; a raw SQLite error carries newlines.
    let root = fresh_root("sqliteline");
    let paths = Paths::at(&root);
    let report = check(&paths);
    for check in &report.checks {
        let lines = check.lines();
        assert_eq!(lines.len(), if check.fix.is_some() { 2 } else { 1 });
        assert!(!lines[0].contains('\n'), "{:?}", lines[0]);
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_missing_model_fix_names_the_file_to_place() {
    let root = fresh_root("modelfix");
    let paths = Paths::at(&root);
    let check = model_check(&paths, Some(&atlas_config::Config::load_at(&paths).unwrap()));
    assert_eq!(check.status, Status::Fail);
    let fix = check.fix.expect("a failure carries the fix");
    assert!(fix.contains(&paths.model_file.display().to_string()), "{}", fix);
    // The fix has to be the step a person can perform *and* the one the boot performs: the
    // app provisions itself now, so "place it by hand" is no longer the only answer, and
    // "the app fetches nothing at run time" is no longer true of `atlas start`.
    assert!(fix.contains("atlas-setup --install"), "{}", fix);
    assert!(fix.contains("atlas start"), "{}", fix);
    assert!(fix.contains("--no-install"), "{}", fix);
    // And the command that closes the line on its own: the path alone is not a step.
    assert!(fix.contains("atlas-setup model --yes"), "{}", fix);
    let _ = std::fs::remove_dir_all(&root);
}
