//! The `atlas-net` binary: argv, stdin, stdout — and nothing else.
//!
//! Same wire contract as every other ATLAS binary — one JSON object in, one JSON
//! object out, and `{"error": …}` with exit 1 only for *bad input*:
//!
//!     echo '{"url":"https://example.com/a"}' | atlas-net gate
//!     {"ok":true,"url":"https://example.com/a",…}
//!
//!     echo '{"url":"http://127.0.0.1:8787/","max_bytes":600000}' | atlas-net fetch
//!     {"refused":"private, loopback or link-local address is not allowed"}
//!
//! A gate refusal and a transport failure both exit 0 with a reason in the body:
//! a timed-out page is a fact about the network, not a broken binary, and the
//! bridge must not count it as one.
//!
//! Which subcommand answers what is a decision, so it lives in the library:
//! [`atlas_net::wire`].

#![forbid(unsafe_code)]

fn main() {
    atlas_net::wire::serve();
}
