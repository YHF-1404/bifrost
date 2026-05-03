//! Shared application state pulled in by every handler.

use std::path::PathBuf;

use bifrost_core::HubHandle;

/// Cheap-to-clone state injected into axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub hub: HubHandle,
    /// Directory under which per-network UI state lives. Currently
    /// holds graph node-position layouts (`<layout_dir>/<nid>.json`).
    /// Pre-created at startup so handlers don't race on it.
    pub layout_dir: PathBuf,
}
