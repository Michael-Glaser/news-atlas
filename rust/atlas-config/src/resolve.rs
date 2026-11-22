//! Which path a setting may actually become.
//!
//! Decisions: a relative path resolves under the project root, a path that climbs
//! out of it (directly, through `..`, or through a symlink) is refused rather than
//! honoured, and a binary is only accepted when it really is executable. A model
//! is looked up by its pinned name, never by a glob from the environment.
//!
//! It does not: read the value out of the config (callers pass the raw string), or
//! decide where the project root is — [`crate::paths::Paths`] does.

use std::path::{Path, PathBuf};

use crate::paths::{
    expand_user, inside_real, is_executable, prefix_inside, is_absolute_shaped, normalize_slash, path_is_file, separators_to_slash,
    strip_trailing_slash,
};
use crate::warn::warn;

/// What a configured path is allowed to be. One struct instead of Python's
/// seven keyword arguments, so the per-setting policy is data.
#[derive(Clone, Debug)]
pub struct PathPolicy {
    /// Used when the configured value is empty.
    pub default: String,
    /// Named in the hardening note, e.g. `model path`.
    pub label: &'static str,
    /// The project root a relative value is resolved against.
    pub base: PathBuf,
    /// Refuse a value that does not exist.
    pub must_exist: bool,
    /// Refuse a value that is not a regular executable file.
    pub want_exec: bool,
    /// A deliberately configured system browser lives in /Applications or
    /// Program Files; an absolute path outside the project is then allowed.
    pub allow_outside: bool,
}

impl PathPolicy {
    /// A path pinned to the project.
    pub fn pinned(base: impl AsRef<Path>, default: &str, label: &'static str) -> PathPolicy {
        PathPolicy {
            default: default.to_string(),
            label,
            base: base.as_ref().to_path_buf(),
            must_exist: false,
            want_exec: false,
            allow_outside: false,
        }
    }

    /// The policy for the model file: pinned, may not exist yet.
    pub fn model(base: impl AsRef<Path>, default: &str) -> PathPolicy {
        PathPolicy::pinned(base, default, "model path")
    }

    /// The policy for a system browser: an absolute path outside the project is
    /// legitimate, so it is named rather than refused.
    pub fn browser(base: impl AsRef<Path>) -> PathPolicy {
        let mut policy = PathPolicy::pinned(base, "", "browser path");
        policy.allow_outside = true;
        policy
    }
}

/// An absolute path pinned to the project, or `""` when it is not acceptable.
///
/// A value that climbs out of the project with `..`, or a path inside the
/// project whose symlink target lives outside it, is refused. Windows-shaped
/// input (backslashes, a drive letter) is understood on every platform: a drive
/// path is absolute, and therefore refused unless the policy allows an outside
/// path.
pub fn resolve_path(raw: Option<&str>, policy: &PathPolicy) -> String {
    let mut text = raw.map(|value| value.trim().to_string()).unwrap_or_default();
    if text.is_empty() {
        text = policy.default.trim().to_string();
    }
    if text.is_empty() {
        return String::new();
    }
    let root = strip_trailing_slash(&separators_to_slash(&policy.base.to_string_lossy()));
    let expanded = expand_user(&text);
    let shaped = separators_to_slash(&expanded);
    let absolute = is_absolute_shaped(&shaped);
    let candidate = if absolute {
        normalize_slash(&shaped)
    } else {
        normalize_slash(&format!("{}/{}", root, shaped))
    };
    let explicit_outside = policy.allow_outside && absolute;
    // Containment is asked twice: over the text, and again once both sides are resolved.
    //
    // The second question is what makes an already-resolved absolute value usable. A project
    // under a symlinked directory has two names: on macOS `/var/…` is `/private/var/…` once
    // resolved, so a caller that canonicalised its path — the settings check in
    // `crate::checks`, which resolves the model's symlinks before it decides anything — handed
    // this function a path that is inside the project and does not *start* with the project's
    // lexical text. `Config::save` then stored `""` for a model that had just been verified to
    // be inside `models/` (measured: `tests/settings_safety.rs`, one test red, the settings
    // file holding `"model_local": ""`). The decision belongs here, where containment is
    // decided, not in the caller's spelling of the path.
    let inside = prefix_inside(&root, &candidate) || inside_real(&root, &candidate);
    if !inside {
        if !explicit_outside {
            warn(&format!(
                "{} rejected: '{}' climbs out of the project",
                policy.label, text
            ));
            return String::new();
        }
        warn(&format!(
            "{} outside the project (explicitly configured): {}",
            policy.label, candidate
        ));
    } else if !inside_real(&root, &candidate) {
        // Inside the project lexically, but a symlink points somewhere else.
        if !policy.allow_outside {
            warn(&format!(
                "{} rejected: {} is a symlink to a target outside the project",
                policy.label, text
            ));
            return String::new();
        }
        warn(&format!(
            "{} follows a symlink out of the project: {}",
            policy.label, candidate
        ));
    }
    if policy.want_exec {
        let inside = inside_real(&root, &candidate);
        if !is_executable(&candidate) || (!inside && !policy.allow_outside) {
            warn(&format!(
                "{} rejected: '{}' is not an executable file inside the project",
                policy.label, text
            ));
            return String::new();
        }
        if !inside {
            warn(&format!(
                "{} executable outside the project (explicitly configured): {}",
                policy.label, candidate
            ));
        }
    } else if policy.must_exist && !path_is_file(&candidate) {
        return String::new();
    }
    candidate
}

