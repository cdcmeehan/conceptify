//! Route table (PRD §7.8).
//!
//! Everything lives under `/api/v1/` except `GET /health`, which is also
//! mirrored at `/api/v1/health` for callers that only ever talk to the
//! versioned namespace. `/health` (either path) is the one unauthenticated
//! route; everything else requires the bearer token (see `auth.rs`).

use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tauri::Emitter;

use crate::db;

use super::auth;
use super::state::ApiState;

pub fn build_router(state: ApiState) -> Router {
    // Authenticated routes, versioned from day one (FR-8 / §7.8).
    let protected = Router::new()
        .route("/ping", get(ping))
        .route("/debug/db-check", get(db_check))
        .merge(super::projects_routes::router())
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_bearer_token,
        ));

    // /health is unauthenticated both at the root and under /api/v1, so
    // callers of either shape can use it as a liveness probe (§5.2's
    // launch-and-wait contract, and this bead's occupant-detection probe).
    let api_v1 = Router::new()
        .route("/health", get(health))
        .merge(protected);

    Router::new()
        .route("/health", get(health))
        .nest("/api/v1", api_v1)
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    Json(json!({
        "service": "conceptify",
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Demo authenticated route. Also demonstrates an axum handler emitting a
/// Tauri event that the webview can subscribe to (PRD §5.1: "the AppHandle
/// is shared into axum state so HTTP handlers can emit Tauri events").
async fn ping(State(state): State<ApiState>) -> impl IntoResponse {
    let payload = json!({
        "message": "pong",
        "unix_ms": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default(),
    });

    match state.app_handle.emit("api-ping", &payload) {
        Ok(()) => eprintln!("[conceptify-server] emitted api-ping event to webview"),
        Err(e) => eprintln!("[conceptify-server] failed to emit api-ping event: {e}"),
    }

    Json(json!({ "pong": true }))
}

/// Demo authenticated route proving the shared `DbHandle` (PRD §5.1, §4) is
/// reachable from axum, not just from `#[tauri::command]` handlers. Runs a
/// trivial read (`SELECT count(*) FROM projects`) through `db::with_conn` so
/// the query itself executes on a blocking-pool thread rather than an axum
/// worker.
async fn db_check(State(state): State<ApiState>) -> impl IntoResponse {
    let result = db::with_conn(&state.db, |conn| {
        conn.query_row("SELECT count(*) FROM projects", [], |row| row.get::<_, i64>(0))
    })
    .await;

    match result {
        Ok(project_count) => Json(json!({
            "ok": true,
            "project_count": project_count,
        }))
        .into_response(),
        Err(e) => {
            eprintln!("[conceptify-server] db-check query failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": e.to_string() })),
            )
                .into_response()
        }
    }
}
