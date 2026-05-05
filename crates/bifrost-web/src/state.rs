//! Shared application state pulled in by every handler.

use std::path::PathBuf;

use bifrost_core::HubHandle;

/// Cheap-to-clone state injected into axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub hub: HubHandle,
    /// Directory under which UI state lives. Phase 3 collapses the old
    /// per-network layout files into a single `ui-layout.json` here
    /// (per-network frame + per-client position + table-view
    /// preferences). Pre-created at startup so handlers don't race
    /// on it.
    pub layout_dir: PathBuf,
}
