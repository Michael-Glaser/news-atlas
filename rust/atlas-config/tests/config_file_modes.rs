//! `data/config.json` and `data/` are the reader's own file — owner-only from the first byte.
//!
//! What this file pins, and why it needed pinning: `config.json` is the one file in `data/`
//! a person edits by hand, so it is also the one a backup, a sync client or a second local
//! account is most likely to create with a loose mode. The rules the crate keeps are in
//! `atlas-platform/src/private.rs` (the modes) and `atlas-config/src/modes.rs` (applying
//! them); this file is the *behavioural* check: create the file through the shipped
//! `Config::save`, then read the number the kernel reports.
//!
//! Two properties are asserted separately, because they fail separately:
//!
//! * **nothing for group or other**: `mode & 0o077 == 0` — the security property;
//! * **the owner can use it**: `0o600` for a file, `0o700` for a directory — a mode of
//!   `0000` would also satisfy the first, and would be a settings file nobody can read.
//!
//! The Windows answer is asserted as a *value*, not a `cfg!`: `permission_basis`,
//! `acl_inspected` and `report` are parameterised by platform (`atlas-config/src/modes.rs:38`,
//! `:51`, `atlas-platform/src/private.rs:187`, `:199`), so the honest Windows sentence —
//! "an inherited ACL, not inspected" — is testable on the machine that builds this crate.
//! Pretending a `chmod` happened on Windows is the failure mode: `Privacy::Inherited` must
//! not be reported as a mode that was verified.

use std::path::PathBuf;

use atlas_config::{Config, Paths};
use serde_json::{json, Map};

/// A scratch HOME that does not exist yet, unique to this process.
fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|spent| spent.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "atlas-config-modes-{}-{}-{}",
        tag,
        std::process::id(),
        nanos
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch home");
    dir
}

/// The mode the kernel reports, the same number `atlas_platform::private::mode_of` reads.
#[cfg(unix)]
fn mode_of(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(path)
        .unwrap_or_else(|error| panic!("metadata of {}: {}", path.display(), error));
    metadata.permissions().mode() & 0o777
}

/// Assert owner-only, and say which of the two halves failed.
#[cfg(unix)]
fn assert_owner_only(path: &std::path::Path, expected: u32, what: &str) {
    let mode = mode_of(path);
    assert_eq!(
        mode & 0o077,
        0,
        "{} at {} is {:04o}: group or other can read it",
        what,
        path.display(),
        mode
    );
    assert_eq!(
        mode, expected,
        "{} at {} is {:04o}, not {:04o}",
        what,
        path.display(),
        mode,
        expected
    );
}

