//! One ATLAS per data directory, and what to say about the last one.
//!
//! Two instances on `data/` means two writers on one SQLite file and two model
//! servers fighting for one port. The guard is an **OS advisory lock**
//! ([`std::fs::File::try_lock`]): the lock is released by the kernel when the
//! holder dies, so "is the previous instance still alive?" is not guessed from a
//! pid — it is answered by the lock itself.
//!
//! That makes *stale lock detection* exact: if the record file exists but the lock
//! is free, the instance that wrote it is gone, and the owner it names is reported
//! before it is replaced. If the lock is held, the record is not read as history —
//! the answer is "another ATLAS is running", with the pid and start time to
//! recognise it. **Nothing in the record decides anything**: a file that *looks*
//! stale (a dead pid, last year's timestamp) is not a stale lock, and a file that
//! looks live is not a live one. The kernel's answer is the only answer.
//!
//! ## The two locks, and why there are two
//!
//! A lock on `data/atlas.lock` is a lock on an **inode**, not on a name. Delete the
//! name and a second `atlas start` creates a new file, locks *that* inode and runs
//! beside the first one — which is the exact failure this module exists to prevent
//! (`rm data/atlas.lock`, a cleanup script, a sync client that replaces the file).
//! So the **data directory itself** is locked first: a directory inode cannot be
//! swapped for a fresh one the way an empty file can, and both instances derive the
//! same inode from the same `--root`. The file lock is kept as well — it is what
//! Windows can lock (a directory cannot be opened as a file handle there) and it
//! says which file is the record.
//!
//! ## The record file is opened as a file, not as a name
//!
//! `data/` is `0700` and ours, but the *name* `data/atlas.lock` is still not
//! trusted: a symlink planted there would make this module open, truncate and
//! overwrite whatever it points at (a target of the planter's choosing), a device
//! would be read forever, and a giant regular file would be read into memory
//! before the lock is even asked for. So the record is opened with `O_NOFOLLOW`,
//! must be a regular file, is created `0600` from the first byte, and its contents
//! are read through a cap. Every one of those is a refusal with a sentence, because
//! a lock that cannot be taken safely is not a lock that may be taken anyway.
//!
//! The file is deliberately not deleted on exit: the owner of the last run is the
//! first thing the next `atlas start` can report, and deleting a lock file while
//! holding it is exactly the race the lock exists to prevent.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The record file inside `data/`.
pub const LOCK_NAME: &str = "atlas.lock";

/// How much of the record file is read. Three short lines; anything longer was not
/// written by this module, and reading it whole is how a 4 GB file (or `/dev/zero`
/// through a planted symlink) becomes a boot that never ends.
pub const MAX_RECORD_BYTES: u64 = 8_192;

/// The longest one record field may be. A path can be long; a megabyte is not a
/// path, and the value is printed back at a user.
pub const MAX_FIELD: usize = 4_096;

/// Who held the lock, as the file records it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Owner {
    pub pid: u32,
    /// When that process started (`2026-11-21T13:07:04Z`).
    pub started: String,
    /// The root it was started for, so a lock in the wrong directory is visible.
    pub root: String,
}

impl Owner {
    /// The sentence a user reads about this owner.
    pub fn sentence(&self) -> String {
        let mut text = format!("pid {}", self.pid);
        if !self.started.is_empty() {
            text.push_str(&format!(", started {}", self.started));
        }
        if !self.root.is_empty() {
            text.push_str(&format!(", for {}", self.root));
        }
        text
    }
}

/// One record field, escaped so a value cannot become a second line.
///
/// The record is `key=value` lines, and the values are a path (`--root`) and a
/// timestamp — both chosen by whoever started this process. A root containing a
/// newline used to be written verbatim, so `--root '/tmp/x<newline>pid=1'` produced
/// a file that claimed pid 1 when it was read back: the record could be forged by
/// the very thing it records. `%` is escaped first so the encoding round-trips, and
/// only the characters that would break a line are touched, because the value is a
/// path a user has to be able to read.
pub fn escape_record(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch == '%' {
            out.push_str("%25");
        } else if ch.is_control() {
            // ESC included: the record is printed, and an escape sequence on a
            // terminal is a second line ATLAS never wrote (see `report`).
            out.push_str(&format!("%{:02X}", ch as u32 & 0xFF));
        } else {
            out.push(ch);
        }
    }
    out
}

