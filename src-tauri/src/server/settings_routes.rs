//! Display-settings HTTP route (epic conceptify-89k, bead 89k.2).
//!
//! One authenticated endpoint under `/api/v1`:
//! - `GET /settings/display` — the skill-facing read of the app-level display
//!   settings the artifact author needs at generation time: the chosen artifact
//!   theme and the video-offer mode. The response shape
//!   (`{ "artifactTheme": …, "videoMode": … }`, [`DisplaySettingsResponse`]) is
//!   the home for further author-time display settings.
//!
//! Why a dedicated authed route rather than folding these into `/health`:
//! `/health` is the one unauthenticated liveness probe, hit in tight
//! boot-polling loops and by the raw-socket occupant detection in `net.rs`. It
//! is deliberately DB-free and must not gain a failure mode from a settings
//! read. The CLI's `conceptify status` calls this endpoint after health and
//! folds `artifactTheme` + `videoMode` into its JSON, so the skill gets both in
//! a single CLI invocation.

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
    // Both author-time display settings live in their own rows; read them in one
    // connection borrow so the endpoint stays a single DB round-trip.
    let read = db::with_conn_result(&state.db, |conn| {
        let theme = settings::get_artifact_theme(conn)?;
        let video_mode = settings::get_video_mode(conn)?;
        Ok::<_, settings::SettingsError>((theme, video_mode))
    })
    .await;
    match read {
        Ok((theme, video_mode)) => Json(DisplaySettingsResponse {
            artifact_theme: theme.as_str().to_owned(),
            video_mode: video_mode.as_str().to_owned(),
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
