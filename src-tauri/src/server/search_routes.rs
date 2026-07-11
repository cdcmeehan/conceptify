use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::{db, search};
use super::state::ApiState;

#[derive(Debug, Deserialize)]
struct SearchParams {
    q: String,
    project_id: Option<String>,
    limit: Option<usize>,
}

pub fn router<R: tauri::Runtime>() -> Router<ApiState<R>> {
    Router::new().route("/search", axum::routing::get(get_search))
}

async fn get_search<R: tauri::Runtime>(
    State(state): State<ApiState<R>>,
    Query(params): Query<SearchParams>,
) -> impl IntoResponse {
    let project = params.project_id;
    match db::with_conn(&state.db, move |conn| {
        search::query(conn, &params.q, project.as_deref(), params.limit.unwrap_or(40))
    }).await {
        Ok(response) => Json(response).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": error.to_string()}))).into_response(),
    }
}
