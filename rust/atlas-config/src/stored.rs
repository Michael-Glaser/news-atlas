//! `data/config.json`: the one file a caller's settings live in.
//!
//! Decisions: a missing file is not an error (defaults apply), a file that is not
//! a JSON object is, and a write is 0600 in a 0700 `data/` — a settings file is
//! not world-readable. A write merges into what is already there rather than
//! replacing the object wholesale.
//!
//! It also decides that this file is *this file*: a `data/config.json` that is a
//! symlink is refused rather than followed, and the temporary name a write
//! creates is never written through a link that is already sitting there. Both
//! halves are the same rule — the settings ATLAS acts on are the settings in its
//! own `data/`, so a link swapped in by a backup, a sync client or a second local
//! account cannot choose what the app reads, and a write cannot be steered into
//! somebody else's file (which `fs::write` followed, and `ensure_file` then
//! chmod'ed to 0600 on the way past).
//!
//! It does not: validate a value, decide a default, or redact a credential —
//! [`crate::Config::save`] does all three before it hands anything here.

use std::io::{Read, Write};

use serde_json::{Map, Value};

use crate::modes::{ensure_dir, ensure_file};
use crate::paths::Paths;
use crate::problem::ConfigError;

/// The stored choices, as a JSON object. A missing file is an empty object — a
/// file that exists but cannot be used is an error, never silently empty.
pub fn read_stored(paths: &Paths) -> Result<Map<String, Value>, ConfigError> {
    let origin = paths.config_origin();
    let refuse = |detail: String| ConfigError::File {
        origin: origin.clone(),
        detail,
    };
    match std::fs::symlink_metadata(&paths.config_file) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => return Err(refuse(format!("cannot be read ({})", error.kind()))),
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(refuse(
                "is a symlink: ATLAS reads the settings in its own data directory, not whatever \
                 this link points at (replace the link with the file itself)"
                    .to_string(),
            ))
        }
        Ok(_) => {}
    }
    // `O_NOFOLLOW` where the platform has it: the file that was just checked is
    // the file that is read, even if the name is swapped between the two steps.
    let mut file = match atlas_platform::private::open_readonly_nofollow(&paths.config_file) {
        Some(file) => file,
        None => {
            return Err(refuse(
                "cannot be opened (the file changed while it was being checked)".to_string(),
            ))
        }
    };
    let mut text = String::new();
    if let Err(error) = file.read_to_string(&mut text) {
        return Err(refuse(format!("cannot be read ({})", error.kind())));
    }
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(refuse("must hold a JSON object".to_string())),
        Err(error) => Err(refuse(format!("is not valid JSON ({})", error))),
    }
}

