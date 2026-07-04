use std::sync::Arc;

use tauri::AppHandle;

use crate::db::DbHandle;

/// Shared state handed to every axum handler.
///
/// Cloned per-request by axum's `State` extractor; all fields are cheap to
/// clone (`AppHandle` is an `Arc` internally, `token` is wrapped in
/// `Arc<str>`, and `db` is an `Arc<Mutex<Connection>>` — see `crate::db`).
/// Because this is cloned per request, DB access always goes through the
/// shared `Mutex` inside `DbHandle`; handlers must not assume exclusive
/// access to the connection.
#[derive(Clone)]
pub struct ApiState {
    /// Handle back into the Tauri app, used by handlers that need to emit
    /// events to the webview (e.g. `artifact-updated`, `comment-resolved`).
    pub app_handle: AppHandle,
    /// The bearer token required on every route except `GET /health`.
    pub token: Arc<str>,
    /// Shared SQLite connection (PRD §5.1, §4). Prefer `db::with_conn` over
    /// locking this directly in async handlers — see its doc comment.
    pub db: DbHandle,
}
