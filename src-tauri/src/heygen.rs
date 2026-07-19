//! HeyGen avatar-video client (video epic conceptify-z9y, bead z9y.4).
//!
//! App-mediated rendering: the HeyGen API key is read from its write-only
//! settings row (`settings::get_heygen_api_key`) by the axum handlers in
//! `server::video_routes` and handed to the functions here — it never leaves
//! the app process, never appears in any response, event, error string, or
//! log line, and is never sent anywhere except `api.heygen.com` (in
//! particular, NOT to the presigned file host serving the finished mp4).
//!
//! # API surface targeted (verified against live docs, 2026-07-19)
//!
//! HeyGen's **v3 platform** (developers.heygen.com). The v2 API this bead's
//! description anticipated has been superseded — v1/v2 remain operational
//! only through 2026-10-31, so building on them now would be dead on arrival:
//!
//! - `POST /v3/videos` — create an avatar video from a script
//!   (`{"type":"avatar","avatar_id",…,"script",…}` → `data.video_id`).
//! - `GET /v3/videos/{video_id}` — status (`pending|processing|waiting|…` →
//!   `completed|failed`); on completion `video_url` is a presigned mp4 URL.
//! - `GET /v3/avatars/looks` — avatar discovery; each look's `id` is the
//!   `avatar_id` to pass when creating a video.
//!
//! Auth is `x-api-key: <key>`. Webhooks (`callback_url`) exist but polling is
//! used instead: the app is a local desktop process with no public ingress.
//!
//! # Failure-mode mapping
//!
//! [`HeygenError`] folds upstream failures into the small set of legible,
//! user-facing messages the bead requires: key rejected / quota exhausted
//! point at Settings; network failures say so plainly. Callers (API/CLI)
//! surface these verbatim so a skill-side fallback (z9y.5) can branch on them.
//!
//! # Storage seam with z9y.6 (recorded design decision)
//!
//! The thread-scoped content-addressed asset store (`PUT
//! /threads/:id/assets/:sha256`, bead z9y.6) is not merged yet, so completed
//! renders take the documented integration-seam path: the mp4 is downloaded
//! and staged **content-addressed** under
//! `~/Documents/conceptify/video-renders/<sha256>.mp4` (temp+rename, PRD N4),
//! and the job-status response returns `{sha256, bytes, filePath}` — exactly
//! the inputs `conceptify save-asset --thread <id> --file <path>` (z9y.6's
//! CLI) needs to register the clip with a thread and obtain its
//! `cfy-asset://` URL. Because the staging file is already named by its
//! sha256, registration is a pure copy/upload; nothing is re-encoded. Once
//! z9y.6 lands, `video_routes` can optionally accept a `threadId` and call
//! the asset-store domain function directly, retiring the staging hop.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

/// Base URL for the HeyGen platform API. Every keyed request goes here and
/// nowhere else.
const HEYGEN_API_BASE: &str = "https://api.heygen.com";

/// Per-request timeouts. Submissions/status checks are small JSON exchanges;
/// the download pulls up to ~20 MiB (the z9y.1 asset budget) from a CDN.
const SUBMIT_TIMEOUT: Duration = Duration::from_secs(30);
const STATUS_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);

/// Requested output geometry: 720p/16:9 mp4 — matches the artifact-spec §1.4
/// budgets fixed by z9y.1 (≤ 1280×720 SHOULD, MP4/H.264 floor), so a finished
/// render is uploadable as an asset without re-encoding.
const RESOLUTION: &str = "720p";
const ASPECT_RATIO: &str = "16:9";

/// How long a fetched avatar list stays served from memory. Avatar rosters
/// change rarely; 5 minutes keeps `list-avatars` snappy during a settings
/// session without ever being meaningfully stale.
const AVATAR_CACHE_TTL: Duration = Duration::from_secs(300);

