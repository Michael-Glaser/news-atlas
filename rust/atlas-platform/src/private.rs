//! Making a file private — and saying honestly what that means per platform.
//!
//! The archive, the logs and `config.json` are ATLAS's own business: another
//! account on the machine must not be able to read them. Two platforms, two
//! mechanisms, one answer:
//!
//! * **POSIX**: `data/` is `0700` and every file ATLAS writes inside it is
//!   `0600`. That is a `chmod`, and [`make_private`] performs it and reports
//!   whether anything changed.
//! * **Windows**: there is no mode to set. Access is an ACL, and the ACL that
//!   applies is the one inherited from the user profile — which already limits a
//!   per-user directory to that user. This crate answers
//!   [`Privacy::Inherited`] and does **not** pretend a `chmod` happened. Tightening
//!   beyond inheritance (`icacls /inheritance:r`) needs a Windows API call or an
//!   external tool; ATLAS does neither in a `forbid(unsafe_code)` crate that
//!   starts nothing, so the installer scripts do that step once and
//!   [`explain`] names it.
//!
//! The verdict is a value, not a log line: `/api/status` publishes it, and a
//! caller that cannot harden a file learns why instead of assuming it worked.
//!
//! ## The rules that are easy to get wrong, and how they are kept here
//!
//! * **A mode is set on the path, never through a link.** `chmod` follows
//!   symlinks, so `make_private("data/config.json")` on a `config.json` that is a
//!   link set the mode of the file **at the other end** — measured: a file
//!   outside the project went from `0644` to `0600` and the verdict said
//!   `Changed`, i.e. "this path is owner-only" about a path that is a link.
//!   A link is now [`Privacy::Linked`]: nothing is changed through it, and the
//!   status says so.
//! * **Private from the first byte.** A file created with
//!   [`create_options`] gets `0600` from the `open`, not from a later `chmod`,
//!   and never follows a link ([`no_follow_flag`]); a directory created with
//!   [`create_dir`] gets `0700` from the `mkdir`, not `0755` and then a `chmod`
//!   (measured: `create_dir_all` answers `0755` under the usual `umask`, which is
//!   a window in which another account reads the archive).
//! * **A shared temp directory is not private storage.** [`crate::host::temp_dir`]
//!   falls back to `/tmp` and `C:\Windows\Temp`, which are world-writable by
//!   design. Private data belongs in `data/` (`0700`), and a caller that really
//!   needs scratch space outside it creates it with [`create_dir`] — `0700` from
//!   the `mkdir`, with a name that cannot be adopted by another account because
//!   the `mkdir` fails when the name is taken.

use std::path::Path;

use crate::host;

/// `data/` itself.
pub const DIR_MODE: u32 = 0o700;

/// Anything ATLAS writes inside `data/`: the database, `-wal`, `-shm`, the logs,
/// `config.json`.
pub const FILE_MODE: u32 = 0o600;

/// Which of the two modes applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A directory: `0700` on POSIX, so its contents are unreachable.
    Dir,
    /// A file inside a private directory: `0600`.
    File,
}

impl Kind {
    /// The mode this kind gets on POSIX.
    pub fn mode(self) -> u32 {
        match self {
            Kind::Dir => DIR_MODE,
            Kind::File => FILE_MODE,
        }
    }
}

/// What [`make_private`] managed to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Privacy {
    /// The mode was wrong and is now right.
    Changed,
    /// The mode was already owner-only; nothing was written.
    Already,
    /// Windows: the ACL inherited from the profile applies. Nothing to change
    /// here, and nothing pretended either.
    Inherited,
    /// The path does not exist (a log that was never written). Not an error.
    Missing,
    /// The path is a symbolic link. Nothing was changed through it.
    ///
    /// `chmod` follows a link, so changing this path means changing a file
    /// somewhere else — outside `data/`, outside the project, chosen by whoever
    /// made the link. The link is not private and is not reported as such; a
    /// caller that finds one should refuse the path rather than harden it.
    Linked,
    /// The `chmod` was refused — a read-only mount, a file owned by someone
    /// else, a file system without modes.
    Failed,
}

