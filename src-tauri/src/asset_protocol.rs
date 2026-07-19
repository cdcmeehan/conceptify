//! The `cfy-asset://` custom URI scheme (epic conceptify-z9y, artifact-spec
//! §1.4) — Range-capable video-asset delivery into the sandboxed viewer.
//!
//! Serves the content-addressed clips stored by `crate::assets` to `<video>`
//! elements inside artifacts. A *second* scheme (distinct from `artifact://`)
//! keeps the served document and its media cross-origin from each other and
//! from the app shell; the viewer CSP admits it via
//! `media-src cfy-asset://localhost` (`artifact_protocol::CSP`).
//!
//! ## URL contract (spec §1.4)
//!
//! ```text
//! cfy-asset://localhost/<thread-id>/<sha256>.mp4
//! ```
//!
//! - `<thread-id>` — `[A-Za-z0-9_-]{1,128}`, resolved to (project, slug) via
//!   the DB exactly like `artifact_protocol`.
//! - `<sha256>` — 64 lowercase hex; the file name under the thread's
//!   `assets/` dir. No percent-decoding anywhere on this scheme.
//! - GET only; anything else is 405.
//!
//! ## Range semantics (the load-bearing part)
//!
//! The z9y.1 prototype (`prototypes/wkwebview-video-range/`) established that
//! Range support here is **existential**: AVFoundation's first media request
//! is `Range: bytes=0-1`, and with no 206 answer nothing plays at all. During
//! playback it issues hundreds of small bounded range GETs (never open-ended,
//! never HEAD). This handler therefore implements single-range HTTP semantics
//! matching tauri core's own `asset://` protocol:
//!
//! - `Range: bytes=a-b` / `bytes=a-` / `bytes=-n` → `206` with
//!   `Content-Range: bytes a-e/total` and `Accept-Ranges: bytes`.
//! - Responses are capped at [`CHUNK_CAP`] (~1 MiB) because wry buffers each
//!   response body fully; short 206s are prototype-verified safe (AVF simply
//!   re-requests the remainder).
//! - The file is opened + seeked and **only the requested span is read** —
//!   never the whole file per range request (hundreds of requests per
//!   playback).
//! - Unsatisfiable ranges (start ≥ length, zero-length suffix) → `416` with
//!   `Content-Range: bytes */total`. A syntactically invalid Range header is
//!   ignored per RFC 9110 (→ `200` full body).
//! - Multi-range requests are answered with the **first** range as a
//!   single-part 206 (AVF never sends multi-range; a spec-permitted subset).
//!
//! Caching: content-addressed ⇒ a URL's bytes can never change ⇒ every
//! success is `Cache-Control: public, max-age=31536000, immutable`. Errors
//! are `no-store` plain text (media element consumers, not humans, read
//! these — no styled page like `artifact://`'s).

use std::fs;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::Path;

use rusqlite::{Connection, OptionalExtension};
use tauri::http::{header, Method, Request, Response, StatusCode};
use tauri::{Manager, UriSchemeContext, UriSchemeResponder};

use crate::artifact_protocol::is_safe_segment;
use crate::assets;
use crate::db::DbHandle;

/// Per-response body cap. wry marshals each response fully into memory, so a
/// 20 MiB clip must not ride out in one response; 1 MiB matches tauri core's
/// asset protocol and is prototype-verified (AVF re-requests the rest).
pub const CHUNK_CAP: u64 = 1_048_576; // 1 MiB

// ---------------------------------------------------------------------------
// Request parsing (pure)
// ---------------------------------------------------------------------------

/// A successfully parsed `cfy-asset://` request path.
#[derive(Debug, PartialEq, Eq)]
pub struct AssetPath {
    pub thread_id: String,
    pub sha256: String,
}

/// Everything that can go wrong serving an asset URL.
#[derive(Debug)]
pub enum ServeError {
    BadPath(String),
    MethodNotAllowed(String),
    ThreadNotFound(String),
    /// The thread exists but no such asset was uploaded for it.
    AssetNotFound(String),
    /// The requested range cannot be satisfied (RFC 9110 §14.1.2); carries
    /// the total file length for `Content-Range: bytes */<len>`.
    RangeNotSatisfiable(u64),
    Internal(String),
}

