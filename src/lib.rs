#![deny(missing_docs)]

//! Modern tmux plugin manager.

/// KDL configuration file parsing.
pub mod config;
/// Config-mode resolution and multi-source loading.
pub mod config_mode;
/// TPM-style tmux config scanning and plugin extraction.
pub mod config_tpm;
/// Async git operations (clone, fetch, checkout, publish).
pub mod git;
/// Tmux load-plan construction (apply environment and options, then source `*.tmux` scripts).
pub mod loader;
/// Lock file persistence (read, write, fingerprint).
pub mod lockfile;
/// Core data types shared across the crate.
pub mod model;
/// Plugin health inspection and status computation.
pub mod planner;
/// High-level install, update, restore, clean, and list operations.
pub mod plugin;
/// Bounded concurrent prepare executor for remote plugin operations.
pub mod prepare;
/// Structured progress events, reporter factory, and error summarization.
pub mod progress;
/// Persistent repo cache management and staging preparation.
pub mod repo;
/// Filesystem paths, operation locking, and build-failure marker I/O.
pub mod state;
/// Reconcile the on-disk plugin tree against config and lock.
pub mod sync;
/// Terminal styling helpers (colored, aligned label lines).
pub mod termui;
/// tmux command execution and display-message helpers.
pub mod tmux;

/// Return the first 7 characters of a commit hash for display.
pub fn short_hash(hash: &str) -> &str {
    &hash[..7.min(hash.len())]
}
