//! Which loopback ports are picked, and the promise that they are picked once.
//!
//! The app, `llama-server` and the reader browser's debug port are three
//! separate processes that must agree on three numbers, so a port that has never
//! been stored is chosen here, written into `data/config.json` (0600) and
//! reported with source `auto` from then on. A stored port is never re-rolled;
//! a stored port the SPEC has to clamp is clamped in place, and the clamp is
//! reported by [`crate::Config::build`] rather than performed in silence.
//!
//! Every probe is a `TcpListener` on `127.0.0.1` that is dropped immediately:
//! the kernel is asked about one loopback port at a time and nothing is left
//! listening here. The listener itself belongs to the server, not to settings.
//!
//! It does not: read the environment or the file (the [`crate::Config`] it is
//! handed is already built), decide what a port is ([`crate::spec`]), or write
//! anything except through [`crate::stored::write_stored`].

use crate::config::Config;
use crate::paths::Paths;
use crate::problem::{ConfigError, Origin};
use crate::spawn::snapshot_env;
use crate::spec::{number_value, spec_of};
use crate::stored::{read_stored, write_stored};

/// The ports that are picked once and persisted: the server, llama.cpp and the
/// browser are separate processes and must agree, so a stored value is never
/// re-rolled.
pub const PERSISTED_PORTS: [&str; 3] = ["app_port", "llama_port", "cdp_port"];

/// The lowest port a setting may hold: below 1024 the kernel usually needs a
/// privilege ATLAS does not ask for.
pub const PORT_LOW: u16 = 1024;

/// The highest port there is.
pub const PORT_HIGH: u16 = u16::MAX;

/// How far above its preference a pick may look before it gives the preference
/// back. A window, not a scan of the whole range: 30 candidates is enough to
/// step over whatever else is running, and the answer stays predictable.
pub const PORT_WINDOW: u16 = 30;

/// The first free loopback port at or above `preferred` — the kernel is asked,
/// never assumed.
///
/// `preferred` is first pulled into `1024..=65535` (a preference outside the
/// range cannot be honoured, and answering `0` would hand a port setting a value
/// the SPEC would refuse). The search then walks up to [`PORT_WINDOW`] ports
/// *including* the last one, and answers `preferred` when every candidate is
/// busy: the caller's own preference is the honest answer there, because the
/// bind that follows reports the collision in its own words.
pub fn free_port(preferred: u16) -> u16 {
    free_port_avoiding(preferred, &[])
}

/// Is this one loopback port free right now?
///
/// The kernel is asked, and the listener is dropped in the same expression that
/// creates it: this function never leaves a socket listening. It is one named
/// helper rather than a bind inlined in the search loop so the audit has exactly
/// one line to look at — the listener allowlist in
/// `atlas-guard/tests/audit_binds.rs` names this function, and a second bind added
/// to the loop above would be a finding.
fn port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// [`free_port`], never answering a port that is already spoken for.
///
/// The three ports are picked one after another; two of them landing on the same
/// number is a start that half works (the second bind fails, and which process
/// loses is a race). `taken` is what the rest of this load already claimed.
pub fn free_port_avoiding(preferred: u16, taken: &[u16]) -> u16 {
    let start = preferred.clamp(PORT_LOW, PORT_HIGH);
    let last = start.saturating_add(PORT_WINDOW).min(PORT_HIGH);
    let mut port = start;
    loop {
        if !taken.contains(&port) && port_free(port) {
            return port;
        }
        if port >= last {
            return start;
        }
        port += 1;
    }
}

