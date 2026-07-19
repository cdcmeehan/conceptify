//! Video-asset HTTP route (epic conceptify-z9y, artifact-spec §1.4/§8.3).
//!
//! `PUT /api/v1/threads/{thread_id}/assets/{sha256}` — the save-asset upload
//! path. The request body is the **raw clip bytes** (send `Content-Type:
//! video/mp4`; not enforced), not JSON/multipart — same rationale as
//! save-artifact: nothing but the bytes, and the CLI just streams the file.
//! Validation (`E-ASSET-HASH`/`E-ASSET-SIZE`/`E-ASSET-TYPE`/
//! `E-ASSET-DURATION`, warnings `W-ASSET-RES`/`W-ASSET-LONG`) and storage
//! live in `crate::assets`; error/warning shapes match save-artifact.
//!
//! Side effects: **none** — no thread status change, no event. The
//! `artifact-updated` event on the *subsequent* save-artifact remains the
//! viewer's refresh trigger (docs/api.md).

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Json, Router};
use serde_json::json;

use conceptify_types::{ArtifactIssue, SaveArtifactErrorResponse, SaveAssetResponse};

use crate::assets::{self, AssetError};
use crate::{artifacts, db};

use super::state::ApiState;

/// Transport cap, shared rationale with save-artifact: comfortably above the
/// 20 MiB spec cap (`E-ASSET-SIZE`) so oversized-but-plausible bodies get the
/// structured spec error rather than a bare 413.
const BODY_LIMIT_BYTES: usize = 60 * 1024 * 1024;

pub fn router<R: tauri::Runtime>() -> Router<ApiState<R>> {
    Router::new()
        .route(
            "/threads/{thread_id}/assets/{sha256}",
            axum::routing::put(save_asset),
        )
        .layer(DefaultBodyLimit::max(BODY_LIMIT_BYTES))
}

