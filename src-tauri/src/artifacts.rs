//! Artifact ingestion domain (PRD §5.6, §7.3 FR-3.6, N2/N4).
//!
//! Owns the `save-artifact` pipeline: validation (the rule set in
//! `docs/artifact-spec.md` §8 — that doc is the contract, rule IDs `E-*`/`W-*`
//! are stable identifiers), versioned storage under
//! `~/Documents/conceptify/artifacts/<project-id>/threads/<thread-slug>/`,
//! and the thread's `→ ready` status transition.
//!
//! Crash-safety ordering (N4): every file is written temp + rename (never a
//! partial file at its final name), and files are written **before** the DB
//! row is inserted. A crash between the two leaves an orphan file that no DB
//! row references — invisible to the app, and simply overwritten when the
//! save is retried (the version number is derived from the DB, not the
//! filesystem). The reverse order could leave a DB row pointing at a missing
//! file, which *would* be corruption.
//!
//! Concurrency: `save_artifact` is designed to run inside the single shared
//! connection lock (`db::with_conn_result`), so the version query, file
//! writes, and insert are one serialized unit — two concurrent saves can
//! never race to the same version number.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use ego_tree::NodeRef;
use rusqlite::{Connection, OptionalExtension};
use scraper::node::Element;
use scraper::{Html, Node};

use crate::threads::{self, ThreadStatus};

/// Hard cap: files above this are rejected (`E-SIZE-MAX`, spec §8.1).
pub const MAX_SIZE_BYTES: usize = 52_428_800; // 50 MiB
/// Soft cap: files above this warn (`W-SIZE`, spec §8.2).
pub const WARN_SIZE_BYTES: usize = 5_242_880; // 5 MiB

/// The Tier-2 pinned-CDN allowlist (spec §7.1). One host by design so this
/// stays a pure URL-prefix match. Extending it means editing the spec table
/// first (which implies updating the runtime CSP alongside).
const CDN_ALLOWLIST: &[&str] = &[
    "https://cdn.jsdelivr.net/npm/mermaid@11",
    "https://cdn.jsdelivr.net/npm/@mermaid-js/layout-elk@0",
    "https://cdn.jsdelivr.net/npm/motion@12",
    "https://cdn.jsdelivr.net/npm/animejs@4",
    "https://cdn.jsdelivr.net/npm/gsap@3",
    "https://cdn.jsdelivr.net/npm/d3@7",
    "https://cdn.jsdelivr.net/npm/katex@0.17",
    "https://cdn.jsdelivr.net/npm/markmap-lib@0",
    "https://cdn.jsdelivr.net/npm/markmap-view@0",
    "https://cdn.jsdelivr.net/npm/markmap-toolbar@0",
    "https://cdn.jsdelivr.net/npm/@highlightjs/cdn-assets@11",
];

/// One validation-rule outcome. `code` is a stable identifier from the spec
/// (§8): `E-*` = hard failure, `W-*` = warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub code: &'static str,
    pub message: String,
}

impl Issue {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Issue {
            code,
            message: message.into(),
        }
    }
}

/// The validator's verdict: any entry in `errors` rejects the save.
#[derive(Debug, Default)]
pub struct Validation {
    pub errors: Vec<Issue>,
    pub warnings: Vec<Issue>,
}

/// Result of a successful save.
#[derive(Debug)]
pub struct SavedArtifact {
    pub thread_id: String,
    pub project_id: String,
    pub version: i64,
    /// `initial` for v1, `follow_up` for later versions (inferred — see the
    /// API doc; the caller never supplies it).
    pub created_by: &'static str,
    /// Absolute path of the immutable versioned file (`artifact.vN.html`).
    pub file_path: PathBuf,
    pub warnings: Vec<Issue>,
    /// Comments whose rows changed in the FR-4.4 re-attachment pass that runs
    /// with every follow-up save (advanced to this version, anchor rewritten,
    /// and/or flagged `moved`). The route layer emits one `comment-updated`
    /// event per entry. Empty for v1 (nothing to re-attach).
    pub reattached: Vec<crate::comments::Comment>,
}

/// Errors from the save pipeline. Variants map to HTTP statuses in
/// `server::artifacts_routes`.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    #[error("thread not found: {0}")]
    ThreadNotFound(String),

    /// Validation hard failures (spec §8.1) — nothing was stored.
    #[error("artifact rejected: {}", .0.first().map(|i| i.code).unwrap_or("E-?"))]
    Rejected(Vec<Issue>),

    #[error("artifact candidate was based on version {base:?}, but current is {current:?}")]
    Conflict {
        run_id: String,
        base: Option<i64>,
        current: Option<i64>,
        candidate_path: PathBuf,
    },

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

// ---------------------------------------------------------------------------
// Storage layout (PRD §5.6)
// ---------------------------------------------------------------------------

/// `~/Documents/conceptify/artifacts` (created if missing). Centralized per
/// the OQ2 decision — never inside the mapped project directory.
///
/// # Test isolation (bead `conceptify-028`)
///
/// In `cfg(test)` builds the real-Documents branch is compiled out entirely:
/// `artifacts_root()` can *only* resolve to a per-process scratch dir under
/// `std::env::temp_dir()` ([`test_artifacts_root`]). This is structural, not a
/// convention a test must remember. Previously a test-ordering race let any
/// code path that reached `artifacts_root()` *before* a harness had set
/// `CONCEPTIFY_TEST_ARTIFACTS_DIR` fall through here and write `proj-*` dirs
/// into the user's real `~/Documents/conceptify/artifacts`; the harness `Drop`
/// then cleaned up the (empty) scratch subtree instead, so the real dirs
/// leaked. Now there is no production fall-through to reach in test builds.
pub fn artifacts_root() -> io::Result<PathBuf> {
    #[cfg(test)]
    {
        // The real ~/Documents root is unreachable under `cfg(test)`: resolve to
        // the shared per-process scratch root instead (single source of truth,
        // so harness cleanup converges on the same dir).
        let dir = test_artifacts_root();
        fs::create_dir_all(&dir)?;
        return Ok(dir);
    }

    #[cfg(not(test))]
    {
        let base = dirs::document_dir().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "could not resolve the platform Documents directory",
            )
        })?;
        let dir = base.join("conceptify").join("artifacts");
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}

/// The single process-wide scratch artifacts root every test path resolves to
/// (bead `conceptify-028`). The real `~/Documents` root is unreachable in test
/// builds, so this is the *only* place tests ever write artifacts.
///
/// Source of truth for both the production [`artifacts_root`] (under
/// `cfg(test)`) and every module's test harness (`runs`, `flows`,
/// `server::artifacts_routes`), which delegate here. Computed exactly once via
/// a `OnceLock` — so at most one `set_var` runs per process, avoiding the
/// multi-writer env races the old per-module helpers had. An explicit
/// `CONCEPTIFY_TEST_ARTIFACTS_DIR` pinned before first use is honored (an escape
/// hatch, e.g. `CONCEPTIFY_TEST_ARTIFACTS_DIR=/dir cargo test`); otherwise a
/// deterministic per-process path under `temp_dir()` is derived and published
/// back to the env var so any out-of-process consumer converges too. Isolation
/// between concurrent tests comes from unique per-test project ids under this
/// shared root, matching the existing harness design.
#[cfg(test)]
pub(crate) fn test_artifacts_root() -> PathBuf {
    use std::sync::OnceLock;
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::env::var_os("CONCEPTIFY_TEST_ARTIFACTS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!(
                    "conceptify-test-artifact-roots-{}",
                    std::process::id()
                ))
            });
        std::env::set_var("CONCEPTIFY_TEST_ARTIFACTS_DIR", root.as_os_str());
        root
    })
    .clone()
}

/// `<root>/<project-id>/threads/<thread-slug>` — the per-thread artifact dir.
/// Public so the thread-delete command (bead conceptify-0kt) can remove the
/// whole directory when a thread is retired, using the same path construction
/// as every save/version/log helper below (single source of truth).
pub fn thread_dir(root: &Path, project_id: &str, slug: &str) -> PathBuf {
    root.join(project_id).join("threads").join(slug)
}

/// `<root>/<project-id>/threads/<slug>/artifact.v<version>.html` — the
/// immutable versioned artifact file written by `save_artifact`. Used by the
/// `artifact://` protocol handler (`crate::artifact_protocol`) to map a
/// DB-resolved (project, slug, version) triple back to the on-disk file.
/// Must stay in lockstep with the naming inside `save_artifact`; the
/// protocol tests pin the two together by saving through `save_artifact`
/// and reading back through this helper.
pub fn version_file_path(root: &Path, project_id: &str, slug: &str, version: i64) -> PathBuf {
    thread_dir(root, project_id, slug).join(format!("artifact.v{version}.html"))
}

/// `<root>/<project-id>/threads/<slug>/artifact.html` — the always-latest
/// copy `save_artifact` atomically rewrites on every save (§5.6). This is
/// the file "Open in browser" (FR-2.5) hands to the system default browser:
/// a real, pristine, self-contained HTML file with zero Conceptify residue.
/// Must stay in lockstep with the naming inside `save_artifact` (pinned by
/// the `latest_copy_path_matches_save_output` test).
pub fn latest_copy_path(root: &Path, project_id: &str, slug: &str) -> PathBuf {
    thread_dir(root, project_id, slug).join("artifact.html")
}

/// `<root>/<project-id>/threads/<slug>/runs/<run-id>.log` — a headless
/// agent-run transcript (§5.6), written by the run engine (`crate::runs`).
/// The `runs/` directory itself is created by `save_artifact` on first save
/// and (defensively) by the run engine before it writes.
pub fn run_log_path(root: &Path, project_id: &str, slug: &str, run_id: &str) -> PathBuf {
    thread_dir(root, project_id, slug)
        .join("runs")
        .join(format!("{run_id}.log"))
}