/// A path that is a regular, executable file.
pub fn resolve_binary(raw: Option<&str>, policy: &PathPolicy) -> String {
    let mut policy = policy.clone();
    policy.want_exec = true;
    resolve_path(raw, &policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::*;
    use crate::warn::warnings;


#[test]
fn a_relative_path_resolves_under_the_project() {
    let dir = TempDir::new("resolve-rel");
    let policy = PathPolicy::model(&dir.path, "");
    let root = crate::paths::show(&dir.path);
    assert_eq!(
        resolve_path(Some("models/thing.gguf"), &policy),
        format!("{}/models/thing.gguf", root)
    );
    assert_eq!(
        resolve_path(Some("~/thing.gguf"), &policy),
        String::new(),
        "a home directory is not inside the project"
    );
}


#[test]
fn a_path_that_climbs_out_of_the_project_is_refused() {
    let dir = TempDir::new("resolve-climb");
    let policy = PathPolicy::model(&dir.path, "");
    let root = crate::paths::show(&dir.path);
    assert_eq!(resolve_path(Some("../../etc/passwd"), &policy), "");
    assert_eq!(resolve_path(Some("/etc/passwd"), &policy), "");
    assert_eq!(resolve_path(Some(""), &policy), "");
    assert_eq!(resolve_path(None, &policy), "");
    // `..` that stays inside is fine.
    assert_eq!(
        resolve_path(Some("models/../models/x.gguf"), &policy),
        format!("{}/models/x.gguf", root)
    );
}

#[cfg(unix)]

#[test]
fn a_symlink_pointing_out_of_the_project_is_refused() {
    let dir = TempDir::new("resolve-link");
    let outside = TempDir::new("resolve-link-outside");
    outside.write("model.gguf", "x");
    std::os::unix::fs::symlink(outside.path.join("model.gguf"), dir.path.join("model.gguf"))
        .expect("symlink");
    let policy = PathPolicy::model(&dir.path, "");
    assert_eq!(resolve_path(Some("model.gguf"), &policy), "");
    // The same value with `allow_outside` names the escape instead of refusing.
    let mut loose = policy.clone();
    loose.allow_outside = true;
    assert!(!resolve_path(Some("model.gguf"), &loose).is_empty());
}


#[test]
fn a_configured_model_path_that_escapes_is_emptied_not_honoured() {
    let dir = TempDir::new("model-escape");
    dir.write("data/config.json", r#"{"model_local": "../../../etc/passwd"}"#);
    let config = load_env_free(&dir).expect("load");
    assert_eq!(config.model_local, "");
    assert!(
        config.validate().is_empty(),
        "a refused path is a hardening note, not a problem"
    );
    assert!(warnings()
        .iter()
        .any(|note| note.contains("model path rejected")));
}


#[cfg(unix)]
#[test]
fn a_path_that_is_already_resolved_is_still_inside_the_project() {
    // Measured through `Config::save`: a canonical model path was refused as a climb-out,
    // because the project's own name and the file's resolved name differ wherever the root
    // sits under a symlink (macOS `/var` → `/private/var`). The setting then became `""`.
    let dir = TempDir::new("resolve-canonical");
    std::fs::create_dir_all(dir.path.join("models")).expect("models");
    let model = dir.path.join("models/x.gguf");
    std::fs::write(&model, b"GGUF").expect("model");
    let canonical_root = std::fs::canonicalize(&dir.path).expect("canonical root");
    let canonical_model = std::fs::canonicalize(&model).expect("canonical model");
    let policy = PathPolicy::model(&dir.path, "");
    let resolved = resolve_path(Some(&canonical_model.to_string_lossy()), &policy);
    assert_eq!(
        resolved,
        crate::paths::show(&canonical_model),
        "an absolute path inside the project was read as a climb-out"
    );
    assert!(crate::paths::inside_real(
        &crate::paths::show(&dir.path),
        &crate::paths::show(&canonical_root)
    ));
    // A real escape is still refused after the second question.
    assert_eq!(resolve_path(Some("/etc/passwd"), &policy), "");
}

#[test]
fn windows_shaped_input_still_produces_a_sane_path() {
    let dir = TempDir::new("resolve-windows");
    let policy = PathPolicy::model(&dir.path, "");
    let root = crate::paths::show(&dir.path);

    // Backslashes are separators, and the result stays inside the project.
    assert_eq!(
        resolve_path(Some("models\\qwen.gguf"), &policy),
        format!("{}/models/qwen.gguf", root)
    );
    assert_eq!(
        resolve_path(Some(".\\models\\sub\\qwen.gguf"), &policy),
        format!("{}/models/sub/qwen.gguf", root)
    );
    // Climbing out with backslashes is refused just like with slashes.
    assert_eq!(
        resolve_path(Some("..\\..\\Windows\\System32\\drivers\\x.gguf"), &policy),
        ""
    );
    // A drive letter is absolute on every platform, so it is outside.
    assert_eq!(resolve_path(Some("C:\\Users\\ci\\model.gguf"), &policy), "");
    assert_eq!(resolve_path(Some("D:/data/model.gguf"), &policy), "");
    // A UNC path is absolute too.
    assert_eq!(resolve_path(Some("\\\\server\\share\\model.gguf"), &policy), "");
    // A browser may be given that drive path explicitly.
    let browser = PathPolicy::browser(&dir.path);
    assert_eq!(
        resolve_path(Some("C:\\Program Files\\Chrome\\chrome.exe"), &browser),
        "C:/Program Files/Chrome/chrome.exe"
    );
    // And a drive path with `..` inside it is still lexically collapsed.
    assert_eq!(
        resolve_path(Some("C:\\Program Files\\..\\Chrome\\chrome.exe"), &browser),
        "C:/Chrome/chrome.exe"
    );
}

}
