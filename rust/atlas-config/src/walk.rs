//! The walk: which directories are entered, which files are read, in what order.
//!
//! A tree walk that a security claim rests on has to be readable on its own: the
//! rules are sorted so a finding list does not depend on the filesystem's mood,
//! symlinks are never followed (a loop must not become an endless walk), the
//! depth is capped, and every file that was read is recorded so "ok" can be
//! checked instead of trusted.
//!
//! Four decisions that used to be silent and are not any more:
//!
//! * **Every line of a file is read.** The walk stopped at line 6000
//!   (`text.lines().take(6000)`), so 6001 blank lines and then a `curl -O` was a
//!   file the audit reported as scanned and clean.
//! * **A directory is skipped by name only at the root**, and deeper only when its
//!   whole path is in [`AUDIT_SKIP_PATHS`]. Skipping `vendor` at any depth meant a
//!   `scripts/vendor/` was invisible; a directory name is one `mkdir` away from
//!   being whatever the attacker needs.
//! * **A file that could not be read is counted**, in `skipped` and in `unread`, so
//!   an unreadable `.sh` cannot pass as a clean one.
//! * **Test code inside a `.rs` file is not shipped code**: `#[cfg(test)]` blocks
//!   are blanked by `atlas_guard::source`, the same one answer the crate audits
//!   use, and `tests/` directories and `*_tests.rs` files are skipped whole. A
//!   fixture that says `pip install requests` is a fixture, and reporting the
//!   audit's own test data is how an audit teaches its readers to ignore it.
//!
//! It does not: decide whether a line downloads something ([`crate::download`]),
//! or turn what it collected into a verdict ([`crate::audit`]).

use std::path::{Path, PathBuf};

use crate::audit::{
    Finding, AUDIT_EXTENSIONS, AUDIT_GENERATED_DIRS, AUDIT_INSTALL_TOOL, AUDIT_LIST_MAX,
    AUDIT_SKIP_DIRS, AUDIT_SKIP_PATHS,
};
use crate::download::{audit_line_allowed, rule_hit};

/// Files directly in a directory that the walk refused to enter, so `skipped`
/// counts one skipped path per file, not per directory.
pub(crate) fn direct_file_count(dir: &Path) -> usize {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .filter(|entry| {
                std::fs::symlink_metadata(entry.path())
                    .map(|meta| !meta.is_dir() && !meta.file_type().is_symlink())
                    .unwrap_or(false)
            })
            .count(),
        Err(_) => 0,
    }
}

/// What one walk accumulates: one field for each number [`crate::Audit`]
/// publishes. Bundled so the recursion passes one mutable answer instead of
/// five.
#[derive(Default)]
pub(crate) struct Walk {
    pub(crate) findings: Vec<Finding>,
    pub(crate) scanned: usize,
    pub(crate) skipped: usize,
    /// Files exempted by a written-down decision rather than skipped: today only
    /// AUDIT_INSTALL_TOOL, which is not part of the app.
    pub(crate) exempt: usize,
    pub(crate) read: Vec<String>,
    /// Code files that were opened and could not be read as text. Counted in
    /// `skipped` too, and named, because a file the audit could not read is not a
    /// file the audit cleared.
    pub(crate) unread: Vec<String>,
}

