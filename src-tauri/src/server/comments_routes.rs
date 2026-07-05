//! Comments HTTP routes (PRD §7.4, §7.8).
//!
//! Thin transport layer over `crate::comments`: create / list / update. Anchor
//! JSON is carried opaquely as `serde_json::Value` (validated against the
//! `Anchor` schema in the domain layer, stored verbatim). Mutations emit
//! `comment-created` / `comment-updated` Tauri events carrying `project_id` +
//! `thread_id` so the webview refetches just the affected view.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tauri::Emitter;

use conceptify_types::{
    CommentResponse, CreateCommentRequest, ListCommentsResponse, UpdateCommentRequest,
};

use crate::comments::{self, AnchorState, Comment, CommentError, CommentStatus};
use crate::db;

use super::state::ApiState;

pub fn router<R: tauri::Runtime>() -> Router<ApiState<R>> {
    Router::new()
        .route("/comments", axum::routing::post(create_comment))
        .route("/comments", axum::routing::get(list_comments))
        .route("/comments/{id}", axum::routing::patch(update_comment))
}

/// Map a domain `Comment` (+ its `parent_id`) to its API shape. `parent_id` rides
/// alongside because it is tracked next to `Comment` (in `CommentContext` /
/// `list_comments_with_parent`) rather than on the shared struct. `pub(super)` so
/// the thread-context route (`threads_routes`) can reuse the exact same mapping.
pub(super) fn to_response(c: Comment, parent_id: Option<String>) -> CommentResponse {
    CommentResponse {
        id: c.id,
        thread_id: c.thread_id,
        parent_id,
        artifact_version: c.artifact_version,
        anchor: c.anchor,
        body: c.body,
        status: c.status.as_str().to_owned(),
        answer_html: c.answer_html,
        anchor_state: c.anchor_state.as_str().to_owned(),
        created_at: c.created_at,
        resolved_at: c.resolved_at,
    }
}

async fn create_comment<R: tauri::Runtime>(
    State(state): State<ApiState<R>>,
    Json(req): Json<CreateCommentRequest>,
) -> impl IntoResponse {
    let thread_id = req.thread_id.clone();
    let artifact_version = req.artifact_version;
    let anchor = req.anchor.clone();
    let body = req.body.clone();
    let parent_id = req.parent_id.clone();

    // A reply attaches to a root, not to a region of the artifact — reject an
    // anchor supplied alongside a `parent_id` up front (structured 400).
    if parent_id.is_some() && anchor.is_some() {
        return create_error_response(CommentError::ReplyWithAnchor);
    }

    let result = db::with_conn_result(&state.db, move |conn| match parent_id {
        Some(parent_id) => comments::create_reply(conn, &thread_id, &parent_id, &body),
        None => comments::create_comment(conn, &thread_id, artifact_version, anchor.as_ref(), &body),
    })
    .await;

    match result {
        Ok(ctx) => {
            let comments::CommentContext {
                comment,
                project_id,
                parent_id,
                reopened_root,
            } = ctx;
            let response = to_response(comment, parent_id);
            if let Err(e) = state.app_handle.emit(
                "comment-created",
                &json!({
                    "project_id": project_id,
                    "thread_id": response.thread_id,
                    "comment_id": response.id,
                }),
            ) {
                eprintln!("[conceptify-server] failed to emit comment-created event: {e}");
            }
            // A user reply on an answered/applied root flips it back to `open`;
            // emit `comment-updated` for the root so the sidebar reflects it.
            if let Some(root) = reopened_root {
                if let Err(e) = state.app_handle.emit(
                    "comment-updated",
                    &json!({
                        "project_id": project_id,
                        "thread_id": root.thread_id,
                        "comment_id": root.id,
                        "status": root.status.as_str(),
                    }),
                ) {
                    eprintln!("[conceptify-server] failed to emit comment-updated (re-open): {e}");
                }
            }
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(e) => create_error_response(e),
    }
}

#[derive(Deserialize)]
struct ListCommentsQuery {
    thread_id: String,
    #[serde(default)]
    status: Option<String>,
}

async fn list_comments<R: tauri::Runtime>(
    State(state): State<ApiState<R>>,
    Query(query): Query<ListCommentsQuery>,
) -> impl IntoResponse {
    // Strict parse of the optional status filter — an unknown value is a client
    // error, not an empty result.
    let status = match query.status.as_deref() {
        None => None,
        Some(s) => match CommentStatus::parse(s) {
            Some(parsed) => Some(parsed),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": format!(
                            "invalid status filter \"{s}\" (expected open|answered|applied)"
                        )
                    })),
                )
                    .into_response();
            }
        },
    };

    let thread_id = query.thread_id;
    let result = db::with_conn_result(&state.db, move |conn| {
        comments::list_comments_with_parent(conn, &thread_id, status)
    })
    .await;

    match result {
        Ok(list) => {
            let comments: Vec<CommentResponse> = list
                .into_iter()
                .map(|(comment, parent_id)| to_response(comment, parent_id))
                .collect();
            Json(ListCommentsResponse { comments }).into_response()
        }
        Err(e) => {
            eprintln!("[conceptify-server] list_comments error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "database error" })),
            )
                .into_response()
        }
    }
}

