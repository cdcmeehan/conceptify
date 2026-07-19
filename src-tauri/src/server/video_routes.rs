//! Avatar render-job HTTP routes (video epic conceptify-z9y, bead z9y.4).
//!
//! App-mediated HeyGen rendering, all under the bearer-token middleware like
//! every other `/api/v1` route:
//!
//! - `POST /video/avatar-jobs` — submit a render (script + avatar/voice, with
//!   settings-row defaults) → `{ jobId, status: "submitted" }`.
//! - `GET  /video/avatar-jobs/{id}` — poll; on completion downloads the mp4
//!   into local content-addressed staging and returns
//!   `{ sha256, bytes, filePath }` (the z9y.6 `save-asset` inputs — see the
//!   storage-seam note in `crate::heygen`).
//! - `GET  /video/avatars` — cached avatar discovery.
//!
//! # Key hygiene
//!
//! The HeyGen key is read from its write-only settings row inside each
//! handler, handed to `crate::heygen`, and dropped — it never appears in a
//! response body, error string, event, or log line, and the endpoints are
//! deliberately explicit-invocation only (no implicit/automatic triggering;
//! the ALWAYS-confirm-before-paid-render UX lives in the skill, z9y.5). With
//! no key configured every endpoint fails fast with `412` and a
//! Settings-pointing message, before any network I/O.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde_json::json;

use conceptify_types::{
    AvatarJobStatusResponse, AvatarListItem, AvatarListResponse, CreateAvatarJobRequest,
    CreateAvatarJobResponse,
};

use crate::{db, heygen, settings};

use super::state::ApiState;

pub fn router<R: tauri::Runtime>() -> Router<ApiState<R>> {
    Router::new()
        .route("/video/avatar-jobs", axum::routing::post(create_job))
        .route("/video/avatar-jobs/{id}", axum::routing::get(get_job))
        .route("/video/avatars", axum::routing::get(list_avatars))
}

/// The absent-key failure every endpoint here shares: `412 Precondition
/// Failed` with the Settings pointer the bead requires. Chosen over `400`
/// so a caller (skill/CLI) can branch on "feature not configured" vs "bad
/// request" without string matching.
fn no_key_response() -> Response {
    (
        StatusCode::PRECONDITION_FAILED,
        Json(json!({
            "error": "no HeyGen API key is configured — avatar rendering is \
                      disabled. Add a key in Settings to enable it."
        })),
    )
        .into_response()
}

fn bad_request(message: String) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
}

/// Map an upstream [`heygen::HeygenError`] onto a response. Everything is
/// `502 Bad Gateway` — the local API worked; the upstream call did not — with
/// the error's already-legible, Settings-pointing message as the body.
fn upstream_error(e: heygen::HeygenError) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({ "error": e.to_string() })),
    )
        .into_response()
}

/// Everything the handlers need from settings, read in one blocking hop.
struct RenderConfig {
    key: Option<String>,
    default_avatar: Option<String>,
    default_voice: Option<String>,
}

async fn render_config<R: tauri::Runtime>(
    state: &ApiState<R>,
) -> Result<RenderConfig, Response> {
    db::with_conn_result(&state.db, |conn| {
        Ok::<_, settings::SettingsError>(RenderConfig {
            key: settings::get_heygen_api_key(conn)?,
            default_avatar: settings::get_heygen_default_avatar_id(conn)?,
            default_voice: settings::get_heygen_default_voice_id(conn)?,
        })
    })
    .await
    .map_err(|e| {
        eprintln!("[conceptify-server] heygen settings read failed: {e}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "settings error" })),
        )
            .into_response()
    })
}

/// `POST /video/avatar-jobs` — validate, resolve avatar/voice against the
/// settings defaults, submit to HeyGen, return the job id immediately
/// (rendering is async; poll the GET).
async fn create_job<R: tauri::Runtime>(
    State(state): State<ApiState<R>>,
    Json(req): Json<CreateAvatarJobRequest>,
) -> Response {
    let config = match render_config(&state).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let Some(key) = config.key else {
        return no_key_response();
    };

    let script = req.script.trim();
    if script.is_empty() {
        return bad_request("script must not be empty".into());
    }
    let avatar_id = match req.avatar_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(a) => a.to_owned(),
        None => match config.default_avatar {
            Some(a) => a,
            None => {
                return bad_request(
                    "no avatarId given and no default avatar is configured — \
                     pass avatarId, or set a default avatar in Settings \
                     (discover ids with `conceptify list-avatars`)"
                        .into(),
                )
            }
        },
    };
    let voice_id = req
        .voice_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or(config.default_voice);

    match heygen::submit_avatar_job(&key, script, &avatar_id, voice_id.as_deref()).await {
        Ok(video_id) => Json(CreateAvatarJobResponse {
            job_id: video_id,
            status: "submitted".into(),
        })
        .into_response(),
        Err(e) => upstream_error(e),
    }
}