/// Upstream failures, folded into the legible user-facing categories the bead
/// requires. `Display` strings are shown verbatim by the API/CLI — they must
/// stay actionable and must NEVER contain the API key (no variant carries it).
#[derive(Debug, thiserror::Error)]
pub enum HeygenError {
    /// 401/403 from HeyGen: the stored key is invalid or was revoked.
    #[error(
        "HeyGen rejected the configured API key — update or re-add it in \
         Settings (the key is stored write-only; re-paste it to replace it)"
    )]
    KeyRejected,

    /// 402/429 from HeyGen: plan quota exhausted or rate limited.
    #[error(
        "HeyGen refused the request (quota exhausted or rate limited): {0} — \
         check your HeyGen plan/credits, or try again later; the key can be \
         changed in Settings"
    )]
    QuotaOrRateLimited(String),

    /// Any other non-2xx from HeyGen, with their error message when parseable.
    #[error("HeyGen API error (HTTP {status}): {message}")]
    Api { status: u16, message: String },

    /// Transport-level failure (DNS, TLS, connect, timeout).
    #[error(
        "could not reach HeyGen ({0}) — check your network connection and try \
         again"
    )]
    Network(String),

    /// A 2xx whose body did not match the documented shape.
    #[error("unexpected response from HeyGen: {0}")]
    UnexpectedResponse(String),

    /// Local disk failure while staging a finished render.
    #[error("failed to store the downloaded render locally: {0}")]
    Io(String),
}

impl From<io::Error> for HeygenError {
    fn from(e: io::Error) -> Self {
        HeygenError::Io(e.to_string())
    }
}

/// One render job's upstream status, as reported by `GET /v3/videos/{id}`.
#[derive(Debug, Clone)]
pub struct VideoStatus {
    /// HeyGen's raw status string (`pending`, `processing`, `waiting`,
    /// `completed`, `failed`, …). Callers should branch only on
    /// `completed`/`failed` and treat everything else as in-progress.
    pub status: String,
    /// Presigned mp4 URL, present once `status == "completed"`.
    pub video_url: Option<String>,
    /// Duration in seconds, when reported.
    pub duration: Option<f64>,
    /// Human-readable failure detail, when `status == "failed"`.
    pub failure: Option<String>,
}

/// A finished render staged in local content-addressed storage.
#[derive(Debug, Clone)]
pub struct StagedRender {
    /// SHA-256 of the mp4 bytes, 64 lowercase hex — the asset identity the
    /// z9y.6 store keys on.
    pub sha256: String,
    /// File size in bytes.
    pub bytes: u64,
    /// Absolute path of the staged `<sha256>.mp4`.
    pub file_path: PathBuf,
    /// Duration in seconds, when HeyGen reported one.
    pub duration: Option<f64>,
}

fn client(timeout: Duration) -> Result<reqwest::Client, HeygenError> {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| HeygenError::Network(e.to_string()))
}

/// Fold a non-success HeyGen response into a [`HeygenError`]. Consumes the
/// body to extract their documented `{"error":{"code","message"}}` shape.
async fn error_from_response(resp: reqwest::Response) -> HeygenError {
    let status = resp.status().as_u16();
    let message = match resp.json::<serde_json::Value>().await {
        Ok(v) => v
            .pointer("/error/message")
            .and_then(|m| m.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("HTTP {status}")),
        Err(_) => format!("HTTP {status}"),
    };
    match status {
        401 | 403 => HeygenError::KeyRejected,
        402 | 429 => HeygenError::QuotaOrRateLimited(message),
        _ => HeygenError::Api { status, message },
    }
}

fn transport_error(e: reqwest::Error) -> HeygenError {
    // reqwest::Error's Display can embed the URL but never request headers,
    // so the key cannot leak through this path. Strip to a compact cause.
    let kind = if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "connection failed"
    } else {
        "request failed"
    };
    HeygenError::Network(format!("{kind}: {e}"))
}

