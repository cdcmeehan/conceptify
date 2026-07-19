//! Self-contained artifact export (bead `conceptify-z9y.7`).
//!
//! The z9y.1 hybrid delivery decision keeps video clips OUT of the artifact
//! HTML: a `<figure class="cfy-video">` references its clip through the
//! reserved `cfy-asset://localhost/<thread-id>/<sha256>.mp4` scheme, served
//! in-app by the Range-capable [`crate::asset_protocol`] handler. That
//! deliberately breaks the "one file survives alone forever" story (N6 /
//! FR-2.5) when the file is opened outside the app — Finder, email, a plain
//! browser — where the scheme is unresolvable and the reader sees only the
//! embedded poster + transcript.
//!
//! This module restores that story on demand. Given a saved artifact version,
//! it produces a **derived copy** in which every `cfy-asset://` reference is
//! replaced by a `data:video/mp4;base64,…` URI carrying the stored clip's
//! exact bytes. Nothing else in the document is touched — no bridge injection,
//! no CSP rewrite, byte-identical everywhere the video URL does not appear.
//!
//! ## What this is NOT
//!
//! Export copies are derived output, not authored artifacts (artifact-spec
//! §2): they are **never re-validated** through the save-artifact validator
//! and **never written back into version history**. The 20 MiB per-asset cap
//! (E-ASSET-SIZE, enforced at upload) keeps the base64-inflated copy
//! (~27 MiB per clip) under the 50 MiB viewer backstop by construction, but
//! the export is not validated anyway — it is a one-off file for browsers, not
//! for re-import.
//!
//! ## Failure discipline (N4)
//!
//! The full transformed document is built in memory first; a missing asset
//! file aborts the export with a clear error naming the offending clip
//! *before* anything is written, so no partial/corrupt output file is ever
//! left behind. The final write reuses [`crate::artifacts::atomic_write`]
//! (temp `.tmp` sibling + `rename`), the same atomic discipline every other
//! on-disk write in this codebase uses.

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use rusqlite::{Connection, OptionalExtension};

use crate::{artifacts, assets};

/// The reserved video-asset scheme prefix (spec §1.4). A well-formed reference
/// is exactly `<PREFIX><thread-id>/<sha256>.mp4`.
const ASSET_URL_PREFIX: &str = "cfy-asset://localhost/";

/// MIME used for the inlined `data:` URI. Every asset is MP4/H.264 by the §8.3
/// upload-time codec allowlist, so this is the only possible media type.
const DATA_URI_MIME: &str = "data:video/mp4;base64,";

/// Failures from the export pipeline. Rendered to a user-facing string by the
/// `export_artifact` command; each variant identifies precisely what went
/// wrong (which asset, which version) so the error the reader sees is
/// actionable.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("thread not found: {0}")]
    ThreadNotFound(String),

    /// The thread exists but has no saved artifact for the requested (or
    /// latest) version — nothing to export.
    #[error("no artifact to export for thread {thread_id}{}", match .version {
        Some(v) => format!(" at version {v}"),
        None => String::new(),
    })]
    NoArtifact {
        thread_id: String,
        version: Option<i64>,
    },

    /// A `cfy-asset://` URL appears in the saved HTML but its referenced clip
    /// file is absent from asset storage (deleted thread dir race, partial
    /// restore). Export fails rather than emit a file with a dangling scheme
    /// URL that would silently show nothing in a browser.
    #[error(
        "asset {sha256} referenced by the artifact is missing from storage \
         (thread {thread_id}): {}",
        .path.display()
    )]
    AssetMissing {
        thread_id: String,
        sha256: String,
        path: PathBuf,
    },

    /// A `cfy-asset://localhost/` prefix appears in the HTML but the text
    /// following it does not match the §1.4 grammar. A saved artifact has
    /// already passed `E-ASSET-REF`, so this indicates on-disk corruption;
    /// erroring (rather than leaving the malformed URL in place) keeps the
    /// export honest.
    #[error("malformed cfy-asset reference in artifact HTML near: {0}")]
    MalformedReference(String),

    /// The saved artifact bytes are not valid UTF-8 (artifacts are HTML text).
    #[error("artifact HTML is not valid UTF-8")]
    NotUtf8,

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// One `cfy-asset://` reference located in the source HTML, with the byte span
/// `[start, end)` of the full URL token so it can be spliced out verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AssetRef {
    thread_id: String,
    sha256: String,
    start: usize,
    end: usize,
}

