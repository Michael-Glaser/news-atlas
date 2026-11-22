//! `spawn_env` — the only environment a child process of ATLAS may see.
//!
//! The rule and the two allowlists are `atlas-platform`'s
//! ([`atlas_platform::child`]): a child never inherits the calling environment, so
//! nothing from the user's shell — tokens, proxies, API keys,
//! `LD_PRELOAD`/`DYLD_*`, `GIT_*` — reaches `llama-server`, Chrome or an
//! `atlas-*` engine. This module is the config crate's door to that answer: it
//! keeps the names and the shapes `server/config.py` published, and it *says out
//! loud* which extra variable it refused instead of dropping it silently.
//!
//! What lives here and nowhere else: the warning. What lives in `atlas-platform`:
//! which names are allowed, in which order, and what a child gets when the caller
//! has nothing.

use crate::warn::warn;

pub use atlas_platform::child::{
    key_is_secret, valid_extra_name, ENV_ALLOWLIST, ENV_ALLOWLIST_NT,
};
pub use atlas_platform::host::{os as platform, Platform};

/// The allowlist for `platform`.
pub fn allowlist(platform: Platform) -> &'static [&'static str] {
    atlas_platform::child::allowed(platform)
}

/// The allowlist a child gets, built from a caller environment passed *in* —
/// never read from the process here, so a test can prove what a child sees.
pub fn spawn_env_from(
    caller: &[(String, String)],
    extra: &[(String, String)],
    platform: Platform,
) -> Vec<(String, String)> {
    let curated = atlas_platform::child::curate(caller, extra, platform);
    for name in &curated.refused {
        warn(&format!("refusing to pass {} to a subprocess", name));
    }
    curated.env
}

/// The environment a child of this process may see: built from this process's
/// environment but containing only allowlisted names. A caller passes this to
/// its spawn primitive and never the process environment itself.
pub fn spawn_env(extra: &[(String, String)]) -> Vec<(String, String)> {
    spawn_env_from(&snapshot_env(), extra, platform())
}

/// This process's environment, as a snapshot. `vars_os` because one non-UTF-8
/// variable must not abort the process on the boot path.
pub fn snapshot_env() -> Vec<(String, String)> {
    atlas_platform::child::snapshot()
}