/// Submit an avatar render job. Returns HeyGen's `video_id`, which doubles as
/// the app's job id (jobs are stateless app-side: the id alone is enough to
/// poll, and survives an app restart).
pub async fn submit_avatar_job(
    api_key: &str,
    script: &str,
    avatar_id: &str,
    voice_id: Option<&str>,
) -> Result<String, HeygenError> {
    #[derive(Deserialize)]
    struct Data {
        video_id: String,
    }
    #[derive(Deserialize)]
    struct Body {
        data: Data,
    }

    let mut body = json!({
        "type": "avatar",
        "avatar_id": avatar_id,
        "script": script,
        "resolution": RESOLUTION,
        "aspect_ratio": ASPECT_RATIO,
        "output_format": "mp4",
    });
    if let Some(v) = voice_id {
        body["voice_id"] = json!(v);
    }

    let resp = client(SUBMIT_TIMEOUT)?
        .post(format!("{HEYGEN_API_BASE}/v3/videos"))
        .header("x-api-key", api_key)
        .json(&body)
        .send()
        .await
        .map_err(transport_error)?;

    if !resp.status().is_success() {
        return Err(error_from_response(resp).await);
    }
    resp.json::<Body>()
        .await
        .map(|b| b.data.video_id)
        .map_err(|e| HeygenError::UnexpectedResponse(e.to_string()))
}

/// Poll one job's status.
pub async fn fetch_video_status(
    api_key: &str,
    video_id: &str,
) -> Result<VideoStatus, HeygenError> {
    let resp = client(STATUS_TIMEOUT)?
        .get(format!("{HEYGEN_API_BASE}/v3/videos/{video_id}"))
        .header("x-api-key", api_key)
        .send()
        .await
        .map_err(transport_error)?;

    if !resp.status().is_success() {
        return Err(error_from_response(resp).await);
    }
    let v = resp
        .json::<serde_json::Value>()
        .await
        .map_err(|e| HeygenError::UnexpectedResponse(e.to_string()))?;
    parse_video_status(&v)
}

/// Extract a [`VideoStatus`] from a `GET /v3/videos/{id}` body. Split out for
/// unit testing; tolerant of extra fields, strict about `data.status`.
fn parse_video_status(v: &serde_json::Value) -> Result<VideoStatus, HeygenError> {
    let data = v
        .get("data")
        .ok_or_else(|| HeygenError::UnexpectedResponse("missing `data` object".into()))?;
    let status = data
        .get("status")
        .and_then(|s| s.as_str())
        .ok_or_else(|| HeygenError::UnexpectedResponse("missing `data.status`".into()))?
        .to_owned();
    let failure = match (
        data.get("failure_code").and_then(|c| c.as_str()),
        data.get("failure_message").and_then(|m| m.as_str()),
    ) {
        (Some(c), Some(m)) => Some(format!("{m} ({c})")),
        (None, Some(m)) => Some(m.to_owned()),
        (Some(c), None) => Some(c.to_owned()),
        (None, None) => None,
    };
    Ok(VideoStatus {
        status,
        video_url: data
            .get("video_url")
            .and_then(|u| u.as_str())
            .map(str::to_owned),
        duration: data.get("duration").and_then(serde_json::Value::as_f64),
        failure,
    })
}