/// Load exactly as the server always has: a port that has never been stored is
/// picked once from the free range, written to `data/config.json` (0600), and
/// reported with source `auto` from then on.
pub fn load_managed_at(paths: &Paths) -> Result<Config, ConfigError> {
    let mut stored = read_stored(paths)?;
    let mut config = Config::build(paths, &snapshot_env(), &stored);
    // A port the environment or the file already fixed is claimed before
    // anything is picked, so a picked port can never be one of those three.
    let mut claimed: Vec<u16> = PERSISTED_PORTS
        .iter()
        .filter(|key| {
            matches!(
                config.sources.get(**key),
                Some(Origin::Env(_)) | Some(Origin::Config)
            )
        })
        .filter_map(|key| config.number(key))
        .map(|port| port as u16)
        .collect();
    let mut changed = false;
    for key in PERSISTED_PORTS {
        let spec = match spec_of(key) {
            Some(spec) => spec,
            None => continue,
        };
        match config.sources.get(key) {
            Some(Origin::Config) => {
                let want = config.number(key).unwrap_or(spec.default);
                let have = stored.get(key).and_then(|value| value.as_f64());
                if have != Some(want) {
                    // Clamp a hand-edited or legacy value in place.
                    stored.insert(key.to_string(), number_value(*spec, want));
                    changed = true;
                }
            }
            Some(Origin::Default) => {
                let picked = free_port_avoiding(spec.default as u16, &claimed);
                config.set_number(key, picked as f64);
                config.sources.insert(key.to_string(), Origin::Auto);
                stored.insert(key.to_string(), number_value(*spec, picked as f64));
                claimed.push(picked);
                changed = true;
            }
            _ => {}
        }
    }
    if changed {
        write_stored(paths, &stored)?;
        config.stored = stored;
    }
    Ok(config)
}

/// [`load_managed_at`] for the process this binary runs in.
pub fn load_managed() -> Result<Config, ConfigError> {
    load_managed_at(&Paths::discover())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::*;
    use serde_json::Value;

    #[test]
    fn a_pick_always_lands_inside_the_range_the_spec_allows() {
        // A preference below the range cannot be honoured; one above it must not
        // be pulled down silently either (`free_port(65535)` answers 65535, the
        // port it was asked about).
        for preferred in [0, 1, 80, 1023, 1024, 8787, 65505, 65535] {
            let port = free_port(preferred);
            assert!(
                (PORT_LOW..=PORT_HIGH).contains(&port),
                "free_port({}) answered {}, outside {}..={}",
                preferred,
                port,
                PORT_LOW,
                PORT_HIGH
            );
        }
        assert_eq!(free_port(65535), 65535);
    }

    #[test]
    fn a_port_already_claimed_is_never_handed_out_twice() {
        let held = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let taken = held.local_addr().expect("addr").port();
        let first = free_port(taken);
        assert_ne!(first, taken, "a bound port was answered again");
        // With the first pick claimed, the next pick steps over it: the app,
        // llama.cpp and the browser cannot share a number.
        let second = free_port_avoiding(taken, &[first]);
        assert_ne!(second, first, "the same port was picked twice");
        assert_ne!(second, taken);
        assert!(std::net::TcpListener::bind(("127.0.0.1", second)).is_ok());
    }

    #[test]
    fn the_three_persisted_ports_are_distinct_and_written_once() {
        let dir = TempDir::new("ports-managed");
        let first = load_managed_at(&dir.at()).expect("managed");
        let ports = [first.app_port, first.llama_port, first.cdp_port];
        for (index, port) in ports.iter().enumerate() {
            assert!(
                (PORT_LOW..=PORT_HIGH).contains(port),
                "port {} outside its range",
                port
            );
            assert!(
                !ports[index + 1..].contains(port),
                "two engines got {}: {:?}",
                port,
                ports
            );
        }
        assert_eq!(first.source_of("app_port"), "auto");
        // Written once: the second load reads the same three numbers back.
        let again = load_managed_at(&dir.at()).expect("managed");
        assert_eq!(
            [again.app_port, again.llama_port, again.cdp_port],
            ports,
            "a persisted port was re-rolled"
        );
        assert_eq!(again.source_of("app_port"), "config");
    }

    #[test]
    fn a_persisted_port_is_never_re_rolled_and_a_clamped_one_is_written_back() {
        let dir = TempDir::new("ports-clamp");
        dir.write("data/config.json", r#"{"app_port": 80, "llama_port": 99999}"#);
        let config = load_managed_at(&dir.at()).expect("managed");
        assert_eq!(config.app_port, 1024);
        assert_eq!(config.llama_port, 65535);
        let file = read_stored(&dir.at()).expect("stored");
        assert_eq!(file.get("app_port").and_then(Value::as_u64), Some(1024));
        assert_eq!(file.get("llama_port").and_then(Value::as_u64), Some(65535));
        // The clamp is a sentence, not a silence: it names the key, the value
        // that was written and what is in force.
        let notes = config.notes.join("\n");
        assert!(notes.contains("data/config.json:app_port=80"), "{}", notes);
        assert!(notes.contains("using 1024"), "{}", notes);
    }
}
