//! Artifact HTTP routes (PRD §7.3 FR-3.6, §5.6, N2).
//!
//! `POST /api/v1/threads/{thread_id}/artifact` — the save-artifact ingestion
//! path. The request body is the **raw artifact HTML bytes** (send
//! `Content-Type: text/html`; not enforced), not JSON: a JSON wrapper can't
//! carry invalid UTF-8 (so the validator's `E-UTF8` rule would be dead code)
//! and needlessly escapes multi-megabyte payloads. Validation rules live in
//! docs/artifact-spec.md §8 — implemented in `crate::artifacts`.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Json, Router};
use serde_json::json;
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
        .layer(DefaultBodyLimit::max(BODY_LIMIT_BYTES))
}

async fn save_artifact<R: tauri::Runtime>(
    State(state): State<ApiState<R>>,
    Path(thread_id): Path<String>,
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
    let result = db::with_conn_result(&state.db, move |conn| {
        artifacts::save_artifact(conn, &root, &tid, &body)
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
        db_path: std::path::PathBuf,
        artifacts_dir: std::path::PathBuf,
        // Keeps the mock app (and its event system) alive for the test body.
        _app: tauri::App<tauri::test::MockRuntime>,
    }

    /// The artifacts-root override env var is process-wide, so it's set
    /// exactly once to a shared per-process root; isolation between the
    /// parallel route tests comes from each harness using a unique project
    /// id (and its own subtree under the shared root).
    fn shared_artifacts_root() -> &'static std::path::Path {
        static ROOT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        ROOT.get_or_init(|| {
            let root = std::env::temp_dir().join(format!(
                "conceptify-test-artifact-roots-{}",
                std::process::id()
            ));
            std::env::set_var("CONCEPTIFY_TEST_ARTIFACTS_DIR", root.as_os_str());
            root
        })
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