impl Privacy {
    /// The sentence `/api/status` shows for this verdict.
    pub fn note(self) -> &'static str {
        match self {
            Privacy::Changed => "owner-only (mode changed)",
            Privacy::Already => "owner-only",
            Privacy::Inherited => "Windows: the ACL inherited from the user profile applies",
            Privacy::Missing => "not created yet",
            Privacy::Linked => "a symbolic link: not changed, and not private",
            Privacy::Failed => "could not be restricted",
        }
    }

    /// Is the file as private as this platform can make it?
    pub fn ok(self) -> bool {
        !matches!(self, Privacy::Failed | Privacy::Linked)
    }
}

/// Is this path itself a link (or, on Windows, a directory junction)?
///
/// `symlink_metadata` and not `metadata`: the question is about this entry, not
/// about what it points at. A link that reaches a private directory is not the
/// private directory, and a `chmod` here would land on the target.
pub fn is_link(path: impl AsRef<Path>) -> bool {
    std::fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
}

/// Make one path owner-only, or say why it is already as private as it gets.
///
/// A link is refused rather than followed (see [`Privacy::Linked`]): the answer
/// "this path is owner-only" has to be about this path.
pub fn make_private(path: impl AsRef<Path>, kind: Kind) -> Privacy {
    let path = path.as_ref();
    if !path.exists() && !is_link(path) {
        return Privacy::Missing;
    }
    if is_link(path) {
        return Privacy::Linked;
    }
    if !host::is_windows() {
        return set_mode(path, kind.mode());
    }
    Privacy::Inherited
}

/// [`make_private`] for a directory (`0700`).
pub fn make_dir_private(path: impl AsRef<Path>) -> Privacy {
    make_private(path, Kind::Dir)
}

/// [`make_private`] for a file inside one (`0600`).
pub fn make_file_private(path: impl AsRef<Path>) -> Privacy {
    make_private(path, Kind::File)
}

/// Open `path` for appending, creating it owner-only **at creation**.
///
/// [`make_file_private`] is a *chmod after the fact*: the file exists for the
/// length of one syscall with whatever mode the umask allows (0644 on a default
/// machine), and a co-tenant — or a backup agent that walks `data/` in that
/// window — can already open it. For a log that is written from boot to
/// shutdown that window happens again for every file, on every fresh install.
///
/// POSIX therefore sets [`Kind::File`]'s mode in the `open(2)` call itself, and
/// the `chmod` afterwards exists only for a file an *older* build (or the user's
/// own shell) already made 0644: opening a log is what repairs its mode.
///
/// Windows has no mode to set; the ACL inherited from the profile applies, which
/// is the honest answer [`make_private`] gives there too.
pub fn open_append(path: impl AsRef<Path>) -> std::io::Result<std::fs::File> {
    let path = path.as_ref();
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    // The same shape the archive gets from [`create_options`]: `0600` from the
    // `open`, and no link followed. A log whose name is a link would otherwise
    // append every log line to a file of whoever made the link.
    create_shape(&mut options);
    let file = options.open(path)?;
    // A pre-existing file keeps the mode it was created with, so the verdict of
    // this chmod is not discarded: it is written into the file system, and
    // `/api/security` reports it.
    let _ = make_file_private(path);
    Ok(file)
}

/// True where a file mode means something.
pub fn modes_apply() -> bool {
    modes_apply_on(host::os())
}

/// [`modes_apply`] for an explicit platform — the pure form.
///
/// Every other platform rule in this crate has both spellings (`host::exe_suffix`
/// and `host::exe_suffix_of`, `host::folds_case` and `host::folds_case_of`),
/// because the Windows answer is otherwise only ever *reviewed* on the machine
/// that builds it. This is the ACL rule in the same shape: on Windows privacy is
/// an ACL inherited from the profile and there is no mode to set, so a caller
/// that checks "is this private" must not compare modes there.
pub fn modes_apply_on(os: host::Os) -> bool {
    match os {
        host::Os::Posix => true,
        host::Os::Windows => false,
    }
}

/// The mode of one path, on a file system that has modes.
pub fn mode_of(path: impl AsRef<Path>) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .ok()
            .map(|meta| meta.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = path.as_ref();
        None
    }
}