/// [`escape_record`] read back. A `%` that does not start a valid pair stays a
/// literal `%`, so a hand-edited record still parses.
pub fn unescape_record(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%' && index + 2 < bytes.len() {
            let pair = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(value) = pair.and_then(|pair| u8::from_str_radix(pair, 16).ok()) {
                out.push(value as char);
                index += 3;
                continue;
            }
        }
        // Not an escape: take one whole UTF-8 character, not one byte, so a
        // non-ASCII path survives.
        let rest = &value[index..];
        match rest.chars().next() {
            Some(ch) => {
                out.push(ch);
                index += ch.len_utf8();
            }
            None => break,
        }
    }
    out
}

/// The file's contents, as written by [`lock_file_text`]. Anything unreadable is
/// `None` rather than an error: a truncated lock file must not stop the app.
pub fn parse_owner(text: &str) -> Option<Owner> {
    let mut owner = Owner::default();
    let mut seen = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some(pair) => pair,
            None => continue,
        };
        // A field that cannot be printed in full is cut, not carried around: the
        // value comes from a file, and a file is not a place to trust for size.
        let value: String = unescape_record(value.trim()).chars().take(MAX_FIELD).collect();
        match key {
            "pid" => {
                if let Ok(pid) = value.trim().parse::<u32>() {
                    owner.pid = pid;
                    seen = true;
                }
            }
            "started" => owner.started = value,
            "root" => owner.root = value,
            _ => {}
        }
    }
    if seen {
        Some(owner)
    } else {
        None
    }
}

/// The file's contents for one owner.
pub fn lock_file_text(owner: &Owner) -> String {
    format!(
        "pid={}\nstarted={}\nroot={}\n",
        owner.pid,
        escape_record(&owner.started),
        escape_record(&owner.root)
    )
}

/// Why [`acquire`] returned nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockError {
    /// Another process holds the lock.
    Held { owner: Option<Owner>, path: PathBuf },
    /// The file or the directory could not be used at all.
    Io { path: PathBuf, detail: String },
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Held { owner, path } => {
                let whose = match owner {
                    Some(owner) => owner.sentence(),
                    None => "an instance that did not write its name".to_string(),
                };
                write!(
                    f,
                    "another ATLAS is already running on this data directory ({}; lock: {}). \
                     Nothing was changed and nothing was started: stop that instance first, or \
                     start ATLAS with --root pointing at a different directory.",
                    whose,
                    path.display()
                )
            }
            LockError::Io { path, detail } => write!(
                f,
                "the lock file {} cannot be used: {}. Check that {} exists and is writable by \
                 you, then start ATLAS again",
                path.display(),
                detail,
                path.parent()
                    .map(|dir| dir.display().to_string())
                    .unwrap_or_else(|| path.display().to_string())
            ),
        }
    }
}

/// A held lock. Dropping it releases the locks; the record file stays for the next
/// run.
pub struct Held {
    /// The record file, locked where the platform allows it.
    file: File,
    /// The data directory itself, locked so the record file cannot be swapped for
    /// another inode under a running instance. `None` on Windows, which cannot open
    /// a directory as a file handle.
    directory: Option<File>,
    path: PathBuf,
    /// The owner the record named before this run took it, when there was one. That
    /// is the *stale* owner: the file was there and nobody held the lock.
    previous: Option<Owner>,
}

impl Held {
    /// The path of the record file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The owner a dead instance left behind, if any.
    pub fn previous(&self) -> Option<&Owner> {
        self.previous.as_ref()
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        // Both files close on the next lines and the kernel releases both locks with
        // them; the explicit unlocks just make the release visible in this module.
        let _ = self.file.unlock();
        if let Some(directory) = &self.directory {
            let _ = directory.unlock();
        }
    }
}