/// Root of the local render-staging area:
/// `~/Documents/conceptify/video-renders` in production, a sibling of the
/// per-process scratch artifacts root under `cfg(test)` (reusing
/// `artifacts_root`'s structural test isolation — a test can never touch the
/// real Documents tree). Created if missing.
fn video_renders_root() -> io::Result<PathBuf> {
    let artifacts = crate::artifacts::artifacts_root()?;
    let base = artifacts
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or(artifacts);
    let dir = base.join("video-renders");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Lowercase-hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Download a completed render from its presigned URL and stage it
/// content-addressed (`<sha256>.mp4`, temp+rename per PRD N4; an existing
/// file with the same hash is left untouched — content addressing makes the
/// stage idempotent, and a crash mid-download leaves at most a `.tmp`
/// sibling, never a corrupt destination).
///
/// Deliberately sends **no** `x-api-key` header: the URL is presigned and the
/// file host is not the API host — the key goes to `api.heygen.com` only.
pub async fn download_and_stage(
    video_url: &str,
    duration: Option<f64>,
) -> Result<StagedRender, HeygenError> {
    let resp = client(DOWNLOAD_TIMEOUT)?
        .get(video_url)
        .send()
        .await
        .map_err(transport_error)?;
    if !resp.status().is_success() {
        return Err(HeygenError::Api {
            status: resp.status().as_u16(),
            message: "download of the finished render failed (the presigned \
                      URL may have expired; poll the job again)"
                .into(),
        });
    }
    let bytes = resp.bytes().await.map_err(transport_error)?;

    let sha256 = sha256_hex(&bytes);
    let file_path = video_renders_root()?.join(format!("{sha256}.mp4"));
    if !file_path.exists() {
        crate::artifacts::atomic_write(&file_path, &bytes)?;
    }
    Ok(StagedRender {
        sha256,
        bytes: bytes.len() as u64,
        file_path,
        duration,
    })
}

// --- In-memory session caches ----------------------------------------------
//
// Module-level statics rather than fields on `ApiState`: the app is
// single-instance (net.rs occupant detection), the data is a pure
// avoid-repeat-work cache, and keeping it here leaves the shared server state
// untouched (z9y.6 is extending those files in parallel). Losing the caches
// on restart is harmless: a re-poll of a completed job simply re-downloads
// into the same content-addressed path.

/// Completed-job memo: video_id → staged result, so repeated polls of a
/// finished job (and the CLI's final re-read) don't re-download the mp4.
fn completed_jobs() -> &'static Mutex<HashMap<String, StagedRender>> {
    static CACHE: OnceLock<Mutex<HashMap<String, StagedRender>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

pub fn cached_completed(video_id: &str) -> Option<StagedRender> {
    completed_jobs()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(video_id)
        .cloned()
}

pub fn cache_completed(video_id: &str, staged: StagedRender) {
    completed_jobs()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(video_id.to_owned(), staged);
}

/// One avatar look, shaped for discovery (`conceptify list-avatars`): the
/// `id` is exactly the `avatar_id` to pass when rendering.
#[derive(Debug, Clone)]
pub struct AvatarLook {
    pub id: String,
    pub name: String,
    pub avatar_type: Option<String>,
    pub gender: Option<String>,
    pub preview_image_url: Option<String>,
    pub default_voice_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AvatarRoster {
    pub avatars: Vec<AvatarLook>,
    /// True when HeyGen reported more pages beyond the first `limit=50`.
    pub has_more: bool,
}

/// Avatar-roster cache entry. The key fingerprint (sha256 of the key — never
/// the key itself) scopes the cache to the credential that fetched it, so
/// swapping keys in Settings can't serve another account's roster.
struct AvatarCacheEntry {
    fetched_at: Instant,
    key_fingerprint: String,
    roster: AvatarRoster,
}

fn avatar_cache() -> &'static Mutex<Option<AvatarCacheEntry>> {
    static CACHE: OnceLock<Mutex<Option<AvatarCacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

/// List available avatar looks (first page, 50), served from a 5-minute
/// in-memory cache so repeated discovery calls don't hammer HeyGen.
pub async fn list_avatars(api_key: &str) -> Result<AvatarRoster, HeygenError> {
    let fingerprint = sha256_hex(api_key.as_bytes());
    {
        let cache = avatar_cache().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = cache.as_ref() {
            if entry.key_fingerprint == fingerprint
                && entry.fetched_at.elapsed() < AVATAR_CACHE_TTL
            {
                return Ok(entry.roster.clone());
            }
        }
    }

    let resp = client(STATUS_TIMEOUT)?
        .get(format!("{HEYGEN_API_BASE}/v3/avatars/looks?limit=50"))
        .header("x-api-key", api_key)
        .send()
        .await
        .map_err(transport_error)?;
    if !resp.status().is_success() {
        return Err(error_from_response(resp).await);
    }
    let v = resp
        .json::<serde_json::Value>()
        .await
        .map_err(|e| HeygenError::UnexpectedResponse(e.to_string()))?;
    let roster = parse_avatar_roster(&v)?;

    *avatar_cache().lock().unwrap_or_else(|p| p.into_inner()) = Some(AvatarCacheEntry {
        fetched_at: Instant::now(),
        key_fingerprint: fingerprint,
        roster: roster.clone(),
    });
    Ok(roster)
}

/// Extract the roster from a `GET /v3/avatars/looks` body. Tolerant: an entry
/// without an `id` is skipped rather than failing the listing.
fn parse_avatar_roster(v: &serde_json::Value) -> Result<AvatarRoster, HeygenError> {
    let data = v
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| HeygenError::UnexpectedResponse("missing `data` array".into()))?;
    let opt = |e: &serde_json::Value, k: &str| {
        e.get(k).and_then(|s| s.as_str()).map(str::to_owned)
    };
    let avatars = data
        .iter()
        .filter_map(|e| {
            Some(AvatarLook {
                id: opt(e, "id")?,
                name: opt(e, "name").unwrap_or_default(),
                avatar_type: opt(e, "avatar_type"),
                gender: opt(e, "gender"),
                preview_image_url: opt(e, "preview_image_url"),
                default_voice_id: opt(e, "default_voice_id"),
            })
        })
        .collect();
    Ok(AvatarRoster {
        avatars,
        has_more: v.get("has_more").and_then(|b| b.as_bool()).unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_matches_known_vector() {
        // SHA-256("") — the canonical empty-input vector.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(sha256_hex(b"abc").len(), 64);
    }

    #[test]
    fn error_messages_are_settings_pointing_and_keyless() {
        // The bead's failure-mode contract: key/quota errors name Settings,
        // and no variant can carry the key (KeyRejected is payload-free).
        let e = HeygenError::KeyRejected.to_string();
        assert!(e.contains("Settings"), "{e}");
        let e = HeygenError::QuotaOrRateLimited("credits exhausted".into()).to_string();
        assert!(e.contains("Settings"), "{e}");
        assert!(e.contains("credits exhausted"), "{e}");
        let e = HeygenError::Network("connection failed: dns error".into()).to_string();
        assert!(e.contains("network"), "{e}");
    }

    #[test]
    fn parse_video_status_completed_and_failed() {
        let completed = serde_json::json!({
            "data": {
                "id": "vid_1",
                "status": "completed",
                "video_url": "https://files.example/vid_1.mp4",
                "duration": 32.5
            }
        });
        let s = parse_video_status(&completed).unwrap();
        assert_eq!(s.status, "completed");
        assert_eq!(s.video_url.as_deref(), Some("https://files.example/vid_1.mp4"));
        assert_eq!(s.duration, Some(32.5));
        assert!(s.failure.is_none());

        let failed = serde_json::json!({
            "data": {
                "status": "failed",
                "failure_code": "MODERATION",
                "failure_message": "script rejected"
            }
        });
        let s = parse_video_status(&failed).unwrap();
        assert_eq!(s.status, "failed");
        assert_eq!(s.failure.as_deref(), Some("script rejected (MODERATION)"));

        let bogus = serde_json::json!({ "data": {} });
        assert!(parse_video_status(&bogus).is_err());
    }

    #[test]
    fn parse_avatar_roster_skips_idless_entries() {
        let v = serde_json::json!({
            "data": [
                { "id": "lk_1", "name": "Suit", "gender": "female",
                  "default_voice_id": "vc_9" },
                { "name": "broken entry with no id" }
            ],
            "has_more": true
        });
        let roster = parse_avatar_roster(&v).unwrap();
        assert_eq!(roster.avatars.len(), 1);
        assert_eq!(roster.avatars[0].id, "lk_1");
        assert_eq!(roster.avatars[0].default_voice_id.as_deref(), Some("vc_9"));
        assert!(roster.has_more);
    }

    #[test]
    fn completed_job_cache_round_trips() {
        let staged = StagedRender {
            sha256: "ab".repeat(32),
            bytes: 42,
            file_path: PathBuf::from("/tmp/x.mp4"),
            duration: Some(12.0),
        };
        cache_completed("vid_cache_test", staged.clone());
        let got = cached_completed("vid_cache_test").expect("cached");
        assert_eq!(got.sha256, staged.sha256);
        assert_eq!(got.bytes, 42);
        assert!(cached_completed("vid_absent").is_none());
    }
}