async fn save_asset<R: tauri::Runtime>(
    State(state): State<ApiState<R>>,
    Path((thread_id, sha256)): Path<(String, String)>,
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

    let result = db::with_conn_result(&state.db, move |conn| {
        assets::save_asset(conn, &root, &thread_id, &sha256, &body)
    })
    .await;

    match result {
        Ok(saved) => {
            let response = SaveAssetResponse {
                thread_id: saved.thread_id,
                sha256: saved.sha256,
                bytes: saved.bytes,
                url: saved.url,
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
        Err(AssetError::ThreadNotFound(id)) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("thread not found: {}", id) })),
        )
            .into_response(),
        Err(AssetError::Rejected(errors)) => {
            // Spec §8.3: same error shape as save-artifact — { error, code }
            // for the first violated rule, `errors` listing all of them.
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
        Err(AssetError::Io(e)) => {
            eprintln!("[conceptify-server] save_asset io error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to write asset to disk" })),
            )
                .into_response()
        }
        Err(e) => {
            eprintln!("[conceptify-server] save_asset error: {e}");
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
    use tower::ServiceExt;

    use super::super::routes;
    use super::super::state::ApiState;
    use crate::assets::tests::{TINY_MP4, TINY_MP4_AAC};
    use crate::assets::sha256_hex;

    const TOKEN: &str = "test-token";

    /// Full-stack harness: real migrations, mock Tauri app, the real
    /// `build_router` (auth middleware included) — same pattern as
    /// `artifacts_routes::tests`.
    struct Harness {
        router: axum::Router,
        db: crate::db::DbHandle,
        thread_id: String,
        project_id: String,
        slug: String,
        db_path: std::path::PathBuf,
        artifacts_dir: std::path::PathBuf,
        _app: tauri::App<tauri::test::MockRuntime>,
    }

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
        let db_path = std::env::temp_dir().join(format!("conceptify-test-assets-{unique}.db"));
        let project_id = format!("proj-{unique}");
        let artifacts_dir = shared_artifacts_root().join(&project_id);

        let db = crate::db::init_at(&db_path).expect("test db should init");
        let (thread_id, slug) = {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO projects (id, name, root_path) VALUES (?1, 'Proj', ?2)",
                [&project_id, &format!("/tmp/{unique}")],
            )
            .unwrap();
            let t = crate::threads::create_thread(&conn, &project_id, "Asset Test", "q").unwrap();
            (t.id, t.slug)
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
            thread_id,
            project_id,
            slug,
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

    fn put_asset(thread_id: &str, sha: &str, body: &[u8], token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("PUT")
            .uri(format!("/api/v1/threads/{thread_id}/assets/{sha}"))
            .header("content-type", "video/mp4");
        if let Some(t) = token {
            builder = builder.header("authorization", format!("Bearer {t}"));
        }
        builder.body(Body::from(body.to_vec())).unwrap()
    }

    fn post_artifact(thread_id: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(format!("/api/v1/threads/{thread_id}/artifact"))
            .header("content-type", "text/html")
            .header("authorization", format!("Bearer {TOKEN}"))
            .body(Body::from(body.to_owned()))
            .unwrap()
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// A spec-conformant artifact carrying one complete §1.4 cfy-video figure
    /// referencing `url`.
    fn video_artifact(version: i64, url: &str) -> String {
        format!(
            r##"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<title>Video artifact</title>
<meta name="cfy:question" content="q">
<meta name="cfy:version" content="{version}">
<meta name="cfy:generated-by" content="claude-code/test">
</head><body>
<h1 data-cfy-id="sec-video">Video</h1>
<figure class="cfy-video" data-cfy-id="vid-demo">
  <video controls preload="metadata" playsinline
         poster="data:image/jpeg;base64,/9j/4AAQ"
         src="{url}"></video>
  <details class="cfy-details cfy-video-transcript">
    <summary>Transcript</summary>
    <p>The full narration, verbatim, for readers who never play the clip.</p>
  </details>
  <figcaption><strong>One request, start to finish.</strong></figcaption>
</figure>
</body></html>"##
        )
    }

    #[tokio::test]
    async fn upload_requires_auth() {
        let h = harness("auth");
        let sha = sha256_hex(TINY_MP4);
        let res = h
            .router
            .clone()
            .oneshot(put_asset(&h.thread_id, &sha, TINY_MP4, None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn upload_stores_and_is_idempotent() {
        let h = harness("store");
        let sha = sha256_hex(TINY_MP4);

        let res = h
            .router
            .clone()
            .oneshot(put_asset(&h.thread_id, &sha, TINY_MP4, Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let json = body_json(res).await;
        assert_eq!(json["thread_id"], h.thread_id.as_str());
        assert_eq!(json["sha256"], sha.as_str());
        assert_eq!(json["bytes"], TINY_MP4.len() as u64);
        assert_eq!(
            json["url"],
            format!("cfy-asset://localhost/{}/{sha}.mp4", h.thread_id)
        );
        assert_eq!(json["warnings"].as_array().unwrap().len(), 0);

        let file = crate::assets::asset_file_path(
            shared_artifacts_root(),
            &h.project_id,
            &h.slug,
            &sha,
        );
        assert_eq!(std::fs::read(&file).unwrap(), TINY_MP4);

        // Idempotent re-upload: 200, file untouched.
        let mtime = std::fs::metadata(&file).unwrap().modified().unwrap();
        let res = h
            .router
            .clone()
            .oneshot(put_asset(&h.thread_id, &sha, TINY_MP4, Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), mtime);
    }

    #[tokio::test]
    async fn upload_validation_failures_are_structured_422s() {
        let h = harness("reject");

        // Hash mismatch.
        let wrong_sha = "a".repeat(64);
        let res = h
            .router
            .clone()
            .oneshot(put_asset(&h.thread_id, &wrong_sha, TINY_MP4, Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let json = body_json(res).await;
        assert_eq!(json["code"], "E-ASSET-HASH");
        assert_eq!(json["errors"][0]["code"], "E-ASSET-HASH");

        // Not an MP4 (hash correct, so the type rule is what trips).
        let garbage = b"<!doctype html><p>not a video</p>";
        let res = h
            .router
            .clone()
            .oneshot(put_asset(&h.thread_id, &sha256_hex(garbage), garbage, Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let json = body_json(res).await;
        assert_eq!(json["code"], "E-ASSET-TYPE");

        // Nothing stored.
        let assets_dir =
            crate::assets::assets_dir(shared_artifacts_root(), &h.project_id, &h.slug);
        assert!(!assets_dir.exists());
    }

    #[tokio::test]
    async fn upload_unknown_thread_is_404() {
        let h = harness("ghost");
        let sha = sha256_hex(TINY_MP4);
        let res = h
            .router
            .clone()
            .oneshot(put_asset("ghost", &sha, TINY_MP4, Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    /// The z9y.6 verification checkpoint: the full upload → save-artifact →
    /// serve pipeline over the real router. Publishes a real (ffmpeg-encoded)
    /// clip as a demo video artifact, asserts the §1.4 validator accepts it
    /// with zero warnings, asserts E-ASSET-REF blocks un-uploaded/foreign
    /// references, and drives the cfy-asset protocol handler with the
    /// AVFoundation-shaped range requests the z9y.1 prototype recorded
    /// (bytes=0-1 probe first, then bounded reads), asserting byte-exact 206s.
    #[tokio::test]
    async fn video_artifact_end_to_end_upload_validate_serve() {
        let h = harness("e2e-video");
        let sha = sha256_hex(TINY_MP4_AAC);
        let url = format!("cfy-asset://localhost/{}/{sha}.mp4", h.thread_id);

        // 1. Referencing the asset before upload is E-ASSET-REF (nothing saved).
        let res = h
            .router
            .clone()
            .oneshot(post_artifact(&h.thread_id, &video_artifact(1, &url)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let json = body_json(res).await;
        assert_eq!(json["code"], "E-ASSET-REF");

        // 2. Upload the clip (AAC variant — exercises the audio rules too).
        let res = h
            .router
            .clone()
            .oneshot(put_asset(&h.thread_id, &sha, TINY_MP4_AAC, Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let upload = body_json(res).await;
        assert_eq!(upload["url"], url.as_str());

        // 3. Now the same artifact saves cleanly — and with ZERO warnings:
        //    the complete §1.4 figure (poster + transcript + caption, no
        //    autoplay) trips none of the W-VIDEO-* rules.
        let res = h
            .router
            .clone()
            .oneshot(post_artifact(&h.thread_id, &video_artifact(1, &url)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let json = body_json(res).await;
        assert_eq!(json["version"], 1);
        assert_eq!(json["warnings"].as_array().unwrap().len(), 0, "{json}");

        // 4. A reference to a *different* thread's asset is E-ASSET-REF even
        //    though the sha exists (for the other thread).
        let foreign_url = format!("cfy-asset://localhost/other-thread/{sha}.mp4");
        let res = h
            .router
            .clone()
            .oneshot(post_artifact(&h.thread_id, &video_artifact(2, &foreign_url)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let json = body_json(res).await;
        assert_eq!(json["code"], "E-ASSET-REF");

        // 5. Serve the clip through the cfy-asset protocol handler with the
        //    prototype-recorded request shapes, against the same DB + root.
        let conn = h.db.lock().unwrap();
        let root = shared_artifacts_root();
        let path = format!("/{}/{sha}.mp4", h.thread_id);
        let len = TINY_MP4_AAC.len();

        // AVF's existential probe.
        let res = crate::asset_protocol::respond(
            &conn,
            root,
            &axum::http::Method::GET,
            &path,
            Some("bytes=0-1"),
        );
        assert_eq!(res.status(), 206);
        assert_eq!(res.body().as_slice(), &TINY_MP4_AAC[..2]);
        assert_eq!(
            res.headers().get("content-range").unwrap(),
            &format!("bytes 0-1/{len}")
        );

        // A typical bounded moov read, then a tail read (seek pattern).
        let res = crate::asset_protocol::respond(
            &conn,
            root,
            &axum::http::Method::GET,
            &path,
            Some("bytes=32-4095"),
        );
        assert_eq!(res.status(), 206);
        assert_eq!(res.body().as_slice(), &TINY_MP4_AAC[32..4096]);

        let res = crate::asset_protocol::respond(
            &conn,
            root,
            &axum::http::Method::GET,
            &path,
            Some(&format!("bytes={}-", len - 512)),
        );
        assert_eq!(res.status(), 206);
        assert_eq!(res.body().as_slice(), &TINY_MP4_AAC[len - 512..]);

        // Past-EOF seek → 416 so AVF can recover.
        let res = crate::asset_protocol::respond(
            &conn,
            root,
            &axum::http::Method::GET,
            &path,
            Some(&format!("bytes={}-", len + 1)),
        );
        assert_eq!(res.status(), 416);
    }

    /// The W-VIDEO-* warnings fire (and do not block) when the figure is
    /// incomplete: stripped poster/transcript/caption + autoplay yields all
    /// four warnings over HTTP while still saving.
    #[tokio::test]
    async fn incomplete_video_figure_warns_but_saves() {
        let h = harness("video-warns");
        let sha = sha256_hex(TINY_MP4);
        let url = format!("cfy-asset://localhost/{}/{sha}.mp4", h.thread_id);
        let res = h
            .router
            .clone()
            .oneshot(put_asset(&h.thread_id, &sha, TINY_MP4, Some(TOKEN)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let bare = video_artifact(1, &url)
            .replace(" poster=\"data:image/jpeg;base64,/9j/4AAQ\"", " autoplay")
            .replace(
                r#"<details class="cfy-details cfy-video-transcript">
    <summary>Transcript</summary>
    <p>The full narration, verbatim, for readers who never play the clip.</p>
  </details>"#,
                "",
            )
            .replace(
                "<figcaption><strong>One request, start to finish.</strong></figcaption>",
                "",
            );
        let res = h
            .router
            .clone()
            .oneshot(post_artifact(&h.thread_id, &bare))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let json = body_json(res).await;
        let codes: Vec<&str> = json["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap())
            .collect();
        for expected in [
            "W-VIDEO-POSTER",
            "W-VIDEO-TRANSCRIPT",
            "W-VIDEO-CAPTION",
            "W-VIDEO-AUTOPLAY",
        ] {
            assert!(codes.contains(&expected), "missing {expected}: {codes:?}");
        }
    }
}