/// Job ids are upstream-issued opaque tokens; constrain the charset before
/// interpolating one into an upstream URL path (same strict-segment hygiene
/// as the artifact protocol handlers).
fn valid_job_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn completed_response(job_id: &str, staged: &heygen::StagedRender) -> Response {
    Json(AvatarJobStatusResponse {
        job_id: job_id.to_owned(),
        status: "completed".into(),
        heygen_status: None,
        sha256: Some(staged.sha256.clone()),
        bytes: Some(staged.bytes),
        file_path: Some(staged.file_path.to_string_lossy().into_owned()),
        duration_seconds: staged.duration,
        error: None,
    })
    .into_response()
}

/// `GET /video/avatar-jobs/{id}` — poll HeyGen; on first observed completion
/// download + stage the mp4 (idempotent, content-addressed) and memoize so
/// later polls answer from memory.
async fn get_job<R: tauri::Runtime>(
    State(state): State<ApiState<R>>,
    Path(id): Path<String>,
) -> Response {
    if !valid_job_id(&id) {
        return bad_request("invalid job id".into());
    }

    // A job finished earlier in this session never re-hits the network.
    if let Some(staged) = heygen::cached_completed(&id) {
        return completed_response(&id, &staged);
    }

    let config = match render_config(&state).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let Some(key) = config.key else {
        return no_key_response();
    };

    let status = match heygen::fetch_video_status(&key, &id).await {
        Ok(s) => s,
        Err(e) => return upstream_error(e),
    };

    match status.status.as_str() {
        "completed" => {
            let Some(url) = status.video_url else {
                return upstream_error(heygen::HeygenError::UnexpectedResponse(
                    "completed job carried no video_url".into(),
                ));
            };
            match heygen::download_and_stage(&url, status.duration).await {
                Ok(staged) => {
                    heygen::cache_completed(&id, staged.clone());
                    completed_response(&id, &staged)
                }
                Err(e) => upstream_error(e),
            }
        }
        "failed" => Json(AvatarJobStatusResponse {
            job_id: id,
            status: "failed".into(),
            heygen_status: None,
            sha256: None,
            bytes: None,
            file_path: None,
            duration_seconds: None,
            error: Some(
                status
                    .failure
                    .unwrap_or_else(|| "HeyGen reported the render as failed".into()),
            ),
        })
        .into_response(),
        other => Json(AvatarJobStatusResponse {
            job_id: id,
            status: "processing".into(),
            heygen_status: Some(other.to_owned()),
            sha256: None,
            bytes: None,
            file_path: None,
            duration_seconds: None,
            error: None,
        })
        .into_response(),
    }
}

