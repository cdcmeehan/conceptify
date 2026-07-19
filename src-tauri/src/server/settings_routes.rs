//! Display-settings HTTP route (epic conceptify-89k, bead 89k.2).
//!
//! One authenticated endpoint under `/api/v1`:
//! - `GET /settings/display` — the skill-facing read of the app-level display
//!   settings the artifact author needs at generation time. Today that is just
//!   the chosen artifact theme; the response shape
//!   (`{ "artifactTheme": … }`, [`DisplaySettingsResponse`]) is the
//!   forward-looking home for other author-time display settings (a future
//!   `videoMode`, etc.).
//!
//! Why a dedicated authed route rather than folding the theme into `/health`:
//! `/health` is the one unauthenticated liveness probe, hit in tight
//! boot-polling loops and by the raw-socket occupant detection in `net.rs`. It
//! is deliberately DB-free and must not gain a failure mode from a settings
//! read. The CLI's `conceptify status` calls this endpoint after health and
//! folds `artifactTheme` into its JSON, so the skill still gets the theme in a
//! single CLI invocation.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Json, Router};
use serde_json::json;

use conceptify_types::DisplaySettingsResponse;

use crate::{db, settings};

use super::state::ApiState;

pub fn router<R: tauri::Runtime>() -> Router<ApiState<R>> {
    Router::new().route("/settings/display", axum::routing::get(get_display))
}

async fn get_display<R: tauri::Runtime>(State(state): State<ApiState<R>>) -> impl IntoResponse {
    match db::with_conn_result(&state.db, settings::get_artifact_theme).await {
        Ok(theme) => Json(DisplaySettingsResponse {
            artifact_theme: theme.as_str().to_owned(),
        })
        .into_response(),
        Err(e) => {
            eprintln!("[conceptify-server] settings/display read failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "settings error" })),
            )
                .into_response()
        }
    }
}
