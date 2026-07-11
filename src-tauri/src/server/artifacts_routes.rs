//! Artifact HTTP routes (PRD §7.3 FR-3.6, §5.6, N2).
//!
//! `POST /api/v1/threads/{thread_id}/artifact` — the save-artifact ingestion
//! path. The request body is the **raw artifact HTML bytes** (send
//! `Content-Type: text/html`; not enforced), not JSON: a JSON wrapper can't
//! carry invalid UTF-8 (so the validator's `E-UTF8` rule would be dead code)
//! and needlessly escapes multi-megabyte payloads. Validation rules live in
//! docs/artifact-spec.md §8 — implemented in `crate::artifacts`.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::{Json, Router};
use serde_json::json;
use serde::Deserialize;
use tauri::Emitter;

use conceptify_types::{ArtifactIssue, SaveArtifactErrorResponse, SaveArtifactResponse};

use crate::artifacts::{self, ArtifactError};
use crate::db;

use super::state::ApiState;

/// Body-size ceiling for this route. Deliberately above the validator's
/// 50 MiB `E-SIZE-MAX` so files in the 50–60 MiB band get the structured
/// spec error rather than a bare transport-level 413; anything larger is
/// rejected by axum before buffering.
const BODY_LIMIT_BYTES: usize = 60 * 1024 * 1024;

pub fn router<R: tauri::Runtime>() -> Router<ApiState<R>> {
    Router::new()
        .route(
            "/threads/{thread_id}/artifact",
            axum::routing::post(save_artifact),
        )
        .route(
            "/threads/{thread_id}/artifacts/diff",
            axum::routing::get(diff_versions),
        )
        .layer(DefaultBodyLimit::max(BODY_LIMIT_BYTES))
}

#[derive(Deserialize)]
struct DiffQuery {
    from_version: i64,
    to_version: i64,
}

async fn diff_versions<R: tauri::Runtime>(
    State(state): State<ApiState<R>>,
    Path(thread_id): Path<String>,
    Query(query): Query<DiffQuery>,
) -> impl IntoResponse {
    let tid = thread_id.clone();
    let result = db::with_conn_result(&state.db, move |conn| {
        crate::artifact_diff::diff_versions(conn, &tid, query.from_version, query.to_version)
    })
    .await;
    match result {
        Ok(diff) => (StatusCode::OK, Json(diff)).into_response(),
        Err(
            error @ (crate::artifact_diff::DiffError::ThreadNotFound(_)
            | crate::artifact_diff::DiffError::VersionNotFound { .. }),
        ) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response(),
        Err(error) => {
            eprintln!("[conceptify-server] artifact diff failed: {error}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": error.to_string() })),
            )
                .into_response()
        }
    }
}