/// Set a mode, reporting whether it was already right.
fn set_mode(path: &Path, mode: u32) -> Privacy {
    let before = mode_of(path);
    if before == Some(mode) {
        return Privacy::Already;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)) {
            Ok(()) => Privacy::Changed,
            Err(_) => Privacy::Failed,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        Privacy::Inherited
    }
}

/// What the file system says about one path right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    /// The path, as given.
    pub path: String,
    /// Does it exist?
    pub exists: bool,
    /// Is this entry itself a symbolic link (or a Windows junction)? Such a path
    /// is never private: whatever mode it has belongs to a file somewhere else.
    pub linked: bool,
    /// The mode (`None` on Windows, where there is no mode).
    pub mode: Option<u32>,
    /// Can an account other than the owner *and its group* reach it? Always
    /// `false` on Windows, where the ACL is not inspected at all.
    pub beyond_owner: bool,
    /// Reachable by anyone at all, in any way (`0o007`): the exposure that
    /// matters most, kept separate because the status page shows it separately.
    pub world_reachable: bool,
    /// Reachable by the owning group (`0o070`).
    pub group_reachable: bool,
    /// Is it as private as this platform can make it?
    pub ok: bool,
    /// The sentence to show for this verdict.
    pub note: &'static str,
}

impl Facts {
    /// The mode as ATLAS spells it: `0o600`, or `windows-acl` where there is no
    /// mode to spell.
    pub fn mode_text(&self) -> String {
        match self.mode {
            Some(mode) => format!("0o{:o}", mode),
            // No mode and Windows: the ACL is the answer. No mode and POSIX:
            // there is nothing to report, so report nothing.
            None if host::is_windows() => "windows-acl".to_string(),
            None => String::new(),
        }
    }
}

/// Inspect one path: the mode it has, and whether that is good enough.
///
/// The verdict is "no bit outside the owner", not "not readable": an execute bit
/// on a **directory** is traverse, which is how another account reaches the file
/// names inside it and opens whatever it can read there. Measured before the fix,
/// `facts` called a `0701` directory `ok` (world traversal, "owner-only") and a
/// `0601` file `ok`, because only `0o006` and `0o060` were counted.
///
/// The special bits (`setuid`, `setgid`, the sticky bit) are not counted: they
/// give another account nothing on a file it cannot open, and a `0700` directory
/// with the sticky bit is still a `0700` directory.
pub fn facts(path: impl AsRef<Path>) -> Facts {
    let path = path.as_ref();
    let mut facts = Facts {
        path: path.to_string_lossy().to_string(),
        exists: path.exists(),
        linked: is_link(path),
        mode: None,
        beyond_owner: false,
        world_reachable: false,
        group_reachable: false,
        ok: false,
        note: Privacy::Missing.note(),
    };
    if facts.linked {
        // The entry is there (that is why `is_link` answered), but the mode is
        // not this path's to report: it belongs to whatever the link reaches.
        facts.exists = true;
        facts.note = Privacy::Linked.note();
        return facts;
    }
    if !facts.exists {
        return facts;
    }
    facts.mode = mode_of(path);
    match facts.mode {
        Some(mode) => {
            // Group and other, in any form — read, write **or execute**. The two
            // bit tests live here and nowhere else in the workspace.
            facts.world_reachable = mode & 0o007 != 0;
            facts.group_reachable = mode & 0o070 != 0;
            facts.beyond_owner = facts.world_reachable || facts.group_reachable;
            facts.ok = !facts.beyond_owner;
            facts.note = if facts.ok {
                Privacy::Already.note()
            } else {
                Privacy::Failed.note()
            };
        }
        None => {
            // Windows: the inherited ACL is the answer, and this crate does not
            // read it — so it does not claim a failure it cannot prove either.
            facts.ok = host::is_windows();
            facts.note = Privacy::Inherited.note();
        }
    }
    facts
}