/// Take the lock for one data directory, reporting a stale one instead of failing.
pub fn acquire(data_dir: &Path, owner: &Owner) -> Result<Held, LockError> {
    atlas_config::ensure_dir(data_dir);
    let path = data_dir.join(LOCK_NAME);

    // 1. the directory, which no name swap can move.
    let directory = match lock_directory(data_dir) {
        Ok(directory) => directory,
        Err(LockError::Held { .. }) => {
            return Err(LockError::Held {
                owner: read_record(&path),
                path,
            })
        }
        Err(error) => return Err(error),
    };

    // 2. the record file: opened as *this* file (no symlink), and only a regular
    //    one, so the truncate-and-write below cannot be pointed somewhere else.
    let mut file = open_record(&path)?;
    // Bounded, because a record is three short lines and a hostile or corrupt file
    // must not be read into memory before the lock is even asked for.
    let mut before = String::new();
    let _ = (&file).take(MAX_RECORD_BYTES).read_to_string(&mut before);
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            return Err(LockError::Held {
                owner: parse_owner(&before),
                path,
            })
        }
        Err(TryLockError::Error(error)) => {
            return Err(LockError::Io {
                path,
                detail: error.to_string(),
            })
        }
    }
    let previous = parse_owner(&before);
    let _ = file.set_len(0);
    let _ = file.seek(SeekFrom::Start(0));
    let _ = file.write_all(lock_file_text(owner).as_bytes());
    let _ = file.flush();
    Ok(Held {
        file,
        directory,
        path,
        previous,
    })
}

/// Lock the data directory itself. `Ok(None)` on Windows, where a directory is not
/// a file handle and the record-file lock is the only one available.
fn lock_directory(data_dir: &Path) -> Result<Option<File>, LockError> {
    if atlas_platform::host::is_windows() {
        return Ok(None);
    }
    let path = data_dir.to_path_buf();
    let directory = File::open(&path).map_err(|error| LockError::Io {
        path: path.clone(),
        detail: error.to_string(),
    })?;
    match directory.try_lock() {
        Ok(()) => Ok(Some(directory)),
        // Held: the caller words it, because only the caller knows the record path.
        Err(TryLockError::WouldBlock) => Err(LockError::Held {
            owner: None,
            path,
        }),
        Err(TryLockError::Error(error)) => Err(LockError::Io {
            path,
            detail: error.to_string(),
        }),
    }
}

/// Open the record file this exact file: no symlink, no device, no directory, and
/// `0600` from the first byte.
///
/// `O_NOFOLLOW` is `atlas_platform`'s constant, so the workspace has one answer to
/// "the file at this name, not whatever it points at" (the same one the static-file
/// handler uses). The check before it only exists to say *why* in the sentence:
/// `ELOOP` from `open` would otherwise reach the user as "Too many levels of
/// symbolic links".
fn open_record(path: &Path) -> Result<File, LockError> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            return Err(LockError::Io {
                path: path.to_path_buf(),
                detail: "it is a symbolic link. A link here would point the lock — and the \
                         record written into it — at another file, so it is refused; remove it \
                         and start ATLAS again"
                    .to_string(),
            });
        }
    }
    let mut options: OpenOptions = atlas_platform::private::create_options();
    options.read(true);
    // The same refusal, this time as a flag, so the race between the check above
    // and the open cannot be used.
    let flag = atlas_platform::private::no_follow_flag();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(flag);
    }
    let file = options.open(path).map_err(|error| LockError::Io {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })?;
    // `metadata` of the *open file*, not of the name: whatever the path is now, the
    // truncate and the write below go to the file this handle holds.
    let regular = file.metadata().map(|meta| meta.is_file()).unwrap_or(false);
    if !regular {
        return Err(LockError::Io {
            path: path.to_path_buf(),
            detail: "it is not a regular file. A device, a pipe or a directory here would make \
                     the lock read forever or write somewhere it does not belong, so it is \
                     refused; remove it and start ATLAS again"
                .to_string(),
        });
    }
    Ok(file)
}

/// The record at `path`, best effort: `None` when it cannot be read safely.
///
/// Used for the *message* about a held lock, so a record that cannot be read is
/// "an instance that did not write its name" and never an error, and never a
/// reason to open something that is not the file this module writes.
fn read_record(path: &Path) -> Option<Owner> {
    let file = open_record(path).ok()?;
    let mut text = String::new();
    (&file).take(MAX_RECORD_BYTES).read_to_string(&mut text).ok()?;
    parse_owner(&text)
}

