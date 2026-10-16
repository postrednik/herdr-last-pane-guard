//! Recovery decisions and one-shot native hooks for Herdr Last Pane Guard.

pub mod config;
pub mod signals;
pub use signals::check as check_interrupted;
pub mod plan;
pub mod runtime;
pub mod state;
pub mod transport;

use std::io;

/// Failures remain distinct from authoritative object-not-found API replies.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("server stopped")]
    Stopped,
    #[error("server generation changed; operation skipped")]
    Stale,
    #[error("operation timed out; state not assumed missing")]
    Timeout,
    #[error("{0}")]
    Invalid(&'static str),
    #[error("I/O failure; state not assumed missing")]
    Io(#[from] io::Error),
    #[error("Herdr API rejected the request; state not assumed missing")]
    Api(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn log(message: &str) {
    eprintln!("Last Pane Guard: {message}");
}