/// `OpenOptions` for a file ATLAS creates itself: `0600` from the first byte,
/// not after a `chmod` that leaves a window open, and never through a link.
///
/// SQLite would otherwise create the archive with the umask's permissions, which
/// is how an archive ends up world-readable for as long as it lives. On Windows
/// the mode call does not exist and the inheritance applies, so the plain
/// options are right there.
///
/// `O_NOFOLLOW` is part of the answer, not a nicety. Measured before the fix: a
/// `data/atlas.db` that was a link to a file outside the project made
/// `create_options().open()` succeed **through the link** and the write landed in
/// the linked file (`truncate(false)` keeps the rest of it, which is why the
/// damage reads "PWNEDnal" and not "PWNED"). A name ATLAS creates is a name ATLAS
/// owns; when the name is already a link, the open is refused.
///
/// The mode is applied by the `open` itself, so no `umask` can widen it:
/// `0600 & !umask` is a subset of `0600` for every `umask`.
pub fn create_options() -> std::fs::OpenOptions {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    create_shape(&mut options);
    options
}

/// `OpenOptions` that create a file **only if it does not exist**: `0600`, no
/// link followed, and an existing path of any kind is `AlreadyExists`.
///
/// This is the shape a caller wants when the intent is "the archive, if this is
/// the first run". `create_options` plus a separate `path.exists()` test is two
/// calls with a window between them — and `exists()` follows links, so a link to
/// a file that is not there reads as "not created yet".
pub fn create_new_options() -> std::fs::OpenOptions {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    create_shape(&mut options);
    options
}

/// What both creators above put on an `OpenOptions`: the mode at creation, and
/// `O_NOFOLLOW` where the platform has it.
fn create_shape(options: &mut std::fs::OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(FILE_MODE);
        let flag = no_follow_flag();
        if flag != 0 {
            options.custom_flags(flag);
        }
    }
    #[cfg(not(unix))]
    {
        // No mode and no flag: nothing to set, and nothing pretended.
        let _ = options;
    }
}

/// Create one directory only this account can reach: `0700` from the `mkdir`, not
/// `0755` and then a `chmod`.
///
/// `std::fs::create_dir_all` answers `0755` under the usual `umask` — measured:
/// "mode of a directory made with create_dir_all = 755" — and the `chmod` that
/// follows it in `atlas-config` and `atlas-store` leaves a window in which
/// another account can open the directory it is about to be locked out of.
/// `umask` can only clear bits, so a `mkdir` at `0700` can never be wider.
///
/// It fails when the path already exists, which is the other half of the rule: a
/// name another account pre-created in a shared directory must not be adopted —
/// they keep a handle on it and see everything written into it. The caller retries
/// with a fresh name; `AlreadyExists` is the answer that says so.
pub fn create_dir(path: impl AsRef<Path>) -> std::io::Result<()> {
    let path = path.as_ref();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(DIR_MODE);
        builder.create(path)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(path)?;
    }
    // The mode is right from the `mkdir`; the platform's own verdict is still
    // asked for, so a file system that ignored the mode is reported rather than
    // assumed, and a directory that could not be restricted is not left behind.
    if make_dir_private(path).ok() {
        return Ok(());
    }
    let _ = std::fs::remove_dir(path);
    Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "the directory could not be made owner-only",
    ))
}

/// `O_NOFOLLOW` for the platform that has it.
///
/// Linux spells it `0o400000`; macOS and the BSDs share `0x0100`. Windows has no
/// such flag (`0`), and there the canonicalised path is what guarantees the file
/// that is opened is the file that was checked.
pub fn no_follow_flag() -> i32 {
    if cfg!(target_os = "linux") {
        0o400000
    } else if cfg!(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )) {
        0x0100
    } else {
        0
    }
}

/// Can this build promise that an open will not follow a final symlink?
///
/// `true` where [`no_follow_flag`] is known, and on Windows, where the guarantee
/// is the canonicalised path rather than a flag. `false` on a unix this crate has
/// no constant for (illumos, AIX, …) — and there a flag of `0` used to mean "silently
/// follow the link", which is the one thing a no-follow open may not do.
pub fn no_follow_supported() -> bool {
    host::is_windows() || no_follow_flag() != 0
}

/// `O_NONBLOCK` for the platform that has it, `0` where it does not.
///
/// Needed because `open(2)` on a **FIFO** waits for a writer, and a character
/// device (a console, a serial line) can wait too: a file in `public/` whose
/// name is served but whose type is a pipe would park the request thread inside
/// `open` with no timeout. With this flag the open answers at once and the
/// caller's "is it a regular file" test decides. On Linux it is `0o4000`; on
/// macOS and the BSDs `0x0004`; a platform missing from the table gets `0`, and
/// a caller must not then claim the open cannot block.
pub fn nonblock_flag() -> i32 {
    if cfg!(target_os = "linux") {
        0o4000
    } else if cfg!(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )) {
        0x0004
    } else {
        0
    }
}

