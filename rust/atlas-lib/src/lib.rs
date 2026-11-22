//! Shared, dependency-free foundations for the ATLAS native binaries.
//!
//! Everything here is `std`-only on purpose: the workspace has no third-party
//! crates, so a fresh machine can build it without a package registry and the
//! app never downloads anything at build or run time.
//!
//! * [`json`] — the minimal parser/writer every binary speaks (JSON in on
//!   stdin, JSON out on stdout, one subcommand per invocation).
//! * [`cli`] — the argument/dispatch/stdin plumbing shared by all binaries.

#![forbid(unsafe_code)]

pub mod cli;
pub mod json;
