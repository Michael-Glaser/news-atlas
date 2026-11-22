//! The install tool is exempt from the no-download audit, and nothing else is.
//!
//! `atlas-setup` has to fetch a 6.5 GB model file and run cmake once, on a machine
//! that has never seen this project. The app itself must never fetch anything. Those
//! two statements are only both true if the audit can tell the two apart, so this
//! test builds a tree that contains the *same download line* twice — once under
//! `rust/atlas-setup/`, once under `rust/atlas-web/` — and asks the audit to judge
//! it.
//!
//! Measured before the exemption was written down (by appending `curl -O` to
//! `atlas-setup/src/sha.rs` in the real tree): the walk scanned the crate like any
//! other and reported the line. So the walk was never skipping it — the crate was
//! simply not matching any rule, which is a false clean, not an exemption. This test
//! keeps the difference honest in both directions:
//!
//! * the file under `rust/atlas-setup/` produces no finding **and** is counted in
//!   `exempt`, so the verdict says a decision was made instead of hiding one;
//! * the same line under `rust/atlas-web/` still produces a finding, so the
//!   exemption cannot silently widen into "no crate fetches anything, probably".

use std::path::{Path, PathBuf};

use atlas_config::audit_no_downloads;

/// A scratch directory that does not exist yet, unique to this process.
fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|spent| spent.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "atlas-config-exempt-{}-{}-{}",
        tag,
        std::process::id(),
        nanos
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// Write `body` to `root/relative`, creating the directories on the way.
fn write(root: &Path, relative: &str, body: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parent directory");
    }
    std::fs::write(&path, body).expect("write file");
}

/// The line both halves of the test use: an unambiguous download, matched by the
/// `curl-download` rule, in a `.rs` file, outside any `#[cfg(test)]` block.
const DOWNLOAD_LINE: &str = "    let out = run(\"curl -O https://example.com/model.gguf\");\n";

#[test]
fn the_install_tool_is_exempt_and_the_app_is_not() {
    let root = scratch("both");

    write(
        &root,
        "rust/atlas-setup/src/model.rs",
        &format!("//! Fetches the pinned model file.\n\nfn fetch() {{\n{}}}\n", DOWNLOAD_LINE),
    );
    write(
        &root,
        "rust/atlas-web/src/fetch.rs",
        &format!("//! Somewhere in the app.\n\nfn fetch() {{\n{}}}\n", DOWNLOAD_LINE),
    );

    let audit = audit_no_downloads(&root);

    let in_setup = audit
        .findings
        .iter()
        .filter(|finding| finding.file.contains("atlas-setup"))
        .count();
    let in_web = audit
        .findings
        .iter()
        .filter(|finding| finding.file.contains("atlas-web"))
        .count();

    assert_eq!(
        in_setup, 0,
        "the install tool is exempt; findings were {:?}",
        audit.findings
    );
    assert_eq!(
        in_web, 1,
        "the same line in the app must still be a finding; findings were {:?}",
        audit.findings
    );
    assert_eq!(
        audit.exempt, 1,
        "the exempt file must be counted, not silently dropped"
    );
    assert!(!audit.ok, "one finding means the tree is not clean");
    assert!(
        audit.summary().contains("1 exempt"),
        "the verdict has to say an exemption was used: {}",
        audit.summary()
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_clean_tree_uses_no_exemption() {
    let root = scratch("clean");

    write(
        &root,
        "rust/atlas-web/src/main.rs",
        "fn main() {\n    println!(\"hello\");\n}\n",
    );

    let audit = audit_no_downloads(&root);

    assert!(audit.ok, "findings were {:?}", audit.findings);
    assert_eq!(audit.exempt, 0);
    assert_eq!(audit.scanned, 1);
    assert!(
        audit.summary().contains("0 exempt"),
        "the count is always reported, even when it is zero: {}",
        audit.summary()
    );

    let _ = std::fs::remove_dir_all(&root);
}