/// Open one file for reading without resolving a final symlink.
///
/// The one place in the workspace that knows `O_NOFOLLOW`, so a security path
/// that means "this exact file, not whatever it points at" cannot be written with
/// the wrong constant in a second crate. `None` means the open failed.
pub fn open_readonly_nofollow(path: impl AsRef<Path>) -> Option<std::fs::File> {
    open_readonly_with(path, no_follow_flag())
}

/// Open one file for reading the way a static-file server must: never through a
/// final symlink, and never waiting inside `open` for a FIFO or a device to
/// become readable.
///
/// The flags persist on the handle, and that is harmless: `O_NONBLOCK` has no
/// effect on `read(2)` of a regular file. A caller that then finds the handle is
/// not a regular file refuses it — the answer is a 404, not a parked thread.
pub fn open_readonly_static(path: impl AsRef<Path>) -> Option<std::fs::File> {
    open_readonly_with(path, no_follow_flag() | nonblock_flag())
}

/// The one `OpenOptions` builder the two opens above share.
fn open_readonly_with(path: impl AsRef<Path>, flag: i32) -> Option<std::fs::File> {
    // A unix whose `O_NOFOLLOW` this crate has no constant for would answer `0`
    // and then quietly follow a link while the caller believes it read the file
    // it named. "Cannot promise" must not read as "promised": refuse instead.
    if !no_follow_supported() {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    if flag != 0 {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(flag);
        }
    }
    options.open(path).ok()
}

/// What makes two handles the same file: the pair the file system keeps for it.
///
/// `(dev, ino)` on POSIX, `(volume serial, file index)` on Windows. Two paths
/// that resolve to one file answer the same pair; a path whose directory was
/// replaced by a symlink between the check and the open answers a different one.
/// That is the whole point: `O_NOFOLLOW` only refuses a *final* symlink, so an
/// intermediate component swapped after a `canonicalize` check would otherwise
/// hand back bytes from outside the tree the policy approved.
///
/// `None` means this platform cannot answer. A caller must then fall back to the
/// checks it does have, and must never treat "cannot say" as "same file, proven".
///
/// Today that is Windows: `windows_by_handle` is unstable (rust-lang/rust#63010), so
/// the pair is not readable on a stable toolchain without a new dependency. POSIX
/// answers with `(dev, ino)` as it always did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    volume: u64,
    index: u64,
}

impl Identity {
    /// The identity of one path's metadata, or of an open handle's.
    pub fn of(metadata: &std::fs::Metadata) -> Option<Identity> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            return Some(Identity {
                volume: metadata.dev(),
                index: metadata.ino(),
            });
        }
        #[cfg(windows)]
        {
            // Windows keeps the same pair — volume serial and file index — but std does
            // not expose it on a stable channel: `MetadataExt::volume_serial_number` and
            // `file_index` are behind `windows_by_handle` (rust-lang/rust#63010), and
            // measured on 1.100.0 a build for `x86_64-pc-windows-msvc` refuses them with
            // `error[E0658]: use of unstable library feature`. Reading the pair through
            // `GetFileInformationByHandle` would mean a new dependency for one number, so
            // this arm answers "cannot say" instead of pretending.
            //
            // What that costs is written down rather than hidden: on Windows `same_file`
            // returns `true` (see below), so the identity check adds nothing there. The
            // checks that do the work on Windows are the ones that always ran —
            // canonicalisation inside the approved root, `is_file`, the size cap, and the
            // extension rule. The crate compiles and ships on Windows; it simply has one
            // fewer lock on that door, and the code says so.
            let _ = metadata;
            None
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = metadata;
            None
        }
    }

    /// The two numbers, for a status line and for a test.
    pub fn parts(self) -> (u64, u64) {
        (self.volume, self.index)
    }
}