#[test]
#[cfg(unix)]
fn the_first_save_leaves_data_0700_and_the_settings_file_0600() {
    // The failure this pins, measured on the old path: `create_dir_all` answers `0755`
    // under the usual umask, so `data/` was world-readable from the moment the first save
    // created it until something else happened to tighten it. The directory is now made by
    // `mkdir` with `0700` and the file by `open` with `0600`
    // (`atlas-platform/src/private.rs:412`, `:359`), so there is no window at all.
    let home = scratch("first-save");
    let paths = Paths::at(&home);
    let mut overrides = Map::new();
    overrides.insert("model_url".to_string(), json!("http://127.0.0.1:8080"));

    Config::save(&paths, &overrides).expect("the first save");

    assert!(paths.config_file.exists(), "the save wrote no file");
    assert_owner_only(&paths.data_dir, 0o700, "the data directory");
    assert_owner_only(&paths.config_file, 0o600, "the settings file");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
#[cfg(unix)]
fn a_loose_mode_is_repaired_by_the_next_save() {
    // The file is created owner-only, but nothing stops another tool (a `cp -p` from
    // another checkout, an editor that writes a fresh inode, `chmod -R a+r` in a backup
    // script) from loosening it afterwards. `ensure_file` is the boundary that repairs it,
    // and this asserts the repair rather than the intention: `0644`/`0755` before, `0600`/
    // `0700` after a save that writes the same value.
    let home = scratch("repair");
    let paths = Paths::at(&home);
    Config::save(&paths, &Map::new()).expect("the first save");

    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&paths.config_file, std::fs::Permissions::from_mode(0o644))
        .expect("loosen the file");
    std::fs::set_permissions(&paths.data_dir, std::fs::Permissions::from_mode(0o755))
        .expect("loosen the directory");
    assert_eq!(mode_of(&paths.config_file), 0o644, "the fixture is loose");
    assert_eq!(mode_of(&paths.data_dir), 0o755, "the fixture is loose");

    Config::save(&paths, &Map::new()).expect("the second save");

    assert_owner_only(&paths.data_dir, 0o700, "the data directory after a save");
    assert_owner_only(&paths.config_file, 0o600, "the settings file after a save");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
#[cfg(unix)]
fn a_settings_file_that_is_a_link_is_refused_and_the_file_it_points_at_is_untouched() {
    // `chmod` follows a symlink, so a `data/config.json` that is a link once set the mode of
    // the file at the other end — measured in the platform crate: a file outside the project
    // went `0644` → `0600` and the report said `Changed`, i.e. "this path is owner-only"
    // about a path that was a link. `atlas-config/src/stored.rs:41` refuses to read such a
    // file; this pins that a write cannot be steered through one either, and that the
    // target keeps the mode it had.
    let home = scratch("link");
    let paths = Paths::at(&home);
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(&paths.data_dir).expect("data dir");
    let target = home.join("somebody-elses-settings.json");
    std::fs::write(&target, "{}").expect("the file at the other end");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644))
        .expect("the target is a normal file");
    std::os::unix::fs::symlink(&target, &paths.config_file).expect("the link");

    let refused = Config::save(&paths, &Map::new());

    assert!(
        refused.is_err(),
        "a settings file that is a link was written through: {:?}",
        refused.map(|config| config.values_json().to_string())
    );
    assert_eq!(
        mode_of(&target),
        0o644,
        "the file at the other end of the link had its mode changed"
    );
    assert_eq!(
        std::fs::read_to_string(&target).expect("the target is still a file"),
        "{}",
        "the file at the other end of the link was written"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_windows_answer_is_an_inherited_acl_and_says_so() {
    // No `cfg!`: the platform is an argument, because the sentence has to be the same
    // sentence on every machine that builds this crate. What must never happen is a mode
    // being *claimed* where none exists — `acl_inspected` is `false` on Windows precisely so
    // the report cannot present `ok: true` as "the ACL was read"
    // (`atlas-config/src/modes.rs:38`, `:51`).
    use atlas_config::{acl_inspected, permission_basis, Platform};

    assert_eq!(permission_basis(Platform::Posix), "posix-mode");
    assert_eq!(permission_basis(Platform::Windows), "windows-acl-inherited");
    assert!(acl_inspected(Platform::Posix), "a POSIX mode is read");
    assert!(
        !acl_inspected(Platform::Windows),
        "a Windows ACL is not inspected from this crate, and the report must say so"
    );
    // The mode constants are the POSIX ones. They are what the `open`/`mkdir` ask the kernel
    // for on a POSIX build (`atlas-platform/src/private.rs:359`, `:412`) and nothing on
    // Windows, where the profile's ACL applies instead.
    assert_eq!(atlas_config::DATA_MODE, 0o700);
    assert_eq!(atlas_config::FILE_MODE, 0o600);
}

#[test]
#[cfg(unix)]
fn the_report_publishes_the_mode_it_read_not_a_constant() {
    // `/api/security`'s report must publish a mode it *read*. The failure this pins: a
    // report that answers `0600` because that is what ATLAS intends, while the file on disk
    // is `0644`. So the test sets a distinctive mode (`0640`) that is neither intended nor
    // the umask default, and requires the report to carry *that*.
    let home = scratch("read");
    let paths = Paths::at(&home);
    Config::save(&paths, &Map::new()).expect("the first save");
    let shown = paths.config_file.to_string_lossy().to_string();

    let tight = atlas_config::permissions(&shown);
    assert!(tight.exists, "the settings file exists: {:?}", tight);
    assert!(!tight.world_readable && !tight.group_readable, "{:?}", tight);
    assert!(tight.ok, "{:?}", tight);
    assert_eq!(tight.mode, "0o600", "the report carries the mode it read");

    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&paths.config_file, std::fs::Permissions::from_mode(0o640))
        .expect("a distinctive mode");

    let loose = atlas_config::permissions(&shown);
    assert_eq!(
        loose.mode, "0o640",
        "the report must read the mode, not answer a constant"
    );
    assert!(!loose.ok, "0640 is group-readable: {:?}", loose);
    assert!(loose.group_readable, "{:?}", loose);

    let (ok, entries) = atlas_config::permission_report(&[shown.clone()]);
    assert!(!ok, "a group-readable settings file is not a clean report");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].mode, "0o640");

    // A path that does not exist answers "no mode" instead of a default.
    let missing = paths.data_dir.join("a-log-that-was-never-written.log");
    assert!(
        atlas_platform::private::mode_of(&missing).is_none(),
        "a path that does not exist must not answer a mode"
    );
    assert_eq!(
        atlas_platform::private::make_file_private(&missing),
        atlas_platform::private::Privacy::Missing,
        "a missing file is `Missing`, not `Already`"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
#[cfg(unix)]
fn a_file_the_private_open_creates_is_0600_before_any_chmod() {
    // The window `create_options` closes, and the reason the test above cannot see it: a
    // `create` followed by a `chmod` leaves the file readable between the two calls
    // (`atlas-platform/src/private.rs:359` — the mode is asked for *in* the `open`). This
    // test writes nothing but the create, and reads the mode the kernel reports at once.
    use std::io::Write;

    let home = scratch("first-byte-file");
    let file = home.join("atlas.log");
    let mut handle = atlas_platform::private::create_options()
        .open(&file)
        .expect("the private open");
    handle.write_all(b"one line\n").expect("write");
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(&file).expect("metadata").permissions().mode() & 0o777
    };
    drop(handle);
    assert_eq!(
        mode & 0o077,
        0,
        "the open left the file readable beyond its owner ({:04o}): a `chmod` after the \
         write is a window, not a guarantee",
        mode
    );
    assert_eq!(mode, 0o600, "a file inside data/ is 0600");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
#[cfg(unix)]
fn a_directory_the_private_mkdir_creates_is_0700_from_the_mkdir() {
    // `create_dir_all` answers `0755` under the usual umask (`022`), which is a directory
    // another account can list between the create and the `chmod`
    // (`atlas-platform/src/private.rs:412`). What this test can and cannot see: it pins the
    // *end state* of `create_dir`. The end state alone cannot tell a `mkdir`-with-mode from a
    // `chmod` right after it — for that half see
    // `a_file_the_private_open_creates_is_0600_before_any_chmod`, where nothing repairs a
    // missing mode, and `atlas-platform/src/private.rs`'s own tests, which assert the
    // builder's `mode()` call.
    let home = scratch("first-byte-dir");
    let dir = home.join("archive");
    atlas_platform::private::create_dir(&dir).expect("the private mkdir");
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(&dir).expect("metadata").permissions().mode() & 0o777
    };
    assert_eq!(mode & 0o077, 0, "another account can list it ({:04o})", mode);
    assert_eq!(mode, 0o700, "a data directory is 0700");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_credential_inside_a_legitimate_value_is_redacted_by_values_json() {
    // Flipped. This test used to pin the leak: `values_json` (`atlas-config/src/view.rs:24`)
    // filtered by *key name* (`crate::key_is_secret`) and never asked the redaction rule about
    // a *value*, so a model address with credentials in it — the exact example
    // `atlas-desktop/src/app.rs:134` names — was printed verbatim by `atlas-config config`
    // (`cli.rs:91`). Every string value now goes through `atlas_guard::redact` before it is
    // inserted, so the printed object keeps the shape and loses the password.
    let home = scratch("userinfo");
    let paths = Paths::at(&home);
    let mut overrides = Map::new();
    overrides.insert(
        "model_url".to_string(),
        json!("http://alice:secret@127.0.0.1:8080"),
    );
    Config::save(&paths, &overrides).expect("save");
    let config = Config::load_at(&paths).expect("load");
    let printed = config.values_json().to_string();
    assert!(
        !printed.contains("alice:secret@"),
        "the credential is published by values_json: {printed}"
    );
    assert!(
        printed.contains("[redacted]"),
        "the value must be redacted, not dropped — a person has to see which setting is \
         wrong: {printed}"
    );
    assert!(
        printed.contains("127.0.0.1:8080"),
        "the address must survive: only the credential is removed: {printed}"
    );
    // `describe()` carries the same object, so it inherits the fix.
    let described = config.describe().to_string();
    assert!(!described.contains("alice:secret@"), "{described}");
    // The file itself still holds the value: it is the reader's own 0600 file, and ATLAS does
    // not rewrite somebody's configuration behind their back — `save` is the write path, and
    // what it writes is what the reader asked for.
    let on_disk = std::fs::read_to_string(&paths.config_file).expect("file");
    assert!(on_disk.contains("alice:secret@"));
    // The mode half is POSIX-only, and it is `#[cfg(unix)]` rather than merely skipped at
    // run time: `mode_of` is defined behind that gate and reads `PermissionsExt`, so an
    // unguarded call made this whole test target fail to *compile* for Windows —
    // measured with `cargo check --tests --target x86_64-pc-windows-msvc -p atlas-config`:
    // `error[E0425]: cannot find function 'mode_of' in this scope` at this line. The
    // redaction assertions above are the part that has to hold on every platform.
    #[cfg(unix)]
    assert_eq!(mode_of(&paths.config_file), 0o600, "the file is owner-only");
    let _ = std::fs::remove_dir_all(&home);
}