async fn save_artifact<R: tauri::Runtime>(
    State(state): State<ApiState<R>>,
    Path(thread_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let root = match artifacts::artifacts_root() {
        Ok(root) => root,
        Err(e) => {
            eprintln!("[conceptify-server] cannot resolve artifacts root: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "cannot resolve artifact storage directory" })),
            )
                .into_response();
        }
    };

    // The whole pipeline (version assignment → validation → atomic file
    // writes → DB transaction) runs inside the shared connection lock so
    // concurrent saves serialize and can never race a version number.
    let tid = thread_id.clone();
    let source_run_id = headers
        .get("x-conceptify-run-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let result = db::with_conn_result(&state.db, move |conn| match source_run_id.as_deref() {
        Some(run_id) => {
            artifacts::save_artifact_for_run(conn, &root, &tid, &body, Some(run_id), None)
        }
        None => artifacts::save_artifact(conn, &root, &tid, &body),
    })
    .await;

    match result {
        Ok(saved) => {
            // Live refresh (N2): the viewer subscribes to this and reloads
            // the artifact iframe well under the 500ms budget.
            if let Err(e) = state.app_handle.emit(
                "artifact-updated",
                &json!({
                    "project_id": saved.project_id,
                    "thread_id": saved.thread_id,
                    "version": saved.version,
                }),
            ) {
                eprintln!("[conceptify-server] failed to emit artifact-updated event: {e}");
            }

            // FR-4.4 re-attachment: one `comment-updated` per migrated/flagged
            // comment (the same event shape PATCH /comments emits), so the
            // sidebar refetches and shows the advanced version / "reference
            // moved" badge live.
            for comment in &saved.reattached {
                if let Err(e) = state.app_handle.emit(
                    "comment-updated",
                    &json!({
                        "project_id": saved.project_id,
                        "thread_id": comment.thread_id,
                        "comment_id": comment.id,
                        "status": comment.status.as_str(),
                    }),
                ) {
                    eprintln!("[conceptify-server] failed to emit comment-updated event: {e}");
                }
            }

            let response = SaveArtifactResponse {
                thread_id: saved.thread_id,
                project_id: saved.project_id,
                version: saved.version,
                created_by: saved.created_by.to_owned(),
                file_path: saved.file_path.to_string_lossy().into_owned(),
                warnings: saved
                    .warnings
                    .into_iter()
                    .map(|i| ArtifactIssue {
                        code: i.code.to_owned(),
                        message: i.message,
                    })
                    .collect(),
            };
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(ArtifactError::ThreadNotFound(id)) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("thread not found: {}", id) })),
        )
            .into_response(),
        Err(ArtifactError::Rejected(errors)) => {
            // Spec §8: non-2xx with { error, code } for the (first) violated
            // rule; `errors` additionally lists every hard failure found.
            let first = errors.first().expect("Rejected always carries ≥ 1 issue");
            let body = SaveArtifactErrorResponse {
                error: first.message.clone(),
                code: first.code.to_owned(),
                errors: errors
                    .into_iter()
                    .map(|i| ArtifactIssue {
                        code: i.code.to_owned(),
                        message: i.message,
                    })
                    .collect(),
            };
            (StatusCode::UNPROCESSABLE_ENTITY, Json(body)).into_response()
        }
        Err(ArtifactError::Conflict { run_id, base, current, .. }) => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "stale artifact base; candidate retained for review",
                "code": "STALE_BASE",
                "run_id": run_id,
                "base_version": base,
                "current_version": current,
            })),
        )
            .into_response(),
        Err(ArtifactError::Io(e)) => {
            eprintln!("[conceptify-server] save_artifact io error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to write artifact to disk" })),
            )
                .into_response()
        }
        Err(e) => {
            eprintln!("[conceptify-server] save_artifact error: {e}");
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
    use tauri::Listener;
    use tower::ServiceExt;

    use super::super::routes;
    use super::super::state::ApiState;

    const TOKEN: &str = "test-token";

    /// Full-stack harness: real migrations (throwaway on-disk DB), a mock
    /// Tauri app for the `AppHandle`, and the real `build_router` (auth
    /// middleware included).
    struct Harness {
        router: axum::Router,
        db: crate::db::DbHandle,
        project_id: String,
        thread_id: String,
        /// `artifact-updated` payloads captured via `listen_any`.
        events: Arc<Mutex<Vec<String>>>,
        /// `comment-updated` payloads (emitted per re-attached comment,
        /// FR-4.4) captured via `listen_any`.
        comment_events: Arc<Mutex<Vec<String>>>,
        db_path: std::path::PathBuf,
        artifacts_dir: std::path::PathBuf,
        // Keeps the mock app (and its event system) alive for the test body.
        _app: tauri::App<tauri::test::MockRuntime>,
    }

    /// The one shared per-process scratch artifacts root (bead
    /// `conceptify-028`). Delegates to `artifacts::test_artifacts_root`, the
    /// single source of truth that the route handlers' own `artifacts_root()`
    /// call also resolves to in test builds. Isolation between the parallel
    /// route tests comes from each harness using a unique project id (its own
    /// subtree under this shared root).
    fn shared_artifacts_root() -> &'static std::path::Path {
        static ROOT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        ROOT.get_or_init(crate::artifacts::test_artifacts_root)
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
        let db_path = std::env::temp_dir().join(format!("conceptify-test-routes-{unique}.db"));
        let project_id = format!("proj-{unique}");
        let artifacts_dir = shared_artifacts_root().join(&project_id);

        let db = crate::db::init_at(&db_path).expect("test db should init");
        let thread_id = {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO projects (id, name, root_path) VALUES (?1, 'Proj', ?2)",
                [&project_id, &format!("/tmp/{unique}")],
            )
            .unwrap();
            crate::threads::create_thread(&conn, &project_id, "Route Test", "q")
                .unwrap()
                .id
        };

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let app_handle = app.handle().clone();

        let events: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = events.clone();
        app_handle.listen_any("artifact-updated", move |event| {
            sink.lock().unwrap().push(event.payload().to_owned());
        });
        let comment_events: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = comment_events.clone();
        app_handle.listen_any("comment-updated", move |event| {
            sink.lock().unwrap().push(event.payload().to_owned());
        });

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
            events,
            comment_events,
            db_path,
            artifacts_dir,
            _app: app,
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.artifacts_dir);
            let _ = std::fs::remove_file(&self.db_path);
            let _ = std::fs::remove_file(self.db_path.with_extension("db-wal"));
            let _ = std::fs::remove_file(self.db_path.with_extension("db-shm"));
        }
    }

    fn post(thread_id: &str, body: &str, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(format!("/api/v1/threads/{thread_id}/artifact"))
            .header("content-type", "text/html");
        if let Some(t) = token {
            builder = builder.header("authorization", format!("Bearer {t}"));
        }
        builder.body(Body::from(body.to_owned())).unwrap()
    }

    fn post_run(thread_id: &str, body: &str, run_id: &str) -> Request<Body> {
        let mut request = post(thread_id, body, Some(TOKEN));
        request
            .headers_mut()
            .insert("x-conceptify-run-id", run_id.parse().unwrap());
        request
    }

    fn get_diff(thread_id: &str, from: i64, to: i64, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method("GET").uri(format!(
            "/api/v1/threads/{thread_id}/artifacts/diff?from_version={from}&to_version={to}"
        ));
        if let Some(t) = token {
            builder = builder.header("authorization", format!("Bearer {t}"));
        }
        builder.body(Body::empty()).unwrap()
    }

    fn valid_html(version: i64) -> String {
        format!(
            r#"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<title>T</title>
<meta name="cfy:question" content="q">
<meta name="cfy:version" content="{version}">
<meta name="cfy:generated-by" content="claude-code/test">
</head><body><h1 data-cfy-id="sec-t">T</h1></body></html>"#
        )
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn save_artifact_over_http_end_to_end() {
        let h = harness("e2e");

        // Wrong/missing token → 401 before any domain logic runs.
        let res = h
            .router
            .clone()
            .oneshot(post(&h.thread_id, &valid_html(1), None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        // v1 saves clean.
        let res = h
            .router
            .clone()
            .oneshot(post(&h.thread_id, &valid_html(1), Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let json = body_json(res).await;
        assert_eq!(json["version"], 1);
        assert_eq!(json["created_by"], "initial");
        assert_eq!(json["project_id"], h.project_id.as_str());
        assert_eq!(json["warnings"].as_array().unwrap().len(), 0);

        // v2 saves as follow_up; artifact.html tracks the latest.
        let res = h
            .router
            .clone()
            .oneshot(post(&h.thread_id, &valid_html(2), Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let json = body_json(res).await;
        assert_eq!(json["version"], 2);
        assert_eq!(json["created_by"], "follow_up");

        let dir = h.artifacts_dir.join("threads").join("route-test");
        assert!(dir.join("artifact.v1.html").is_file());
        assert!(dir.join("artifact.v2.html").is_file());
        assert_eq!(
            std::fs::read_to_string(dir.join("artifact.html")).unwrap(),
            valid_html(2)
        );

        // Thread flipped to ready.
        let status: String = {
            let conn = h.db.lock().unwrap();
            conn.query_row(
                "SELECT status FROM threads WHERE id = ?1",
                [&h.thread_id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(status, "ready");

        // artifact-updated fired for both saves with the documented payload.
        let events = h.events.lock().unwrap().clone();
        assert_eq!(events.len(), 2, "events: {events:?}");
        let payload: serde_json::Value = serde_json::from_str(&events[1]).unwrap();
        assert_eq!(payload["project_id"], h.project_id.as_str());
        assert_eq!(payload["thread_id"], h.thread_id.as_str());
        assert_eq!(payload["version"], 2);
    }

    #[tokio::test]
    async fn diff_artifact_versions_over_http() {
        let h = harness("diff");
        for body in [
            valid_html(1),
            valid_html(2).replace(">T</h1>", ">Changed title</h1>"),
        ] {
            let response = h
                .router
                .clone()
                .oneshot(post(&h.thread_id, &body, Some(TOKEN)))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        let unauthorized = h
            .router
            .clone()
            .oneshot(get_diff(&h.thread_id, 1, 2, None))
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let response = h
            .router
            .clone()
            .oneshot(get_diff(&h.thread_id, 1, 2, Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["changes"][0]["cfy_id"], "sec-t");
        assert_eq!(json["changes"][0]["kind"], "modified");

        let missing = h
            .router
            .clone()
            .oneshot(get_diff(&h.thread_id, 1, 99, Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn current_base_revision_is_retained_until_explicit_publish() {
        let h = harness("revision-preview");
        let first = h.router.clone().oneshot(post(&h.thread_id, &valid_html(1), Some(TOKEN))).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        {
            let conn = h.db.lock().unwrap();
            conn.execute(
                "INSERT INTO follow_up_runs
                     (id, thread_id, agent, model, mode, status, status_reason, log_path,
                      run_class, base_artifact_version)
                 VALUES ('preview-run', ?1, 'claude', 'm', 'apply', 'running',
                         'preview_required:comment-1', '/r.log', 'mutation', 1)",
                [&h.thread_id],
            ).unwrap();
        }
        let candidate = valid_html(2).replace(">T</h1>", ">Scoped proposal</h1>");
        let response = h.router.clone().oneshot(post_run(&h.thread_id, &candidate, "preview-run")).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let conn = h.db.lock().unwrap();
        let (status, reason, path): (String, String, String) = conn.query_row(
            "SELECT status, status_reason, candidate_path FROM follow_up_runs WHERE id = 'preview-run'",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        assert_eq!(status, "conflicted");
        assert_eq!(reason, "preview_required:comment-1");
        assert_eq!(std::fs::read_to_string(path).unwrap(), candidate);
        let versions: i64 = conn.query_row(
            "SELECT COUNT(*) FROM artifacts WHERE thread_id = ?1", [&h.thread_id], |r| r.get(0),
        ).unwrap();
        assert_eq!(versions, 1, "proposal must not publish before acceptance");
    }

    #[tokio::test]
    async fn stale_run_retains_candidate_and_explicit_separate_publish_recovers() {
        let h = harness("stale-candidate");
        let first = h
            .router
            .clone()
            .oneshot(post(&h.thread_id, &valid_html(1), Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        {
            let conn = h.db.lock().unwrap();
            conn.execute(
                "INSERT INTO follow_up_runs
                     (id, thread_id, agent, model, mode, status, log_path,
                      run_class, base_artifact_version,
                      response_intent_json, selected_skills_json)
                 VALUES ('stale-run', ?1, 'claude', 'm', 'apply', 'running', '/r.log',
                         'mutation', 1, ?2, ?3)",
                rusqlite::params![
                    h.thread_id,
                    r#"{"version":1,"depth":"deep","language":"plain","visuals":"avoid","shape":"reference"}"#,
                    r#"[{"id":"conceptify","name":"Conceptify artifact","capability_version":1,"selection":"manual"}]"#,
                ],
            ).unwrap();
        }
        let second = h
            .router
            .clone()
            .oneshot(post(&h.thread_id, &valid_html(2), Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        let candidate = valid_html(3).replace(">T</h1>", ">Stale candidate</h1>");
        let conflict = h
            .router
            .clone()
            .oneshot(post_run(&h.thread_id, &candidate, "stale-run"))
            .await
            .unwrap();
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        let body = body_json(conflict).await;
        assert_eq!(body["code"], "STALE_BASE");
        assert_eq!(body["base_version"], 1);
        assert_eq!(body["current_version"], 2);

        let conn = h.db.lock().unwrap();
        let (status, candidate_path): (String, String) = conn.query_row(
            "SELECT status, candidate_path FROM follow_up_runs WHERE id = 'stale-run'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(status, "conflicted");
        assert_eq!(std::fs::read_to_string(&candidate_path).unwrap(), candidate);
        let versions: i64 = conn.query_row(
            "SELECT COUNT(*) FROM artifacts WHERE thread_id = ?1",
            [&h.thread_id], |r| r.get(0),
        ).unwrap();
        assert_eq!(versions, 2, "stale candidate must not publish");

        let saved = crate::artifacts::save_artifact_for_run(
            &conn,
            shared_artifacts_root(),
            &h.thread_id,
            candidate.as_bytes(),
            Some("stale-run"),
            Some("separate"),
        )
        .unwrap();
        assert_eq!(saved.version, 3);
        let provenance: (Option<String>, Option<i64>, Option<String>, String, String) = conn
            .query_row(
                "SELECT source_run_id, source_base_version, resolution,
                    response_intent_json, selected_skills_json
             FROM artifacts WHERE thread_id = ?1 AND version = 3",
                [&h.thread_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(provenance.0, Some("stale-run".into()));
        assert_eq!(provenance.1, Some(1));
        assert_eq!(provenance.2, Some("separate".into()));
        assert!(provenance.3.contains("\"depth\":\"deep\""));
        assert!(provenance.4.contains("\"capability_version\":1"));
    }

    #[tokio::test]
    async fn save_artifact_rejects_disallowed_script_with_structured_error() {
        let h = harness("reject");

        let bad = valid_html(1).replace(
            "</body>",
            r#"<script src="https://evil.example.com/x.js"></script></body>"#,
        );
        let res = h
            .router
            .clone()
            .oneshot(post(&h.thread_id, &bad, Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let json = body_json(res).await;
        assert_eq!(json["code"], "E-EXTERNAL-CODE");
        assert!(json["error"].as_str().unwrap().contains("evil.example.com"));
        assert_eq!(json["errors"][0]["code"], "E-EXTERNAL-CODE");

        // Nothing stored, no event, status untouched.
        assert!(h.events.lock().unwrap().is_empty());
        let count: i64 = {
            let conn = h.db.lock().unwrap();
            conn.query_row("SELECT count(*) FROM artifacts", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(count, 0);
    }

    /// FR-4.4 over HTTP: a follow-up save re-attaches earlier-version
    /// comments and emits one `comment-updated` per changed comment (same
    /// payload shape as PATCH /comments), so the sidebar refreshes live.
    #[tokio::test]
    async fn save_artifact_emits_comment_updated_for_reattached_comments() {
        let h = harness("reattach-events");

        // v1 with an anchorable paragraph.
        let v1 = valid_html(1).replace(
            "</body>",
            r#"<p data-cfy-id="sec-a">alpha beta gamma</p></body>"#,
        );
        let res = h
            .router
            .clone()
            .oneshot(post(&h.thread_id, &v1, Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // Comment anchored to v1 (created through the domain layer — the
        // comments routes have their own HTTP tests).
        let comment_id = {
            let conn = h.db.lock().unwrap();
            crate::comments::create_comment(
                &conn,
                &h.thread_id,
                1,
                Some(&serde_json::json!({
                    "v": 1, "type": "text", "cfy_id": "sec-a", "start": 6, "end": 10,
                    "quote": { "exact": "beta" }
                })),
                "why beta?",
            )
            .unwrap()
            .comment
            .id
        };
        assert!(h.comment_events.lock().unwrap().is_empty());

        // v2 keeps the anchored content → the comment migrates to v2.
        let v2 = valid_html(2).replace(
            "</body>",
            r#"<p data-cfy-id="sec-a">alpha beta gamma</p></body>"#,
        );
        let res = h
            .router
            .clone()
            .oneshot(post(&h.thread_id, &v2, Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let events = h.comment_events.lock().unwrap().clone();
        assert_eq!(events.len(), 1, "events: {events:?}");
        let payload: serde_json::Value = serde_json::from_str(&events[0]).unwrap();
        assert_eq!(payload["project_id"], h.project_id.as_str());
        assert_eq!(payload["thread_id"], h.thread_id.as_str());
        assert_eq!(payload["comment_id"], comment_id.as_str());
        assert_eq!(payload["status"], "open");

        // And the row really advanced.
        let (version, state): (i64, String) = {
            let conn = h.db.lock().unwrap();
            conn.query_row(
                "SELECT artifact_version, anchor_state FROM comments WHERE id = ?1",
                [&comment_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };
        assert_eq!(version, 2);
        assert_eq!(state, "anchored");
    }

    #[tokio::test]
    async fn save_artifact_unknown_thread_is_404() {
        let h = harness("missing");
        let res = h
            .router
            .clone()
            .oneshot(post("ghost", &valid_html(1), Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let json = body_json(res).await;
        assert!(json["error"].as_str().unwrap().contains("ghost"));
    }
}