/// Persist choices atomically: write a 0600 temp file next to `config.json`,
/// then rename it into place, so a half-written config never exists.
pub fn write_stored(paths: &Paths, stored: &Map<String, Value>) -> Result<(), ConfigError> {
    let origin = paths.config_origin();
    ensure_dir(&paths.data_dir);
    let body = format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(stored.clone()))
            .unwrap_or_else(|_| "{}".to_string())
    );
    let tmp = paths.config_file.with_extension("json.tmp");
    let fail = |error: std::io::Error| ConfigError::File {
        origin: origin.clone(),
        detail: format!("cannot be written ({})", error.kind()),
    };
    // What is at the temporary name is not trusted to be a leftover: a symlink
    // there used to be followed by `fs::write`, so the *target* received the
    // settings, and `ensure_file` then set that target to 0600 — a chmod on a file
    // ATLAS never created. Removing the name first (a link is removed, not
    // followed) and then creating it exclusively is the same rule `read_stored`
    // applies to the file itself.
    match std::fs::symlink_metadata(&tmp) {
        Ok(meta) if meta.is_file() || meta.file_type().is_symlink() => {
            std::fs::remove_file(&tmp).map_err(fail)?;
        }
        Ok(_) => {
            return Err(ConfigError::File {
                origin,
                detail: "the temporary settings file is not a regular file".to_string(),
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(fail(error)),
    }
    // 0600 from the first byte (`atlas-platform` owns the number) and
    // `O_NOFOLLOW` where the platform has it, so the create cannot resolve a link
    // planted between the removal above and this call.
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(atlas_platform::private::FILE_MODE);
        let no_follow = atlas_platform::private::no_follow_flag();
        if no_follow != 0 {
            options.custom_flags(no_follow);
        }
    }
    let mut file = options.open(&tmp).map_err(fail)?;
    file.write_all(body.as_bytes()).map_err(fail)?;
    file.flush().map_err(fail)?;
    drop(file);
    std::fs::rename(&tmp, &paths.config_file).map_err(fail)?;
    ensure_file(&paths.config_file);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::*;
    use crate::config::Config;


#[test]
fn a_missing_config_file_is_not_an_error() {
    let dir = TempDir::new("missing");
    let file = read_stored(&dir.at()).expect("missing file is empty");
    assert!(file.is_empty());
    let config = load_env_free(&dir).expect("load");
    assert_eq!(config.app_port, 8787);
    assert!(config.validate().is_empty());
}

#[test]
fn an_unreadable_config_file_is_an_error_naming_the_file() {
    let dir = TempDir::new("unreadable");
    // A directory where the file belongs: reading it fails on every platform,
    // including as root, so this assertion holds in CI too.
    std::fs::create_dir_all(dir.at().config_file).expect("dir as file");
    let error = read_stored(&dir.at()).expect_err("must refuse");
    assert_eq!(error.origin().as_deref(), Some("data/config.json"));
    assert!(error.to_string().starts_with("data/config.json: "));
    assert_eq!(error.key(), None);
    assert!(Config::load_strict_at(&dir.at()).is_err());
}

#[cfg(unix)]
#[test]
fn a_config_file_without_read_permission_is_an_error() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("chmod0");
    dir.write("data/config.json", r#"{"app_port": 9001}"#);
    std::fs::set_permissions(&dir.at().config_file, std::fs::Permissions::from_mode(0o000))
        .expect("chmod");
    let outcome = read_stored(&dir.at());
    let _ = std::fs::set_permissions(
        &dir.at().config_file,
        std::fs::Permissions::from_mode(0o600),
    );
    // A privileged runner can still read it; then there is nothing to prove.
    if outcome.is_ok() {
        return;
    }
    assert!(outcome.unwrap_err().to_string().contains("data/config.json"));
}

#[test]
fn a_config_file_that_is_not_an_object_is_an_error() {
    let dir = TempDir::new("array");
    dir.write("data/config.json", "[1, 2, 3]");
    let error = read_stored(&dir.at()).expect_err("must refuse");
    assert!(error.to_string().contains("must hold a JSON object"));

    let dir = TempDir::new("broken");
    dir.write("data/config.json", "{not json");
    let error = read_stored(&dir.at()).expect_err("must refuse");
    assert!(error.to_string().contains("is not valid JSON"));
}

#[cfg(unix)]
#[test]
fn a_symlinked_settings_file_is_refused_rather_than_followed() {
    // The settings decide the ports, the model path and the browser path. A
    // `data/config.json` that is a link is a file somebody else can swap for the
    // app's own: the read used to follow it and act on whatever was at the other
    // end, with the path in every message still reading `data/config.json`.
    let dir = TempDir::new("stored-link");
    let elsewhere = TempDir::new("stored-link-outside");
    let target = elsewhere.path.join("payload.json");
    std::fs::write(&target, r#"{"app_port": 9401}"#).expect("payload");
    std::fs::create_dir_all(dir.at().data_dir).expect("data");
    std::os::unix::fs::symlink(&target, &dir.at().config_file).expect("symlink");

    let error = read_stored(&dir.at()).expect_err("a link is not this file");
    assert_eq!(error.origin().as_deref(), Some("data/config.json"));
    assert!(
        error.to_string().contains("is a symlink"),
        "the refusal names what is wrong: {}",
        error
    );
    // Failing closed: the app refuses to start on its own settings rather than
    // running on somebody else's.
    assert!(Config::load_strict_at(&dir.at()).is_err());
}

#[cfg(unix)]
#[test]
fn a_symlink_at_the_temporary_name_is_removed_not_written_through() {
    // `fs::write` follows a symlink, so a link planted at `config.json.tmp`
    // received the settings body and `ensure_file` then set *its target* to 0600
    // — a write into, and a chmod of, a file ATLAS never created. Both halves
    // were measured on the old code: the victim's bytes were replaced and its
    // mode went from 0644 to 0600.
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("stored-tmp-link");
    let victim_dir = TempDir::new("stored-tmp-victim");
    let victim = victim_dir.path.join("victim.txt");
    std::fs::write(&victim, "not ours").expect("victim");
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let paths = dir.at();
    std::fs::create_dir_all(&paths.data_dir).expect("data");
    std::os::unix::fs::symlink(&victim, paths.config_file.with_extension("json.tmp"))
        .expect("symlink");

    let mut overrides = serde_json::Map::new();
    overrides.insert("app_port".to_string(), Value::from(9401));
    Config::save(&paths, &overrides).expect("save");

    assert_eq!(
        std::fs::read_to_string(&victim).expect("victim"),
        "not ours",
        "the settings were written through the link"
    );
    let mode = std::fs::metadata(&victim).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode, 0o644, "the link's target was chmod'ed: {:o}", mode);
    // …and the settings still landed where they belong, as a real file.
    assert!(
        !crate::modes::is_symlink(&paths.config_file),
        "config.json is a link"
    );
    let stored = read_stored(&paths).expect("stored");
    assert_eq!(stored.get("app_port").and_then(Value::as_u64), Some(9401));
    let mode = std::fs::metadata(&paths.config_file).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "the settings file is not 0600: {:o}", mode);
}

}