/// Result of a successful in-memory export transform.
#[derive(Debug)]
pub struct Exported {
    /// The derived, self-contained HTML bytes.
    pub bytes: Vec<u8>,
    /// How many distinct `cfy-asset://` URLs were inlined (0 = plain copy).
    pub inlined_assets: usize,
}

/// Scan raw HTML for every `cfy-asset://localhost/<thread-id>/<sha256>.mp4`
/// reference, returning each with its byte span. Works on the raw text (never
/// a DOM round-trip) so the output stays byte-identical outside the replaced
/// URL spans.
///
/// A `cfy-asset://localhost/` prefix that is not followed by a grammar-valid
/// `<thread-id>/<sha256>.mp4` is an error (`MalformedReference`): a saved
/// artifact has already passed `E-ASSET-REF`, so a malformed reference on disk
/// is corruption we refuse to paper over.
fn find_asset_refs(html: &str) -> Result<Vec<AssetRef>, ExportError> {
    let mut refs = Vec::new();
    let mut cursor = 0;
    while let Some(rel) = html[cursor..].find(ASSET_URL_PREFIX) {
        let start = cursor + rel;
        let after_prefix = start + ASSET_URL_PREFIX.len();
        let rest = &html[after_prefix..];

        // thread-id: `[A-Za-z0-9_-]{1,128}` up to the segment separator `/`.
        let Some(slash) = rest.find('/') else {
            return Err(ExportError::MalformedReference(snippet(html, start)));
        };
        let thread_id = &rest[..slash];
        let thread_ok = (1..=128).contains(&thread_id.len())
            && thread_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !thread_ok {
            return Err(ExportError::MalformedReference(snippet(html, start)));
        }

        // sha256: exactly 64 lowercase hex, then the literal `.mp4`.
        let sha_start = after_prefix + slash + 1;
        let tail = &html[sha_start..];
        const SHA_LEN: usize = 64;
        if tail.len() < SHA_LEN + 4 {
            return Err(ExportError::MalformedReference(snippet(html, start)));
        }
        let sha = &tail[..SHA_LEN];
        if !assets::is_valid_sha256(sha) || &tail[SHA_LEN..SHA_LEN + 4] != ".mp4" {
            return Err(ExportError::MalformedReference(snippet(html, start)));
        }

        let end = sha_start + SHA_LEN + 4;
        refs.push(AssetRef {
            thread_id: thread_id.to_owned(),
            sha256: sha.to_owned(),
            start,
            end,
        });
        cursor = end;
    }
    Ok(refs)
}

/// A short, single-line window around `at` for error messages.
fn snippet(html: &str, at: usize) -> String {
    let end = html[at..]
        .char_indices()
        .nth(48)
        .map(|(i, _)| at + i)
        .unwrap_or(html.len());
    html[at..end].replace(['\n', '\r'], " ")
}

/// Resolve a URL thread-id segment to its `(project_id, slug)` on-disk pair,
/// the same DB lookup the `cfy-asset://` protocol handler uses to serve the
/// clip in-app (single source of truth for where an asset physically lives).
fn resolve_thread(conn: &Connection, thread_id: &str) -> Result<(String, String), ExportError> {
    conn.query_row(
        "SELECT project_id, slug FROM threads WHERE id = ?1",
        [thread_id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )
    .optional()?
    .ok_or_else(|| ExportError::ThreadNotFound(thread_id.to_owned()))
}

/// Transform saved artifact `html_bytes` into a self-contained copy: replace
/// every `cfy-asset://` reference with a `data:video/mp4;base64,…` URI of the
/// stored clip. The entire document is assembled in memory; any missing asset
/// aborts with [`ExportError::AssetMissing`] before a byte is written.
///
/// An artifact with zero references round-trips byte-for-byte (a faithful
/// copy), so the no-video case is a plain pass-through.
pub fn export_html(
    conn: &Connection,
    root: &Path,
    html_bytes: &[u8],
) -> Result<Exported, ExportError> {
    let html = std::str::from_utf8(html_bytes).map_err(|_| ExportError::NotUtf8)?;
    let refs = find_asset_refs(html)?;

    if refs.is_empty() {
        // No video: a faithful, byte-identical copy.
        return Ok(Exported {
            bytes: html_bytes.to_vec(),
            inlined_assets: 0,
        });
    }

    // Build each distinct clip's data: URI once (an artifact may reference the
    // same clip in `src` and a `<source>`, and dedupe avoids re-reading /
    // re-encoding a 20 MiB file twice). Keyed by (thread-id, sha).
    let mut data_uris: std::collections::HashMap<(String, String), String> =
        std::collections::HashMap::new();
    for r in &refs {
        let key = (r.thread_id.clone(), r.sha256.clone());
        if data_uris.contains_key(&key) {
            continue;
        }
        let (project_id, slug) = resolve_thread(conn, &r.thread_id)?;
        let path = assets::asset_file_path(root, &project_id, &slug, &r.sha256);
        let clip = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ExportError::AssetMissing {
                    thread_id: r.thread_id.clone(),
                    sha256: r.sha256.clone(),
                    path,
                });
            }
            Err(e) => return Err(ExportError::Io(e)),
        };
        let mut uri = String::with_capacity(DATA_URI_MIME.len() + (clip.len() * 4 / 3) + 4);
        uri.push_str(DATA_URI_MIME);
        base64::engine::general_purpose::STANDARD.encode_string(&clip, &mut uri);
        data_uris.insert(key, uri);
    }

    // Splice each URL span out in order, copying the untouched text between
    // spans verbatim so the output is byte-identical except at the URLs.
    let mut out = String::with_capacity(html.len());
    let mut pos = 0;
    for r in &refs {
        out.push_str(&html[pos..r.start]);
        out.push_str(&data_uris[&(r.thread_id.clone(), r.sha256.clone())]);
        pos = r.end;
    }
    out.push_str(&html[pos..]);

    Ok(Exported {
        bytes: out.into_bytes(),
        inlined_assets: data_uris.len(),
    })
}