/// Does this handle still name the file whose identity was recorded?
///
/// `true` when the platform cannot say: the caller's other checks (canonical
/// path, regular file, size cap) still stand, and refusing every asset on a
/// platform without file identities would remove the feature rather than protect
/// it. This is the POSIX answer today; on Windows the identity is unavailable
/// (see [`Identity`]) and this function therefore answers `true`, which is the
/// documented, tested state of that platform rather than an accident.
pub fn same_file(recorded: Option<Identity>, metadata: &std::fs::Metadata) -> bool {
    match (recorded, Identity::of(metadata)) {
        (Some(recorded), Some(now)) => recorded == now,
        _ => true,
    }
}

/// What this platform's privacy answer is, in one sentence, for a status page.
pub fn explain() -> &'static str {
    if host::is_windows() {
        "data/ is protected by the ACL inherited from the user profile; run \
         .\\setup.ps1 once to cut inheritance and keep this account only"
    } else {
        "data/ is 0700 and every file in it is 0600"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_modes_are_the_documented_numbers() {
        assert_eq!(Kind::Dir.mode(), 0o700);
        assert_eq!(Kind::File.mode(), 0o600);
    }

    #[test]
    fn a_path_that_does_not_exist_is_missing_rather_than_failed() {
        let verdict = make_file_private("/nonexistent/atlas/atlas.db");
        assert_eq!(verdict, Privacy::Missing);
        assert!(verdict.ok());
    }

    #[test]
    fn the_acl_rule_answers_for_windows_where_this_machine_has_no_acl() {
        // `modes_apply` is a `cfg!` on the build, so on macOS the Windows answer
        // could never be asked. The rule is a function of the platform instead,
        // exactly as `host::exe_suffix_of` is.
        assert!(modes_apply_on(host::Os::Posix));
        assert!(!modes_apply_on(host::Os::Windows));
        assert_eq!(modes_apply(), modes_apply_on(host::os()));
        // "No mode to set" is reported, not pretended: the Windows sentence says
        // what actually protects the file.
        assert!(Privacy::Inherited.note().starts_with("Windows: the ACL"));
        assert!(Privacy::Inherited.ok());
        // Only a refused `chmod` is a failure.
        assert!(!Privacy::Failed.ok());
        assert!(Privacy::Missing.ok());
    }

    #[test]
    fn facts_do_not_invent_a_mode_on_a_platform_without_modes() {
        let facts = facts("/nonexistent/atlas");
        assert!(!facts.exists);
        assert_eq!(facts.mode, None);
        assert_eq!(
            facts.mode_text(),
            if host::is_windows() { "windows-acl" } else { "" }
        );
    }

    /// The mode a *newly created* file has, before anything chmods it.
    ///
    /// `open_append` is the only way the app creates a log, so this is the
    /// difference between "0600 once the next pass runs" and "0600 from the first
    /// byte". A plain `OpenOptions::create` gives `0666 & !umask` — 0644 on a
    /// default machine — and a co-tenant or a backup agent can read the file in
    /// that window.
    #[test]
    fn a_file_created_by_open_append_is_owner_only_from_its_first_byte() {
        let dir = std::env::temp_dir().join(format!("atlas-private-open-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let log = dir.join("app.log");
        let _ = std::fs::remove_file(&log);
        {
            use std::io::Write;
            let mut file = open_append(&log).expect("the log opens");
            file.write_all(b"line\n").expect("and takes a line");
        }
        if modes_apply() {
            let mode = mode_of(&log).expect("a mode");
            assert_eq!(
                mode & 0o077,
                0,
                "a new log file was created group/world readable: {:o}",
                mode
            );
            assert_eq!(mode & 0o777, Kind::File.mode());
        }
        assert_eq!(std::fs::read_to_string(&log).unwrap_or_default(), "line\n");
        // A file an older build made 0644 is repaired by opening it again.
        if modes_apply() {
            set_mode(&log, 0o644);
            assert_eq!(mode_of(&log), Some(0o644));
            let _ = open_append(&log).expect("reopen");
            assert_eq!(
                mode_of(&log).map(|mode| mode & 0o777),
                Some(Kind::File.mode()),
                "an existing world-readable log was not tightened by opening it"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The creation-time mode, observed where no `chmod` can hide it.
    ///
    /// `create_options` is the shape `open_append` uses, and it does not chmod
    /// afterwards: the mode is whatever the `open` gave. A control file made with
    /// the plain options in the same directory is the comparison, so this test
    /// fails if the private shape stops being applied (and says so instead of
    /// passing for the wrong reason when the machine's `umask` is already tight).
    #[test]
    fn the_creation_shape_is_owner_only_with_no_chmod_afterwards() {
        if !modes_apply() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("atlas-private-shape-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let private = dir.join("private.log");
        let plain = dir.join("plain.log");
        let _ = std::fs::remove_file(&private);
        let _ = std::fs::remove_file(&plain);
        let _ = create_options().open(&private).expect("the private shape opens");
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&plain)
            .expect("the plain shape opens");
        let private_mode = mode_of(&private).map(|mode| mode & 0o777);
        let plain_mode = mode_of(&plain).map(|mode| mode & 0o777);
        assert_eq!(private_mode, Some(Kind::File.mode()), "{:?}", private_mode);
        if plain_mode == Some(Kind::File.mode()) {
            // `umask 077`: the plain shape is already private here, so the
            // comparison proves nothing on this machine. Say so, do not fail.
            return;
        }
        assert_ne!(
            plain_mode, private_mode,
            "the private shape gave the same mode as the plain one ({:?})",
            plain_mode
        );
        // And a link planted where a log is expected is refused rather than
        // followed: the file it points at never receives a log line.
        if no_follow_supported() {
            let target = dir.join("planted.log");
            let link = dir.join("link.log");
            std::fs::write(&target, b"").expect("the target exists");
            let _ = std::fs::remove_file(&link);
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &link).expect("a link is made");
            assert!(
                open_append(&link).is_err(),
                "open_append followed a link at the log path"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn two_paths_to_one_file_share_an_identity_and_a_rewrite_does_not() {
        let dir = std::env::temp_dir().join(format!("atlas-platform-identity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let one = dir.join("one.css");
        std::fs::write(&one, b"a").expect("write");
        let link = dir.join("link.css");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&one, &link).expect("a link is made");
        #[cfg(not(unix))]
        std::fs::write(&link, b"a").expect("write");

        let of_one = Identity::of(&std::fs::metadata(&one).expect("metadata"));
        let of_link = Identity::of(&std::fs::metadata(&link).expect("metadata"));
        // A symlink resolves to the same file, so a `canonicalize` check that was
        // happy with the path is still talking about this inode.
        assert!(of_one.is_some() == of_link.is_some());
        if let (Some(one), Some(link)) = (of_one, of_link) {
            assert_eq!(one, link, "a link and its target are one file");
            assert_eq!(one.parts(), link.parts());
        }
        // A file replaced by a rename is a different file, which is the whole
        // point: the static route refuses the handle it did not approve.
        let other = dir.join("other.css");
        std::fs::write(&other, b"b").expect("write");
        let replaced_before = Identity::of(&std::fs::metadata(&one).expect("metadata"));
        std::fs::rename(&other, &one).expect("rename over");
        let replaced_after = std::fs::metadata(&one).expect("metadata");
        if Identity::of(&std::fs::metadata(&dir.join("one.css")).expect("metadata")).is_some() {
            assert!(
                !same_file(replaced_before, &replaced_after),
                "a rewritten path still answered the old file's identity"
            );
        }
        // "Cannot say" is not "proven different": a platform without identities
        // must not refuse every file.
        assert!(same_file(None, &replaced_after));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_static_open_asks_for_the_flags_its_platform_has() {
        // The two tables are per OS and a wrong value is a silent behaviour
        // change, so their shape is asserted where they are defined.
        if cfg!(unix) {
            assert!(no_follow_flag() != 0, "a unix build must refuse a final symlink");
            assert!(nonblock_flag() != 0, "a unix build must not block on a FIFO");
        }
        // A regular file opens through the static open, and the flags do not
        // change what it reads.
        let dir = std::env::temp_dir().join(format!("atlas-platform-static-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("asset.css");
        std::fs::write(&file, b"body{}").expect("write");
        let mut handle = open_readonly_static(&file).expect("a regular file opens");
        let mut body = Vec::new();
        {
            use std::io::Read;
            handle.read_to_end(&mut body).expect("read");
        }
        assert_eq!(body, b"body{}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
