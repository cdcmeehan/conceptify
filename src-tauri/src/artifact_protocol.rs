//! The `artifact://` custom URI scheme (PRD §5.4, §9 S2).
//!
//! Serves stored artifact HTML into the viewer's sandboxed iframe from a
//! scheme that is a *different origin* than the app shell — cross-scheme
//! isolation is the containment boundary: combined with the iframe's
//! `sandbox="allow-scripts"` (no `allow-same-origin`, owned by the viewer,
//! bead `conceptify-nsy.4`), artifact JS runs in an opaque origin with no
//! path to the app DOM, Tauri IPC, storage, or the localhost API.
//!
//! ## URL contract (consumed by the viewer, bead `conceptify-nsy.4`)
//!
//! ```text
//! artifact://localhost/<thread-id>/<version>
//! ```
//!
//! - `<thread-id>` — the thread's DB id (a UUID). Strictly validated:
//!   `[A-Za-z0-9_-]{1,128}`. No percent-decoding is performed anywhere on
//!   this scheme; ids never need encoding.
//! - `<version>` — either a bare decimal integer ≥ 1 (`…/3` serves the
//!   immutable `artifact.v3.html`) or the literal lowercase `latest`, which
//!   resolves to the thread's highest version **via the DB** (`MAX(version)`
//!   over the `artifacts` table) and serves that versioned file. The
//!   `artifact.html` disk copy is deliberately *not* served: files are
//!   written before the DB row commits (see `crate::artifacts`), so in a
//!   crash window the copy can be ahead of the DB — the DB is the source of
//!   truth the rest of the app (version switcher, events) reads.
//! - GET only; anything else is 405.
//!
//! Caching: numeric versions are immutable → `Cache-Control: public,
//! max-age=31536000, immutable`. `latest` and every error → `no-store`.
//!
//! ## Per-response CSP
//!
//! [`CSP`] is exactly the reference policy of docs/artifact-spec.md §3 and
//! must stay in lockstep with the §7.1 allowlist (single pinned CDN host,
//! `cdn.jsdelivr.net`; `font-src` includes it or KaTeX breaks). The load-
//! bearing directive is `connect-src 'none'`: artifact JS can never reach
//! the localhost API (or anything else) at runtime. Every response on the
//! scheme — success or error page — carries this header.
//!
//! ## Bridge injection (G3: the on-disk file stays pristine)
//!
//! [`inject_bridge`] splices [`BRIDGE_TAG`] into the served bytes just
//! before the closing `</body>` tag; the file on disk is never modified, so
//! opened directly in a browser it has no Conceptify residue. The tag wraps
//! the real postMessage bridge (M4, bead `conceptify-94m.1`), which lives as
//! an editable JS asset at `src-tauri/assets/bridge.js` and is compiled in
//! via `include_str!`. The message protocol it speaks is documented in
//! docs/api.md, "Bridge protocol" — the shell counterpart is
//! `src/lib/bridge.ts`.
//!
//! ## WKWebView note (Appendix A, wry #168)
//!
//! Subresource fetches through custom protocols are unreliable in WKWebView
//! (`<img src>` in particular). This handler therefore serves exactly one
//! document per request and nothing else — artifacts are self-contained by
//! spec (inline SVG, `data:` URIs), so no subresource ever loads via
//! `artifact://`. Don't build anything that relies on that changing.

use std::fs;
use std::io;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension};
use tauri::http::{header, Method, Request, Response, StatusCode};
use tauri::{Manager, UriSchemeContext, UriSchemeResponder};

use crate::artifacts;
use crate::db::DbHandle;

/// The reference CSP from docs/artifact-spec.md §3, verbatim. The spec is
/// the contract: authors treat this as the runtime environment, so any
/// change here must go through the spec first (and the §7.1 allowlist /
/// `artifacts::CDN_ALLOWLIST` alongside).
pub const CSP: &str = "default-src 'none'; \
     script-src 'unsafe-inline' https://cdn.jsdelivr.net; \
     style-src 'unsafe-inline' https://cdn.jsdelivr.net; \
     font-src data: https://cdn.jsdelivr.net; \
     img-src data:; \
     connect-src 'none'";

