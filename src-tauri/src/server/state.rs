use std::sync::Arc;

use tauri::AppHandle;

/// Shared state handed to every axum handler.
///
/// Cloned per-request by axum's `State` extractor; both fields are cheap to
/// clone (`AppHandle` is an `Arc` internally, `token` is wrapped in `Arc<str>`).
#[derive(Clone)]
pub struct ApiState {
    /// Handle back into the Tauri app, used by handlers that need to emit
    /// events to the webview (e.g. `artifact-updated`, `comment-resolved`).
    pub app_handle: AppHandle,
    /// The bearer token required on every route except `GET /health`.
    pub token: Arc<str>,
}
