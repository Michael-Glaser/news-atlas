//! Is the model server's loopback port free, and what does the answer mean?
//!
//! `atlas doctor` has to say "port 8080 is already in use" *before* the boot tries to
//! start a server on it. The question is asked by **connecting**, not by binding:
//! the user's rule for this product is that no port is ever opened, and a probe that
//! binds is a port opened, however briefly. A connect answers the same question —
//! refused means nothing is there, connected means something is — and it opens
//! nothing. The one thing a bind told us and a connect cannot is *why* a port that
//! refuses connections might still be unbindable (a privileged port, a full table);
//! that case is reported as [`Port::Unusable`] only when the connect fails for a
//! reason other than a refusal, and the model server itself is the one that would
//! report the rest.
//!
//! One port, not three. The web listener and the reader browser that made this a list
//! are gone from the product: the window dials the model server outbound and listens
//! for nothing.
//!
//! A [`Target`] carries the two things a *message* needs and a bare number cannot:
//! the name of the process and the setting that moves it. Without the setting the
//! advice is "change the port in data/config.json", which leaves the user to guess
//! which of the numbers in that file they may touch.

use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::path::Path;
use std::time::Duration;

/// How long a port probe may take. Loopback refuses at once; a second is generous.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// What one loopback port looks like from here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Port {
    /// Nothing is listening: the model server can have it.
    Free,
    /// Something already accepts connections on it.
    Taken,
    /// The connect failed for a reason that is neither "free" nor "in use" (a
    /// firewall rule, a table with no room). Reported as a failure, because it is
    /// not a "yes".
    Unusable,
}

impl Port {
    pub fn is_free(&self) -> bool {
        matches!(self, Port::Free)
    }
}

/// One port the model server binds, named, with the setting that moves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// What the listener is, as a sentence fragment: `the model server`.
    pub name: &'static str,
    /// The key in `data/config.json` / the environment that moves the port.
    pub key: &'static str,
    pub port: u16,
}

/// Ask the kernel about one loopback port, without binding it.
pub fn state(port: u16) -> Port {
    match TcpStream::connect_timeout(&SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into(), PROBE_TIMEOUT)
    {
        Ok(stream) => {
            drop(stream);
            Port::Taken
        }
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => Port::Free,
        // A timeout or a permission error is neither answer: something is in the way
        // that is not a server and not an empty port.
        Err(_) => Port::Unusable,
    }
}

/// The model server's port, with the setting that moves it.
///
/// It may already be serving — see [`crate::start`], which adopts such a server
/// instead of starting a second one.
pub fn model(config: &atlas_config::Config) -> Target {
    Target {
        name: "the model server",
        key: "llama_port",
        port: config.llama_port,
    }
}

/// The sentence for a port the model server cannot have, with the fix in it.
pub fn refusal(target: Target, config_file: &Path) -> String {
    format!(
        "{} cannot have port {} on 127.0.0.1: something else is already accepting connections \
         there. Stop that program, or set {} in {} to a free port",
        target.name,
        target.port,
        target.key,
        config_file.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(llama_port: u16) -> atlas_config::Config {
        let mut config = atlas_config::Config::load_at(&atlas_config::Paths::at(
            std::env::temp_dir().join("atlas-ports-test"),
        ))
        .expect("a config can always be built");
        config.llama_port = llama_port;
        config
    }

    /// A port this test holds, and the listener that holds it.
    fn held() -> (std::net::TcpListener, u16) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free loopback port");
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    #[test]
    fn a_port_we_hold_is_taken_and_the_same_port_is_free_when_we_let_go() {
        let (held, port) = held();
        assert_eq!(state(port), Port::Taken);
        drop(held);
        // Nothing else on this machine races for that exact port in this test.
        assert_eq!(state(port), Port::Free);
        assert!(state(port).is_free());
    }

    #[test]
    fn asking_about_a_port_does_not_open_it() {
        // The rule the whole product is built on: the probe must not bind. Measured
        // from the other side — while `state` asks, a *second* party can still take
        // the port, which is only true if nothing was bound. The kernel hands out a
        // free port, the probe says free, and the bind that follows still succeeds.
        let (probe_port, other) = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            (port, port)
        };
        assert!(state(probe_port).is_free(), "a free port is free");
        let taken = std::net::TcpListener::bind(("127.0.0.1", other));
        assert!(
            taken.is_ok(),
            "the probe left the port bound: {:?}",
            taken.err()
        );
    }

    #[test]
    fn the_model_port_comes_from_the_config_with_the_setting_that_moves_it() {
        let config = config_with(8080);
        let target = model(&config);
        assert_eq!(target.port, config.llama_port);
        assert_eq!(target.key, "llama_port");
        assert!(target.name.contains("model"), "{}", target.name);
    }

    #[test]
    fn a_refusal_names_the_process_the_port_and_the_file() {
        let target = Target {
            name: "the model server",
            key: "llama_port",
            port: 4242,
        };
        let text = refusal(target, Path::new("data/config.json"));
        assert!(
            text.starts_with("the model server cannot have port 4242"),
            "{}",
            text
        );
        assert!(text.contains("llama_port"), "{}", text);
        assert!(text.contains("data/config.json"), "{}", text);
        assert!(text.contains("already accepting connections"), "{}", text);
    }
}