/// Attribute marking the injected bridge script tag. `data-cfy-*` is a
/// reserved namespace (spec §1), so a conforming artifact never contains
/// this marker itself — it doubles as the idempotence check in
/// [`inject_bridge`].
const BRIDGE_MARKER: &str = "data-cfy-bridge";

/// The injected bridge `<script>` tag: the real postMessage bridge (M4,
/// bead `conceptify-94m.1`) — selection reporting, click-to-comment,
/// highlight decorations, scroll-to-anchor; protocol documented in
/// docs/api.md "Bridge protocol". The script body lives as a lintable JS
/// asset (`assets/bridge.js`) and MUST NOT contain a literal `</script>`
/// (it would close this tag early — guarded by a test below). The
/// `data-cfy-bridge` attribute and the `window.__cfyBridge` global are
/// reserved names the spec promises artifacts will not touch. Never present
/// in the on-disk file.
const BRIDGE_TAG: &str = concat!(
    "\n<script data-cfy-bridge=\"v1\">\n",
    include_str!("../assets/bridge.js"),
    "\n</script>\n",
);

// ---------------------------------------------------------------------------
// Request path parsing (pure)
// ---------------------------------------------------------------------------

/// Which artifact version a URL addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionSpec {
    /// A bare integer segment — an immutable, cacheable version.
    Number(i64),
    /// The literal `latest` segment — resolved via the DB per request.
    Latest,
}

/// A successfully parsed `artifact://` request path.
#[derive(Debug, PartialEq, Eq)]
pub struct ArtifactPath {
    pub thread_id: String,
    pub version: VersionSpec,
}

/// Everything that can go wrong serving an artifact URL. Each variant maps
/// to a status + styled error page (rendered inside the viewer iframe).
#[derive(Debug)]
pub enum ServeError {
    /// Malformed path (wrong segment count, bad characters, traversal
    /// attempts, bad version syntax).
    BadPath(String),
    MethodNotAllowed(String),
    ThreadNotFound(String),
    /// The thread exists but has no artifact rows yet (`latest` on a
    /// still-generating thread).
    NoVersions(String),
    /// The thread exists but the requested version number doesn't.
    VersionNotFound { thread_id: String, version: i64 },
    /// The DB row exists but the file is gone from disk.
    FileMissing(String),
    /// DB error, unsafe DB contents, unreadable file — anything internal.
    Internal(String),
}