async fn update_comment<R: tauri::Runtime>(
    State(state): State<ApiState<R>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateCommentRequest>,
) -> impl IntoResponse {
    // Parse the enum-valued fields up front so a bad value is a 400 before we
    // touch the DB.
    let status = match req.status.as_deref() {
        None => None,
        Some(s) => match CommentStatus::parse(s) {
            Some(parsed) => Some(parsed),
            None => return bad_request(format!("invalid status \"{s}\"")),
        },
    };
    let anchor_state = match req.anchor_state.as_deref() {
        None => None,
        Some(s) => match AnchorState::parse(s) {
            Some(parsed) => Some(parsed),
            None => return bad_request(format!("invalid anchor_state \"{s}\"")),
        },
    };
    let answer_html = req.answer_html.clone();

    let comment_id = id.clone();
    let result = db::with_conn_result(&state.db, move |conn| {
        comments::update_comment(
            conn,
            &comment_id,
            status,
            answer_html.as_deref(),
            anchor_state,
        )
    })
    .await;

    match result {
        Ok(ctx) => {
            let comments::CommentContext {
                comment,
                project_id,
                parent_id,
                ..
            } = ctx;
            let response = to_response(comment, parent_id);
            if let Err(e) = state.app_handle.emit(
                "comment-updated",
                &json!({
                    "project_id": project_id,
                    "thread_id": response.thread_id,
                    "comment_id": response.id,
                    "status": response.status,
                }),
            ) {
                eprintln!("[conceptify-server] failed to emit comment-updated event: {e}");
            }
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(e) => update_error_response(e),
    }
}

fn bad_request(message: String) -> axum::response::Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
}

/// Errors on the create path. `EmptyBody`/`InvalidAnchor` → 400; missing thread
/// or artifact version → 404; anything else → 500.
fn create_error_response(err: CommentError) -> axum::response::Response {
    match err {
        CommentError::EmptyBody => bad_request("comment body must not be empty".into()),
        CommentError::InvalidAnchor(msg) => bad_request(format!("invalid anchor: {msg}")),
        // Reply-rule violations are all client errors → 400, except an unknown
        // parent (404, mirroring thread/version not-found).
        CommentError::ReplyWithAnchor => bad_request("a reply must not carry an anchor".into()),
        CommentError::ReplyToReply(id) => bad_request(format!(
            "cannot reply to a reply ({id} is itself a reply); reply to the root comment"
        )),
        CommentError::ParentDifferentThread {
            parent_id,
            thread_id,
        } => bad_request(format!(
            "parent comment {parent_id} is not in thread {thread_id}"
        )),
        CommentError::ParentNotFound(id) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("parent comment not found: {id}") })),
        )
            .into_response(),
        CommentError::ThreadNotFound(id) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("thread not found: {id}") })),
        )
            .into_response(),
        CommentError::ArtifactVersionNotFound { thread_id, version } => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": format!("artifact version {version} not found for thread {thread_id}")
            })),
        )
            .into_response(),
        other => {
            eprintln!("[conceptify-server] create_comment error: {other}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "database error" })),
            )
                .into_response()
        }
    }
}