/// The latest (highest-version) artifact stored for a thread. Returns the
/// version and the absolute path of its immutable `artifact.vN.html` file, or
/// `None` when the thread has no artifact yet (still `generating`).
///
/// Read helper composed by the thread-context aggregation (`crate::context`,
/// §5.2 `get-context`); the path comes from the DB (`file_path`), which
/// `save_artifact` wrote as the absolute versioned path.
pub fn latest_artifact(
    conn: &Connection,
    thread_id: &str,
) -> Result<Option<LatestArtifact>, rusqlite::Error> {
    conn.query_row(
        "SELECT version, file_path FROM artifacts
         WHERE thread_id = ?1
         ORDER BY version DESC
         LIMIT 1",
        [thread_id],
        |row| {
            Ok(LatestArtifact {
                version: row.get(0)?,
                file_path: row.get(1)?,
            })
        },
    )
    .optional()
}

/// The latest artifact's version and on-disk path (see `latest_artifact`).
#[derive(Debug, Clone)]
pub struct LatestArtifact {
    pub version: i64,
    pub file_path: String,
}

/// Write `bytes` to `path` atomically: full write + fsync to a `.tmp` sibling
/// in the same directory, then `rename` over the destination (atomic on the
/// same filesystem, per N4). A crash mid-write leaves only the `.tmp` file;
/// the destination is either the old content or the new, never a mix.
fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?
        .to_os_string();
    file_name.push(".tmp");
    let tmp = path.with_file_name(file_name);

    let write_result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()
    })();

    if let Err(e) = write_result {
        let _ = fs::remove_file(&tmp); // best-effort cleanup, never visible anyway
        return Err(e);
    }

    fs::rename(&tmp, path)
}

// ---------------------------------------------------------------------------
// The save pipeline (FR-3.6)
// ---------------------------------------------------------------------------

/// Validate and store `bytes` as the next artifact version for `thread_id`.
///
/// Runs entirely under the caller's connection lock (see the module doc):
/// thread lookup → version assignment → validation → atomic file writes →
/// one DB transaction (artifact row + thread `→ ready`). Any failure before
/// the transaction commits leaves the DB untouched and no *visible* partial
/// file on disk.
pub fn save_artifact(
    conn: &Connection,
    root: &Path,
    thread_id: &str,
    bytes: &[u8],
) -> Result<SavedArtifact, ArtifactError> {
    save_artifact_for_run(conn, root, thread_id, bytes, None, None)
}