impl ServeError {
    pub fn status(&self) -> StatusCode {
        match self {
            ServeError::BadPath(_) => StatusCode::BAD_REQUEST,
            ServeError::MethodNotAllowed(_) => StatusCode::METHOD_NOT_ALLOWED,
            ServeError::ThreadNotFound(_) | ServeError::AssetNotFound(_) => StatusCode::NOT_FOUND,
            ServeError::RangeNotSatisfiable(_) => StatusCode::RANGE_NOT_SATISFIABLE,
            ServeError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn detail(&self) -> String {
        match self {
            ServeError::BadPath(msg) => msg.clone(),
            ServeError::MethodNotAllowed(m) => {
                format!("{m} is not supported on the cfy-asset:// scheme — only GET.")
            }
            ServeError::ThreadNotFound(id) => format!("No thread with id \"{id}\" exists."),
            ServeError::AssetNotFound(sha) => {
                format!("No asset {sha} has been uploaded for this thread.")
            }
            ServeError::RangeNotSatisfiable(len) => {
                format!("Requested range is not satisfiable (asset is {len} bytes).")
            }
            ServeError::Internal(msg) => msg.clone(),
        }
    }
}

/// Parse a request path (`/​<thread-id>/<sha256>.mp4`) into an [`AssetPath`].
/// Strict by construction: exactly two segments, thread id against the closed
/// `artifact://` charset, file name exactly `<64 lowercase hex>.mp4` — so
/// traversal/encoding tricks are structurally impossible before any I/O.
pub fn parse_path(path: &str) -> Result<AssetPath, ServeError> {
    let bad = |msg: String| Err(ServeError::BadPath(msg));

    let Some(rest) = path.strip_prefix('/') else {
        return bad(format!("Path \"{path}\" must start with /."));
    };
    let segments: Vec<&str> = rest.split('/').collect();
    let [thread_id, file] = segments[..] else {
        return bad(format!(
            "Expected exactly cfy-asset://localhost/<thread-id>/<sha256>.mp4, got \"{path}\"."
        ));
    };

    if !is_safe_segment(thread_id) {
        return bad(format!(
            "Invalid thread id segment \"{thread_id}\" (allowed: [A-Za-z0-9_-], 1–128 chars)."
        ));
    }

    let Some(sha) = file.strip_suffix(".mp4") else {
        return bad(format!("Asset file name \"{file}\" must end in .mp4."));
    };
    if !assets::is_valid_sha256(sha) {
        return bad(format!(
            "Asset name \"{sha}\" is not a 64-char lowercase-hex SHA-256."
        ));
    }

    Ok(AssetPath {
        thread_id: thread_id.to_owned(),
        sha256: sha.to_owned(),
    })
}

// ---------------------------------------------------------------------------
// Range header parsing (pure)
// ---------------------------------------------------------------------------

/// One `bytes=` range spec, before clamping against the file length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeSpec {
    /// `bytes=a-b` (inclusive).
    FromTo(u64, u64),
    /// `bytes=a-` (open end).
    From(u64),
    /// `bytes=-n` (final n bytes).
    Suffix(u64),
}

/// Parse a `Range` header value. `None` means "ignore the header and serve
/// 200" — RFC 9110 §14.2: a recipient MUST ignore a Range header it cannot
/// parse (unknown unit, garbage, `a-b` with `a > b`). Multi-range requests
/// yield only the first spec (served as a single-part 206 — a permitted
/// subset; AVFoundation never sends multi-range).
pub fn parse_range(value: &str) -> Option<RangeSpec> {
    let ranges = value.trim().strip_prefix("bytes=")?;
    let first = ranges.split(',').next()?.trim();
    let (start, end) = first.split_once('-')?;
    let (start, end) = (start.trim(), end.trim());
    match (start.is_empty(), end.is_empty()) {
        (true, false) => end.parse().ok().map(RangeSpec::Suffix),
        (false, true) => start.parse().ok().map(RangeSpec::From),
        (false, false) => {
            let (a, b) = (start.parse().ok()?, end.parse().ok()?);
            (a <= b).then_some(RangeSpec::FromTo(a, b))
        }
        (true, true) => None,
    }
}

/// Clamp a parsed range against the file length and the per-response chunk
/// cap. Returns the inclusive `(start, end)` span to serve, or the 416 error.
fn resolve_range(spec: RangeSpec, len: u64) -> Result<(u64, u64), ServeError> {
    let unsatisfiable = ServeError::RangeNotSatisfiable(len);
    let (start, requested_end) = match spec {
        RangeSpec::FromTo(a, b) => (a, b.min(len.saturating_sub(1))),
        RangeSpec::From(a) => (a, len.saturating_sub(1)),
        RangeSpec::Suffix(0) => return Err(unsatisfiable),
        RangeSpec::Suffix(n) => (len.saturating_sub(n), len.saturating_sub(1)),
    };
    if start >= len {
        return Err(unsatisfiable);
    }
    let end = requested_end.min(start + CHUNK_CAP - 1);
    Ok((start, end))
}

// ---------------------------------------------------------------------------
// DB resolution + serving
// ---------------------------------------------------------------------------

/// Thread id → (project, slug), with the same defense-in-depth re-validation
/// of DB-sourced path segments as `artifact_protocol::resolve`.
fn resolve_thread(conn: &Connection, thread_id: &str) -> Result<(String, String), ServeError> {
    let row = conn
        .query_row(
            "SELECT project_id, slug FROM threads WHERE id = ?1",
            [thread_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|e| ServeError::Internal(format!("Database error: {e}.")))?;
    let Some((project_id, slug)) = row else {
        return Err(ServeError::ThreadNotFound(thread_id.to_owned()));
    };
    if !is_safe_segment(&project_id) || !is_safe_segment(&slug) {
        return Err(ServeError::Internal(
            "Stored project id or thread slug is not a safe path segment.".into(),
        ));
    }
    Ok((project_id, slug))
}

fn media_response(status: StatusCode) -> tauri::http::response::Builder {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "video/mp4")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        // Content-addressed: the bytes behind a URL can never change.
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
}

fn error_response(err: &ServeError) -> Response<Vec<u8>> {
    let mut builder = Response::builder()
        .status(err.status())
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CACHE_CONTROL, "no-store");
    if let ServeError::RangeNotSatisfiable(len) = err {
        // RFC 9110 §14.4: a 416 SHOULD carry the current length.
        builder = builder.header(header::CONTENT_RANGE, format!("bytes */{len}"));
    }
    builder
        .body(err.detail().into_bytes())
        .expect("static error response must build")
}