/// Errors on the update path. `NotFound` → 404; illegal status transition → 409
/// with a structured `{ error, from, to }`; `NoUpdateFields` → 400; else 500.
fn update_error_response(err: CommentError) -> axum::response::Response {
    match err {
        CommentError::NotFound(id) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("comment not found: {id}") })),
        )
            .into_response(),
        CommentError::IllegalTransition { from, to } => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": format!("illegal status transition: {from} -> {to}"),
                "from": from,
                "to": to,
            })),
        )
            .into_response(),
        CommentError::NoUpdateFields => {
            bad_request("no fields to update (supply status, answer_html, or anchor_state)".into())
        }
        // `applied` is root-only; a reply advances open → answered.
        CommentError::AppliedOnReply(id) => bad_request(format!(
            "cannot apply a reply ({id}); the `applied` status is root-only"
        )),
        other => {
            eprintln!("[conceptify-server] update_comment error: {other}");
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
    use std::sync::{Arc, Mutex};

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::json;
    use tauri::Listener;
    use tower::ServiceExt;

    use super::super::routes;
    use super::super::state::ApiState;

    const TOKEN: &str = "test-token";

    /// Full-stack harness: real migrations (throwaway on-disk DB), a mock Tauri
    /// app for the `AppHandle`, the real `build_router` (auth included), and a
    /// seeded project / thread / artifact-v1 row so the comment FK is
    /// satisfiable. `comment-created` / `comment-updated` payloads are captured
    /// via `listen_any`.
    struct Harness {
        router: axum::Router,
        db: crate::db::DbHandle,
        project_id: String,
        thread_id: String,
        created_events: Arc<Mutex<Vec<String>>>,
        updated_events: Arc<Mutex<Vec<String>>>,
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
        let db_path = std::env::temp_dir().join(format!("conceptify-test-comments-{unique}.db"));
        let project_id = format!("proj-{unique}");

        let db = crate::db::init_at(&db_path).expect("test db should init");
        let thread_id = {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO projects (id, name, root_path) VALUES (?1, 'Proj', ?2)",
                [&project_id, &format!("/tmp/{unique}")],
            )
            .unwrap();
            let thread_id = crate::threads::create_thread(&conn, &project_id, "Route Test", "q")
                .unwrap()
                .id;
            // Seed artifact v1 directly (the comment FK needs the row, not a
            // real file on disk).
            conn.execute(
                "INSERT INTO artifacts (id, thread_id, version, file_path, created_by)
                 VALUES (?1, ?2, 1, '/tmp/x.html', 'initial')",
                rusqlite::params![format!("art-{unique}"), thread_id],
            )
            .unwrap();
            thread_id
        };

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let app_handle = app.handle().clone();

        let created_events: Arc<Mutex<Vec<String>>> = Arc::default();
        let updated_events: Arc<Mutex<Vec<String>>> = Arc::default();
        {
            let sink = created_events.clone();
            app_handle.listen_any("comment-created", move |event| {
                sink.lock().unwrap().push(event.payload().to_owned());
            });
            let sink = updated_events.clone();
            app_handle.listen_any("comment-updated", move |event| {
                sink.lock().unwrap().push(event.payload().to_owned());
            });
        }

        let router = routes::build_router(ApiState {
            app_handle,
            token: TOKEN.into(),
            db: db.clone(),
        });

        Harness {
            router,
            db,
            project_id,
            thread_id,
            created_events,
            updated_events,
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

    fn req(
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(t) = token {
            builder = builder.header("authorization", format!("Bearer {t}"));
        }
        let body = match body {
            Some(v) => {
                builder = builder.header("content-type", "application/json");
                Body::from(serde_json::to_vec(&v).unwrap())
            }
            None => Body::empty(),
        };
        builder.body(body).unwrap()
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn text_anchor() -> serde_json::Value {
        json!({
            "v": 1,
            "type": "text",
            "cfy_id": "sec-walkthrough",
            "start": 142,
            "end": 210,
            "quote": {
                "exact": "the token is refreshed here",
                "prefix": "why ",
                "suffix": " on every request"
            }
        })
    }

    #[tokio::test]
    async fn create_list_update_end_to_end() {
        let h = harness("e2e");

        // Missing token → 401 before any domain logic.
        let res = h
            .router
            .clone()
            .oneshot(req(
                "POST",
                "/api/v1/comments",
                None,
                Some(json!({ "thread_id": h.thread_id, "artifact_version": 1, "body": "x" })),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        // Create a comment carrying both primary + fallback anchor data.
        let anchor = text_anchor();
        let res = h
            .router
            .clone()
            .oneshot(req(
                "POST",
                "/api/v1/comments",
                Some(TOKEN),
                Some(json!({
                    "thread_id": h.thread_id,
                    "artifact_version": 1,
                    "anchor": anchor,
                    "body": "I don't get why the token is refreshed here"
                })),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let created = body_json(res).await;
        let comment_id = created["id"].as_str().unwrap().to_owned();
        assert_eq!(created["status"], "open");
        assert_eq!(created["anchor_state"], "anchored");
        assert!(created["answer_html"].is_null());
        assert!(created["resolved_at"].is_null());
        // The anchor (primary offsets + fallback quote) round-trips over HTTP.
        assert_eq!(created["anchor"], anchor);

        // A null-anchor direct follow-up is accepted.
        let res = h
            .router
            .clone()
            .oneshot(req(
                "POST",
                "/api/v1/comments",
                Some(TOKEN),
                Some(json!({
                    "thread_id": h.thread_id,
                    "artifact_version": 1,
                    "body": "a direct follow-up question"
                })),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(body_json(res).await["anchor"].is_null());

        // List all → 2; list open → 2.
        let res = h
            .router
            .clone()
            .oneshot(req(
                "GET",
                &format!("/api/v1/comments?thread_id={}", h.thread_id),
                Some(TOKEN),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            body_json(res).await["comments"].as_array().unwrap().len(),
            2
        );

        // Update: answer the first comment.
        let res = h
            .router
            .clone()
            .oneshot(req(
                "PATCH",
                &format!("/api/v1/comments/{comment_id}"),
                Some(TOKEN),
                Some(json!({ "status": "answered", "answer_html": "<p>because …</p>" })),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let updated = body_json(res).await;
        assert_eq!(updated["status"], "answered");
        assert_eq!(updated["answer_html"], "<p>because …</p>");
        assert!(!updated["resolved_at"].is_null());

        // Filter open → now only the direct follow-up remains.
        let res = h
            .router
            .clone()
            .oneshot(req(
                "GET",
                &format!("/api/v1/comments?thread_id={}&status=open", h.thread_id),
                Some(TOKEN),
                None,
            ))
            .await
            .unwrap();
        let open = body_json(res).await;
        assert_eq!(open["comments"].as_array().unwrap().len(), 1);
        assert_eq!(open["comments"][0]["status"], "open");

        // Events: two creates, one update — each with the documented payload.
        let created_events = h.created_events.lock().unwrap().clone();
        assert_eq!(created_events.len(), 2, "creates: {created_events:?}");
        let payload: serde_json::Value = serde_json::from_str(&created_events[0]).unwrap();
        assert_eq!(payload["project_id"], h.project_id.as_str());
        assert_eq!(payload["thread_id"], h.thread_id.as_str());
        assert_eq!(payload["comment_id"], comment_id.as_str());

        let updated_events = h.updated_events.lock().unwrap().clone();
        assert_eq!(updated_events.len(), 1, "updates: {updated_events:?}");
        let payload: serde_json::Value = serde_json::from_str(&updated_events[0]).unwrap();
        assert_eq!(payload["project_id"], h.project_id.as_str());
        assert_eq!(payload["thread_id"], h.thread_id.as_str());
        assert_eq!(payload["comment_id"], comment_id.as_str());
        assert_eq!(payload["status"], "answered");
    }

    #[tokio::test]
    async fn illegal_transition_rejected_with_structured_error() {
        let h = harness("illegal");

        // Create then apply.
        let res = h
            .router
            .clone()
            .oneshot(req(
                "POST",
                "/api/v1/comments",
                Some(TOKEN),
                Some(json!({ "thread_id": h.thread_id, "artifact_version": 1, "body": "q" })),
            ))
            .await
            .unwrap();
        let id = body_json(res).await["id"].as_str().unwrap().to_owned();

        let res = h
            .router
            .clone()
            .oneshot(req(
                "PATCH",
                &format!("/api/v1/comments/{id}"),
                Some(TOKEN),
                Some(json!({ "status": "applied" })),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // applied → answered is a regression → 409 with structured from/to.
        let res = h
            .router
            .clone()
            .oneshot(req(
                "PATCH",
                &format!("/api/v1/comments/{id}"),
                Some(TOKEN),
                Some(json!({ "status": "answered" })),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let err = body_json(res).await;
        assert_eq!(err["from"], "applied");
        assert_eq!(err["to"], "answered");
        assert!(err["error"]
            .as_str()
            .unwrap()
            .contains("applied -> answered"));

        // Only the one legal update emitted an event.
        assert_eq!(h.updated_events.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn create_rejects_malformed_anchor_and_missing_version() {
        let h = harness("bad");

        // Malformed anchor → 400.
        let res = h
            .router
            .clone()
            .oneshot(req(
                "POST",
                "/api/v1/comments",
                Some(TOKEN),
                Some(json!({
                    "thread_id": h.thread_id,
                    "artifact_version": 1,
                    "anchor": { "type": "region" },
                    "body": "q"
                })),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(body_json(res).await["error"]
            .as_str()
            .unwrap()
            .contains("anchor"));

        // Unknown artifact version → 404.
        let res = h
            .router
            .clone()
            .oneshot(req(
                "POST",
                "/api/v1/comments",
                Some(TOKEN),
                Some(json!({ "thread_id": h.thread_id, "artifact_version": 9, "body": "q" })),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        // Nothing stored, no events.
        assert!(h.created_events.lock().unwrap().is_empty());
        let count: i64 = {
            let conn = h.db.lock().unwrap();
            conn.query_row("SELECT count(*) FROM comments", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn list_rejects_invalid_status_filter() {
        let h = harness("filter");
        let res = h
            .router
            .clone()
            .oneshot(req(
                "GET",
                &format!("/api/v1/comments?thread_id={}&status=bogus", h.thread_id),
                Some(TOKEN),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    // -- replies (epic conceptify-6xi) ---------------------------------------

    /// Create a comment over HTTP and return its id. `parent_id`/`anchor` are
    /// included when `Some`.
    async fn create(
        h: &Harness,
        body: &str,
        anchor: Option<serde_json::Value>,
        parent_id: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let mut payload = json!({ "thread_id": h.thread_id, "artifact_version": 1, "body": body });
        if let Some(a) = anchor {
            payload["anchor"] = a;
        }
        if let Some(p) = parent_id {
            payload["parent_id"] = json!(p);
        }
        let res = h
            .router
            .clone()
            .oneshot(req("POST", "/api/v1/comments", Some(TOKEN), Some(payload)))
            .await
            .unwrap();
        let status = res.status();
        (status, body_json(res).await)
    }

    #[tokio::test]
    async fn reply_persists_and_lists_with_parent_id() {
        let h = harness("reply-ok");

        let (status, root) = create(&h, "root question", Some(text_anchor()), None).await;
        assert_eq!(status, StatusCode::OK);
        let root_id = root["id"].as_str().unwrap().to_owned();
        assert!(root["parent_id"].is_null(), "root has no parent");

        let (status, reply) = create(&h, "still confused", None, Some(&root_id)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(reply["parent_id"], root_id.as_str());
        assert!(reply["anchor"].is_null(), "replies carry no anchor");
        assert_eq!(reply["status"], "open");
        assert_eq!(reply["artifact_version"], 1, "reply inherits parent's version");

        // The list surfaces parent_id per item.
        let res = h
            .router
            .clone()
            .oneshot(req(
                "GET",
                &format!("/api/v1/comments?thread_id={}", h.thread_id),
                Some(TOKEN),
                None,
            ))
            .await
            .unwrap();
        let list = body_json(res).await;
        let items = list["comments"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        let root_item = items.iter().find(|c| c["id"] == root_id.as_str()).unwrap();
        let reply_item = items.iter().find(|c| c["parent_id"] == root_id.as_str());
        assert!(root_item["parent_id"].is_null());
        assert!(reply_item.is_some(), "reply lists with its parent_id");

        // Two creates, and NO updated event (the root was open — no re-open).
        assert_eq!(h.created_events.lock().unwrap().len(), 2);
        assert_eq!(h.updated_events.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn reply_with_anchor_and_reply_to_reply_are_400() {
        let h = harness("reply-bad");
        let (_, root) = create(&h, "root", None, None).await;
        let root_id = root["id"].as_str().unwrap().to_owned();

        // A reply carrying an anchor → 400.
        let (status, err) = create(&h, "bad", Some(text_anchor()), Some(&root_id)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(err["error"].as_str().unwrap().contains("anchor"));

        // A reply-to-reply → 400.
        let (_, reply) = create(&h, "r1", None, Some(&root_id)).await;
        let reply_id = reply["id"].as_str().unwrap().to_owned();
        let (status, err) = create(&h, "r2", None, Some(&reply_id)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(err["error"].as_str().unwrap().contains("reply to a reply"));

        // An unknown parent → 404.
        let (status, _) = create(&h, "orphan", None, Some("ghost")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn reply_cross_thread_parent_is_400() {
        let h = harness("reply-xthread");
        // A second thread (+ artifact v1) in the same project.
        let other_thread = {
            let conn = h.db.lock().unwrap();
            let tid = crate::threads::create_thread(&conn, &h.project_id, "Other", "q")
                .unwrap()
                .id;
            conn.execute(
                "INSERT INTO artifacts (id, thread_id, version, file_path, created_by)
                 VALUES (?1, ?2, 1, '/tmp/y.html', 'initial')",
                rusqlite::params![format!("art2-{tid}"), tid],
            )
            .unwrap();
            tid
        };
        // Root lives in h.thread_id; the reply claims to be in `other_thread`.
        let (_, root) = create(&h, "root in thread 1", None, None).await;
        let root_id = root["id"].as_str().unwrap().to_owned();

        let payload = json!({
            "thread_id": other_thread,
            "artifact_version": 1,
            "parent_id": root_id,
            "body": "wrong thread"
        });
        let res = h
            .router
            .clone()
            .oneshot(req("POST", "/api/v1/comments", Some(TOKEN), Some(payload)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(body_json(res).await["error"]
            .as_str()
            .unwrap()
            .contains("not in thread"));
    }

    #[tokio::test]
    async fn user_reply_reopens_answered_root_firing_both_events() {
        let h = harness("reopen");
        let (_, root) = create(&h, "root question", None, None).await;
        let root_id = root["id"].as_str().unwrap().to_owned();

        // Answer the root.
        let res = h
            .router
            .clone()
            .oneshot(req(
                "PATCH",
                &format!("/api/v1/comments/{root_id}"),
                Some(TOKEN),
                Some(json!({ "status": "answered", "answer_html": "<p>a</p>" })),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // Reply → the root re-opens.
        let (status, reply) = create(&h, "I still don't get it", None, Some(&root_id)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(reply["status"], "open");

        // The re-open PATCH-equivalent event fired (comment-updated for the root,
        // status open) IN ADDITION to the answer's own comment-updated.
        let updated = h.updated_events.lock().unwrap().clone();
        let reopen = updated.iter().find_map(|p| {
            let v: serde_json::Value = serde_json::from_str(p).unwrap();
            (v["comment_id"] == root_id.as_str() && v["status"] == "open").then_some(v)
        });
        assert!(reopen.is_some(), "a comment-updated re-open event fires: {updated:?}");

        // Two creates (root + reply).
        assert_eq!(h.created_events.lock().unwrap().len(), 2);

        // The root is open again (batch/open-count semantics intact): its DB status.
        let status: String = {
            let conn = h.db.lock().unwrap();
            conn.query_row(
                "SELECT status FROM comments WHERE id = ?1",
                [&root_id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(status, "open");
    }

    #[tokio::test]
    async fn applied_on_reply_is_rejected_400() {
        let h = harness("applied-reply");
        let (_, root) = create(&h, "root", None, None).await;
        let root_id = root["id"].as_str().unwrap().to_owned();
        let (_, reply) = create(&h, "follow-up", None, Some(&root_id)).await;
        let reply_id = reply["id"].as_str().unwrap().to_owned();

        // applied is root-only → 400.
        let res = h
            .router
            .clone()
            .oneshot(req(
                "PATCH",
                &format!("/api/v1/comments/{reply_id}"),
                Some(TOKEN),
                Some(json!({ "status": "applied" })),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(body_json(res).await["error"]
            .as_str()
            .unwrap()
            .contains("root-only"));

        // The reply CAN be answered via the same path.
        let res = h
            .router
            .clone()
            .oneshot(req(
                "PATCH",
                &format!("/api/v1/comments/{reply_id}"),
                Some(TOKEN),
                Some(json!({ "status": "answered", "answer_html": "<p>ok</p>" })),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await["status"], "answered");
    }
}