pub(crate) fn descend(dir: &Path, rel: &str, walk: &mut Walk, depth: usize) {
    if depth > 24 {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        // Symlinks are not followed: a loop must not become an endless walk.
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            // By name at the root of the walk only; by name anywhere for a build
            // tool's output directory. See `audit::AUDIT_SKIP_DIRS`.
            let root_level = rel.is_empty();
            if (root_level && AUDIT_SKIP_DIRS.contains(&name.as_str()))
                || AUDIT_GENERATED_DIRS.contains(&name.as_str())
            {
                // Counted, not silently dropped: the verdict publishes how many
                // files it did not read, and a directory skipped by name is one
                // of the ways that number is earned.
                walk.skipped += direct_file_count(&path);
                continue;
            }
            dirs.push(path);
        } else if meta.is_file() {
            files.push(path);
        }
    }
    // Sorted, so a finding list does not depend on the filesystem's mood.
    dirs.sort();
    files.sort();
    for path in files {
        let name = match path.file_name().and_then(|name| name.to_str()) {
            Some(name) => name.to_ascii_lowercase(),
            None => continue,
        };
        if !AUDIT_EXTENSIONS.iter().any(|ext| name.ends_with(ext)) {
            continue;
        }
        let base = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let file_rel = if rel.is_empty() {
            base
        } else {
            format!("{}/{}", rel, base)
        };
        if AUDIT_SKIP_PATHS
            .iter()
            .any(|skip| file_rel == *skip || file_rel.starts_with(&format!("{}/", skip)))
        {
            walk.skipped += 1;
            continue;
        }
        // The install tool is the one place in this tree that fetches something, and
        // it is not the app: no crate depends on it (atlas-guard's
        // "the_install_tool_is_not_linked_into_the_app" proves that from the
        // manifests), "atlas start" never spawns it, and it acts only on a subcommand
        // a person typed. Exempt — and *counted*, not silently passed: the verdict
        // says how many files were left to that decision, so "0 findings" cannot be
        // read as "the whole tree".
        if file_rel.starts_with(AUDIT_INSTALL_TOOL) {
            walk.exempt += 1;
            continue;
        }
        // Rust test code is not shipped code: `tests/`, `tests.rs`, `*_tests.rs`.
        // The same rule the crate audits use, asked of the same function, so a
        // fixture line and a shipped line cannot be confused for each other. A
        // script has no such mark and is read wherever it is.
        if file_rel.ends_with(".rs") && atlas_guard::source::is_test_path(&file_rel) {
            walk.skipped += 1;
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => {
                // A code file the audit could not read is not a code file the
                // audit cleared. It is counted as skipped *and* named, so the
                // verdict cannot be read as "I looked at it and it was clean".
                walk.skipped += 1;
                if walk.unread.len() < AUDIT_LIST_MAX {
                    walk.unread.push(file_rel.clone());
                }
                continue;
            }
        };
        walk.scanned += 1;
        if walk.read.len() < AUDIT_LIST_MAX {
            walk.read.push(file_rel.clone());
        }
        let suffix = match path.extension().and_then(|ext| ext.to_str()) {
            Some(ext) => format!(".{}", ext.to_ascii_lowercase()),
            None => String::new(),
        };
        // For Rust, the shipped part of the file: `#[cfg(test)]` blocks are not
        // compiled into the app. A test that writes `pip install requests` into a
        // fixture is test data, and the crate audits blank exactly the same lines
        // with the same function.
        let shipped = if suffix == ".rs" {
            atlas_guard::source::code_of(&text).to_string()
        } else {
            text
        };
        // Every line: the walk used to stop at 6000, which is a place to hide one.
        for (number, line) in shipped.lines().enumerate() {
            if audit_line_allowed(line, &suffix) {
                continue;
            }
            if let Some(rule) = rule_hit(line) {
                walk.findings.push(Finding {
                    file: file_rel.clone(),
                    line: number + 1,
                    rule: rule.to_string(),
                });
            }
        }
    }
    for path in dirs {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let child_rel = if rel.is_empty() {
            name
        } else {
            format!("{}/{}", rel, name)
        };
        if AUDIT_SKIP_PATHS
            .iter()
            .any(|skip| child_rel == *skip || child_rel.starts_with(&format!("{}/", skip)))
        {
            // The count is the files *directly* in the skipped directory, which
            // is what the Python walk reported: it pruned the subtree without
            // descending into it.
            walk.skipped += direct_file_count(&path);
            continue;
        }
        descend(&path, &child_rel, walk, depth + 1);
    }
}
