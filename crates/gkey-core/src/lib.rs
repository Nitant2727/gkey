//! Shared model for gkey: physical keys, normal-mode actions, and the config
//! schema. Used by both the daemon (`gkeyd`) and the settings GUI so key names
//! and config structure can never drift between them.

pub mod action;
pub mod config;
pub mod keys;

use std::path::PathBuf;

/// Path of the file the daemon writes with currently workspace-hidden window
/// handles, read by the watcher to restore them if the daemon dies uncleanly.
pub fn hidden_state_path() -> PathBuf {
    let base = std::env::var("TEMP").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("gkey-hidden.state")
}