/// Resolve the on-disk source file for the artifact to export: the given
/// `version`, or the latest when `None`.
fn resolve_source_file(
    conn: &Connection,
    thread_id: &str,
    version: Option<i64>,
) -> Result<PathBuf, ExportError> {
    // Thread must exist first, so a bad id is a clear ThreadNotFound rather
    // than a confusing NoArtifact.
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM threads WHERE id = ?1)",
        [thread_id],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(ExportError::ThreadNotFound(thread_id.to_owned()));
    }

    let file_path: Option<String> = match version {
        Some(v) => conn
            .query_row(
                "SELECT file_path FROM artifacts WHERE thread_id = ?1 AND version = ?2",
                rusqlite::params![thread_id, v],
                |r| r.get(0),
            )
            .optional()?,
        None => artifacts::latest_artifact(conn, thread_id)?.map(|a| a.file_path),
    };

    file_path.map(PathBuf::from).ok_or(ExportError::NoArtifact {
        thread_id: thread_id.to_owned(),
        version,
    })
}

/// End-to-end export to a user-chosen destination: resolve the version's saved
/// HTML, inline its assets, and atomically write the derived copy to `dest`.
///
/// The transform runs to completion in memory before the write, so the failure
/// paths (missing asset, malformed reference) leave `dest` — and its `.tmp`
/// sibling — untouched: no partial output file is ever produced.
pub fn export_to_path(
    conn: &Connection,
    root: &Path,
    thread_id: &str,
    version: Option<i64>,
    dest: &Path,
) -> Result<Exported, ExportError> {
    let source = resolve_source_file(conn, thread_id, version)?;
    let html_bytes = fs::read(&source)?;
    let exported = export_html(conn, root, &html_bytes)?;
    artifacts::atomic_write(dest, &exported.bytes)?;
    Ok(exported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::tests::TINY_MP4;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE threads (
                 id TEXT PRIMARY KEY,
                 project_id TEXT NOT NULL,
                 slug TEXT NOT NULL
             );
             CREATE TABLE artifacts (
                 thread_id TEXT NOT NULL,
                 version INTEGER NOT NULL,
                 file_path TEXT NOT NULL
             );
             INSERT INTO threads (id, project_id, slug) VALUES ('t1', 'p1', 'oauth-flow');",
        )
        .unwrap();
        conn
    }

    fn tmp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "conceptify-export-test-{}-{}-{}",
            tag,
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// Store TINY_MP4 in t1's asset dir and register a v1 artifact whose HTML
    /// references it. Returns (root, sha, html).
    fn fixture(tag: &str, conn: &Connection) -> (PathBuf, String, String) {
        let root = tmp_root(tag);
        let sha = assets::sha256_hex(TINY_MP4);
        assets::save_asset(conn, &root, "t1", &sha, TINY_MP4).unwrap();
        let html = video_html(&sha);
        let file = artifacts::version_file_path(&root, "p1", "oauth-flow", 1);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, html.as_bytes()).unwrap();
        conn.execute(
            "INSERT INTO artifacts (thread_id, version, file_path) VALUES ('t1', 1, ?1)",
            [file.to_string_lossy()],
        )
        .unwrap();
        (root, sha, html)
    }

    fn video_html(sha: &str) -> String {
        format!(
            "<!doctype html><html><body>\n<figure class=\"cfy-video\" data-cfy-id=\"vid-x\">\n\
             <video controls preload=\"metadata\" playsinline poster=\"data:image/jpeg;base64,AAAA\" \
             src=\"cfy-asset://localhost/t1/{sha}.mp4\"></video>\n\
             <details class=\"cfy-details cfy-video-transcript\"><summary>Transcript</summary>\
             <p>Narration.</p></details>\n<figcaption><strong>Cap.</strong></figcaption>\n\
             </figure>\n</body></html>\n"
        )
    }

    #[test]
    fn find_refs_parses_canonical_and_dedupes_positions() {
        let sha = "a".repeat(64);
        let html = format!(
            "x src=\"cfy-asset://localhost/t1/{sha}.mp4\" y \
             <source src=\"cfy-asset://localhost/thread-2/{sha}.mp4\">"
        );
        let refs = find_asset_refs(&html).unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].thread_id, "t1");
        assert_eq!(refs[0].sha256, sha);
        assert_eq!(&html[refs[0].start..refs[0].end], format!("cfy-asset://localhost/t1/{sha}.mp4"));
        assert_eq!(refs[1].thread_id, "thread-2");
    }

    #[test]
    fn find_refs_rejects_malformed_prefix() {
        let good = "a".repeat(64);
        for bad in [
            "cfy-asset://localhost/t1/tooshort.mp4".to_string(),
            format!("cfy-asset://localhost/t1/{}.webm", good), // wrong ext
            format!("cfy-asset://localhost/t1/{}.mp4", "A".repeat(64)), // uppercase sha
            format!("cfy-asset://localhost//{good}.mp4"),      // empty thread-id
            "cfy-asset://localhost/t1-no-slash-or-sha".to_string(),
        ] {
            assert!(
                matches!(find_asset_refs(&bad), Err(ExportError::MalformedReference(_))),
                "expected malformed error for {bad}"
            );
        }
    }

    #[test]
    fn no_video_html_is_byte_identical_copy() {
        let conn = test_conn();
        let root = tmp_root("passthrough");
        let html = b"<!doctype html><html><body><p>No video here.</p></body></html>\n";
        let out = export_html(&conn, &root, html).unwrap();
        assert_eq!(out.inlined_assets, 0);
        assert_eq!(out.bytes, html.to_vec());
    }

    #[test]
    fn inlines_video_asset_and_roundtrips_bytes() {
        let conn = test_conn();
        let (root, sha, html) = fixture("inline", &conn);

        let out = export_html(&conn, &root, html.as_bytes()).unwrap();
        assert_eq!(out.inlined_assets, 1);
        let text = String::from_utf8(out.bytes).unwrap();

        assert!(!text.contains("cfy-asset://"), "no scheme URL must survive");
        assert!(text.contains("src=\"data:video/mp4;base64,"), "data URI present");
        // Everything outside the URL is untouched: poster, transcript, caption.
        assert!(text.contains("poster=\"data:image/jpeg;base64,AAAA\""));
        assert!(text.contains("cfy-video-transcript"));

        // The inlined base64 decodes to exactly the stored clip bytes.
        let start = text.find(DATA_URI_MIME).unwrap() + DATA_URI_MIME.len();
        let b64 = &text[start..text[start..].find('"').unwrap() + start];
        let decoded = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        assert_eq!(decoded, TINY_MP4);
        assert_eq!(assets::sha256_hex(&decoded), sha);
    }

    #[test]
    fn missing_asset_errors_and_leaves_no_output_file() {
        let conn = test_conn();
        let root = tmp_root("missing");
        // Reference a sha that was never uploaded.
        let ghost = "b".repeat(64);
        let html = video_html(&ghost);
        let file = artifacts::version_file_path(&root, "p1", "oauth-flow", 1);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, html.as_bytes()).unwrap();
        conn.execute(
            "INSERT INTO artifacts (thread_id, version, file_path) VALUES ('t1', 1, ?1)",
            [file.to_string_lossy()],
        )
        .unwrap();

        let dest = root.join("out.html");
        let err = export_to_path(&conn, &root, "t1", None, &dest).unwrap_err();
        match err {
            ExportError::AssetMissing { sha256, .. } => assert_eq!(sha256, ghost),
            other => panic!("expected AssetMissing, got {other:?}"),
        }
        // No partial/corrupt output, and no leftover temp sibling.
        assert!(!dest.exists(), "destination must not exist after failure");
        let mut tmp = dest.clone().into_os_string();
        tmp.push(".tmp");
        assert!(!PathBuf::from(tmp).exists(), "no .tmp sibling may linger");
    }

    #[test]
    fn export_to_path_writes_playable_copy() {
        let conn = test_conn();
        let (root, _sha, _html) = fixture("topath", &conn);
        let dest = root.join("exported.html");

        let out = export_to_path(&conn, &root, "t1", Some(1), &dest).unwrap();
        assert_eq!(out.inlined_assets, 1);
        let written = fs::read_to_string(&dest).unwrap();
        assert!(written.contains("data:video/mp4;base64,"));
        assert!(!written.contains("cfy-asset://"));
    }

    #[test]
    fn latest_version_resolves_when_none() {
        let conn = test_conn();
        let (root, _sha, _html) = fixture("latest", &conn);
        let dest = root.join("latest.html");
        // version = None must resolve to the latest artifact row.
        let out = export_to_path(&conn, &root, "t1", None, &dest).unwrap();
        assert_eq!(out.inlined_assets, 1);
    }

    /// Manual end-to-end export against a real, playable MP4 (mirrors the
    /// env-gated `#[ignore]` live-harness pattern used elsewhere). Provide a
    /// real H.264 clip and an output path:
    ///
    /// ```sh
    /// CFY_EXPORT_E2E_MP4=/tmp/clip.mp4 CFY_EXPORT_E2E_OUT=/tmp/exported.html \
    ///   cargo test -p conceptify export::tests::e2e_real_mp4 -- --ignored --nocapture
    /// ```
    ///
    /// Stores the clip through the real `assets::save_asset` pipeline (so it
    /// must pass the §8.3 codec/size sniffer), builds a spec-shaped artifact
    /// referencing it, exports, and writes a self-contained `.html` you can
    /// open from `file://` to confirm playback + seek in Safari/Chrome.
    #[test]
    #[ignore = "manual: needs CFY_EXPORT_E2E_MP4 + CFY_EXPORT_E2E_OUT"]
    fn e2e_real_mp4() {
        let src = std::env::var("CFY_EXPORT_E2E_MP4").expect("set CFY_EXPORT_E2E_MP4");
        let out = std::env::var("CFY_EXPORT_E2E_OUT").expect("set CFY_EXPORT_E2E_OUT");
        let clip = fs::read(&src).unwrap();

        let conn = test_conn();
        let root = tmp_root("e2e");
        let sha = assets::sha256_hex(&clip);
        let saved = assets::save_asset(&conn, &root, "t1", &sha, &clip).unwrap();
        println!("stored {} bytes as {}", saved.bytes, saved.url);

        let html = video_html(&sha);
        let vfile = artifacts::version_file_path(&root, "p1", "oauth-flow", 1);
        fs::create_dir_all(vfile.parent().unwrap()).unwrap();
        fs::write(&vfile, html.as_bytes()).unwrap();
        conn.execute(
            "INSERT INTO artifacts (thread_id, version, file_path) VALUES ('t1', 1, ?1)",
            [vfile.to_string_lossy()],
        )
        .unwrap();

        let dest = PathBuf::from(&out);
        let result = export_to_path(&conn, &root, "t1", None, &dest).unwrap();
        let written = fs::metadata(&dest).unwrap().len();
        println!(
            "exported {} clip(s); source {} B -> self-contained {} B at {}",
            result.inlined_assets, saved.bytes, written, out
        );
        assert_eq!(result.inlined_assets, 1);
        let text = fs::read_to_string(&dest).unwrap();
        assert!(text.contains("data:video/mp4;base64,"));
        assert!(!text.contains("cfy-asset://"));
    }

    #[test]
    fn unknown_thread_and_missing_version_error_distinctly() {
        let conn = test_conn();
        let (_root, _sha, _html) = fixture("errs", &conn);
        assert!(matches!(
            resolve_source_file(&conn, "ghost", None),
            Err(ExportError::ThreadNotFound(_))
        ));
        assert!(matches!(
            resolve_source_file(&conn, "t1", Some(99)),
            Err(ExportError::NoArtifact { .. })
        ));
    }
}
