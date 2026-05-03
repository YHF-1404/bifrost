//! Shared application state pulled in by every handler.

use bifrost_core::HubHandle;

/// Cheap-to-clone state injected into axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub hub: HubHandle,
}
