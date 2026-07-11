//! Model catalog HTTP routes (epic conceptify-e7m, bead e7m.6).
//!
//! Two authenticated endpoints under `/api/v1`:
//! - `GET  /catalog/models`  — the catalog filtered to the enabled provider
//!   suites, plus the full provider list with counts (never touches the network;
//!   serves the disk cache or the bundled snapshot).
//! - `POST /catalog/refresh` — force a live re-fetch, update the cache, and
//!   return the fresh catalog. Failure-silent: on a network error it degrades to
//!   the cache/snapshot rather than erroring.
//!
//! Enabled providers come from the shared agent settings (`enabled_providers`);
//! the same value backs the Settings suite toggles (bead e7m.3).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Json, Router};
use serde_json::json;
use tauri::Emitter;

use crate::{catalog, db, settings};

use super::state::ApiState;

pub fn router<R: tauri::Runtime>() -> Router<ApiState<R>> {
    Router::new()
        .route("/catalog/models", axum::routing::get(get_models))
        .route("/catalog/refresh", axum::routing::post(refresh))
}

/// Read the enabled provider suites from settings (merged over code defaults).
async fn agent_settings<R: tauri::Runtime>(
    state: &ApiState<R>,
) -> Result<crate::settings::AgentSettings, String> {
    db::with_conn_result(&state.db, |conn| {
        settings::get_settings(conn)
    })
    .await
    .map_err(|e| e.to_string())
}

async fn get_models<R: tauri::Runtime>(State(state): State<ApiState<R>>) -> impl IntoResponse {
    let settings = match agent_settings(&state).await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[conceptify-server] catalog get_models settings error: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "settings error" })),
            )
                .into_response();
        }
    };
    let (cat, source) = catalog::load_for_serving();
    let mut response = catalog::build_response(&cat, source, &settings.enabled_providers);
    catalog::add_local_endpoint(&mut response, settings.local_endpoint.as_ref(), &settings.enabled_providers);
    Json(response).into_response()
}

async fn refresh<R: tauri::Runtime>(State(state): State<ApiState<R>>) -> impl IntoResponse {
    // Force a re-fetch first (failure-silent — falls back to cache/snapshot).
    let (cat, source) = catalog::refresh_now().await;

    let settings = match agent_settings(&state).await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[conceptify-server] catalog refresh settings error: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "settings error" })),
            )
                .into_response();
        }
    };

    // Let any live surface (settings UI, pickers) refresh their model lists.
    if let Err(e) = state.app_handle.emit("catalog-refreshed", &()) {
        eprintln!("[conceptify-server] failed to emit catalog-refreshed event: {e}");
    }

    let mut response = catalog::build_response(&cat, source, &settings.enabled_providers);
    catalog::add_local_endpoint(&mut response, settings.local_endpoint.as_ref(), &settings.enabled_providers);
    Json(response).into_response()
}