/// `GET /video/avatars` — cached passthrough of HeyGen's avatar-look listing.
async fn list_avatars<R: tauri::Runtime>(State(state): State<ApiState<R>>) -> Response {
    let config = match render_config(&state).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let Some(key) = config.key else {
        return no_key_response();
    };

    match heygen::list_avatars(&key).await {
        Ok(roster) => Json(AvatarListResponse {
            avatars: roster
                .avatars
                .into_iter()
                .map(|a| AvatarListItem {
                    id: a.id,
                    name: a.name,
                    avatar_type: a.avatar_type,
                    gender: a.gender,
                    preview_image_url: a.preview_image_url,
                    default_voice_id: a.default_voice_id,
                })
                .collect(),
            has_more: roster.has_more,
        })
        .into_response(),
        Err(e) => upstream_error(e),
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

    /// Router harness: real migrations on a throwaway on-disk DB, mock Tauri
    /// app, the real `build_router` (auth middleware included). No HeyGen
    /// network calls happen in these tests — every asserted path fails before
    /// upstream I/O (absent key, invalid input).
    struct Harness {
        router: axum::Router,
        db: crate::db::DbHandle,
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
        let db_path = std::env::temp_dir().join(format!("conceptify-test-video-{unique}.db"));
        let db = crate::db::init_at(&db_path).expect("test db should init");

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let app_handle = app.handle().clone();

        let router = routes::build_router(ApiState {
            app_handle,
            token: TOKEN.into(),
            db: db.clone(),
        });

        Harness {
            router,
            db,
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

    fn store_key(h: &Harness, key: &str) {
        let conn = h.db.lock().unwrap();
        crate::settings::set_heygen_api_key(&conn, Some(key)).unwrap();
    }

    #[tokio::test]
    async fn all_video_routes_require_the_bearer_token() {
        let h = harness("auth");
        for (method, uri) in [
            ("POST", "/api/v1/video/avatar-jobs"),
            ("GET", "/api/v1/video/avatar-jobs/vid_1"),
            ("GET", "/api/v1/video/avatars"),
        ] {
            let body = (method == "POST").then(|| json!({ "script": "hi" }));
            let resp = h
                .router
                .clone()
                .oneshot(req(method, uri, None, body))
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri} must be bearer-authed"
            );
        }
    }

    #[tokio::test]
    async fn absent_key_fails_cleanly_with_settings_pointer_on_every_route() {
        // The bead's absent-key acceptance criterion, verified at the HTTP
        // boundary: no key stored → 412 + a message naming Settings, no
        // panic, no opaque upstream error, no network I/O.
        let h = harness("nokey");
        for (method, uri, body) in [
            (
                "POST",
                "/api/v1/video/avatar-jobs",
                Some(json!({ "script": "hello", "avatarId": "lk_1" })),
            ),
            ("GET", "/api/v1/video/avatar-jobs/vid_1", None),
            ("GET", "/api/v1/video/avatars", None),
        ] {
            let resp = h
                .router
                .clone()
                .oneshot(req(method, uri, Some(TOKEN), body))
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::PRECONDITION_FAILED,
                "{method} {uri}"
            );
            let v = body_json(resp).await;
            let msg = v["error"].as_str().unwrap();
            assert!(msg.contains("Settings"), "{msg}");
            assert!(msg.contains("HeyGen API key"), "{msg}");
        }
    }

    #[tokio::test]
    async fn blank_script_is_rejected_before_any_upstream_call() {
        let h = harness("script");
        store_key(&h, "hg_test_key");
        let resp = h
            .router
            .clone()
            .oneshot(req(
                "POST",
                "/api/v1/video/avatar-jobs",
                Some(TOKEN),
                Some(json!({ "script": "   ", "avatarId": "lk_1" })),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let v = body_json(resp).await;
        assert!(v["error"].as_str().unwrap().contains("script"));
    }

    #[tokio::test]
    async fn missing_avatar_with_no_default_is_actionable() {
        let h = harness("avatar");
        store_key(&h, "hg_test_key");
        let resp = h
            .router
            .clone()
            .oneshot(req(
                "POST",
                "/api/v1/video/avatar-jobs",
                Some(TOKEN),
                Some(json!({ "script": "hello" })),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let v = body_json(resp).await;
        let msg = v["error"].as_str().unwrap();
        assert!(msg.contains("avatarId"), "{msg}");
        assert!(msg.contains("list-avatars"), "{msg}");
    }

    #[tokio::test]
    async fn malformed_job_id_is_rejected_without_leaking_anything() {
        let h = harness("jobid");
        store_key(&h, "hg_test_key");
        let resp = h
            .router
            .clone()
            .oneshot(req(
                "GET",
                "/api/v1/video/avatar-jobs/..%2F..%2Fetc",
                Some(TOKEN),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let v = body_json(resp).await;
        // The clean-input error must never echo the stored key.
        assert!(!v.to_string().contains("hg_test_key"));
    }

    #[tokio::test]
    async fn error_paths_never_echo_the_stored_key() {
        // Belt-and-braces sweep over every offline-reachable response on
        // these routes with a key configured: none may contain the raw key.
        let h = harness("noecho");
        store_key(&h, "hg_SUPER_SECRET_KEY");
        for (method, uri, body) in [
            (
                "POST",
                "/api/v1/video/avatar-jobs",
                Some(json!({ "script": "" })),
            ),
            ("GET", "/api/v1/video/avatar-jobs/bad%20id", None),
        ] {
            let resp = h
                .router
                .clone()
                .oneshot(req(method, uri, Some(TOKEN), body))
                .await
                .unwrap();
            let v = body_json(resp).await;
            assert!(
                !v.to_string().contains("SUPER_SECRET"),
                "{method} {uri} leaked the key: {v}"
            );
        }
    }
}
