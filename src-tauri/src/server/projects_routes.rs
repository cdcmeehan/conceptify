//! Projects HTTP routes (PRD §7.1, §7.8).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tauri::Emitter;

use conceptify_types::{
    ArchiveProjectRequest, EnsureProjectRequest, EnsureProjectResponse, ListProjectsResponse,
    ProjectListItem, RenameProjectRequest,
};

use crate::db;
use crate::projects::{self, ProjectError};

use super::state::ApiState;

pub fn router() -> Router<ApiState> {
    Router::new()
        .route("/projects/ensure", axum::routing::post(ensure_project))
        .route("/projects", axum::routing::get(list_projects))
        .route("/projects/{id}", axum::routing::patch(rename_project))
        .route("/projects/{id}/archive", axum::routing::put(archive_project))
}

#[derive(Deserialize)]
struct ListProjectsQuery {
    #[serde(default)]
    archived: bool,
}

async fn ensure_project(
    State(state): State<ApiState>,
    Json(req): Json<EnsureProjectRequest>,
) -> impl IntoResponse {
    let root_path = req.root_path.clone();
    let name_override = req.name.clone();

    let result = db::with_conn_result(&state.db, move |conn| {
        projects::ensure_project(conn, &root_path, name_override.as_deref())
    })
    .await;

    match result {
        Ok(ensure_result) => {
            let p = &ensure_result.project;
            let response = EnsureProjectResponse {
                id: p.id.clone(),
                name: p.name.clone(),
                root_path: p.root_path.clone(),
                created_at: p.created_at.clone(),
                archived: p.archived,
                created: ensure_result.created,
            };

            // Emit event only if we actually created a new project.
            if ensure_result.created {
                if let Err(e) = state.app_handle.emit("projects-changed", &()) {
                    eprintln!("[conceptify-server] failed to emit projects-changed event: {e}");
                }
            }

            (StatusCode::OK, Json(response)).into_response()
        }
        Err(ProjectError::PathNotFound(msg)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("path not found: {}", msg) })),
        )
            .into_response(),
        Err(ProjectError::CanonicalizeFailed(e)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("failed to canonicalize path: {}", e) })),
        )
            .into_response(),
        Err(e) => {
            eprintln!("[conceptify-server] ensure_project error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "database error" })),
            )
                .into_response()
        }
    }
}

async fn list_projects(
    State(state): State<ApiState>,
    Query(query): Query<ListProjectsQuery>,
) -> impl IntoResponse {
    let include_archived = query.archived;
    let result = db::with_conn_result(&state.db, move |conn| {
        projects::list_projects(conn, include_archived)
    })
    .await;

    match result {
        Ok(projects_list) => {
            let items: Vec<ProjectListItem> = projects_list
                .into_iter()
                .map(|p| ProjectListItem {
                    id: p.id,
                    name: p.name,
                    root_path: p.root_path,
                    created_at: p.created_at,
                    archived: p.archived,
                    thread_count: p.thread_count,
                    last_activity: p.last_activity,
                })
                .collect();

            Json(ListProjectsResponse { projects: items }).into_response()
        }
        Err(e) => {
            eprintln!("[conceptify-server] list_projects error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "database error" })),
            )
                .into_response()
        }
    }
}

async fn rename_project(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(req): Json<RenameProjectRequest>,
) -> impl IntoResponse {
    let project_id = id.clone();
    let new_name = req.name.clone();

    let result = db::with_conn_result(&state.db, move |conn| {
        projects::rename_project(conn, &project_id, &new_name)
    })
    .await;

    match result {
        Ok(()) => {
            if let Err(e) = state.app_handle.emit("projects-changed", &()) {
                eprintln!("[conceptify-server] failed to emit projects-changed event: {e}");
            }
            (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
        }
        Err(ProjectError::NotFound(id)) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("project not found: {}", id) })),
        )
            .into_response(),
        Err(e) => {
            eprintln!("[conceptify-server] rename_project error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "database error" })),
            )
                .into_response()
        }
    }
}

async fn archive_project(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(req): Json<ArchiveProjectRequest>,
) -> impl IntoResponse {
    let project_id = id.clone();
    let archived = req.archived;

    let result = db::with_conn_result(&state.db, move |conn| {
        projects::set_archived(conn, &project_id, archived)
    })
    .await;

    match result {
        Ok(()) => {
            if let Err(e) = state.app_handle.emit("projects-changed", &()) {
                eprintln!("[conceptify-server] failed to emit projects-changed event: {e}");
            }
            (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
        }
        Err(ProjectError::NotFound(id)) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("project not found: {}", id) })),
        )
            .into_response(),
        Err(e) => {
            eprintln!("[conceptify-server] archive_project error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "database error" })),
            )
                .into_response()
        }
    }
}