/// Run-aware save path. A headless mutation supplies its durable run id through
/// the inherited CLI environment; the server reads the immutable captured base
/// from that row and retains stale output as a candidate instead of publishing.
pub fn save_artifact_for_run(
    conn: &Connection,
    root: &Path,
    thread_id: &str,
    bytes: &[u8],
    source_run_id: Option<&str>,
    resolution: Option<&str>,
) -> Result<SavedArtifact, ArtifactError> {
    let row = conn
        .query_row(
            "SELECT project_id, slug FROM threads WHERE id = ?1",
            [thread_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((project_id, slug)) = row else {
        return Err(ArtifactError::ThreadNotFound(thread_id.to_owned()));
    };

    // Version is derived from the DB (not the filesystem), so an orphan file
    // from an earlier crashed save is harmlessly overwritten.
    let current_version: Option<i64> = conn.query_row(
        "SELECT MAX(version) FROM artifacts WHERE thread_id = ?1",
        [thread_id],
        |r| r.get(0),
    )?;
    let version = current_version.unwrap_or(0) + 1;

    let validation = validate(bytes, version);
    if !validation.errors.is_empty() {
        return Err(ArtifactError::Rejected(validation.errors));
    }

    let dir = thread_dir(root, &project_id, &slug);
    // `runs/` is reserved for headless-agent transcripts (§5.6); created here
    // so the layout is complete from the first save.
    fs::create_dir_all(dir.join("runs"))?;

    let mut source_base_version = None;
    if let Some(run_id) = source_run_id {
        let run: Option<(String, String, Option<i64>)> = conn
            .query_row(
                "SELECT thread_id, run_class, base_artifact_version
                 FROM follow_up_runs WHERE id = ?1",
                [run_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((run_thread_id, run_class, captured_base)) = run else {
            return Err(ArtifactError::Database(rusqlite::Error::QueryReturnedNoRows));
        };
        if run_thread_id != thread_id || run_class != "mutation" {
            return Err(ArtifactError::Database(rusqlite::Error::InvalidQuery));
        }
        source_base_version = captured_base;
        if captured_base != current_version && resolution.is_none() {
            let candidate_path = dir.join("runs").join(format!("{run_id}.candidate.html"));
            atomic_write(&candidate_path, bytes)?;
            conn.execute(
                "UPDATE follow_up_runs
                 SET status = 'conflicted', status_reason = 'stale_base',
                     candidate_path = ?2, conflict_current_version = ?3,
                     conflict_resolution = 'pending'
                 WHERE id = ?1 AND status IN ('starting', 'running')",
                rusqlite::params![run_id, candidate_path.to_string_lossy(), current_version],
            )?;
            return Err(ArtifactError::Conflict {
                run_id: run_id.to_owned(),
                base: captured_base,
                current: current_version,
                candidate_path,
            });
        }
    }

    let version_path = dir.join(format!("artifact.v{version}.html"));
    atomic_write(&version_path, bytes)?;
    // `artifact.html` is a real copy of the latest version (never a symlink),
    // written with the same temp+rename discipline — a stale/dangling alias
    // is never possible (§5.6).
    atomic_write(&dir.join("artifact.html"), bytes)?;

    let created_by = if version == 1 { "initial" } else { "follow_up" };

    // Artifact row + thread status transition + comment re-attachment commit
    // together: readers never observe a `ready` thread without its artifact
    // row, or the new version without its comments migrated (FR-4.4).
    let tx = conn.unchecked_transaction()?;
    let artifact_id = uuid::Uuid::new_v4().to_string();
    if source_run_id.is_some() {
        tx.execute(
            "INSERT INTO artifacts
                 (id, thread_id, version, file_path, created_by,
                  source_run_id, source_base_version, resolution)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                artifact_id, thread_id, version, version_path.to_string_lossy(),
                created_by, source_run_id, source_base_version, resolution
            ],
        )?;
    } else {
        // Keep the pure/manual path compatible with focused test schemas and
        // older embedders that only model the original artifact columns.
        tx.execute(
            "INSERT INTO artifacts (id, thread_id, version, file_path, created_by)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                artifact_id, thread_id, version, version_path.to_string_lossy(), created_by
            ],
        )?;
    }
    threads::set_thread_status(&tx, thread_id, ThreadStatus::Ready)?;
    if let (Some(run_id), Some(resolution)) = (source_run_id, resolution) {
        tx.execute(
            "UPDATE follow_up_runs
             SET conflict_resolution = ?2,
                 activity_dismissed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1 AND status = 'conflicted'",
            rusqlite::params![run_id, resolution],
        )?;
    }

    // FR-4.4 re-attachment: migrate earlier-version comments onto this new
    // version (or flag them "reference moved"). Runs after the artifact row
    // insert so an advanced `artifact_version` satisfies the composite FK.
    // `bytes` is valid UTF-8 here (validation would have rejected otherwise).
    let reattached = if version > 1 {
        let text = std::str::from_utf8(bytes).expect("validated artifact is UTF-8");
        crate::anchoring::reattach_thread_comments(&tx, text, thread_id, version)?
    } else {
        Vec::new()
    };

    tx.commit()?;

    Ok(SavedArtifact {
        thread_id: thread_id.to_owned(),
        project_id,
        version,
        created_by,
        file_path: version_path,
        warnings: validation.warnings,
        reattached,
    })
}

// ---------------------------------------------------------------------------
// Validation (docs/artifact-spec.md §8 — the spec is the contract)
// ---------------------------------------------------------------------------

/// Run the full §8 rule set against a submitted file. `assigned_version` is
/// the server-assigned version for the `W-VERSION-MISMATCH` check (file-only
/// validators would skip it; we always know it).
///
/// Pure function over the bytes — no I/O, no DB.
pub fn validate(bytes: &[u8], assigned_version: i64) -> Validation {
    let mut v = Validation::default();

    if bytes.len() > MAX_SIZE_BYTES {
        v.errors.push(Issue::new(
            "E-SIZE-MAX",
            format!(
                "file is {} bytes, over the 50 MiB hard cap ({MAX_SIZE_BYTES} bytes)",
                bytes.len()
            ),
        ));
        // Rejected regardless; don't burn time parsing 50+ MiB.
        return v;
    }

    let Ok(text) = std::str::from_utf8(bytes) else {
        v.errors
            .push(Issue::new("E-UTF8", "file is not valid UTF-8"));
        return v;
    };

    if bytes.len() > WARN_SIZE_BYTES {
        v.warnings.push(Issue::new(
            "W-SIZE",
            format!(
                "file is {} bytes, over the 5 MiB advisory cap ({WARN_SIZE_BYTES} bytes)",
                bytes.len()
            ),
        ));
    }

    // W-DOCTYPE: must begin with `<!doctype html>` (case-insensitive; leading
    // whitespace/BOM permitted). Checked on the raw text — the parser
    // normalizes doctypes away.
    let after_bom = text.trim_start_matches('\u{feff}').trim_start();
    let has_doctype = after_bom
        .get(..15)
        .is_some_and(|p| p.eq_ignore_ascii_case("<!doctype html>"));
    if !has_doctype {
        v.warnings.push(Issue::new(
            "W-DOCTYPE",
            "file does not begin with <!doctype html> (quirks mode breaks the design system)",
        ));
    }

    if text.trim().is_empty() {
        v.errors.push(Issue::new("E-PARSE", "file is empty"));
        return v;
    }

    let doc = Html::parse_document(text);
    let mut ctx = DocContext::default();
    walk_document(&doc, &mut ctx, &mut v);

    // E-PARSE: HTML5 parsing is error-recovering, so "unparseable" in
    // practice means "nothing survives parsing" — a <body> with no elements.
    if !ctx.body_has_elements {
        v.errors.push(Issue::new(
            "E-PARSE",
            "HTML parsing yields a document whose <body> contains no elements",
        ));
    }

    // Head metadata (W-CHARSET / W-TITLE / W-META / W-VERSION-MISMATCH).
    if !ctx.has_utf8_charset {
        v.warnings.push(Issue::new(
            "W-CHARSET",
            "no <meta charset=\"utf-8\"> in <head>",
        ));
    }
    if !ctx.has_title {
        v.warnings
            .push(Issue::new("W-TITLE", "missing or empty <title>"));
    }
    for (name, value) in [
        ("cfy:question", &ctx.meta_question),
        ("cfy:version", &ctx.meta_version),
        ("cfy:generated-by", &ctx.meta_generated_by),
    ] {
        if value.as_deref().map(str::trim).unwrap_or("").is_empty() {
            v.warnings.push(Issue::new(
                "W-META",
                format!("required <meta name=\"{name}\"> is missing or has empty content"),
            ));
        }
    }
    if let Some(file_version) = ctx.meta_version.as_deref().map(str::trim) {
        if !file_version.is_empty() && file_version.parse::<i64>() != Ok(assigned_version) {
            v.warnings.push(Issue::new(
                "W-VERSION-MISMATCH",
                format!(
                    "cfy:version is \"{file_version}\" but the server assigned version \
                     {assigned_version} (the server-assigned version is authoritative)"
                ),
            ));
        }
    }

    // W-ANCHOR-NONE / W-ID-DUP over the collected id set.
    if ctx.cfy_ids.is_empty() {
        v.warnings.push(Issue::new(
            "W-ANCHOR-NONE",
            "no data-cfy-id attributes anywhere in the document (comments would be \
             text-quote-only)",
        ));
    } else {
        let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for id in &ctx.cfy_ids {
            *counts.entry(id.as_str()).or_default() += 1;
        }
        for (id, n) in counts {
            if n > 1 {
                v.warnings.push(Issue::new(
                    "W-ID-DUP",
                    format!("data-cfy-id \"{id}\" appears on {n} elements"),
                ));
            }
        }
    }

    v
}

/// Facts accumulated in the single document walk.
#[derive(Default)]
struct DocContext {
    body_has_elements: bool,
    has_utf8_charset: bool,
    has_title: bool,
    meta_question: Option<String>,
    meta_version: Option<String>,
    meta_generated_by: Option<String>,
    /// Every `data-cfy-id` value in document order (for W-ANCHOR-NONE and
    /// W-ID-DUP; W-ID-FORMAT is emitted inline during the walk).
    cfy_ids: Vec<String>,
}

/// One pass over the whole tree. Uses manual traversal (not CSS selectors) so
/// SVG foreign content is matched by local name without namespace surprises.
fn walk_document(doc: &Html, ctx: &mut DocContext, v: &mut Validation) {
    for node in doc.tree.root().descendants() {
        match node.value() {
            Node::Element(el) => visit_element(&node, el, ctx, v),
            Node::Comment(c) => visit_comment(&node, c, v),
            _ => {}
        }
    }
}

fn visit_element(
    node: &NodeRef<'_, Node>,
    el: &Element,
    ctx: &mut DocContext,
    v: &mut Validation,
) {
    let name = el.name();

    // data-cfy-id collection + W-ID-FORMAT.
    if let Some(id) = el.attr("data-cfy-id") {
        if !valid_cfy_id(id) {
            v.warnings.push(Issue::new(
                "W-ID-FORMAT",
                format!(
                    "data-cfy-id \"{id}\" violates the id grammar \
                     (dot-separated kebab-case segments, ≤ 64 chars)"
                ),
            ));
        }
        ctx.cfy_ids.push(id.to_owned());
    }

    // Inline CSS in style="" attributes (url(...) resource refs; @import is
    // not valid in attribute position but the scanner handles it uniformly).
    if let Some(css) = el.attr("style") {
        scan_inline_css(css, v);
    }

    match name {
        "body" => {
            ctx.body_has_elements = node
                .descendants()
                .skip(1)
                .any(|d| matches!(d.value(), Node::Element(_)));
        }
        // E-EXTERNAL-CODE: script[src] against the §7.1 allowlist. Relative
        // and file:// URLs fail by definition (they can't match).
        "script" => {
            if let Some(src) = el.attr("src") {
                if !is_allowlisted(src) {
                    v.errors.push(Issue::new(
                        "E-EXTERNAL-CODE",
                        format!("script src \"{src}\" is not on the Tier-2 CDN allowlist"),
                    ));
                }
            }
        }
        // E-EXTERNAL-CODE: link[href] where rel contains stylesheet /
        // preload / modulepreload (space-separated token list).
        "link" => {
            let rel_is_code = el.attr("rel").is_some_and(|rel| {
                rel.split_ascii_whitespace().any(|t| {
                    t.eq_ignore_ascii_case("stylesheet")
                        || t.eq_ignore_ascii_case("preload")
                        || t.eq_ignore_ascii_case("modulepreload")
                })
            });
            if rel_is_code {
                if let Some(href) = el.attr("href") {
                    if !is_allowlisted(href) {
                        v.errors.push(Issue::new(
                            "E-EXTERNAL-CODE",
                            format!(
                                "link href \"{href}\" (rel makes it code/style-loading) is not \
                                 on the Tier-2 CDN allowlist"
                            ),
                        ));
                    }
                }
            }
        }
        // Inline stylesheet contents: @import (E-EXTERNAL-CODE / W-LOCAL-REF)
        // and url(...) resource refs (W-EXTERNAL-REF / W-LOCAL-REF).
        "style" => {
            let css = collect_text(node);
            scan_inline_css(&css, v);
        }
        "meta" => {
            if parent_is(node, "head") {
                if el
                    .attr("charset")
                    .is_some_and(|c| c.trim().eq_ignore_ascii_case("utf-8"))
                {
                    ctx.has_utf8_charset = true;
                }
                if let (Some(meta_name), content) = (el.attr("name"), el.attr("content")) {
                    let slot = match meta_name {
                        "cfy:question" => Some(&mut ctx.meta_question),
                        "cfy:version" => Some(&mut ctx.meta_version),
                        "cfy:generated-by" => Some(&mut ctx.meta_generated_by),
                        _ => None,
                    };
                    if let Some(slot) = slot {
                        if slot.is_none() {
                            *slot = Some(content.unwrap_or("").to_owned());
                        }
                    }
                }
            }
        }
        // Only the document <title> counts — SVG <title> is a description
        // element (Graphviz emits one per node) and must not satisfy W-TITLE.
        "title" => {
            if parent_is(node, "head") && !collect_text(node).trim().is_empty() {
                ctx.has_title = true;
            }
        }
        // W-ANCHOR-HEADINGS: h1–h4 must carry data-cfy-id.
        "h1" | "h2" | "h3" | "h4" => {
            if el.attr("data-cfy-id").is_none() {
                let heading_text = truncate(collect_text(node).trim(), 60);
                v.warnings.push(Issue::new(
                    "W-ANCHOR-HEADINGS",
                    format!("<{name}> \"{heading_text}\" lacks data-cfy-id"),
                ));
            }
        }
        // W-ANCHOR-DIAGRAM: only outermost <svg> elements are analyzed so a
        // nested <svg> never double-counts.
        "svg" => {
            let nested = node.ancestors().any(|a| {
                matches!(a.value(), Node::Element(ancestor) if ancestor.name() == "svg")
            });
            if !nested {
                check_diagram_coverage(node, el, v);
            }
        }
        // W-EXTERNAL-REF / W-LOCAL-REF resource positions (spec §8.2 list).
        "img" => {
            check_resource_url(el.attr("src"), "img src", v);
            check_srcset(el.attr("srcset"), "img srcset", v);
        }
        "source" => {
            check_resource_url(el.attr("src"), "source src", v);
            check_srcset(el.attr("srcset"), "source srcset", v);
        }
        "video" | "audio" | "track" | "iframe" | "embed" => {
            check_resource_url(el.attr("src"), name, v);
        }
        "object" => {
            check_resource_url(el.attr("data"), "object data", v);
        }
        // W-LOCAL-REF: relative <a href> (absolute http(s) links are fine —
        // §1 explicitly permits them for further reading).
        "a" => {
            if let Some(href) = el.attr("href") {
                match classify_url(href) {
                    UrlKind::File | UrlKind::Relative => {
                        v.warnings.push(Issue::new(
                            "W-LOCAL-REF",
                            format!("a href \"{href}\" is a relative or file:// URL (broken by \
                                     definition — there is no \"next to the file\")"),
                        ));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// `<!--cfy:src ...-->` comments: W-SRC-MALFORMED + W-SRC-ORPHAN (spec §5.1).
fn visit_comment(
    node: &NodeRef<'_, Node>,
    comment: &scraper::node::Comment,
    v: &mut Validation,
) {
    let text: &str = comment;
    // The finder contract is `<!--cfy:src\s` — `cfy:src` immediately after
    // the opener, followed by whitespace.
    let Some(rest) = text.strip_prefix("cfy:src") else {
        return;
    };
    if !rest.starts_with(|c: char| c.is_whitespace()) {
        return;
    }

    let first_line = rest.lines().next().unwrap_or("");
    let attrs = parse_src_attrs(first_line);
    let lang = attrs.iter().find(|(k, _)| k == "lang").map(|(_, v)| v);
    let for_id = attrs.iter().find(|(k, _)| k == "for").map(|(_, v)| v);

    if lang.is_none() || for_id.is_none() {
        v.warnings.push(Issue::new(
            "W-SRC-MALFORMED",
            format!(
                "cfy:src comment lacks a required attribute (lang: {}, for: {})",
                if lang.is_some() { "present" } else { "missing" },
                if for_id.is_some() { "present" } else { "missing" },
            ),
        ));
    }

    // W-SRC-ORPHAN: the next non-whitespace sibling must be an element whose
    // data-cfy-id matches `for`. Only checkable when `for` is present.
    if let Some(for_id) = for_id {
        let next_element = node.next_siblings().find(|sib| match sib.value() {
            Node::Text(t) => !t.trim().is_empty(), // non-ws text breaks adjacency
            Node::Element(_) | Node::Comment(_) => true,
            _ => false,
        });
        let matches = next_element.is_some_and(|sib| {
            matches!(sib.value(), Node::Element(el) if el.attr("data-cfy-id") == Some(for_id))
        });
        if !matches {
            v.warnings.push(Issue::new(
                "W-SRC-ORPHAN",
                format!(
                    "cfy:src comment for=\"{for_id}\" is not immediately followed by an element \
                     with that data-cfy-id"
                ),
            ));
        }
    }
}

/// W-ANCHOR-DIAGRAM: an svg with ≥ 6 shape elements needs ≥ 3 data-cfy-id
/// bearers (the svg itself and all descendants count for both tallies as the
/// spec defines them).
fn check_diagram_coverage(
    node: &NodeRef<'_, Node>,
    el: &Element,
    v: &mut Validation,
) {
    const SHAPES: [&str; 7] = [
        "path", "rect", "circle", "ellipse", "line", "polyline", "polygon",
    ];
    let mut shape_count = 0usize;
    let mut id_count = usize::from(el.attr("data-cfy-id").is_some());

    for d in node.descendants().skip(1) {
        if let Node::Element(child) = d.value() {
            if SHAPES.contains(&child.name()) {
                shape_count += 1;
            }
            if child.attr("data-cfy-id").is_some() {
                id_count += 1;
            }
        }
    }

    if shape_count >= 6 && id_count < 3 {
        let label = el
            .attr("data-cfy-id")
            .or_else(|| el.attr("aria-label"))
            .unwrap_or("unlabeled");
        v.warnings.push(Issue::new(
            "W-ANCHOR-DIAGRAM",
            format!(
                "svg \"{label}\" has thin anchor coverage: {shape_count} shape elements but \
                 only {id_count} data-cfy-id bearers (need ≥ 3)"
            ),
        ));
    }
}

// ---------------------------------------------------------------------------
// URL classification & the allowlist match rule
// ---------------------------------------------------------------------------

/// The §7.1 match rule, exactly: (1) starts with an allowlist prefix,
/// (2) the character immediately after the prefix — if any — is `.` or `/`,
/// (3) the remainder is only `[A-Za-z0-9@._/-]` with no `..` path segment.
/// Case-sensitive throughout.
fn is_allowlisted(url: &str) -> bool {
    CDN_ALLOWLIST.iter().any(|prefix| {
        let Some(rest) = url.strip_prefix(prefix) else {
            return false;
        };
        if rest.is_empty() {
            return true;
        }
        let first = rest.as_bytes()[0];
        if first != b'.' && first != b'/' {
            return false;
        }
        rest.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'@' | b'.' | b'_' | b'/' | b'-'))
            && rest.split('/').all(|seg| seg != "..")
    })
}

enum UrlKind {
    /// Empty or fragment-only (`#…`) — in-document, fine.
    Fragment,
    /// `data:` URI — permitted (spec §1).
    Data,
    /// `http:`/`https:` (or protocol-relative `//…`) — external network ref.
    Http,
    /// `file://` — always broken (spec §1).
    File,
    /// No scheme — relative, always broken (spec §1).
    Relative,
    /// Some other scheme (`mailto:`, `javascript:`, …) — not a resource ref.
    Other,
}

fn classify_url(url: &str) -> UrlKind {
    let u = url.trim();
    if u.is_empty() || u.starts_with('#') {
        return UrlKind::Fragment;
    }
    if u.starts_with("//") {
        return UrlKind::Http; // protocol-relative: resolves to a network host
    }
    match url_scheme(u) {
        Some(s) if s.eq_ignore_ascii_case("http") || s.eq_ignore_ascii_case("https") => {
            UrlKind::Http
        }
        Some(s) if s.eq_ignore_ascii_case("file") => UrlKind::File,
        Some(s) if s.eq_ignore_ascii_case("data") => UrlKind::Data,
        Some(_) => UrlKind::Other,
        None => UrlKind::Relative,
    }
}

/// RFC 3986 scheme: `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) ":"`.
fn url_scheme(url: &str) -> Option<&str> {
    let colon = url.find(':')?;
    let candidate = &url[..colon];
    let mut chars = candidate.chars();
    let first = chars.next()?;
    if first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        Some(candidate)
    } else {
        None
    }
}

/// Non-code resource position (img src, media src, iframe, object/embed):
/// http(s) → W-EXTERNAL-REF (regardless of host — blocked by the runtime CSP
/// and broken offline); relative / file:// → W-LOCAL-REF; data: → fine.
fn check_resource_url(url: Option<&str>, position: &str, v: &mut Validation) {
    let Some(url) = url else { return };
    match classify_url(url) {
        UrlKind::Http => v.warnings.push(Issue::new(
            "W-EXTERNAL-REF",
            format!(
                "{position} \"{}\" is an external URL (blocked by the runtime CSP; breaks \
                 offline rendering)",
                truncate(url, 120)
            ),
        )),
        UrlKind::File | UrlKind::Relative => v.warnings.push(Issue::new(
            "W-LOCAL-REF",
            format!(
                "{position} \"{}\" is a relative or file:// URL (broken by definition)",
                truncate(url, 120)
            ),
        )),
        _ => {}
    }
}

/// A `srcset` is comma-separated `URL [descriptor]` candidates; check each URL.
fn check_srcset(srcset: Option<&str>, position: &str, v: &mut Validation) {
    let Some(srcset) = srcset else { return };
    for candidate in srcset.split(',') {
        if let Some(url) = candidate.split_ascii_whitespace().next() {
            check_resource_url(Some(url), position, v);
        }
    }
}

// ---------------------------------------------------------------------------
// Inline CSS scanning (@import → E-EXTERNAL-CODE; url(...) → W-EXTERNAL-REF)
// ---------------------------------------------------------------------------

/// Linear scan of an inline stylesheet (or style attribute) for `@import`
/// (a code-loading position, spec §8.1) and `url(...)` resource refs
/// (spec §8.2). CSS comments are stripped first so commented-out rules don't
/// trip warnings.
fn scan_inline_css(css: &str, v: &mut Validation) {
    let css = strip_css_comments(css);
    let lower = css.to_ascii_lowercase(); // ASCII-only fold keeps byte offsets aligned
    let bytes = lower.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        let next_import = lower[i..].find("@import").map(|p| i + p);
        let next_url = lower[i..].find("url(").map(|p| i + p);

        match (next_import, next_url) {
            (Some(imp), url) if url.is_none_or(|u| imp < u) => {
                // @import <url> — either a string or a url(...) token.
                let mut j = imp + "@import".len();
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                let (url_value, consumed_to) = if lower[j..].starts_with("url(") {
                    parse_url_token(&css, j + 4)
                } else if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
                    parse_quoted(&css, j)
                } else {
                    (String::new(), j)
                };
                i = consumed_to.max(imp + "@import".len());

                let url_value = url_value.trim().to_owned();
                if url_value.is_empty() {
                    continue;
                }
                match classify_url(&url_value) {
                    // The checked position is "@import of an http(s) URL":
                    // http(s) imports must pass the allowlist match rule.
                    UrlKind::Http => {
                        if !is_allowlisted(&url_value) {
                            v.errors.push(Issue::new(
                                "E-EXTERNAL-CODE",
                                format!(
                                    "CSS @import \"{url_value}\" is not on the Tier-2 CDN \
                                     allowlist"
                                ),
                            ));
                        }
                    }
                    // Non-http(s) imports aren't the E- position; they're
                    // still broken local refs.
                    UrlKind::File | UrlKind::Relative => {
                        v.warnings.push(Issue::new(
                            "W-LOCAL-REF",
                            format!(
                                "CSS @import \"{url_value}\" is a relative or file:// URL \
                                 (broken by definition)"
                            ),
                        ));
                    }
                    _ => {}
                }
            }
            (_, Some(url_pos)) => {
                let (url_value, consumed_to) = parse_url_token(&css, url_pos + 4);
                i = consumed_to.max(url_pos + 4);
                check_resource_url(Some(url_value.trim()), "CSS url()", v);
            }
            _ => break,
        }
    }
}

/// Parse a CSS `url(` token body starting at `start` (just past the open
/// paren). Returns the URL (unquoted, trimmed) and the index just past the
/// closing delimiter.
fn parse_url_token(css: &str, start: usize) -> (String, usize) {
    let bytes = css.as_bytes();
    let mut j = start;
    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
        let (s, end) = parse_quoted(css, j);
        // Skip to the closing paren after the quoted string.
        let close = css[end..].find(')').map(|p| end + p + 1).unwrap_or(end);
        (s, close)
    } else {
        match css[j..].find(')') {
            Some(p) => (css[j..j + p].trim().to_owned(), j + p + 1),
            None => (css[j..].trim().to_owned(), css.len()),
        }
    }
}

/// Parse a quoted string starting at the quote character `css[start]`.
/// Returns the contents and the index just past the closing quote.
fn parse_quoted(css: &str, start: usize) -> (String, usize) {
    let quote = css.as_bytes()[start];
    match css[start + 1..].find(quote as char) {
        Some(p) => (
            css[start + 1..start + 1 + p].to_owned(),
            start + 1 + p + 1,
        ),
        None => (css[start + 1..].to_owned(), css.len()),
    }
}

fn strip_css_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(open) = rest.find("/*") {
        out.push_str(&rest[..open]);
        match rest[open + 2..].find("*/") {
            Some(close) => rest = &rest[open + 2 + close + 2..],
            None => return out, // unterminated comment swallows the tail
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// The §4.2 id grammar: dot-separated segments, each `lower [ (lower / digit
/// / "-")* (lower / digit) ]`, total ≤ 64 chars.
fn valid_cfy_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 64 {
        return false;
    }
    id.split('.').all(|seg| {
        let b = seg.as_bytes();
        !b.is_empty()
            && b[0].is_ascii_lowercase()
            && b[b.len() - 1] != b'-'
            && b.iter()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
    })
}

/// Space-separated `key="value"` attributes on a cfy:src comment's first line
/// (parse contract from spec §5.1: `([a-z]+)="([^"]*)"`).
fn parse_src_attrs(line: &str) -> Vec<(String, String)> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && !bytes[i].is_ascii_lowercase() {
            i += 1;
        }
        let key_start = i;
        while i < bytes.len() && bytes[i].is_ascii_lowercase() {
            i += 1;
        }
        if key_start == i {
            break;
        }
        if i + 1 < bytes.len() && bytes[i] == b'=' && bytes[i + 1] == b'"' {
            let value_start = i + 2;
            match line[value_start..].find('"') {
                Some(p) => {
                    out.push((
                        line[key_start..i].to_owned(),
                        line[value_start..value_start + p].to_owned(),
                    ));
                    i = value_start + p + 1;
                }
                None => break,
            }
        }
    }
    out
}

/// Concatenated descendant text of a node (headings, <title>, <style>).
fn collect_text(node: &NodeRef<'_, Node>) -> String {
    let mut out = String::new();
    for d in node.descendants() {
        if let Node::Text(t) = d.value() {
            out.push_str(t);
        }
    }
    out
}

fn parent_is(node: &NodeRef<'_, Node>, name: &str) -> bool {
    node.parent()
        .is_some_and(|p| matches!(p.value(), Node::Element(el) if el.name() == name))
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_owned()
    } else {
        let cut: String = s.chars().take(max_chars).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- fixtures ----------------------------------------------------------

    /// A fully spec-conformant document (validates with zero errors and zero
    /// warnings at assigned version 1); `body_extra` is injected inside
    /// <article> for per-rule fixtures.
    fn valid_doc(body_extra: &str) -> String {
        format!(
            r#"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<title>Test artifact</title>
<meta name="cfy:question" content="Explain the thing.">
<meta name="cfy:version" content="1">
<meta name="cfy:generated-by" content="claude-code/test">
</head><body><article>
<h1 data-cfy-id="sec-title">Test artifact</h1>
{body_extra}
</article></body></html>"#
        )
    }

    fn error_codes(v: &Validation) -> Vec<&'static str> {
        v.errors.iter().map(|i| i.code).collect()
    }

    fn warning_codes(v: &Validation) -> Vec<&'static str> {
        v.warnings.iter().map(|i| i.code).collect()
    }

    fn validate_str(html: &str) -> Validation {
        validate(html.as_bytes(), 1)
    }

    /// In-memory DB mirroring the shipped projects/threads/artifacts schema
    /// (the real-migration integration test below covers schema drift).
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
        std::env::temp_dir().join(format!(
            "conceptify-artifacts-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn thread_status(conn: &Connection, id: &str) -> String {
        conn.query_row("SELECT status FROM threads WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
    }

    // -- validator: hard failures ------------------------------------------

    #[test]
    fn minimal_valid_artifact_is_clean() {
        let v = validate_str(&valid_doc(""));
        assert!(v.errors.is_empty(), "errors: {:?}", v.errors);
        assert!(v.warnings.is_empty(), "warnings: {:?}", v.warnings);
    }

    #[test]
    fn e_utf8_rejects_invalid_bytes() {
        let v = validate(&[0xff, 0xfe, 0x41], 1);
        assert_eq!(error_codes(&v), vec!["E-UTF8"]);
    }

    #[test]
    fn e_parse_rejects_empty_and_element_free_input() {
        assert_eq!(error_codes(&validate_str("")), vec!["E-PARSE"]);
        assert_eq!(error_codes(&validate_str("   \n\t ")), vec!["E-PARSE"]);
        // Error-recovering parse of bare text yields an element-free <body>.
        let v = validate_str("just some words, no markup");
        assert!(error_codes(&v).contains(&"E-PARSE"), "{:?}", v.errors);
    }

    #[test]
    fn e_external_code_rejects_non_allowlisted_script_src() {
        let v = validate_str(&valid_doc(
            r#"<script src="https://evil.example.com/x.js"></script>"#,
        ));
        assert_eq!(error_codes(&v), vec!["E-EXTERNAL-CODE"]);

        // Relative and file:// script srcs fail by definition.
        let v = validate_str(&valid_doc(r#"<script src="./local.js"></script>"#));
        assert_eq!(error_codes(&v), vec!["E-EXTERNAL-CODE"]);
        let v = validate_str(&valid_doc(
            r#"<script src="file:///Users/x/lib.js"></script>"#,
        ));
        assert_eq!(error_codes(&v), vec!["E-EXTERNAL-CODE"]);
    }

    #[test]
    fn allowlisted_script_src_passes() {
        let v = validate_str(&valid_doc(
            r#"<script src="https://cdn.jsdelivr.net/npm/mermaid@11.15.0/dist/mermaid.esm.min.mjs"></script>"#,
        ));
        assert!(v.errors.is_empty(), "{:?}", v.errors);
        assert!(v.warnings.is_empty(), "{:?}", v.warnings);
    }

    #[test]
    fn allowlist_match_rule_is_exact() {
        // Rule 1+2: prefix then '.' or '/' (or nothing).
        assert!(is_allowlisted("https://cdn.jsdelivr.net/npm/mermaid@11"));
        assert!(is_allowlisted("https://cdn.jsdelivr.net/npm/mermaid@11.15.0"));
        assert!(is_allowlisted(
            "https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.min.js"
        ));
        assert!(is_allowlisted(
            "https://cdn.jsdelivr.net/npm/@highlightjs/cdn-assets@11/styles/github.min.css"
        ));
        // `mermaid@110` must not match `mermaid@11`.
        assert!(!is_allowlisted("https://cdn.jsdelivr.net/npm/mermaid@110"));
        // Other packages / hosts.
        assert!(!is_allowlisted("https://cdn.jsdelivr.net/npm/lodash@4"));
        assert!(!is_allowlisted("https://unpkg.com/mermaid@11"));
        // Rule 3: charset excludes query strings, fragments, percent-escapes.
        assert!(!is_allowlisted("https://cdn.jsdelivr.net/npm/mermaid@11?x=1"));
        assert!(!is_allowlisted("https://cdn.jsdelivr.net/npm/mermaid@11#f"));
        assert!(!is_allowlisted("https://cdn.jsdelivr.net/npm/mermaid@11/%2e%2e/x"));
        // No `..` path segment.
        assert!(!is_allowlisted("https://cdn.jsdelivr.net/npm/mermaid@11/../gsap@3/x.js"));
        // Case-sensitive.
        assert!(!is_allowlisted("HTTPS://cdn.jsdelivr.net/npm/mermaid@11"));
    }

    #[test]
    fn e_external_code_covers_code_loading_link_rels() {
        for rel in ["stylesheet", "preload", "modulepreload", "PRELOAD stylesheet"] {
            let v = validate_str(&valid_doc(&format!(
                r#"<link rel="{rel}" href="https://fonts.example.com/inter.css">"#
            )));
            assert_eq!(error_codes(&v), vec!["E-EXTERNAL-CODE"], "rel={rel}");
        }
        // Non-code rel values are not the E- position (and <link> is not in
        // the W-EXTERNAL-REF resource list either).
        let v = validate_str(&valid_doc(
            r#"<link rel="icon" href="https://example.com/favicon.ico">"#,
        ));
        assert!(v.errors.is_empty(), "{:?}", v.errors);
        // Allowlisted stylesheet passes.
        let v = validate_str(&valid_doc(
            r#"<link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/katex@0.17/dist/katex.min.css">"#,
        ));
        assert!(v.errors.is_empty(), "{:?}", v.errors);
    }

    #[test]
    fn e_external_code_covers_css_imports() {
        let v = validate_str(&valid_doc(
            r#"<style>@import url("https://evil.example.com/x.css");</style>"#,
        ));
        assert_eq!(error_codes(&v), vec!["E-EXTERNAL-CODE"]);

        let v = validate_str(&valid_doc(
            r#"<style>@import "https://evil.example.com/x.css";</style>"#,
        ));
        assert_eq!(error_codes(&v), vec!["E-EXTERNAL-CODE"]);

        // Allowlisted http(s) @import passes.
        let v = validate_str(&valid_doc(
            r#"<style>@import "https://cdn.jsdelivr.net/npm/katex@0.17/dist/katex.min.css";</style>"#,
        ));
        assert!(v.errors.is_empty(), "{:?}", v.errors);

        // A relative @import is not the E- position (it's "@import of an
        // http(s) URL"); it lands in W-LOCAL-REF instead.
        let v = validate_str(&valid_doc(r#"<style>@import "./local.css";</style>"#));
        assert!(v.errors.is_empty(), "{:?}", v.errors);
        assert_eq!(warning_codes(&v), vec!["W-LOCAL-REF"]);
    }

    #[test]
    fn e_size_max_rejects_over_50_mib() {
        let big = vec![b'a'; MAX_SIZE_BYTES + 1];
        let v = validate(&big, 1);
        assert_eq!(error_codes(&v), vec!["E-SIZE-MAX"]);
    }

    // -- validator: warnings ------------------------------------------------

    #[test]
    fn w_size_warns_over_5_mib() {
        let padding = format!("<!-- {} -->", "x".repeat(WARN_SIZE_BYTES));
        let v = validate_str(&valid_doc(&padding));
        assert!(v.errors.is_empty(), "{:?}", v.errors);
        assert_eq!(warning_codes(&v), vec!["W-SIZE"]);
    }

    #[test]
    fn w_doctype_warns_when_missing() {
        let doc = valid_doc("").replacen("<!doctype html>", "", 1);
        let v = validate_str(&doc);
        assert_eq!(warning_codes(&v), vec!["W-DOCTYPE"]);

        // Leading whitespace and BOM are permitted.
        let doc = format!("\u{feff}\n  {}", valid_doc(""));
        let v = validate_str(&doc);
        assert!(v.warnings.is_empty(), "{:?}", v.warnings);

        // Case-insensitive.
        let doc = valid_doc("").replacen("<!doctype html>", "<!DOCTYPE HTML>", 1);
        let v = validate_str(&doc);
        assert!(v.warnings.is_empty(), "{:?}", v.warnings);
    }

    #[test]
    fn w_charset_warns_when_missing() {
        let doc = valid_doc("").replacen("<meta charset=\"utf-8\">\n", "", 1);
        let v = validate_str(&doc);
        assert_eq!(warning_codes(&v), vec!["W-CHARSET"]);
    }

    #[test]
    fn w_title_warns_when_missing_and_svg_title_does_not_count() {
        let doc = valid_doc("<svg><title>node label</title></svg>")
            .replacen("<title>Test artifact</title>\n", "", 1);
        let v = validate_str(&doc);
        assert_eq!(warning_codes(&v), vec!["W-TITLE"]);

        // Empty title also warns.
        let doc = valid_doc("").replacen(
            "<title>Test artifact</title>",
            "<title>  </title>",
            1,
        );
        let v = validate_str(&doc);
        assert_eq!(warning_codes(&v), vec!["W-TITLE"]);
    }

    #[test]
    fn w_meta_warns_once_per_missing_cfy_meta() {
        let doc = valid_doc("").replacen(
            "<meta name=\"cfy:generated-by\" content=\"claude-code/test\">\n",
            "",
            1,
        );
        let v = validate_str(&doc);
        assert_eq!(warning_codes(&v), vec!["W-META"]);
        assert!(v.warnings[0].message.contains("cfy:generated-by"));

        // Empty content counts as missing; all three missing → three W-META.
        let doc = valid_doc("")
            .replacen("content=\"Explain the thing.\"", "content=\"\"", 1)
            .replacen(
                "<meta name=\"cfy:version\" content=\"1\">\n",
                "",
                1,
            )
            .replacen(
                "<meta name=\"cfy:generated-by\" content=\"claude-code/test\">\n",
                "",
                1,
            );
        let v = validate_str(&doc);
        assert_eq!(warning_codes(&v), vec!["W-META", "W-META", "W-META"]);
    }

    #[test]
    fn w_version_mismatch_compares_against_assigned_version() {
        // File says 1, server assigns 2 → mismatch.
        let v = validate(valid_doc("").as_bytes(), 2);
        assert_eq!(warning_codes(&v), vec!["W-VERSION-MISMATCH"]);

        // Matching → clean (covered by minimal_valid_artifact_is_clean at 1).
        // Non-numeric content is "present but ≠ assigned".
        let doc = valid_doc("").replacen("content=\"1\"", "content=\"one\"", 1);
        let v = validate(doc.as_bytes(), 1);
        assert_eq!(warning_codes(&v), vec!["W-VERSION-MISMATCH"]);

        // Missing meta is W-META's job, not a mismatch.
        let doc = valid_doc("").replacen("<meta name=\"cfy:version\" content=\"1\">\n", "", 1);
        let v = validate(doc.as_bytes(), 2);
        assert_eq!(warning_codes(&v), vec!["W-META"]);
    }

    #[test]
    fn w_anchor_headings_warns_per_unanchored_heading() {
        let v = validate_str(&valid_doc(
            "<h2>The mental model</h2><h3 data-cfy-id=\"sec-ok\">Fine</h3><h4>Also bare</h4>",
        ));
        assert_eq!(
            warning_codes(&v),
            vec!["W-ANCHOR-HEADINGS", "W-ANCHOR-HEADINGS"]
        );
        assert!(v.warnings[0].message.contains("The mental model"));
        // h5/h6 are SHOULD, not checked.
        let v = validate_str(&valid_doc("<h5>deep</h5><h6>deeper</h6>"));
        assert!(v.warnings.is_empty(), "{:?}", v.warnings);
    }

    #[test]
    fn w_anchor_diagram_flags_thin_coverage() {
        // ≥ 6 shapes, 1 id (the svg itself) < 3 → warn.
        let v = validate_str(&valid_doc(
            r#"<svg data-cfy-id="fig-map" viewBox="0 0 10 10">
                <rect/><rect/><circle/><path/><line/><polygon/>
            </svg>"#,
        ));
        assert_eq!(warning_codes(&v), vec!["W-ANCHOR-DIAGRAM"]);

        // Same shapes with 3 id bearers → no warning.
        let v = validate_str(&valid_doc(
            r#"<svg data-cfy-id="fig-map" viewBox="0 0 10 10">
                <g data-cfy-id="fig-map.a"><rect/><rect/></g>
                <g data-cfy-id="fig-map.b"><circle/><path/></g>
                <line/><polygon/>
            </svg>"#,
        ));
        assert!(v.warnings.is_empty(), "{:?}", v.warnings);

        // ≤ 5 shapes is a decorative accent → no warning even with zero ids.
        let v = validate_str(&valid_doc(
            "<svg viewBox=\"0 0 10 10\"><rect/><rect/><rect/><rect/><rect/></svg>",
        ));
        assert!(v.warnings.is_empty(), "{:?}", v.warnings);
    }

    #[test]
    fn w_anchor_none_when_document_has_no_ids() {
        let doc = valid_doc("").replacen(" data-cfy-id=\"sec-title\"", "", 1);
        let v = validate_str(&doc);
        let codes = warning_codes(&v);
        assert!(codes.contains(&"W-ANCHOR-NONE"), "{codes:?}");
        assert!(codes.contains(&"W-ANCHOR-HEADINGS"), "{codes:?}");
    }

    #[test]
    fn w_id_format_enforces_grammar() {
        assert!(valid_cfy_id("sec-mental-model"));
        assert!(valid_cfy_id("fig-auth-flow.token-service"));
        assert!(valid_cfy_id("a"));
        assert!(valid_cfy_id("a1.b2-c3"));
        assert!(!valid_cfy_id("Bad_ID"));
        assert!(!valid_cfy_id("3abc"));
        assert!(!valid_cfy_id("a-"));
        assert!(!valid_cfy_id("a..b"));
        assert!(!valid_cfy_id(".a"));
        assert!(!valid_cfy_id(&"a".repeat(65)));

        let v = validate_str(&valid_doc("<div data-cfy-id=\"Bad_ID\">x</div>"));
        assert_eq!(warning_codes(&v), vec!["W-ID-FORMAT"]);
    }

    #[test]
    fn w_id_dup_flags_repeated_values() {
        let v = validate_str(&valid_doc(
            "<div data-cfy-id=\"dup-id\">a</div><div data-cfy-id=\"dup-id\">b</div>",
        ));
        assert_eq!(warning_codes(&v), vec!["W-ID-DUP"]);
        assert!(v.warnings[0].message.contains("dup-id"));
    }

    #[test]
    fn w_src_malformed_requires_lang_and_for() {
        let v = validate_str(&valid_doc(
            "<!--cfy:src lang=\"d2\"\nx -> y\n-->\n<figure data-cfy-id=\"fig-a\"></figure>",
        ));
        assert_eq!(warning_codes(&v), vec!["W-SRC-MALFORMED"]);

        // A non-cfy comment is ignored entirely.
        let v = validate_str(&valid_doc("<!-- just a note -->"));
        assert!(v.warnings.is_empty(), "{:?}", v.warnings);
    }

    #[test]
    fn w_src_orphan_requires_adjacent_matching_element() {
        // Well-formed and adjacent (whitespace between is fine) → clean.
        let v = validate_str(&valid_doc(
            "<!--cfy:src lang=\"d2\" for=\"fig-a\"\nx -> y\n-->\n<figure data-cfy-id=\"fig-a\"></figure>",
        ));
        assert!(v.warnings.is_empty(), "{:?}", v.warnings);

        // Mismatched id → orphan.
        let v = validate_str(&valid_doc(
            "<!--cfy:src lang=\"d2\" for=\"fig-a\"\nx -> y\n-->\n<figure data-cfy-id=\"fig-b\"></figure>",
        ));
        assert_eq!(warning_codes(&v), vec!["W-SRC-ORPHAN"]);

        // Non-whitespace text between comment and element breaks adjacency.
        let v = validate_str(&valid_doc(
            "<!--cfy:src lang=\"d2\" for=\"fig-a\"\nx -> y\n-->\nstray text\n<figure data-cfy-id=\"fig-a\"></figure>",
        ));
        assert_eq!(warning_codes(&v), vec!["W-SRC-ORPHAN"]);
    }

    #[test]
    fn w_external_ref_flags_network_resource_positions() {
        let v = validate_str(&valid_doc(
            r#"<img src="https://example.com/x.png" alt="">"#,
        ));
        assert_eq!(warning_codes(&v), vec!["W-EXTERNAL-REF"]);

        // Even the allowlisted CDN host warns in a resource position.
        let v = validate_str(&valid_doc(
            r#"<img src="https://cdn.jsdelivr.net/npm/some-pkg@1/x.png" alt="">"#,
        ));
        assert_eq!(warning_codes(&v), vec!["W-EXTERNAL-REF"]);

        let v = validate_str(&valid_doc(
            r#"<style>.hero { background: url(https://example.com/bg.png); }</style>"#,
        ));
        assert_eq!(warning_codes(&v), vec!["W-EXTERNAL-REF"]);

        // data: URIs are fine everywhere.
        let v = validate_str(&valid_doc(
            r#"<img src="data:image/png;base64,AAAA" alt="">
               <style>.x { background: url("data:image/svg+xml,<svg/>"); }</style>"#,
        ));
        assert!(v.warnings.is_empty(), "{:?}", v.warnings);

        // srcset candidates are checked individually.
        let v = validate_str(&valid_doc(
            r#"<img srcset="https://example.com/a.png 1x, b.png 2x" src="data:image/png;base64,AA" alt="">"#,
        ));
        assert_eq!(warning_codes(&v), vec!["W-EXTERNAL-REF", "W-LOCAL-REF"]);
    }

    #[test]
    fn w_local_ref_flags_relative_and_file_urls() {
        let v = validate_str(&valid_doc(r#"<img src="./diagram.svg" alt="">"#));
        assert_eq!(warning_codes(&v), vec!["W-LOCAL-REF"]);

        let v = validate_str(&valid_doc(r#"<a href="other.html">next</a>"#));
        assert_eq!(warning_codes(&v), vec!["W-LOCAL-REF"]);

        let v = validate_str(&valid_doc(r#"<img src="file:///Users/x/d.svg" alt="">"#));
        assert_eq!(warning_codes(&v), vec!["W-LOCAL-REF"]);

        let v = validate_str(&valid_doc(
            r#"<div style="background: url('img/x.png')">x</div>"#,
        ));
        assert_eq!(warning_codes(&v), vec!["W-LOCAL-REF"]);

        // Fragment, absolute-http and mailto hrefs are all fine.
        let v = validate_str(&valid_doc(
            r##"<a href="#sec-title">up</a>
               <a href="https://example.com/docs">docs</a>
               <a href="mailto:x@example.com">mail</a>"##,
        ));
        assert!(v.warnings.is_empty(), "{:?}", v.warnings);

        // Commented-out CSS is not scanned.
        let v = validate_str(&valid_doc(
            "<style>/* background: url('img/x.png') */ .a { color: red; }</style>",
        ));
        assert!(v.warnings.is_empty(), "{:?}", v.warnings);
    }

    // -- storage & versioning ------------------------------------------------

    #[test]
    fn save_twice_yields_v1_then_v2_with_latest_copy() {
        let conn = test_conn();
        let root = tmp_root("save-twice");

        let v1_html = valid_doc("<p>first</p>");
        let saved1 = save_artifact(&conn, &root, "t1", v1_html.as_bytes()).unwrap();
        assert_eq!(saved1.version, 1);
        assert_eq!(saved1.created_by, "initial");
        assert!(saved1.warnings.is_empty(), "{:?}", saved1.warnings);

        let dir = root.join("p1").join("threads").join("oauth-flow");
        assert!(dir.join("artifact.v1.html").is_file());
        assert!(dir.join("runs").is_dir(), "runs/ reserved dir created");
        assert_eq!(fs::read_to_string(dir.join("artifact.html")).unwrap(), v1_html);
        assert_eq!(thread_status(&conn, "t1"), "ready");

        let v2_html = valid_doc("<p>second</p>").replacen("content=\"1\"", "content=\"2\"", 1);
        let saved2 = save_artifact(&conn, &root, "t1", v2_html.as_bytes()).unwrap();
        assert_eq!(saved2.version, 2);
        assert_eq!(saved2.created_by, "follow_up");

        // Both versioned files retained; artifact.html now matches v2.
        assert!(dir.join("artifact.v1.html").is_file());
        assert!(dir.join("artifact.v2.html").is_file());
        assert_eq!(fs::read_to_string(dir.join("artifact.html")).unwrap(), v2_html);
        assert_eq!(
            fs::read_to_string(dir.join("artifact.v1.html")).unwrap(),
            v1_html
        );

        // Two DB rows with the right created_by sequence.
        let rows: Vec<(i64, String)> = conn
            .prepare("SELECT version, created_by FROM artifacts WHERE thread_id='t1' ORDER BY version")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(rows, vec![(1, "initial".into()), (2, "follow_up".into())]);

        // No temp files left behind.
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejected_save_stores_nothing_and_leaves_status_alone() {
        let conn = test_conn();
        let root = tmp_root("rejected");

        let bad = valid_doc(r#"<script src="https://evil.example.com/x.js"></script>"#);
        let err = save_artifact(&conn, &root, "t1", bad.as_bytes()).unwrap_err();
        match err {
            ArtifactError::Rejected(errors) => {
                assert_eq!(errors[0].code, "E-EXTERNAL-CODE");
            }
            other => panic!("expected Rejected, got {other:?}"),
        }

        // Nothing stored: no DB row, no files (dir never created), status
        // untouched.
        let count: i64 = conn
            .query_row("SELECT count(*) FROM artifacts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        assert!(!root.join("p1").exists());
        assert_eq!(thread_status(&conn, "t1"), "generating");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unknown_thread_is_not_found() {
        let conn = test_conn();
        let root = tmp_root("not-found");
        let err = save_artifact(&conn, &root, "ghost", valid_doc("").as_bytes()).unwrap_err();
        assert!(matches!(err, ArtifactError::ThreadNotFound(id) if id == "ghost"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn io_failure_leaves_db_and_status_untouched() {
        let conn = test_conn();
        let root = tmp_root("io-fail");

        // Sabotage the layout: make `<root>/p1/threads` a *file* so
        // create_dir_all fails after validation passed.
        fs::create_dir_all(root.join("p1")).unwrap();
        fs::write(root.join("p1").join("threads"), b"not a dir").unwrap();

        let err = save_artifact(&conn, &root, "t1", valid_doc("").as_bytes()).unwrap_err();
        assert!(matches!(err, ArtifactError::Io(_)), "{err:?}");

        let count: i64 = conn
            .query_row("SELECT count(*) FROM artifacts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "failed save must not insert a version row");
        assert_eq!(thread_status(&conn, "t1"), "generating");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn atomic_write_replaces_content_and_leaves_no_tmp() {
        let root = tmp_root("atomic");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("artifact.html");

        atomic_write(&target, b"old").unwrap();
        atomic_write(&target, b"new").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert!(!root.join("artifact.html.tmp").exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn warnings_surface_through_save() {
        let conn = test_conn();
        let root = tmp_root("warnings");

        // Thin-coverage artifact: valid but claims version 9 (assigned 1) and
        // has an unanchored diagram.
        let html = valid_doc(
            "<svg viewBox=\"0 0 10 10\"><rect/><rect/><rect/><rect/><rect/><rect/></svg>",
        )
        .replacen("content=\"1\"", "content=\"9\"", 1);

        let saved = save_artifact(&conn, &root, "t1", html.as_bytes()).unwrap();
        let codes: Vec<_> = saved.warnings.iter().map(|i| i.code).collect();
        assert!(codes.contains(&"W-VERSION-MISMATCH"), "{codes:?}");
        assert!(codes.contains(&"W-ANCHOR-DIAGRAM"), "{codes:?}");
        // Accepted and stored despite warnings.
        assert_eq!(saved.version, 1);
        assert_eq!(thread_status(&conn, "t1"), "ready");

        let _ = fs::remove_dir_all(&root);
    }

    /// End-to-end against the *real* migration chain (like the lib.rs
    /// integration tests): the shipped artifacts schema (UNIQUE(thread_id,
    /// version), created_by CHECK, FK) accepts the pipeline's inserts.
    #[test]
    fn save_pipeline_against_real_migrations() {
        let db_path = std::env::temp_dir().join(format!(
            "conceptify-test-artifacts-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = tmp_root("real-migrations");

        let db_handle = crate::db::init_at(&db_path).expect("test db should init and migrate");
        let conn = db_handle.lock().unwrap();
        conn.execute(
            "INSERT INTO projects (id, name, root_path) VALUES ('p1', 'Proj', '/tmp/px')",
            [],
        )
        .unwrap();
        let thread = crate::threads::create_thread(&conn, "p1", "Real pipeline", "q").unwrap();

        let v1 = valid_doc("<p>one</p>");
        let saved1 = save_artifact(&conn, &root, &thread.id, v1.as_bytes()).unwrap();
        assert_eq!(saved1.version, 1);
        assert_eq!(saved1.created_by, "initial");

        let v2 = valid_doc("<p>two</p>").replacen("content=\"1\"", "content=\"2\"", 1);
        let saved2 = save_artifact(&conn, &root, &thread.id, v2.as_bytes()).unwrap();
        assert_eq!(saved2.version, 2);
        assert_eq!(saved2.created_by, "follow_up");

        let dir = root.join("p1").join("threads").join(&thread.slug);
        assert!(dir.join("artifact.v1.html").is_file());
        assert!(dir.join("artifact.v2.html").is_file());
        assert_eq!(fs::read_to_string(dir.join("artifact.html")).unwrap(), v2);
        assert_eq!(thread_status(&conn, &thread.id), "ready");

        // The stored file_path round-trips to the real file.
        let path: String = conn
            .query_row(
                "SELECT file_path FROM artifacts WHERE thread_id = ?1 AND version = 2",
                [&thread.id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(Path::new(&path).is_file());

        drop(conn);
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_file(&db_path);
        let _ = fs::remove_file(db_path.with_extension("db-wal"));
        let _ = fs::remove_file(db_path.with_extension("db-shm"));
    }

    // -- FR-4.4 re-attachment through the save pipeline ----------------------

    /// The v1 content the re-attachment comments below were captured against.
    fn reattach_v1_body() -> &'static str {
        concat!(
            r#"<p data-cfy-id="sec-a">alpha beta gamma</p>"#,
            r#"<figure data-cfy-id="fig-x"><svg viewBox="0 0 10 10">"#,
            r#"<g data-cfy-id="fig-x.node"><text>Node Label</text></g></svg></figure>"#,
            r#"<p data-cfy-id="sec-gone">unique disappearing sentence</p>"#,
        )
    }

    /// v2: sec-a's text shifted (offsets go stale), the figure survives
    /// untouched, and sec-gone is removed entirely.
    fn reattach_v2_body() -> &'static str {
        concat!(
            r#"<p data-cfy-id="sec-a">intro alpha beta gamma</p>"#,
            r#"<figure data-cfy-id="fig-x"><svg viewBox="0 0 10 10">"#,
            r#"<g data-cfy-id="fig-x.node"><text>Node Label</text></g></svg></figure>"#,
        )
    }

    fn comment_by_id<'a>(
        all: &'a [crate::comments::Comment],
        id: &str,
    ) -> &'a crate::comments::Comment {
        all.iter().find(|c| c.id == id).expect("comment present")
    }

    /// End-to-end FR-4.4: save v1 → comment → save v2 with mutations → every
    /// comment is either migrated onto v2 (with repaired offsets where the
    /// quote shifted) or flagged "reference moved" — never dropped; `applied`
    /// comments are frozen history. A later v3 restoring the content heals
    /// the moved comment.
    #[test]
    fn save_reattaches_comments_across_versions() {
        use crate::comments::{self, AnchorState, CommentStatus};
        use serde_json::json;

        let conn = test_conn();
        let root = tmp_root("reattach");

        save_artifact(&conn, &root, "t1", valid_doc(reattach_v1_body()).as_bytes()).unwrap();

        // Captured against v1 (offsets per the bridge's UTF-16 visible-text
        // convention).
        let text = comments::create_comment(
            &conn,
            "t1",
            1,
            Some(&json!({
                "v": 1, "type": "text", "cfy_id": "sec-a", "start": 6, "end": 10,
                "quote": { "exact": "beta", "prefix": "alpha ", "suffix": " gamma" }
            })),
            "why beta?",
        )
        .unwrap()
        .comment;
        let element = comments::create_comment(
            &conn,
            "t1",
            1,
            Some(&json!({
                "v": 1, "type": "element", "cfy_id": "fig-x.node",
                "quote": { "exact": "Node Label" }
            })),
            "why this node?",
        )
        .unwrap()
        .comment;
        let direct = comments::create_comment(&conn, "t1", 1, None, "a direct question")
            .unwrap()
            .comment;
        let vanishing = comments::create_comment(
            &conn,
            "t1",
            1,
            Some(&json!({
                "v": 1, "type": "text", "cfy_id": "sec-gone", "start": 0, "end": 28,
                "quote": { "exact": "unique disappearing sentence" }
            })),
            "about the doomed sentence",
        )
        .unwrap()
        .comment;
        let applied = comments::create_comment(
            &conn,
            "t1",
            1,
            Some(&json!({
                "v": 1, "type": "text", "cfy_id": "sec-a", "start": 6, "end": 10,
                "quote": { "exact": "beta" }
            })),
            "already applied",
        )
        .unwrap()
        .comment;
        comments::update_comment(&conn, &applied.id, Some(CommentStatus::Applied), None, None)
            .unwrap();

        let saved2 =
            save_artifact(&conn, &root, "t1", valid_doc(reattach_v2_body()).as_bytes()).unwrap();
        assert_eq!(saved2.version, 2);

        // Exactly the four changed rows are reported (for comment-updated
        // events); the frozen `applied` comment is not among them.
        let mut reported: Vec<&str> = saved2.reattached.iter().map(|c| c.id.as_str()).collect();
        reported.sort_unstable();
        let mut expected = vec![
            text.id.as_str(),
            element.id.as_str(),
            direct.id.as_str(),
            vanishing.id.as_str(),
        ];
        expected.sort_unstable();
        assert_eq!(reported, expected);

        let all = comments::list_comments(&conn, "t1", None).unwrap();

        // (b) element text edited but id kept → re-anchored via quote, offsets
        // repaired to the shifted position, version advanced.
        let c = comment_by_id(&all, &text.id);
        assert_eq!(c.artifact_version, 2);
        assert_eq!(c.anchor_state, AnchorState::Anchored);
        let a = c.anchor.as_ref().unwrap();
        assert_eq!(a["cfy_id"], "sec-a");
        assert_eq!(a["start"], 12);
        assert_eq!(a["end"], 16);
        assert_eq!(a["quote"]["exact"], "beta", "quote is never rewritten");

        // (a) unchanged element → anchor holds verbatim, version advanced.
        let c = comment_by_id(&all, &element.id);
        assert_eq!(c.artifact_version, 2);
        assert_eq!(c.anchor_state, AnchorState::Anchored);
        assert_eq!(c.anchor.as_ref().unwrap()["cfy_id"], "fig-x.node");

        // Null anchor → version-agnostic, follows latest trivially.
        let c = comment_by_id(&all, &direct.id);
        assert_eq!(c.artifact_version, 2);
        assert!(c.anchor.is_none());

        // (c) element removed → "reference moved": version STAYS at the last
        // version where the anchor resolved, anchor untouched, body/status
        // intact (still readable and answerable).
        let c = comment_by_id(&all, &vanishing.id);
        assert_eq!(c.artifact_version, 1);
        assert_eq!(c.anchor_state, AnchorState::Moved);
        assert_eq!(c.anchor.as_ref().unwrap()["cfy_id"], "sec-gone");
        assert_eq!(c.status, CommentStatus::Open);
        assert_eq!(c.body, "about the doomed sentence");

        // Applied → frozen history: untouched entirely.
        let c = comment_by_id(&all, &applied.id);
        assert_eq!(c.artifact_version, 1);
        assert_eq!(c.anchor_state, AnchorState::Anchored);

        // v3 restores the vanished section → the moved comment HEALS: it
        // re-attaches (straight from v1's anchor) and the flag clears.
        let v3_body = format!(
            "{}{}",
            reattach_v2_body(),
            r#"<p data-cfy-id="sec-gone">unique disappearing sentence</p>"#
        );
        let saved3 = save_artifact(&conn, &root, "t1", valid_doc(&v3_body).as_bytes()).unwrap();
        assert_eq!(saved3.version, 3);

        let all = comments::list_comments(&conn, "t1", None).unwrap();
        let c = comment_by_id(&all, &vanishing.id);
        assert_eq!(c.artifact_version, 3);
        assert_eq!(c.anchor_state, AnchorState::Anchored);
        // The other live comments advanced with it; applied stayed frozen.
        assert_eq!(comment_by_id(&all, &text.id).artifact_version, 3);
        assert_eq!(comment_by_id(&all, &applied.id).artifact_version, 1);

        let _ = fs::remove_dir_all(&root);
    }

    /// An unchanged row emits nothing: a comment already flagged `moved` whose
    /// anchor is still unresolvable is not reported again on the next save.
    #[test]
    fn still_moved_comment_is_not_rereported() {
        use crate::comments;
        use serde_json::json;

        let conn = test_conn();
        let root = tmp_root("still-moved");

        save_artifact(&conn, &root, "t1", valid_doc(reattach_v1_body()).as_bytes()).unwrap();
        let doomed = comments::create_comment(
            &conn,
            "t1",
            1,
            Some(&json!({
                "v": 1, "type": "text", "cfy_id": "sec-gone", "start": 0, "end": 28,
                "quote": { "exact": "unique disappearing sentence" }
            })),
            "q",
        )
        .unwrap()
        .comment;

        let saved2 =
            save_artifact(&conn, &root, "t1", valid_doc(reattach_v2_body()).as_bytes()).unwrap();
        assert_eq!(saved2.reattached.len(), 1, "flagged moved on v2");
        assert_eq!(saved2.reattached[0].id, doomed.id);

        let saved3 =
            save_artifact(&conn, &root, "t1", valid_doc(reattach_v2_body()).as_bytes()).unwrap();
        assert!(
            saved3.reattached.is_empty(),
            "still moved, row unchanged → no event: {:?}",
            saved3.reattached
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// Lockstep guard for the open-in-browser path (FR-2.5): the helper the
    /// `open_artifact_in_browser` command resolves through must name exactly
    /// the always-latest copy `save_artifact` writes.
    #[test]
    fn latest_copy_path_matches_save_output() {
        let conn = test_conn();
        let root = tmp_root("latest-copy");

        let html = valid_doc("<p>latest</p>");
        save_artifact(&conn, &root, "t1", html.as_bytes()).unwrap();

        let path = latest_copy_path(&root, "p1", "oauth-flow");
        assert!(path.is_file(), "helper must point at the written copy");
        assert_eq!(fs::read_to_string(&path).unwrap(), html);

        let _ = fs::remove_dir_all(&root);
    }

    /// Regression (bead `conceptify-028`): in test builds `artifacts_root()`
    /// must resolve to the shared per-process scratch root and can NEVER return
    /// the user's real `~/Documents/conceptify/artifacts` — the leak that dumped
    /// `proj-*` dirs there. The real-Documents branch is `cfg(not(test))`, so
    /// this holds by construction; the test pins it so a future refactor that
    /// re-introduces a production fall-through fails loudly here instead of on a
    /// user's disk.
    #[test]
    fn artifacts_root_never_resolves_under_real_documents() {
        let root = artifacts_root().expect("test artifacts root resolves");

        // Never under the real Documents artifacts dir.
        if let Some(docs) = dirs::document_dir() {
            let real = docs.join("conceptify").join("artifacts");
            assert!(
                !root.starts_with(&real),
                "artifacts_root() resolved under the REAL documents dir {}: {}",
                real.display(),
                root.display(),
            );
        }

        // It resolves to the single shared scratch root, which lives under the
        // process temp dir (or an explicitly pinned override).
        assert_eq!(root, test_artifacts_root());
        assert!(
            root.starts_with(std::env::temp_dir())
                || std::env::var_os("CONCEPTIFY_TEST_ARTIFACTS_DIR").is_some(),
            "expected a temp scratch root, got {}",
            root.display(),
        );
    }
}
