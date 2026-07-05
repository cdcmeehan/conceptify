//! Threads HTTP routes (PRD §7.2, §7.8).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tauri::Emitter;

use conceptify_types::{
    CreateThreadRequest, CreateThreadResponse, ListThreadsResponse, ThreadContextArtifact,
    ThreadContextComment, ThreadContextProject, ThreadContextResponse, ThreadContextThread,
    ThreadListItem,
};

use crate::context::{self, ContextError};
use crate::db;
use crate::threads::{self, ThreadError};

use super::state::ApiState;

pub fn router<R: tauri::Runtime>() -> Router<ApiState<R>> {
    Router::new()
        .route("/threads", axum::routing::post(create_thread))
        .route("/threads", axum::routing::get(list_threads))
        .route("/threads/{id}/context", axum::routing::get(get_context))
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

/// `GET /api/v1/threads/:id/context` — the one-round-trip aggregate a headless
/// follow-up run needs for prompt assembly (PRD §5.2 `get-context`, §5.5):
/// thread, project, latest artifact, and open comments (anchors verbatim).
/// Composes `crate::context::thread_context`; the same aggregation the internal
/// spawner (bead `conceptify-b12.2`) can reuse directly from Rust.
async fn get_context<R: tauri::Runtime>(
    State(state): State<ApiState<R>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let thread_id = id.clone();
    let result =
        db::with_conn_result(&state.db, move |conn| context::thread_context(conn, &thread_id))
            .await;

    match result {
        Ok(ctx) => {
            let response = ThreadContextResponse {
                thread: ThreadContextThread {
                    id: ctx.thread.id,
                    title: ctx.thread.title,
                    initial_question: ctx.thread.initial_question,
                    status: ctx.thread.status.as_str().to_owned(),
                    slug: ctx.thread.slug,
                },
                project: ThreadContextProject {
                    id: ctx.project.id,
                    name: ctx.project.name,
                    root_path: ctx.project.root_path,
                },
                latest_artifact: ctx.latest_artifact.map(|a| ThreadContextArtifact {
                    version: a.version,
                    file_path: a.file_path,
                }),
                // Open ROOT comments, each with its ordered reply chain nested
                // (epic conceptify-6xi). Reuse the comments route's mapping so the
                // root's fields (anchor, etc.) match GET /comments exactly; a
                // root's parent_id is null, a reply's is its root's id.
                open_comments: ctx
                    .open_comment_threads
                    .into_iter()
                    .map(|ct| {
                        let root_id = ct.root.id.clone();
                        ThreadContextComment {
                            comment: super::comments_routes::to_response(ct.root, None),
                            replies: ct
                                .replies
                                .into_iter()
                                .map(|r| {
                                    super::comments_routes::to_response(r, Some(root_id.clone()))
                                })
                                .collect(),
                        }
                    })
                    .collect(),
            };
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(ContextError::ThreadNotFound(id)) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("thread not found: {}", id) })),
        )
            .into_response(),
        Err(e) => {
            eprintln!("[conceptify-server] get_context error: {e}");
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
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    use super::super::routes;
    use super::super::state::ApiState;

    const TOKEN: &str = "test-token";

    /// Full-stack harness for the context route: real migrations (throwaway
    /// on-disk DB), a mock Tauri app for the `AppHandle`, and the real
    /// `build_router` (auth included), seeded with one project + thread. Tests
    /// insert artifact/comment rows directly (the context aggregation only
    /// reads the DB — no file on disk is required).
    struct Harness {
        router: axum::Router,
        db: crate::db::DbHandle,
        project_id: String,
        thread_id: String,
        db_path: std::path::PathBuf,
        _app: tauri::App<tauri::test::MockRuntime>,
    }

    fn harness(tag: &str) -> Harness {
        let unique = format!(
            "{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let db_path = std::env::temp_dir().join(format!("conceptify-test-context-{unique}.db"));
        let project_id = format!("proj-{unique}");

        let db = crate::db::init_at(&db_path).expect("test db should init");
        let thread_id = {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO projects (id, name, root_path) VALUES (?1, 'Proj', ?2)",
                [&project_id, &format!("/tmp/{unique}")],
            )
            .unwrap();
            crate::threads::create_thread(&conn, &project_id, "Route Test", "explain the flow")
                .unwrap()
                .id
        };

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");

        let router = routes::build_router(ApiState {
            app_handle: app.handle().clone(),
            token: TOKEN.into(),
            db: db.clone(),
        });

        Harness {
            router,
            db,
            project_id,
            thread_id,
            db_path,
            _app: app,
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.db_path);
            let _ = std::fs::remove_file(self.db_path.with_extension("db-wal"));
            let _ = std::fs::remove_file(self.db_path.with_extension("db-shm"));
        }
    }

    fn seed_artifact(h: &Harness, version: i64, file_path: &str, created_by: &str) {
        let conn = h.db.lock().unwrap();
        conn.execute(
            "INSERT INTO artifacts (id, thread_id, version, file_path, created_by)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                format!("art-{version}-{}", h.thread_id),
                h.thread_id,
                version,
                file_path,
                created_by
            ],
        )
        .unwrap();
    }

    /// Insert a comment row directly with a chosen status and anchor JSON.
    fn seed_comment(h: &Harness, id: &str, version: i64, body: &str, status: &str, anchor: Option<serde_json::Value>) {
        let conn = h.db.lock().unwrap();
        conn.execute(
            "INSERT INTO comments (id, thread_id, artifact_version, anchor, body, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                id,
                h.thread_id,
                version,
                anchor.map(|a| a.to_string()),
                body,
                status
            ],
        )
        .unwrap();
    }

    /// Insert a reply row (parent_id set, null anchor) with an explicit
    /// `created_at` so chain ordering is deterministic.
    fn seed_reply(h: &Harness, id: &str, parent_id: &str, body: &str, created_at: &str) {
        let conn = h.db.lock().unwrap();
        conn.execute(
            "INSERT INTO comments (id, thread_id, artifact_version, body, status, parent_id, created_at)
             VALUES (?1, ?2, 1, ?3, 'open', ?4, ?5)",
            rusqlite::params![id, h.thread_id, body, parent_id, created_at],
        )
        .unwrap();
    }

    fn get(uri: &str, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method("GET").uri(uri);
        if let Some(t) = token {
            builder = builder.header("authorization", format!("Bearer {t}"));
        }
        builder.body(Body::empty()).unwrap()
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn context_aggregates_thread_project_latest_artifact_and_open_comments() {
        let h = harness("full");

        // Two versions on disk; the aggregate must report the highest.
        seed_artifact(&h, 1, "/docs/artifact.v1.html", "initial");
        seed_artifact(&h, 2, "/docs/artifact.v2.html", "follow_up");

        let anchor = json!({
            "v": 1,
            "type": "text",
            "cfy_id": "sec-walkthrough",
            "start": 142,
            "end": 210,
            "quote": { "exact": "the token is refreshed here", "prefix": "why ", "suffix": " each time" }
        });
        // One open comment (must appear) + one answered (must be excluded).
        seed_comment(&h, "c-open", 1, "why refresh here?", "open", Some(anchor.clone()));
        seed_comment(&h, "c-done", 2, "already answered", "answered", None);

        // Missing token → 401 before any aggregation runs.
        let res = h
            .router
            .clone()
            .oneshot(get(
                &format!("/api/v1/threads/{}/context", h.thread_id),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        let res = h
            .router
            .clone()
            .oneshot(get(
                &format!("/api/v1/threads/{}/context", h.thread_id),
                Some(TOKEN),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let ctx = body_json(res).await;

        // Thread block.
        assert_eq!(ctx["thread"]["id"], h.thread_id.as_str());
        assert_eq!(ctx["thread"]["slug"], "route-test");
        assert_eq!(ctx["thread"]["initial_question"], "explain the flow");
        assert_eq!(ctx["thread"]["status"], "generating");

        // Project block.
        assert_eq!(ctx["project"]["id"], h.project_id.as_str());
        assert_eq!(ctx["project"]["name"], "Proj");
        assert!(ctx["project"]["root_path"].as_str().unwrap().contains("/tmp/"));

        // Latest artifact = the highest version, with its absolute file path.
        assert_eq!(ctx["latest_artifact"]["version"], 2);
        assert_eq!(ctx["latest_artifact"]["file_path"], "/docs/artifact.v2.html");

        // Only the open comment, with its anchor served verbatim (snake_case).
        let open = ctx["open_comments"].as_array().unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0]["id"], "c-open");
        assert_eq!(open[0]["status"], "open");
        assert_eq!(open[0]["anchor_state"], "anchored");
        assert_eq!(open[0]["anchor"], anchor);
        assert_eq!(open[0]["anchor"]["cfy_id"], "sec-walkthrough");
    }

    #[tokio::test]
    async fn context_thread_without_artifact_has_null_artifact_and_no_open_comments() {
        let h = harness("empty");

        let res = h
            .router
            .clone()
            .oneshot(get(
                &format!("/api/v1/threads/{}/context", h.thread_id),
                Some(TOKEN),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let ctx = body_json(res).await;

        assert!(ctx["latest_artifact"].is_null());
        assert_eq!(ctx["open_comments"].as_array().unwrap().len(), 0);
        // The thread + project still resolve.
        assert_eq!(ctx["thread"]["id"], h.thread_id.as_str());
        assert_eq!(ctx["project"]["id"], h.project_id.as_str());
    }

    #[tokio::test]
    async fn context_nests_open_root_reply_chains_ordered() {
        let h = harness("nest");
        seed_artifact(&h, 1, "/docs/artifact.v1.html", "initial");

        // An open root with two replies (ordered by created_at), an answered root
        // (excluded — not an open question), and a bare open root (empty chain).
        seed_comment(&h, "root-a", 1, "why?", "open", None);
        seed_reply(&h, "reply-2", "root-a", "second", "2020-01-01T00:00:02.000Z");
        seed_reply(&h, "reply-1", "root-a", "first", "2020-01-01T00:00:01.000Z");
        seed_comment(&h, "root-done", 1, "answered", "answered", None);
        // Force root ordering: root-a before root-bare.
        {
            let conn = h.db.lock().unwrap();
            conn.execute(
                "UPDATE comments SET created_at = '2020-01-01T00:00:00.000Z' WHERE id = 'root-a'",
                [],
            )
            .unwrap();
        }
        seed_comment(&h, "root-bare", 1, "lonely", "open", None);

        let res = h
            .router
            .clone()
            .oneshot(get(
                &format!("/api/v1/threads/{}/context", h.thread_id),
                Some(TOKEN),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let ctx = body_json(res).await;

        let open = ctx["open_comments"].as_array().unwrap();
        assert_eq!(open.len(), 2, "only open roots are top-level");

        // First root: flattened fields + its ordered reply chain nested.
        let root_a = open.iter().find(|c| c["id"] == "root-a").unwrap();
        assert!(root_a["parent_id"].is_null(), "root parent_id is null");
        let replies = root_a["replies"].as_array().unwrap();
        let ids: Vec<&str> = replies.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["reply-1", "reply-2"], "chain ordered by created_at");
        // Each nested reply carries its parent_id and null anchor.
        assert_eq!(replies[0]["parent_id"], "root-a");
        assert!(replies[0]["anchor"].is_null());

        // The bare open root nests an empty chain; the answered root is absent.
        let bare = open.iter().find(|c| c["id"] == "root-bare").unwrap();
        assert_eq!(bare["replies"].as_array().unwrap().len(), 0);
        assert!(open.iter().all(|c| c["id"] != "root-done"));
    }

    #[tokio::test]
    async fn context_unknown_thread_is_404() {
        let h = harness("missing");
        let res = h
            .router
            .clone()
            .oneshot(get("/api/v1/threads/ghost/context", Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let err = body_json(res).await;
        assert!(err["error"].as_str().unwrap().contains("ghost"));
    }
}