/// Serve one `cfy-asset://` request against an open connection and an
/// artifacts root. Pure with respect to its inputs (reads DB + disk, writes
/// nothing) — the unit the tests drive; the protocol closure below is wiring.
pub fn respond(
    conn: &Connection,
    root: &Path,
    method: &Method,
    path: &str,
    range: Option<&str>,
) -> Response<Vec<u8>> {
    match serve(conn, root, method, path, range) {
        Ok(response) => response,
        Err(err) => error_response(&err),
    }
}

fn serve(
    conn: &Connection,
    root: &Path,
    method: &Method,
    path: &str,
    range: Option<&str>,
) -> Result<Response<Vec<u8>>, ServeError> {
    if method != Method::GET {
        return Err(ServeError::MethodNotAllowed(method.to_string()));
    }

    let req = parse_path(path)?;
    let (project_id, slug) = resolve_thread(conn, &req.thread_id)?;
    let file_path = assets::asset_file_path(root, &project_id, &slug, &req.sha256);

    let mut file = match fs::File::open(&file_path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(ServeError::AssetNotFound(req.sha256));
        }
        Err(e) => return Err(ServeError::Internal(format!("Failed to open asset: {e}."))),
    };
    let len = file
        .metadata()
        .map_err(|e| ServeError::Internal(format!("Failed to stat asset: {e}.")))?
        .len();

    let io_err = |e: io::Error| ServeError::Internal(format!("Failed to read asset: {e}."));

    match range.and_then(parse_range) {
        Some(spec) => {
            let (start, end) = resolve_range(spec, len)?;
            let span = (end - start + 1) as usize;
            let mut body = vec![0u8; span];
            file.seek(SeekFrom::Start(start)).map_err(io_err)?;
            file.read_exact(&mut body).map_err(io_err)?;
            media_response(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"))
                .body(body)
                .map_err(|e| ServeError::Internal(format!("Failed to build response: {e}.")))
        }
        // No (or unparseable → ignored) Range: the full body. Assets are
        // hard-capped at 20 MiB, so a one-off full read is bounded; AVF never
        // actually takes this path (its very first request carries a Range).
        None => {
            let mut body = Vec::with_capacity(len as usize);
            file.read_to_end(&mut body).map_err(io_err)?;
            media_response(StatusCode::OK)
                .body(body)
                .map_err(|e| ServeError::Internal(format!("Failed to build response: {e}.")))
        }
    }
}

