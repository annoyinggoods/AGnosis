//! The path error the platform layer and the pure data model share.
//!
//! The error type used to live in the platform's path module, which knows
//! the `directories` crate, the config dir and every folder the app
//! writes to. The pure side — the config model, the drawn-asset model,
//! the package model — must be able to report that a path could not be
//! resolved, but it must not know *where* the platform puts things.
//!
//! So the error type lives **here**, in a module of its own that knows
//! nothing about the platform: it only says *that* a path failed and
//! why. The platform's path module re-exports it, so every existing
//! reference keeps working, and the pure side spells it
//! `crate::path_error::PathError` — the same type, two names, one
//! definition.
//!
//! The split is what lets the pure model compile for a platform it was
//! never built for: the *error* is portable, the *lookup* is not.

use std::path::PathBuf;

/// A path could not be resolved, or a file on it could not be reached.
#[derive(Debug, thiserror::Error)]
pub enum PathError {
    /// The platform did not tell us where the config dir is.
    #[error("could not determine platform config dir")]
    NoConfigDir,
    /// An I/O error at a known path.
    #[error("io error at {path}: {source}")]
    Io {
        /// The path the operation was about.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
}
