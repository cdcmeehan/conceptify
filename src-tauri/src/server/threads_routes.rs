//! Threads HTTP routes (PRD §7.2, §7.8).

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tauri::Emitter;

use conceptify_types::{
    CreateThreadRequest, CreateThreadResponse, ListThreadsResponse, ThreadListItem,
};

use crate::db;
use crate::threads::{self, ThreadError};

use super::state::ApiState;

pub fn router<R: tauri::Runtime>() -> Router<ApiState<R>> {
    Router::new()
        .route("/threads", axum::routing::post(create_thread))
        .route("/threads", axum::routing::get(list_threads))
}

#[derive(Deserialize)]
struct ListThreadsQuery {
    project_id: String,
}

async fn create_thread<R: tauri::Runtime>(
    State(state): State<ApiState<R>>,
    Json(req): Json<CreateThreadRequest>,
) -> impl IntoResponse {
    let project_id = req.project_id.clone();
    let title = req.title.clone();
    let initial_question = req.initial_question.clone();

    let result = db::with_conn_result(&state.db, move |conn| {
        threads::create_thread(conn, &project_id, &title, &initial_question)
    })
    .await;

    match result {
        Ok(thread) => {
            let response = CreateThreadResponse {
                id: thread.id.clone(),
                project_id: thread.project_id.clone(),
                title: thread.title,
                slug: thread.slug,
                initial_question: thread.initial_question,
                status: thread.status.as_str().to_owned(),
                created_at: thread.created_at,
                updated_at: thread.updated_at,
            };

            // Carries the ids so the webview can refresh just the affected
            // project's thread list rather than refetching everything.
            if let Err(e) = state.app_handle.emit(
                "thread-created",
                &json!({ "project_id": response.project_id, "thread_id": response.id }),
            ) {
                eprintln!("[conceptify-server] failed to emit thread-created event: {e}");
            }

            (StatusCode::OK, Json(response)).into_response()
        }
        Err(ThreadError::EmptyTitle) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "title must not be empty" })),
        )
            .into_response(),
        Err(ThreadError::ProjectNotFound(id)) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("project not found: {}", id) })),
        )
            .into_response(),
        Err(e) => {
            eprintln!("[conceptify-server] create_thread error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "database error" })),
            )
                .into_response()
        }
    }
}

async fn list_threads<R: tauri::Runtime>(
    State(state): State<ApiState<R>>,
    Query(query): Query<ListThreadsQuery>,
) -> impl IntoResponse {
    let project_id = query.project_id;
    let result =
        db::with_conn_result(&state.db, move |conn| threads::list_threads(conn, &project_id)).await;

    match result {
        Ok(threads_list) => {
            let items: Vec<ThreadListItem> = threads_list
                .into_iter()
                .map(|t| ThreadListItem {
                    id: t.id,
                    project_id: t.project_id,
                    title: t.title,
                    slug: t.slug,
                    initial_question: t.initial_question,
                    status: t.status.as_str().to_owned(),
                    created_at: t.created_at,
                    updated_at: t.updated_at,
                    open_comment_count: t.open_comment_count,
                })
                .collect();

            Json(ListThreadsResponse { threads: items }).into_response()
        }
        Err(e) => {
            eprintln!("[conceptify-server] list_threads error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "database error" })),
            )
                .into_response()
        }
    }
}