/// The owner record for this process.
pub fn this_process(root: &Path) -> Owner {
    Owner {
        pid: std::process::id(),
        started: atlas_store::now_iso(),
        root: root.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "atlas-lock-{}-{}-{}",
            tag,
            std::process::id(),
            atlas_store::now_iso().replace([':', '.'], "-")
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn owner(pid: u32, root: &Path) -> Owner {
        Owner {
            pid,
            started: "2026-11-21T13:07:04Z".to_string(),
            root: root.display().to_string(),
        }
    }

    #[test]
    fn an_owner_round_trips_through_the_file_text() {
        let owner = Owner {
            pid: 4242,
            started: "2026-11-21T13:07:04Z".to_string(),
            root: "/tmp/atlas root".to_string(),
        };
        let parsed = parse_owner(&lock_file_text(&owner)).unwrap();
        assert_eq!(parsed, owner);
        assert!(parsed.sentence().contains("pid 4242"));
        assert!(parsed.sentence().contains("2026-11-21"));
    }

    #[test]
    fn a_truncated_or_alien_file_is_no_owner_and_not_an_error() {
        assert_eq!(parse_owner(""), None);
        assert_eq!(parse_owner("garbage\nnothing=here"), None);
        assert_eq!(parse_owner("pid=notanumber"), None);
        // A partial record keeps what did parse.
        assert_eq!(parse_owner("pid=7\nstarted=").unwrap().pid, 7);
    }

    #[test]
    fn a_root_with_a_newline_cannot_forge_the_record() {
        // The attack: `--root` is a value the starter chooses, and it used to be
        // written into the record verbatim, so a root carrying a newline added
        // fields of its own. The next instance then reported (and printed) an owner
        // that never existed — pid 1, started 1999 — instead of the real one.
        let hostile = Owner {
            pid: 4242,
            started: "2026-11-21T13:07:04Z".to_string(),
            root: "/tmp/atlas\npid=1\nstarted=1999-01-01T00:00:00Z".to_string(),
        };
        let text = lock_file_text(&hostile);
        assert_eq!(
            text.lines().count(),
            3,
            "the record is three lines whatever the root holds: {:?}",
            text
        );
        let parsed = parse_owner(&text).expect("the record still parses");
        assert_eq!(parsed.pid, 4242, "the real pid, not a forged one: {:?}", parsed);
        assert_eq!(parsed.started, hostile.started);
        // And the value a user has to read is still the path they named.
        assert_eq!(parsed, hostile, "the record round-trips: {:?}", parsed);
    }

    #[test]
    fn a_record_value_cannot_carry_a_terminal_escape_sequence() {
        // Same rule one layer down: the record is printed, so a control character
        // in it is a line or an escape sequence ATLAS never wrote.
        let hostile = Owner {
            pid: 3,
            started: "2026-11-21T13:07:04Z".to_string(),
            root: "x\u{1b}[2Ky\nz".to_string(),
        };
        let text = lock_file_text(&hostile);
        assert!(
            !text.chars().any(|ch| ch.is_control() && ch != '\n'),
            "{:?}",
            text
        );
        assert_eq!(parse_owner(&text).unwrap(), hostile);
    }

    #[test]
    fn an_unusable_lock_file_says_what_to_check() {
        // Every message the launcher prints has to say what happened *and* what to
        // do: a bare `Permission denied` leaves a user with nothing to try.
        let error = LockError::Io {
            path: PathBuf::from("/x/data/atlas.lock"),
            detail: "Permission denied (os error 13)".to_string(),
        };
        let text = error.to_string();
        assert!(text.contains("cannot be used"), "{}", text);
        assert!(text.contains("Permission denied"), "{}", text);
        assert!(text.contains("is writable by you"), "{}", text);
        assert!(text.contains("/x/data"), "{}", text);
        // And it is a refusal, not a warning: `atlas start` exits 1 on it.
        assert!(!text.contains("warning"), "{}", text);
    }

    #[test]
    fn a_second_instance_is_refused_and_names_the_first() {
        let dir = temp_dir("held");
        let first = acquire(&dir, &owner(1111, &dir)).expect("the first instance takes the lock");
        let second = acquire(&dir, &this_process(&dir));
        match second {
            Err(LockError::Held { owner: seen, .. }) => {
                let seen = seen.expect("the first instance wrote its name");
                assert_eq!(seen.pid, 1111);
            }
            other => panic!(
                "expected a refused lock, got {:?}",
                other.map(|held| held.path().to_path_buf())
            ),
        }
        // The refusal says what to do.
        let text = match acquire(&dir, &this_process(&dir)) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("still held"),
        };
        assert!(text.contains("another ATLAS is already running"), "{}", text);
        assert!(text.contains("pid 1111"), "{}", text);
        drop(first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_record_that_looks_stale_is_not_a_stale_lock() {
        // The trick this module must not fall for: a record naming a pid that no
        // longer exists and a start time from last year, sitting in a lock that is
        // very much held. "Stale" is the kernel's answer, never the file's.
        let dir = temp_dir("looks-stale");
        let first = acquire(&dir, &owner(999_999, &dir)).expect("the first instance");
        let text = std::fs::read_to_string(dir.join(LOCK_NAME)).unwrap();
        assert!(text.contains("pid=999999"), "{}", text);
        let refused = acquire(&dir, &this_process(&dir));
        match refused {
            Err(LockError::Held { owner: seen, .. }) => {
                assert_eq!(seen.expect("the held owner is reported").pid, 999_999);
            }
            other => panic!("a held lock was taken over: {:?}", other.is_ok()),
        }
        // And nothing of the held record was overwritten by the refused attempt.
        assert_eq!(
            std::fs::read_to_string(dir.join(LOCK_NAME)).unwrap(),
            text,
            "a refused attempt must not touch the record"
        );
        drop(first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_lock_left_by_a_dead_instance_is_reported_and_taken_over() {
        let dir = temp_dir("stale");
        {
            // A "previous run" that exited without cleaning up: it wrote its name
            // and released the lock when it died.
            let ghost = Owner {
                pid: 999_999,
                started: "2026-11-20T09:00:00Z".to_string(),
                root: "/tmp/atlas".to_string(),
            };
            let held = acquire(&dir, &ghost).expect("the ghost takes the lock");
            drop(held);
        }
        let held = acquire(&dir, &this_process(&dir)).expect("the stale lock is taken over");
        let previous = held.previous().expect("the dead owner is still named");
        assert_eq!(previous.pid, 999_999);
        assert_eq!(previous.started, "2026-11-20T09:00:00Z");
        let text = std::fs::read_to_string(held.path()).unwrap();
        assert!(text.contains(&format!("pid={}", std::process::id())), "{}", text);
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fresh_directory_has_no_previous_owner_and_creates_the_data_dir() {
        let dir = temp_dir("fresh").join("nested");
        let held = acquire(&dir, &this_process(&dir)).expect("the lock is taken");
        assert!(held.previous().is_none());
        assert!(dir.join(LOCK_NAME).is_file());
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_record_file_is_refused_and_its_target_untouched() {
        // The attack: plant `data/atlas.lock` as a symlink. The lock is taken
        // through the link and the *target* is truncated and overwritten with the
        // record, so starting ATLAS destroyed a file of the planter's choosing —
        // `data/atlas.db`, a key file, anything this user can write.
        let dir = temp_dir("symlink");
        let victim = dir.join("victim.dat");
        let important = b"IMPORTANT DATA THAT MUST SURVIVE\n";
        std::fs::write(&victim, important).unwrap();
        std::os::unix::fs::symlink(&victim, dir.join(LOCK_NAME)).unwrap();

        let refused = acquire(&dir, &this_process(&dir));
        let text = match refused {
            Err(error) => error.to_string(),
            Ok(_) => panic!("a symlinked lock file was followed"),
        };
        assert!(text.contains("symbolic link"), "{}", text);
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            important,
            "the link's target was written through"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_record_file_that_is_not_a_regular_file_is_refused() {
        // `/dev/zero` through a link was the unbounded read; a directory is the
        // same refusal without needing a device this machine may not have.
        let dir = temp_dir("notregular");
        std::fs::create_dir_all(dir.join(LOCK_NAME)).unwrap();
        match acquire(&dir, &this_process(&dir)) {
            // The refusal is a sentence either way: the open refuses a directory
            // (`EISDIR`), and the `is_file` check refuses a device or a pipe that the
            // open would accept. What must not happen is a lock taken on it.
            Err(error @ LockError::Io { .. }) => {
                let text = error.to_string();
                assert!(text.contains("cannot be used"), "{}", text);
                assert!(text.contains(LOCK_NAME), "{}", text);
            }
            Err(other) => panic!("a directory at the lock path was judged held: {}", other),
            Ok(_) => panic!("a directory at the lock path was used as a record file"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_pipe_at_the_lock_path_is_refused_instead_of_blocking_the_boot() {
        // The one case the byte cap cannot save: a pipe opened read-write is a
        // caller with nothing to read, so `read_to_string` waits for a writer that
        // never comes. `atlas start` would sit there before it had asked for the
        // lock at all — a boot that never finishes rather than one that fails. The
        // `is_file` check is what refuses it.
        let mkfifo = std::path::Path::new("/usr/bin/mkfifo");
        if !mkfifo.is_file() {
            return;
        }
        let dir = temp_dir("fifo");
        let made = std::process::Command::new(mkfifo)
            .arg(dir.join(LOCK_NAME))
            .env_clear()
            .status();
        if !made.map(|status| status.success()).unwrap_or(false) {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let root = dir.clone();
        std::thread::spawn(move || {
            let answer = acquire(&root, &this_process(&root));
            let _ = sender.send(answer.map(|_| ()).map_err(|error| error.to_string()));
        });
        match receiver.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Err(text)) => {
                assert!(text.contains("not a regular file"), "{}", text);
                assert!(text.contains("cannot be used"), "{}", text);
            }
            Ok(Ok(())) => panic!("a pipe at the lock path was used as a record file"),
            Err(_) => panic!("the lock read blocked on a pipe: the boot would never finish"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_giant_record_file_is_read_through_a_cap() {
        // A record whose `pid=` line sits past the cap is not read as history: the
        // read is bounded, so a 4 GB file (or `/dev/zero`) cannot become a boot that
        // never ends and a memory the app does not have.
        let dir = temp_dir("cap");
        let mut padding = vec![b'A'; (MAX_RECORD_BYTES as usize) + 1_024];
        padding.extend_from_slice(b"\npid=999\n");
        std::fs::write(dir.join(LOCK_NAME), &padding).unwrap();
        let held = acquire(&dir, &this_process(&dir)).expect("the lock is still taken");
        assert!(
            held.previous().is_none(),
            "a field past {:?} bytes was parsed: {:?}",
            MAX_RECORD_BYTES,
            held.previous()
        );
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_replaced_record_file_cannot_mint_a_second_instance() {
        // The attack: while the first instance runs, remove the record file and put
        // a new one there (a cleanup script, a sync client, `rm`). A lock on the file
        // was a lock on an inode, so the second instance locked the *new* inode and
        // ran beside the first — two writers on one SQLite archive. The directory
        // lock is on the inode that cannot be swapped.
        let dir = temp_dir("replaced");
        let first = acquire(&dir, &owner(1111, &dir)).expect("the first instance");
        std::fs::remove_file(dir.join(LOCK_NAME)).expect("the record can be removed");
        std::fs::write(dir.join(LOCK_NAME), b"pid=2222\n").expect("and replaced");

        let second = acquire(&dir, &owner(2222, &dir));
        match second {
            Err(LockError::Held { .. }) => {}
            Err(other) => panic!("expected the lock to be held, got {}", other),
            Ok(_) => panic!("a replaced record file let a second instance in"),
        }
        drop(first);
        // With the first instance gone, the same directory is available again.
        let third = acquire(&dir, &owner(3333, &dir));
        assert!(third.is_ok(), "the lock stayed held after its owner dropped it");
        drop(third);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