// ---------------------------------------------------------------------------
// Tauri wiring (kept as thin as possible; mirrors artifact_protocol)
// ---------------------------------------------------------------------------

/// The `register_asynchronous_uri_scheme_protocol("cfy-asset", …)` handler
/// (see `crate::run`). Hundreds of requests arrive per playback; each does
/// one point DB lookup + one bounded read on the blocking pool — verified
/// cheap in the z9y.1 prototype (which pushed >700 requests through an
/// equivalent handler for a single 12s playback with two seeks).
pub fn protocol_handler<R: tauri::Runtime>(
    ctx: UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    let app_handle = ctx.app_handle().clone();
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let range = request
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    tauri::async_runtime::spawn_blocking(move || {
        let response = match app_handle.try_state::<DbHandle>() {
            Some(db) => match crate::artifacts::artifacts_root() {
                Ok(root) => {
                    // Same poison discipline as `db::with_conn`.
                    let conn = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    respond(&conn, &root, &method, &path, range.as_deref())
                }
                Err(e) => error_response(&ServeError::Internal(format!(
                    "Cannot resolve the artifact storage directory: {e}."
                ))),
            },
            None => error_response(&ServeError::Internal("Database not initialized yet.".into())),
        };
        responder.respond(response);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::assets::tests::TINY_MP4;

    // -- fixtures --------------------------------------------------------------

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                slug TEXT NOT NULL
            );
            INSERT INTO threads (id, project_id, slug) VALUES ('t1', 'p1', 'oauth-flow');
            ",
        )
        .unwrap();
        conn
    }

    fn tmp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "conceptify-asset-protocol-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// Store the fixture through the REAL upload pipeline — pins the protocol
    /// handler's path construction to the store's actual output (lockstep).
    fn store_fixture(conn: &Connection, root: &Path) -> String {
        let sha = crate::assets::sha256_hex(TINY_MP4);
        crate::assets::save_asset(conn, root, "t1", &sha, TINY_MP4).unwrap();
        sha
    }

    fn get(
        conn: &Connection,
        root: &Path,
        path: &str,
        range: Option<&str>,
    ) -> Response<Vec<u8>> {
        respond(conn, root, &Method::GET, path, range)
    }

    fn header_str(res: &Response<Vec<u8>>, name: header::HeaderName) -> &str {
        res.headers()
            .get(&name)
            .unwrap_or_else(|| panic!("missing header {name}"))
            .to_str()
            .unwrap()
    }

    // -- path parsing ----------------------------------------------------------

    #[test]
    fn parse_accepts_the_canonical_shape() {
        let sha = "a".repeat(64);
        assert_eq!(
            parse_path(&format!("/7c9e6679-7425-40de-944b-e07fc1f90ae7/{sha}.mp4")).unwrap(),
            AssetPath {
                thread_id: "7c9e6679-7425-40de-944b-e07fc1f90ae7".into(),
                sha256: sha,
            }
        );
    }

    #[test]
    fn parse_rejects_malformed_shapes() {
        let sha = "a".repeat(64);
        for path in [
            String::new(),
            "/".into(),
            format!("/t1/{sha}"),          // missing .mp4
            format!("/t1/{sha}.webm"),     // wrong extension
            format!("/{sha}.mp4"),         // one segment
            format!("/t1/x/{sha}.mp4"),    // three segments
            format!("/t1/{sha}.mp4/"),     // trailing slash
            format!("//{sha}.mp4"),        // empty thread id
            "/t1/.mp4".into(),             // empty sha
            format!("/t1/{}.mp4", "A".repeat(64)), // uppercase hex
            format!("/t1/{}.mp4", "g".repeat(64)), // non-hex
            format!("/t1/{}.mp4", "a".repeat(63)), // wrong length
            format!("/../{sha}.mp4"),      // traversal
            format!("/t1%2f..%2fx/{sha}.mp4"),
            "/t1/..%2e%2e.mp4".into(),
        ] {
            assert!(
                matches!(parse_path(&path), Err(ServeError::BadPath(_))),
                "should reject {path:?}"
            );
        }
    }

    // -- range parsing ---------------------------------------------------------

    #[test]
    fn parse_range_forms() {
        assert_eq!(parse_range("bytes=0-1"), Some(RangeSpec::FromTo(0, 1)));
        assert_eq!(parse_range(" bytes=5-"), Some(RangeSpec::From(5)));
        assert_eq!(parse_range("bytes=-4"), Some(RangeSpec::Suffix(4)));
        // Multi-range: first spec wins.
        assert_eq!(
            parse_range("bytes=0-1, 10-20"),
            Some(RangeSpec::FromTo(0, 1))
        );
        // Malformed / non-bytes units are ignored (→ 200 full body).
        for bad in ["", "bytes=", "bytes=-", "bytes=b-a", "bytes=5-2", "items=0-1", "0-1"] {
            assert_eq!(parse_range(bad), None, "{bad:?}");
        }
    }

    // -- serving ---------------------------------------------------------------

    #[test]
    fn full_get_serves_whole_file_with_immutable_caching() {
        let conn = test_conn();
        let root = tmp_root("full");
        let sha = store_fixture(&conn, &root);

        let res = get(&conn, &root, &format!("/t1/{sha}.mp4"), None);
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.body().as_slice(), TINY_MP4);
        assert_eq!(header_str(&res, header::CONTENT_TYPE), "video/mp4");
        assert_eq!(header_str(&res, header::ACCEPT_RANGES), "bytes");
        assert_eq!(
            header_str(&res, header::CACHE_CONTROL),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(header_str(&res, header::X_CONTENT_TYPE_OPTIONS), "nosniff");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn avf_probe_range_bytes_0_1_gets_a_correct_206() {
        // The existential case from the z9y.1 prototype: AVFoundation's very
        // first media request. No 206 here = no playback at all.
        let conn = test_conn();
        let root = tmp_root("probe");
        let sha = store_fixture(&conn, &root);

        let res = get(&conn, &root, &format!("/t1/{sha}.mp4"), Some("bytes=0-1"));
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(res.body().as_slice(), &TINY_MP4[..2]);
        assert_eq!(
            header_str(&res, header::CONTENT_RANGE),
            format!("bytes 0-1/{}", TINY_MP4.len())
        );
        assert_eq!(header_str(&res, header::ACCEPT_RANGES), "bytes");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bounded_open_ended_and_suffix_ranges_are_byte_exact() {
        let conn = test_conn();
        let root = tmp_root("ranges");
        let sha = store_fixture(&conn, &root);
        let path = format!("/t1/{sha}.mp4");
        let len = TINY_MP4.len();

        // Bounded interior range.
        let res = get(&conn, &root, &path, Some("bytes=100-299"));
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(res.body().as_slice(), &TINY_MP4[100..300]);
        assert_eq!(
            header_str(&res, header::CONTENT_RANGE),
            format!("bytes 100-299/{len}")
        );

        // Open-ended tail.
        let res = get(&conn, &root, &path, Some("bytes=100-"));
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(res.body().as_slice(), &TINY_MP4[100..]);
        assert_eq!(
            header_str(&res, header::CONTENT_RANGE),
            format!("bytes 100-{}/{len}", len - 1)
        );

        // Suffix.
        let res = get(&conn, &root, &path, Some("bytes=-16"));
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(res.body().as_slice(), &TINY_MP4[len - 16..]);
        assert_eq!(
            header_str(&res, header::CONTENT_RANGE),
            format!("bytes {}-{}/{len}", len - 16, len - 1)
        );

        // End clamped to the file length.
        let res = get(&conn, &root, &path, Some("bytes=100-999999999"));
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(res.body().as_slice(), &TINY_MP4[100..]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unsatisfiable_ranges_are_416_with_star_content_range() {
        let conn = test_conn();
        let root = tmp_root("416");
        let sha = store_fixture(&conn, &root);
        let path = format!("/t1/{sha}.mp4");
        let len = TINY_MP4.len();

        for range in [format!("bytes={len}-"), format!("bytes={}-{}", len + 5, len + 9), "bytes=-0".into()] {
            let res = get(&conn, &root, &path, Some(range.as_str()));
            assert_eq!(res.status(), StatusCode::RANGE_NOT_SATISFIABLE, "{range}");
            assert_eq!(
                header_str(&res, header::CONTENT_RANGE),
                format!("bytes */{len}")
            );
            assert_eq!(header_str(&res, header::CACHE_CONTROL), "no-store");
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn malformed_range_headers_are_ignored_and_serve_200() {
        let conn = test_conn();
        let root = tmp_root("badrange");
        let sha = store_fixture(&conn, &root);

        for bad in ["items=0-1", "bytes=", "bytes=5-2", "garbage"] {
            let res = get(&conn, &root, &format!("/t1/{sha}.mp4"), Some(bad));
            assert_eq!(res.status(), StatusCode::OK, "{bad:?}");
            assert_eq!(res.body().as_slice(), TINY_MP4);
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn responses_are_capped_at_the_chunk_size() {
        // A file (deliberately not a real MP4 — the protocol layer serves
        // bytes, it does not sniff) larger than CHUNK_CAP: an open-ended
        // range must come back capped, with the true total in Content-Range.
        let conn = test_conn();
        let root = tmp_root("cap");
        let sha = "d".repeat(64);
        let big: Vec<u8> = (0..(CHUNK_CAP + 4096)).map(|i| (i % 251) as u8).collect();
        let dir = crate::assets::assets_dir(&root, "p1", "oauth-flow");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{sha}.mp4")), &big).unwrap();

        let res = get(&conn, &root, &format!("/t1/{sha}.mp4"), Some("bytes=0-"));
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(res.body().len() as u64, CHUNK_CAP);
        assert_eq!(res.body().as_slice(), &big[..CHUNK_CAP as usize]);
        assert_eq!(
            header_str(&res, header::CONTENT_RANGE),
            format!("bytes 0-{}/{}", CHUNK_CAP - 1, big.len())
        );

        // A bounded range that stays under the cap is untouched.
        let res = get(&conn, &root, &format!("/t1/{sha}.mp4"), Some("bytes=4096-8191"));
        assert_eq!(res.body().as_slice(), &big[4096..8192]);

        // The continuation request picks up exactly where the cap cut off.
        let res = get(
            &conn,
            &root,
            &format!("/t1/{sha}.mp4"),
            Some(&format!("bytes={CHUNK_CAP}-")),
        );
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(res.body().as_slice(), &big[CHUNK_CAP as usize..]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unknown_thread_missing_asset_and_bad_paths_error_cleanly() {
        let conn = test_conn();
        let root = tmp_root("errors");
        let sha = store_fixture(&conn, &root);

        let res = get(&conn, &root, &format!("/ghost/{sha}.mp4"), None);
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        let missing = "b".repeat(64);
        let res = get(&conn, &root, &format!("/t1/{missing}.mp4"), None);
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_eq!(header_str(&res, header::CACHE_CONTROL), "no-store");

        let res = get(&conn, &root, "/../evil.mp4", None);
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn non_get_methods_are_405() {
        let conn = test_conn();
        let root = tmp_root("method");
        let sha = "a".repeat(64);
        for method in [Method::POST, Method::PUT, Method::DELETE, Method::HEAD] {
            let res = respond(&conn, &root, &method, &format!("/t1/{sha}.mp4"), None);
            assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED, "{method}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unsafe_db_segments_are_refused() {
        let conn = test_conn();
        let root = tmp_root("unsafe-db");
        conn.execute("UPDATE threads SET slug = '../../evil' WHERE id = 't1'", [])
            .unwrap();
        let sha = "a".repeat(64);
        let res = get(&conn, &root, &format!("/t1/{sha}.mp4"), None);
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let _ = std::fs::remove_dir_all(&root);
    }
}
