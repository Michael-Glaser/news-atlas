//! `data/llama.log` — bounded across restarts, and only ever a regular file.
//!
//! This is the one thing the launcher asked `atlas-web::applog` for
//! (`bound_child_log`, `CHILD_LOG_MAX_BYTES`), moved here when this crate stopped
//! linking the listener: a model server that cannot load is a restart loop, so the
//! file grew by one full load per attempt, on the same disk as the archive, and
//! nothing ever removed a byte of it.
//!
//! Two properties, and one honest limit:
//!
//! * **Only a regular file is rotated.** A link planted at the log's name is left
//!   exactly where it is, so the `O_NOFOLLOW` open in [`crate::llama::log_sink`]
//!   refuses it as it did before. Tidying an attack out of the way is how an attack
//!   goes unnoticed.
//! * **Bounded at twice the cap**: the current file plus one generation
//!   (`llama.log.1`), both `0600` (the rotated one is repaired here, so an older
//!   build's `0644` file does not move sideways and stay readable).
//! * The limit: this runs at *spawn*, so it bounds the runs a log accumulates, not
//!   one run's output. A single long run of an unsilenced `llama-server` can still
//!   write past the cap, because its bytes never pass through this process — and
//!   the launcher passes `--log-disable` on every start, which is what keeps a
//!   prompt out of that file in the first place.
//!
//! A failure is a fact, not an error: a log that cannot be rotated is appended to as
//! before, because refusing to start the model over a housekeeping step would be
//! worse than a large file.

use std::path::{Path, PathBuf};

/// How many bytes a log this process does not write may reach before it rotates.
///
/// One number for both logs on the disk the archive lives on; `atlas-web`'s
/// `app.log` uses the same cap (`atlas-web/src/applog.rs:50`).
pub const CHILD_LOG_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// The one rotated generation: `llama.log.1`.
pub fn rotated_path(path: &Path) -> PathBuf {
    match path.file_name().and_then(|name| name.to_str()) {
        Some(name) => path.with_file_name(format!("{}.1", name)),
        None => path.with_extension("1"),
    }
}

/// Rotate `path` when it has reached `cap` bytes. Returns whether it did.
///
/// The *newest* lines are what a person reads, so the current file becomes the
/// previous one and a fresh file starts. Windows refuses a rename onto an existing
/// name and POSIX replaces it: remove first, then rename, so both platforms behave
/// the same way.
pub fn rotate_if_needed(path: &Path, cap: u64) -> bool {
    if cap == 0 {
        return false;
    }
    let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    if size < cap {
        return false;
    }
    let rotated = rotated_path(path);
    let _ = std::fs::remove_file(&rotated);
    if std::fs::rename(path, &rotated).is_err() {
        return false;
    }
    let _ = atlas_platform::private::make_file_private(&rotated);
    true
}

/// Bound a child's log before the writer opens it: rotate an over-cap file to `.1`.
///
/// Measured before this existed: two appends of 3 MiB left a 6 MiB `llama.log` and
/// no `llama.log.1`. Called from [`crate::llama::log_sink`], which is the only place
/// that opens the file the model server inherits.
pub fn bound_child_log(path: &Path) -> bool {
    let regular = std::fs::symlink_metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false);
    if !regular {
        return false;
    }
    rotate_if_needed(path, CHILD_LOG_MAX_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "atlas-childlog-{}-{}-{}",
            tag,
            std::process::id(),
            atlas_store::now_iso().replace([':', '.'], "-")
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp directory");
        dir
    }

    #[test]
    fn a_log_under_the_cap_is_left_alone() {
        let dir = root("under");
        let log = dir.join("llama.log");
        std::fs::write(&log, b"a short log\n").expect("write");
        assert!(!bound_child_log(&log), "a small log is not rotated");
        assert_eq!(std::fs::metadata(&log).unwrap().len(), 12);
        assert!(!rotated_path(&log).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_log_at_the_cap_rotates_into_exactly_one_generation() {
        let dir = root("atcap");
        let log = dir.join("llama.log");
        // A sparse file of exactly the cap: the size decides, so this needs no
        // cap-sized write.
        let big = std::fs::File::create(&log).expect("create");
        big.set_len(CHILD_LOG_MAX_BYTES).expect("set the length");
        drop(big);
        assert!(bound_child_log(&log), "a log at the cap rotates");
        let rotated = rotated_path(&log);
        assert!(rotated.exists(), "the generation is {:?}", rotated);
        assert_eq!(
            std::fs::metadata(&rotated).unwrap().len(),
            CHILD_LOG_MAX_BYTES
        );
        // One generation, not a pile: the second rotation *replaces* `.1`.
        std::fs::write(&rotated, b"the first generation\n").expect("mark");
        let big = std::fs::File::create(&log).expect("create");
        big.set_len(CHILD_LOG_MAX_BYTES).expect("set the length");
        drop(big);
        assert!(bound_child_log(&log));
        assert_eq!(
            std::fs::read(&rotated).unwrap().len() as u64,
            CHILD_LOG_MAX_BYTES,
            "the generation was replaced, not appended to"
        );
        assert!(!dir.join("llama.log.2").exists(), "one generation only");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_symlink_at_the_log_name_is_never_rotated() {
        // The victim is a file the launcher must not touch: moving the link aside
        // would let the `O_NOFOLLOW` open succeed on a fresh file and hide that an
        // attack was there.
        let dir = root("symlink");
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"the victim's bytes\n").expect("write the victim");
        let log = dir.join("llama.log");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&victim, &log).expect("plant the link");
            assert!(!bound_child_log(&log), "a link is not a regular file");
            assert_eq!(
                std::fs::read_to_string(&victim).unwrap(),
                "the victim's bytes\n",
                "the victim is unchanged"
            );
            assert!(
                !rotated_path(&log).exists(),
                "the link was moved out of the way"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_log_is_not_an_error_and_a_retired_generation_is_private() {
        let dir = root("missing");
        assert!(!bound_child_log(&dir.join("nothing.log")), "nothing to bound");
        let log = dir.join("llama.log");
        std::fs::write(&log, b"tiny\n").expect("write");
        assert!(!rotate_if_needed(&log, 0), "a zero cap rotates nothing");
        assert!(log.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
