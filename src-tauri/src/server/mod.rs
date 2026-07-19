//! Local HTTP API (PRD §5.1, §7.8, §9 S1).
//!
//! An axum server spawned in the Tauri `setup` hook via
//! `tauri::async_runtime::spawn`, bound to `127.0.0.1:4477` (falling back
//! through `4487` on conflict). The `AppHandle` is shared into axum state so
//! handlers can emit Tauri events that drive live webview updates. Every
//! route except `GET /health` requires a bearer token persisted at
//! `~/Library/Application Support/conceptify/token` (mode 0600).
//!
//! This module only owns the transport/auth/lifecycle plumbing; the actual
//! endpoints (projects, threads, artifacts, comments — later beads) get
//! added to `routes.rs` as the domain model lands.

mod artifacts_routes;
mod auth;
mod catalog_routes;
mod comments_routes;
mod net;
mod open_routes;
pub(crate) mod paths;
mod projects_routes;
mod routes;
mod search_routes;
mod settings_routes;
mod state;
mod threads_routes;

use tauri::{AppHandle, Manager};

pub use state::ApiState;

/// Start the API server. Intended to be spawned once from the Tauri
/// `setup` hook: `tauri::async_runtime::spawn(server::start(app.handle().clone()))`.
///
/// Never propagates an error that would justify crashing the app (PRD N4);
/// failures (no available port, another Conceptify already serving, token
/// file unwritable) are logged and this future simply returns.
///
/// Expects a `crate::db::DbHandle` to already be in Tauri managed state
/// (`app.manage(db)` in `lib.rs`'s `setup` hook, before this is spawned) —
/// panics via `AppHandle::state` if it isn't, since that's a startup wiring
/// bug, not a runtime condition to degrade gracefully from.
pub async fn start(app_handle: AppHandle) {
    let token = match auth::load_or_create_token() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[conceptify-server] failed to load/create auth token: {e}");
            return;
        }
    };

    let db = app_handle.state::<crate::db::DbHandle>().inner().clone();

    match net::bind_with_fallback().await {
        net::BindOutcome::Bound(listener, port) => {
            if let Err(e) = paths::write_port_file(port) {
                eprintln!("[conceptify-server] failed to write port file: {e}");
                // Non-fatal: the CLI just won't find us via the port file.
            }

            eprintln!("[conceptify-server] listening on 127.0.0.1:{port}");

            let state = ApiState {
                app_handle,
                token: token.into(),
                db,
            };
            let router = routes::build_router(state);

            if let Err(e) = axum::serve(listener, router).await {
                eprintln!("[conceptify-server] server error: {e}");
            }
        }
        net::BindOutcome::DeferToExisting(port) => {
            eprintln!(
                "[conceptify-server] another Conceptify instance is already serving on port {port}; this process will not run its own API server"
            );
        }
        net::BindOutcome::NoPortAvailable => {
            eprintln!(
                "[conceptify-server] no free port in {}..={}; API server did not start",
                net::FIRST_PORT,
                net::LAST_PORT
            );
        }
    }
}
