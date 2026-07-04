//! Open / focus HTTP route (PRD §5.2 `conceptify open`).
//!
//! `POST /api/v1/open` focuses the app on a project or thread: it validates
//! the target exists (→ 404 otherwise), brings the main window to the front
//! via the shared `AppHandle` (the window hides rather than quits on close —
//! see the lifecycle code in `lib.rs` — so it must be `show()`n before
//! focusing), and emits a `navigate` event carrying `{project_id, thread_id?}`
//! for the frontend to route on (the subscription lands in a later frontend
//! bead). Focus-on-open is part of UC1's feel: the artifact should be on
//! screen when the agent finishes.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Json, Router};
use rusqlite::Connection;
use serde_json::json;
use tauri::{Emitter, Manager};

use conceptify_types::{OpenRequest, OpenResponse};

use crate::db;
use crate::projects;
use crate::threads;

use super::state::ApiState;

pub fn router<R: tauri::Runtime>() -> Router<ApiState<R>> {
    Router::new().route("/open", axum::routing::post(open))
}

/// The resolved navigation target: always a project, optionally a specific
/// thread within it.
#[derive(Debug)]
struct OpenTarget {
    project_id: String,
    thread_id: Option<String>,
}

/// Errors from resolving an `open` request. Map to HTTP status codes in the
/// handler.
#[derive(Debug)]
enum OpenError {
    /// Neither `thread_id` nor `project_id` supplied.
    MissingTarget,
    ThreadNotFound(String),
    ProjectNotFound(String),
    Database(rusqlite::Error),
}

/// Resolve the request into a concrete navigation target, validating that the
/// referenced thread/project actually exists. `thread_id` is the more specific
/// selector and wins when both are present.
fn resolve_target(
    conn: &Connection,
    thread_id: Option<&str>,
    project_id: Option<&str>,
) -> Result<OpenTarget, OpenError> {
    if let Some(tid) = thread_id {
        match threads::find_thread_project(conn, tid) {
            Ok(Some(pid)) => Ok(OpenTarget {
                project_id: pid,
                thread_id: Some(tid.to_owned()),
            }),
            Ok(None) => Err(OpenError::ThreadNotFound(tid.to_owned())),
            Err(threads::ThreadError::Database(e)) => Err(OpenError::Database(e)),
            Err(_) => Err(OpenError::ThreadNotFound(tid.to_owned())),
        }
    } else if let Some(pid) = project_id {
        match projects::project_exists(conn, pid) {
            Ok(true) => Ok(OpenTarget {
                project_id: pid.to_owned(),
                thread_id: None,
            }),
            Ok(false) => Err(OpenError::ProjectNotFound(pid.to_owned())),
            Err(projects::ProjectError::Database(e)) => Err(OpenError::Database(e)),
            Err(_) => Err(OpenError::ProjectNotFound(pid.to_owned())),
        }
    } else {
        Err(OpenError::MissingTarget)
    }
}

async fn open<R: tauri::Runtime>(
    State(state): State<ApiState<R>>,
    Json(req): Json<OpenRequest>,
) -> impl IntoResponse {
    let thread_id = req.thread_id.clone();
    let project_id = req.project_id.clone();

    let result = db::with_conn_result(&state.db, move |conn| {
        resolve_target(conn, thread_id.as_deref(), project_id.as_deref())
    })
    .await;

    match result {
        Ok(target) => {
            // Bring the main window to the front. It hides (not quits) on
            // close, so show() first, then focus (mirrors the single-instance
            // and Reopen handlers in lib.rs).
            if let Some(window) = state.app_handle.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            } else {
                eprintln!("[conceptify-server] open: no 'main' window to focus");
            }

            // Tell the frontend where to navigate. Payload mirrors OpenResponse
            // (project_id + optional thread_id).
            if let Err(e) = state.app_handle.emit(
                "navigate",
                &json!({ "project_id": target.project_id, "thread_id": target.thread_id }),
            ) {
                eprintln!("[conceptify-server] failed to emit navigate event: {e}");
            }

            (
                StatusCode::OK,
                Json(OpenResponse {
                    ok: true,
                    project_id: target.project_id,
                    thread_id: target.thread_id,
                }),
            )
                .into_response()
        }
        Err(OpenError::MissingTarget) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "must specify thread_id or project_id" })),
        )
            .into_response(),
        Err(OpenError::ThreadNotFound(id)) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("thread not found: {}", id) })),
        )
            .into_response(),
        Err(OpenError::ProjectNotFound(id)) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("project not found: {}", id) })),
        )
            .into_response(),
        Err(OpenError::Database(e)) => {
            eprintln!("[conceptify-server] open error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "database error" })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory DB with the minimal projects/threads schema, seeded with one
    /// project and one thread, enough to exercise `resolve_target`'s validation
    /// and project resolution without a full Tauri app. The window-focus and
    /// event-emit half needs a live app and can't run headlessly (see the note
    /// on the `db_check` command test in lib.rs).
    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE projects (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                root_path TEXT UNIQUE
            );
            CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                title TEXT NOT NULL,
                slug TEXT NOT NULL DEFAULT '',
                initial_question TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            );
            INSERT INTO projects (id, name, root_path) VALUES ('p1', 'Proj One', '/a');
            INSERT INTO threads (id, project_id, title, slug, initial_question, status)
                VALUES ('t1', 'p1', 'A thread', 'a-thread', 'q', 'generating');
            ",
        )
        .unwrap();
        conn
    }

    #[test]
    fn resolves_thread_to_its_project() {
        let conn = test_conn();
        let target = resolve_target(&conn, Some("t1"), None).unwrap();
        assert_eq!(target.project_id, "p1");
        assert_eq!(target.thread_id.as_deref(), Some("t1"));
    }

    #[test]
    fn resolves_project_directly() {
        let conn = test_conn();
        let target = resolve_target(&conn, None, Some("p1")).unwrap();
        assert_eq!(target.project_id, "p1");
        assert_eq!(target.thread_id, None);
    }

    #[test]
    fn thread_id_wins_when_both_present() {
        let conn = test_conn();
        // Even with a project also supplied, the more specific thread is used
        // (and its own project is resolved, ignoring the supplied one).
        let target = resolve_target(&conn, Some("t1"), Some("other")).unwrap();
        assert_eq!(target.project_id, "p1");
        assert_eq!(target.thread_id.as_deref(), Some("t1"));
    }

    #[test]
    fn unknown_thread_is_not_found() {
        let conn = test_conn();
        let err = resolve_target(&conn, Some("ghost"), None).unwrap_err();
        assert!(matches!(err, OpenError::ThreadNotFound(id) if id == "ghost"));
    }

    #[test]
    fn unknown_project_is_not_found() {
        let conn = test_conn();
        let err = resolve_target(&conn, None, Some("ghost")).unwrap_err();
        assert!(matches!(err, OpenError::ProjectNotFound(id) if id == "ghost"));
    }

    #[test]
    fn no_target_is_missing() {
        let conn = test_conn();
        let err = resolve_target(&conn, None, None).unwrap_err();
        assert!(matches!(err, OpenError::MissingTarget));
    }
}