impl ServeError {
    pub fn status(&self) -> StatusCode {
        match self {
            ServeError::BadPath(_) => StatusCode::BAD_REQUEST,
            ServeError::MethodNotAllowed(_) => StatusCode::METHOD_NOT_ALLOWED,
            ServeError::ThreadNotFound(_)
            | ServeError::NoVersions(_)
            | ServeError::VersionNotFound { .. }
            | ServeError::FileMissing(_) => StatusCode::NOT_FOUND,
            ServeError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn title(&self) -> &'static str {
        match self {
            ServeError::BadPath(_) => "Bad artifact URL",
            ServeError::MethodNotAllowed(_) => "Method not allowed",
            ServeError::ThreadNotFound(_) => "Unknown thread",
            ServeError::NoVersions(_) => "No artifact yet",
            ServeError::VersionNotFound { .. } => "Unknown version",
            ServeError::FileMissing(_) => "Artifact file missing",
            ServeError::Internal(_) => "Something went wrong",
        }
    }

    fn detail(&self) -> String {
        match self {
            ServeError::BadPath(msg) => msg.clone(),
            ServeError::MethodNotAllowed(m) => format!(
                "{m} is not supported on the artifact:// scheme — only GET."
            ),
            ServeError::ThreadNotFound(id) => {
                format!("No thread with id \u{201c}{id}\u{201d} exists.")
            }
            ServeError::NoVersions(id) => format!(
                "Thread \u{201c}{id}\u{201d} has no saved artifact versions yet."
            ),
            ServeError::VersionNotFound { thread_id, version } => format!(
                "Thread \u{201c}{thread_id}\u{201d} has no version {version}."
            ),
            ServeError::FileMissing(path) => format!(
                "The artifact is recorded in the database but its file is \
                 missing on disk ({path})."
            ),
            ServeError::Internal(msg) => msg.clone(),
        }
    }
}

/// `true` iff `s` is a safe single path segment: our conservative charset
/// (no `/`, `\`, `.`, `%`, …) structurally rules out traversal, encoding
/// tricks, and hidden-file names.
fn is_safe_segment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Parse a request path (`request.uri().path()`, e.g. `/t-123/latest`) into
/// an [`ArtifactPath`]. Strict by construction: exactly two segments, each
/// validated against a closed charset — `..`, `%2e%2e`, backslashes, empty
/// segments, and trailing slashes are all rejected before any I/O happens.
pub fn parse_path(path: &str) -> Result<ArtifactPath, ServeError> {
    let bad = |msg: String| Err(ServeError::BadPath(msg));

    let Some(rest) = path.strip_prefix('/') else {
        return bad(format!("Path \u{201c}{path}\u{201d} must start with /."));
    };
    let segments: Vec<&str> = rest.split('/').collect();
    let [thread_id, version] = segments[..] else {
        return bad(format!(
            "Expected exactly artifact://localhost/<thread-id>/<version>, \
             got \u{201c}{path}\u{201d}."
        ));
    };

    if !is_safe_segment(thread_id) {
        return bad(format!(
            "Invalid thread id segment \u{201c}{thread_id}\u{201d} \
             (allowed: [A-Za-z0-9_-], 1–128 chars)."
        ));
    }

    let version = if version == "latest" {
        VersionSpec::Latest
    } else if !version.is_empty()
        && version.len() <= 9
        && version.bytes().all(|b| b.is_ascii_digit())
    {
        match version.parse::<i64>() {
            Ok(n) if n >= 1 => VersionSpec::Number(n),
            _ => {
                return bad(format!(
                    "Version must be an integer ≥ 1 or \u{201c}latest\u{201d}, \
                     got \u{201c}{version}\u{201d}."
                ))
            }
        }
    } else {
        return bad(format!(
            "Version must be an integer ≥ 1 or \u{201c}latest\u{201d}, \
             got \u{201c}{version}\u{201d}."
        ));
    };

    Ok(ArtifactPath {
        thread_id: thread_id.to_owned(),
        version,
    })
}

// ---------------------------------------------------------------------------
// DB resolution
// ---------------------------------------------------------------------------

/// A request resolved against the DB: everything needed to locate the file.
#[derive(Debug, PartialEq, Eq)]
pub struct ResolvedArtifact {
    pub project_id: String,
    pub slug: String,
    /// Always a concrete number — `latest` has been resolved by now.
    pub version: i64,
    /// Whether the URL addressed a concrete version (immutable, cacheable)
    /// or `latest` (must not be cached).
    pub immutable: bool,
}

/// Resolve thread id → (project, slug) and the version spec → a concrete
/// version number, entirely from the DB.
pub fn resolve(conn: &Connection, req: &ArtifactPath) -> Result<ResolvedArtifact, ServeError> {
    let db_err = |e: rusqlite::Error| ServeError::Internal(format!("Database error: {e}."));

    let row = conn
        .query_row(
            "SELECT project_id, slug FROM threads WHERE id = ?1",
            [&req.thread_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(db_err)?;
    let Some((project_id, slug)) = row else {
        return Err(ServeError::ThreadNotFound(req.thread_id.clone()));
    };

    // Defense in depth: these come from our own DB (written by our own save
    // pipeline), but they become path components — re-validate as single
    // segments so a corrupted/hand-edited DB can never step outside the
    // artifacts root.
    if !is_safe_segment(&project_id) || !is_safe_segment(&slug) {
        return Err(ServeError::Internal(
            "Stored project id or thread slug is not a safe path segment.".into(),
        ));
    }

    let (version, immutable) = match req.version {
        VersionSpec::Number(n) => {
            let exists: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM artifacts WHERE thread_id = ?1 AND version = ?2)",
                    rusqlite::params![req.thread_id, n],
                    |r| r.get(0),
                )
                .map_err(db_err)?;
            if !exists {
                return Err(ServeError::VersionNotFound {
                    thread_id: req.thread_id.clone(),
                    version: n,
                });
            }
            (n, true)
        }
        VersionSpec::Latest => {
            let max: Option<i64> = conn
                .query_row(
                    "SELECT MAX(version) FROM artifacts WHERE thread_id = ?1",
                    [&req.thread_id],
                    |r| r.get(0),
                )
                .map_err(db_err)?;
            match max {
                Some(n) => (n, false),
                None => return Err(ServeError::NoVersions(req.thread_id.clone())),
            }
        }
    };

    Ok(ResolvedArtifact {
        project_id,
        slug,
        version,
        immutable,
    })
}

// ---------------------------------------------------------------------------
// Bridge injection (pure)
// ---------------------------------------------------------------------------

/// Case-insensitive (ASCII) search for the *last* occurrence of `needle`.
fn rfind_ascii_ci(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len())
        .rev()
        .find(|&i| haystack[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|w| w == needle)
}

/// Splice [`BRIDGE_TAG`] into the served bytes, immediately before the
/// **last** case-insensitive `</body>` (the last occurrence is the real
/// close tag even when earlier script text contains a `</body>` literal —
/// inside `<script>` only `</script>` closes, so such literals are legal).
/// Injecting at end-of-body means the entire artifact DOM above is parsed
/// when the bridge runs — no DOMContentLoaded dance needed.
///
/// Fallbacks keep this total: a document with no `</body>` (HTML5 parsers
/// error-recover it) gets the bridge appended at the end, which still
/// parses into `<body>` and executes. Already-injected input (checked via
/// the reserved `data-cfy-bridge` marker) is returned unchanged, so
/// injection is idempotent.
///
/// Operates on raw bytes and never re-encodes: the served document is the
/// on-disk bytes with exactly one splice — the disk file itself is never
/// touched (G3).
pub fn inject_bridge(bytes: &[u8]) -> Vec<u8> {
    if contains_bytes(bytes, BRIDGE_MARKER.as_bytes()) {
        return bytes.to_vec();
    }
    let bridge = BRIDGE_TAG.as_bytes();
    match rfind_ascii_ci(bytes, b"</body>") {
        Some(i) => {
            let mut out = Vec::with_capacity(bytes.len() + bridge.len());
            out.extend_from_slice(&bytes[..i]);
            out.extend_from_slice(bridge);
            out.extend_from_slice(&bytes[i..]);
            out
        }
        None => {
            let mut out = Vec::with_capacity(bytes.len() + bridge.len());
            out.extend_from_slice(bytes);
            out.extend_from_slice(bridge);
            out
        }
    }
}

// ---------------------------------------------------------------------------
// Response building (pure)
// ---------------------------------------------------------------------------

fn base_response(status: StatusCode, cache_control: &str) -> tauri::http::response::Builder {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CONTENT_SECURITY_POLICY, CSP)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CACHE_CONTROL, cache_control)
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// The small styled error page shown inside the viewer iframe. Interpolated
/// values are HTML-escaped (path segments are request-controlled). Inline
/// style only, dark-mode aware — it renders under the same CSP as artifacts.
fn error_page(err: &ServeError) -> Response<Vec<u8>> {
    let status = err.status();
    let title = escape_html(err.title());
    let detail = escape_html(&err.detail());
    let code = status.as_u16();
    let body = format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
  :root {{ color-scheme: light dark; }}
  body {{
    margin: 0; min-height: 100vh; display: grid; place-items: center;
    font-family: ui-sans-serif, -apple-system, system-ui, sans-serif;
    background: #fafaf9; color: #1c1917;
  }}
  main {{ max-width: 34rem; padding: 2rem; text-align: center; }}
  .code {{
    font-size: 0.8rem; letter-spacing: 0.1em; text-transform: uppercase;
    color: #a8a29e; margin-bottom: 0.75rem;
  }}
  h1 {{ font-size: 1.25rem; font-weight: 600; margin: 0 0 0.5rem; }}
  p {{ font-size: 0.9rem; line-height: 1.6; color: #57534e; margin: 0; }}
  @media (prefers-color-scheme: dark) {{
    body {{ background: #1c1917; color: #fafaf9; }}
    .code {{ color: #57534e; }}
    p {{ color: #a8a29e; }}
  }}
</style>
</head>
<body>
<main>
  <div class="code">{code}</div>
  <h1>{title}</h1>
  <p>{detail}</p>
</main>
</body>
</html>
"#
    );
    base_response(status, "no-store")
        .body(body.into_bytes())
        .expect("static error response must build")
}

// ---------------------------------------------------------------------------
// The testable core: request path → full HTTP response
// ---------------------------------------------------------------------------

/// Serve one `artifact://` request against an open connection and an
/// artifacts root. Pure with respect to its inputs (reads DB + disk, writes
/// nothing) — this is the unit the tests drive; the protocol closure below
/// is only wiring.
pub fn respond(
    conn: &Connection,
    root: &Path,
    method: &Method,
    path: &str,
) -> Response<Vec<u8>> {
    match serve(conn, root, method, path) {
        Ok(response) => response,
        Err(err) => error_page(&err),
    }
}

fn serve(
    conn: &Connection,
    root: &Path,
    method: &Method,
    path: &str,
) -> Result<Response<Vec<u8>>, ServeError> {
    if method != Method::GET {
        return Err(ServeError::MethodNotAllowed(method.to_string()));
    }

    let req = parse_path(path)?;
    let resolved = resolve(conn, &req)?;

    let file = artifacts::version_file_path(
        root,
        &resolved.project_id,
        &resolved.slug,
        resolved.version,
    );
    let bytes = match fs::read(&file) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(ServeError::FileMissing(file.to_string_lossy().into_owned()));
        }
        Err(e) => {
            return Err(ServeError::Internal(format!(
                "Failed to read artifact file: {e}."
            )));
        }
    };

    let cache_control = if resolved.immutable {
        // Versioned files are write-once: cache forever.
        "public, max-age=31536000, immutable"
    } else {
        // `latest` must re-resolve on every load (live refresh, N2).
        "no-store"
    };

    base_response(StatusCode::OK, cache_control)
        .body(inject_bridge(&bytes))
        .map_err(|e| ServeError::Internal(format!("Failed to build response: {e}.")))
}

// ---------------------------------------------------------------------------
// Tauri wiring (not unit-tested; kept as thin as possible)
// ---------------------------------------------------------------------------

/// The `register_asynchronous_uri_scheme_protocol("artifact", …)` handler
/// (see `crate::run`). Runs the blocking work (DB lock + file read) off the
/// invoking thread via `spawn_blocking` and responds asynchronously.
pub fn protocol_handler<R: tauri::Runtime>(
    ctx: UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    let app_handle = ctx.app_handle().clone();
    let method = request.method().clone();
    let path = request.uri().path().to_owned();

    tauri::async_runtime::spawn_blocking(move || {
        let response = match app_handle.try_state::<DbHandle>() {
            Some(db) => match artifacts::artifacts_root() {
                Ok(root) => {
                    // Same poison discipline as `db::with_conn`.
                    let conn = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    respond(&conn, &root, &method, &path)
                }
                Err(e) => error_page(&ServeError::Internal(format!(
                    "Cannot resolve the artifact storage directory: {e}."
                ))),
            },
            // Unreachable in practice: the DB is managed in `setup`, which
            // completes before any webview (and thus any artifact:// load)
            // exists. Kept non-panicking anyway.
            None => error_page(&ServeError::Internal(
                "Database not initialized yet.".into(),
            )),
        };
        responder.respond(response);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // -- fixtures ------------------------------------------------------------

    /// In-memory DB mirroring the shipped schema (same pattern as the
    /// `artifacts` module tests) with one project + one thread.
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
                status TEXT NOT NULL
                    CHECK (status IN ('generating', 'ready', 'updating', 'error')),
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            );
            CREATE TABLE artifacts (
                id TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL,
                version INTEGER NOT NULL,
                file_path TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                created_by TEXT NOT NULL CHECK (created_by IN ('initial', 'follow_up')),
                UNIQUE (thread_id, version)
            );
            -- Follow-up saves run the FR-4.4 re-attachment pass, which reads
            -- the comments table even when it's empty.
            CREATE TABLE comments (
                id TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL,
                artifact_version INTEGER NOT NULL,
                anchor TEXT,
                body TEXT NOT NULL,
                status TEXT NOT NULL
                    CHECK (status IN ('open', 'answered', 'applied')),
                answer_html TEXT,
                anchor_state TEXT NOT NULL DEFAULT 'anchored'
                    CHECK (anchor_state IN ('anchored', 'moved')),
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                resolved_at TEXT,
                parent_id TEXT REFERENCES comments(id) ON DELETE CASCADE
            );
            INSERT INTO projects (id, name, root_path) VALUES ('p1', 'Proj', '/tmp/p1');
            INSERT INTO threads (id, project_id, title, slug, initial_question, status)
                VALUES ('t1', 'p1', 'OAuth flow', 'oauth-flow', 'how?', 'generating');
            ",
        )
        .unwrap();
        conn
    }

    fn tmp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "conceptify-protocol-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn artifact_html(version: i64) -> String {
        format!(
            r#"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<title>T</title>
<meta name="cfy:question" content="q">
<meta name="cfy:version" content="{version}">
<meta name="cfy:generated-by" content="claude-code/test">
</head><body><h1 data-cfy-id="sec-t">Version {version}</h1></body></html>"#
        )
    }

    /// Save real versions through `artifacts::save_artifact` — this pins the
    /// protocol handler's `version_file_path` naming to the save pipeline's
    /// actual output (lockstep guard).
    fn save_versions(conn: &Connection, root: &Path, n: i64) {
        for v in 1..=n {
            artifacts::save_artifact(conn, root, "t1", artifact_html(v).as_bytes())
                .unwrap_or_else(|e| panic!("save v{v}: {e:?}"));
        }
    }

    fn get(conn: &Connection, root: &Path, path: &str) -> Response<Vec<u8>> {
        respond(conn, root, &Method::GET, path)
    }

    fn header_str<'r>(res: &'r Response<Vec<u8>>, name: header::HeaderName) -> &'r str {
        res.headers()
            .get(&name)
            .unwrap_or_else(|| panic!("missing header {name}"))
            .to_str()
            .unwrap()
    }

    fn body_str(res: &Response<Vec<u8>>) -> &str {
        std::str::from_utf8(res.body()).unwrap()
    }

    // -- path parsing ---------------------------------------------------------

    #[test]
    fn parse_accepts_numeric_and_latest() {
        assert_eq!(
            parse_path("/7c9e6679-7425-40de-944b-e07fc1f90ae7/3").unwrap(),
            ArtifactPath {
                thread_id: "7c9e6679-7425-40de-944b-e07fc1f90ae7".into(),
                version: VersionSpec::Number(3),
            }
        );
        assert_eq!(
            parse_path("/t1/latest").unwrap(),
            ArtifactPath {
                thread_id: "t1".into(),
                version: VersionSpec::Latest,
            }
        );
    }

    #[test]
    fn parse_rejects_malformed_shapes() {
        for path in [
            "",              // no leading slash
            "t1/1",          // no leading slash
            "/",             // empty segments
            "/t1",           // one segment
            "/t1/1/extra",   // three segments
            "/t1/1/",        // trailing slash → empty third segment
            "//1",           // empty thread id
            "/t1/",          // empty version
        ] {
            assert!(
                matches!(parse_path(path), Err(ServeError::BadPath(_))),
                "should reject {path:?}"
            );
        }
    }

    #[test]
    fn parse_rejects_traversal_and_encoding_tricks() {
        for path in [
            "/../1",
            "/../../etc/passwd",
            "/t1/..",
            "/%2e%2e/1",              // '%' outside the charset
            "/t1%2f..%2fx/1",
            "/..%5c..%5cx/1",
            "/t1\\evil/1",            // backslash outside the charset
            "/.hidden/1",             // '.' outside the charset
            "/t1/1?x=1",              // '?' would be split off by the URI
                                      // parser normally; reject raw too
        ] {
            assert!(
                matches!(parse_path(path), Err(ServeError::BadPath(_))),
                "should reject {path:?}"
            );
        }
    }

    #[test]
    fn parse_rejects_bad_version_syntax() {
        for path in [
            "/t1/0",           // versions start at 1
            "/t1/-1",
            "/t1/v1",          // bare integer, not vN
            "/t1/1.0",
            "/t1/01x",
            "/t1/Latest",      // case-sensitive literal
            "/t1/LATEST",
            "/t1/1234567890",  // > 9 digits
        ] {
            assert!(
                matches!(parse_path(path), Err(ServeError::BadPath(_))),
                "should reject {path:?}"
            );
        }
    }

    // -- bridge injection -------------------------------------------------------

    #[test]
    fn bridge_tag_is_wellformed_inline_script() {
        // The JS asset is inlined into a <script> tag, so it must not close
        // that tag early: exactly one `</script>` (ours, at the end), and it
        // must not contain a nested `<script` opener either.
        let lower = BRIDGE_TAG.to_ascii_lowercase();
        assert_eq!(
            lower.matches("</script>").count(),
            1,
            "bridge.js must not contain a literal </script>"
        );
        assert_eq!(
            lower.matches("<script").count(),
            1,
            "bridge.js must not contain a nested <script opener"
        );
        // Reserved names the artifact spec promises artifacts won't touch.
        assert!(BRIDGE_TAG.contains("data-cfy-bridge=\"v1\""));
        assert!(BRIDGE_TAG.contains("__cfyBridge"));
        // The protocol messages the shell depends on (docs/api.md
        // "Bridge protocol") — cheap lockstep guards.
        for msg in [
            "ready",
            "selection",
            "selection_cleared",
            "element_click",
            "set_highlights",
            "scroll_to_anchor",
        ] {
            assert!(BRIDGE_TAG.contains(msg), "bridge must reference {msg:?}");
        }
    }

    #[test]
    fn inject_places_bridge_before_last_body_close() {
        let html = b"<html><body><p>hi</p></body></html>";
        let out = inject_bridge(html);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("data-cfy-bridge"));
        let bridge_pos = s.find("data-cfy-bridge").unwrap();
        let body_close = s.rfind("</body>").unwrap();
        assert!(bridge_pos < body_close, "bridge must sit before </body>");
        assert!(s.ends_with("</body></html>"));
    }

    #[test]
    fn inject_uses_last_body_close_and_is_case_insensitive() {
        // A `</body>` literal inside script text is legal HTML (only
        // `</script>` closes a script); the real close tag is the last one.
        let html = b"<body><script>let s = \"</body>\";</script><p>x</p></BODY>";
        let out = inject_bridge(html);
        let s = String::from_utf8(out).unwrap();
        let bridge_pos = s.find("data-cfy-bridge").unwrap();
        let fake_close = s.find("</body>").unwrap();
        assert!(
            bridge_pos > fake_close,
            "bridge must not be injected at the script-text decoy"
        );
        assert!(s.ends_with("</BODY>"));
    }

    #[test]
    fn inject_appends_when_no_body_close_exists() {
        let html = b"<p>error-recovered document";
        let out = inject_bridge(html);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("<p>error-recovered document"));
        assert!(s.contains("data-cfy-bridge"));
    }

    #[test]
    fn inject_is_idempotent() {
        let html = b"<html><body><p>hi</p></body></html>";
        let once = inject_bridge(html);
        let twice = inject_bridge(&once);
        assert_eq!(once, twice);
    }

    // -- serving ----------------------------------------------------------------

    #[test]
    fn serves_versioned_file_with_bridge_and_exact_csp() {
        let conn = test_conn();
        let root = tmp_root("serve");
        save_versions(&conn, &root, 2);

        let res = get(&conn, &root, "/t1/1");
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            header_str(&res, header::CONTENT_SECURITY_POLICY),
            "default-src 'none'; script-src 'unsafe-inline' https://cdn.jsdelivr.net; \
             style-src 'unsafe-inline' https://cdn.jsdelivr.net; \
             font-src data: https://cdn.jsdelivr.net; img-src data:; connect-src 'none'"
        );
        assert_eq!(
            header_str(&res, header::CONTENT_TYPE),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            header_str(&res, header::CACHE_CONTROL),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(header_str(&res, header::X_CONTENT_TYPE_OPTIONS), "nosniff");

        // Served body = disk bytes + exactly the bridge splice.
        let body = body_str(&res);
        assert!(body.contains("Version 1"));
        assert!(body.contains("data-cfy-bridge"));
        assert!(body.contains("__cfyBridge"), "real bridge script present");
        assert_eq!(
            res.body(),
            &inject_bridge(artifact_html(1).as_bytes()),
            "served bytes must be the pristine file with one bridge splice"
        );

        // The on-disk file stays byte-identical (pristine, G3).
        let disk = std::fs::read(artifacts::version_file_path(&root, "p1", "oauth-flow", 1))
            .unwrap();
        assert_eq!(disk, artifact_html(1).as_bytes());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn latest_resolves_via_db_and_is_uncacheable() {
        let conn = test_conn();
        let root = tmp_root("latest");
        save_versions(&conn, &root, 3);

        let res = get(&conn, &root, "/t1/latest");
        assert_eq!(res.status(), StatusCode::OK);
        assert!(body_str(&res).contains("Version 3"));
        assert_eq!(header_str(&res, header::CACHE_CONTROL), "no-store");

        // `latest` follows the DB, not the artifact.html disk copy: delete
        // the DB row for v3 and latest falls back to v2.
        conn.execute("DELETE FROM artifacts WHERE version = 3", [])
            .unwrap();
        let res = get(&conn, &root, "/t1/latest");
        assert!(body_str(&res).contains("Version 2"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unknown_thread_version_and_empty_thread_are_styled_404s() {
        let conn = test_conn();
        let root = tmp_root("notfound");
        save_versions(&conn, &root, 1);

        // Unknown thread.
        let res = get(&conn, &root, "/ghost/1");
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert!(body_str(&res).contains("Unknown thread"));
        assert!(body_str(&res).contains("<style>"), "error page is styled");
        assert_eq!(header_str(&res, header::CONTENT_SECURITY_POLICY), CSP);
        assert_eq!(header_str(&res, header::CACHE_CONTROL), "no-store");

        // Unknown version on a real thread.
        let res = get(&conn, &root, "/t1/99");
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert!(body_str(&res).contains("Unknown version"));
        assert!(body_str(&res).contains("no version 99"));

        // `latest` on a thread with no artifact rows.
        conn.execute("DELETE FROM artifacts", []).unwrap();
        let res = get(&conn, &root, "/t1/latest");
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert!(body_str(&res).contains("No artifact yet"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn db_row_with_missing_file_is_404() {
        let conn = test_conn();
        let root = tmp_root("gone");
        save_versions(&conn, &root, 1);
        std::fs::remove_file(artifacts::version_file_path(&root, "p1", "oauth-flow", 1))
            .unwrap();

        let res = get(&conn, &root, "/t1/1");
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert!(body_str(&res).contains("Artifact file missing"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn traversal_paths_get_400_and_never_touch_disk() {
        let conn = test_conn();
        let root = tmp_root("traverse");
        // Plant a file *outside* the root that a traversal would reach.
        let secret = root.parent().unwrap().join("conceptify-secret.html");
        std::fs::write(&secret, "top secret").unwrap();

        for path in ["/../conceptify-secret/1", "/..%2fconceptify-secret/1", "/t1/../2"] {
            let res = get(&conn, &root, path);
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "path {path:?}");
            assert!(!body_str(&res).contains("top secret"));
        }

        let _ = std::fs::remove_file(&secret);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unsafe_db_segments_are_refused() {
        // Defense in depth: even if the DB hands back a traversal-shaped
        // slug (corrupted / hand-edited DB), the handler refuses to build a
        // path from it.
        let conn = test_conn();
        let root = tmp_root("unsafe-db");
        conn.execute_batch(
            "
            UPDATE threads SET slug = '../../evil' WHERE id = 't1';
            INSERT INTO artifacts (id, thread_id, version, file_path, created_by)
                VALUES ('a1', 't1', 1, '/x', 'initial');
            ",
        )
        .unwrap();

        let res = get(&conn, &root, "/t1/1");
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!body_str(&res).contains("evil"), "must not echo the slug raw");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn non_get_methods_are_405() {
        let conn = test_conn();
        let root = tmp_root("method");
        for method in [Method::POST, Method::PUT, Method::DELETE, Method::HEAD] {
            let res = respond(&conn, &root, &method, "/t1/1");
            assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED, "{method}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn error_pages_escape_request_controlled_input() {
        let conn = test_conn();
        let root = tmp_root("escape");
        let res = get(&conn, &root, "/<script>alert(1)</script>/1");
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let body = body_str(&res);
        assert!(!body.contains("<script>alert"), "raw markup must not reflect");
        assert!(body.contains("&lt;script&gt;"), "escaped form should appear");
        let _ = std::fs::remove_dir_all(&root);
    }
}
