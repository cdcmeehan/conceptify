//! Frontend-facing `#[tauri::command]` handlers for the app shell (PRD §5.3,
//! FR-1.3, FR-2.2).
//!
//! These are thin wrappers over the `projects`/`threads` domain modules,
//! invoked from the Preact shell via `@tauri-apps/api/core`'s `invoke` — the
//! same `invoke`/`listen` pattern M0 established with `db_check`. The shell
//! deliberately does **not** talk to the embedded axum API over HTTP: the
//! webview origin (`tauri://localhost`) is cross-origin to `127.0.0.1:<port>`,
//! so every authenticated route (bearer token = a non-safelisted header)
//! triggers a CORS preflight the API doesn't answer; and the webview can read
//! neither the on-disk token/port files nor the filesystem it needs to check
//! for the "mapped directory vanished" badge (FR-1.3). Commands sidestep all of
//! that. The identical domain functions back the HTTP routes the CLI/skill use,
//! so both surfaces stay consistent.
//!
//! Mutations here do not emit Tauri events; the shell refetches after awaiting
//! a command. Live cross-surface updates (a CLI/skill mutation reflecting in the
//! window) ride the `projects-changed`/`thread-created` events the axum handlers
//! already emit, wired to these same refetch paths by bead `conceptify-qxr.5`.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use tauri::State;

use crate::db::DbHandle;
use crate::{artifacts, comments, projects, threads};

/// A project row for the shell sidebar. Mirrors the HTTP `ProjectListItem` plus
/// `root_exists`: whether the mapped `root_path` still resolves on disk, so the
/// UI can show the FR-1.3 "missing directory" badge + re-map affordance. Only
/// the Rust side can stat the filesystem, so this flag is computed here rather
/// than in the frontend.
#[derive(Serialize)]
pub struct ProjectDto {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub root_exists: bool,
    pub created_at: String,
    pub archived: bool,
    pub thread_count: i64,
    pub last_activity: String,
    pub context: Option<crate::project_context::ProjectContextSummary>,
}

/// A thread row for the shell thread list. Mirrors the HTTP thread list item;
/// `status` is the stored string (`generating`/`ready`/`updating`/`error`).
#[derive(Serialize)]
pub struct ThreadDto {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub slug: String,
    pub initial_question: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub open_comment_count: i64,
}

/// List projects with thread counts, last activity, and per-project
/// `root_exists`. Excludes archived unless `include_archived` is set.
#[tauri::command(rename_all = "snake_case")]
pub fn list_projects(
    db: State<DbHandle>,
    include_archived: bool,
) -> Result<Vec<ProjectDto>, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    let rows = projects::list_projects(&conn, include_archived).map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|p| ProjectDto {
            context: crate::project_context::stored(&conn, &p.id),
            root_exists: std::path::Path::new(&p.root_path).exists(),
            id: p.id,
            name: p.name,
            root_path: p.root_path,
            created_at: p.created_at,
            archived: p.archived,
            thread_count: p.thread_count,
            last_activity: p.last_activity,
        })
        .collect())
}

/// List a project's threads, sorted by last activity (the domain layer sorts),
/// each with status + open-comment count.
#[tauri::command(rename_all = "snake_case")]
pub fn list_threads(db: State<DbHandle>, project_id: String) -> Result<Vec<ThreadDto>, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    let rows = threads::list_threads(&conn, &project_id).map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|t| ThreadDto {
            status: t.status.as_str().to_string(),
            id: t.id,
            project_id: t.project_id,
            title: t.title,
            slug: t.slug,
            initial_question: t.initial_question,
            created_at: t.created_at,
            updated_at: t.updated_at,
            open_comment_count: t.open_comment_count,
        })
        .collect())
}

/// Rename a project (FR-1.3).
#[tauri::command(rename_all = "snake_case")]
pub fn rename_project(db: State<DbHandle>, id: String, name: String) -> Result<(), String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    projects::rename_project(&conn, &id, &name).map_err(|e| e.to_string())
}

/// Archive or unarchive a project (FR-1.3: hide, don't delete).
#[tauri::command(rename_all = "snake_case")]
pub fn set_project_archived(db: State<DbHandle>, id: String, archived: bool) -> Result<(), String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    projects::set_archived(&conn, &id, archived).map_err(|e| e.to_string())
}

/// Re-map a project to a new root directory (FR-1.3).
///
/// The project id is stable and artifacts are keyed by it (§5.6), so repairing
/// a moved/vanished mapping only rewrites `root_path`. The new path must exist;
/// it is canonicalized (matching `projects::ensure_project`) before storing so
/// symlinks/trailing slashes resolve to one identity under `UNIQUE(root_path)`.
/// Kept here rather than in the (concurrently edited) `projects` module to keep
/// this bead's Rust footprint to a single new file; the mutation is a plain
/// column update.
#[tauri::command(rename_all = "snake_case")]
pub fn remap_project(db: State<DbHandle>, id: String, root_path: String) -> Result<(), String> {
    let path = std::path::Path::new(&root_path);
    if !path.exists() {
        return Err(format!("path not found: {root_path}"));
    }
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("failed to canonicalize path: {e}"))?;
    let canonical_str = canonical.to_string_lossy().to_string();

    let conn = db.lock().map_err(|e| e.to_string())?;
    let rows_affected = conn
        .execute(
            "UPDATE projects SET root_path = ?1 WHERE id = ?2",
            rusqlite::params![canonical_str, id],
        )
        .map_err(|e| e.to_string())?;

    if rows_affected == 0 {
        return Err(format!("project not found: {id}"));
    }
    Ok(())
}

/// A project mapped/created via the app's "New project" affordance (FR-1.2,
/// UC6). `created` distinguishes a brand-new mapping from landing on an
/// already-mapped directory (which is not an error — see `ensure_project`), so
/// the shell can select the project either way.
#[derive(Serialize)]
pub struct EnsuredProjectDto {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub created: bool,
}

impl From<projects::EnsureProjectResult> for EnsuredProjectDto {
    fn from(r: projects::EnsureProjectResult) -> Self {
        EnsuredProjectDto {
            id: r.project.id,
            name: r.project.name,
            root_path: r.project.root_path,
            created: r.created,
        }
    }
}

/// Map an existing directory as a project (FR-1.2 / UC6 — native dir-picker
/// path). Thin wrapper over `projects::ensure_project` — the same
/// canonicalize → find-or-create path the HTTP `POST /projects/ensure` route
/// uses. Picking an already-mapped directory lands on the existing project
/// (`created: false`), never an error (UC6 acceptance). `name` is an optional
/// display-name override; the picker leaves it unset so the directory name is
/// used. The frontend passes the path returned by the `@tauri-apps/plugin-dialog`
/// native directory picker.
#[tauri::command(rename_all = "snake_case")]
pub fn ensure_project(
    db: State<DbHandle>,
    root_path: String,
    name: Option<String>,
) -> Result<EnsuredProjectDto, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    projects::ensure_project(&conn, &root_path, name.as_deref())
        .map(EnsuredProjectDto::from)
        .map_err(|e| e.to_string())
}

/// Create a fresh project folder for a non-codebase topic and map it (FR-1.2 /
/// UC6 — "create a folder for me"). The folder is made under the configured
/// auto-project base dir (FR-7.3, default `~/Documents/conceptify/projects`),
/// its name slugified + deduped on disk; the human `name` becomes the project's
/// display name. Domain logic lives in `projects::create_auto_project`; this
/// wrapper resolves the base dir from settings and stringifies errors.
#[tauri::command(rename_all = "snake_case")]
pub fn create_project_folder(
    db: State<DbHandle>,
    name: String,
) -> Result<EnsuredProjectDto, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    let base = crate::settings::get_settings(&conn)
        .map_err(|e| e.to_string())?
        .resolved_auto_project_base_dir()
        .map_err(|e| e.to_string())?;
    projects::create_auto_project(&conn, &base, &name)
        .map(EnsuredProjectDto::from)
        .map_err(|e| e.to_string())
}

/// Best-effort removal of a thread's on-disk artifact directory (bead
/// conceptify-0kt). A missing dir (thread never saved an artifact) is treated
/// as success; any other error is returned so the caller can log it — it is
/// never fatal to the delete, which has already removed the authoritative DB
/// row. Split out (with an explicit `root`) so it's unit-testable without the
/// `artifacts_root()` environment dependency.
fn remove_thread_artifact_dir(root: &Path, project_id: &str, slug: &str) -> std::io::Result<()> {
    let dir = artifacts::thread_dir(root, project_id, slug);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Delete a thread and all of its data (bead conceptify-0kt — the hygiene valve
/// for a thread stuck in `generating` with no artifact, or any thread the user
/// no longer wants). Removes the DB row — which cascades to its
/// artifacts/comments/follow_up_runs via the schema `ON DELETE CASCADE` FKs
/// (`db::migrations`, enforced by the `foreign_keys = ON` pragma) — and then,
/// best-effort, its on-disk artifact directory
/// (`~/Documents/conceptify/artifacts/<project>/threads/<slug>/`). Errors
/// (string) only when the thread doesn't exist or the DB delete fails; a
/// failure to remove the directory is logged, not surfaced.
///
/// Like the other shell mutations here it emits no Tauri event — the invoking
/// window refetches after awaiting (see this module's header). No other surface
/// deletes threads, so there is no cross-surface change to broadcast; if a CLI
/// delete is ever added it should emit `thread-deleted` and wire it in
/// `events.ts`.
#[tauri::command(rename_all = "snake_case")]
pub fn delete_thread(db: State<DbHandle>, thread_id: String) -> Result<(), String> {
    // Resolve the project/slug BEFORE deleting so we can locate the artifact dir
    // once the row (and its slug) is gone.
    let (project_id, slug) = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        let thread = threads::get_thread_opt(&conn, &thread_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("thread not found: {thread_id}"))?;
        let deleted = threads::delete_thread(&conn, &thread_id).map_err(|e| e.to_string())?;
        if !deleted {
            return Err(format!("thread not found: {thread_id}"));
        }
        (thread.project_id, thread.slug)
    };

    if let Ok(root) = artifacts::artifacts_root() {
        if let Err(e) = remove_thread_artifact_dir(&root, &project_id, &slug) {
            eprintln!(
                "[conceptify] delete_thread: failed to remove artifact dir for thread {thread_id}: {e}"
            );
        }
    }
    Ok(())
}

/// One saved artifact version for the viewer's version switcher (FR-2.4).
/// Sorted ascending by `version`; the last entry is the thread's latest.
#[derive(Serialize)]
pub struct ArtifactVersionDto {
    pub version: i64,
    pub created_at: String,
    /// `initial` (v1) or `follow_up` (v2+), mirroring the artifacts table.
    pub created_by: String,
    pub response_intent: Option<crate::skill_catalog::ResponseIntentInput>,
    pub skills: Vec<crate::skill_catalog::SelectedSkill>,
}

/// List a thread's saved artifact versions, oldest first (FR-2.4). An
/// unknown thread (or a thread with no saves yet) yields an empty list —
/// the viewer treats both as "no artifact yet" and renders by status.
#[tauri::command(rename_all = "snake_case")]
pub fn list_artifact_versions(
    db: State<DbHandle>,
    thread_id: String,
) -> Result<Vec<ArtifactVersionDto>, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT version, created_at, created_by,
                    response_intent_json, selected_skills_json
             FROM artifacts
             WHERE thread_id = ?1 ORDER BY version ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([&thread_id], |r| {
            Ok(ArtifactVersionDto {
                version: r.get(0)?,
                created_at: r.get(1)?,
                created_by: r.get(2)?,
                response_intent: r
                    .get::<_, Option<String>>(3)?
                    .and_then(|json| serde_json::from_str(&json).ok()),
                skills: r
                    .get::<_, Option<String>>(4)?
                    .and_then(|json| serde_json::from_str(&json).ok())
                    .unwrap_or_default(),
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub fn diff_versions(
    db: State<DbHandle>,
    thread_id: String,
    from_version: i64,
    to_version: i64,
) -> Result<conceptify_types::ArtifactVersionDiffResponse, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    crate::artifact_diff::diff_versions(&conn, &thread_id, from_version, to_version)
        .map_err(|e| e.to_string())
}

/// Resolve the on-disk `artifact.html` (the always-latest copy, §5.6) for a
/// thread. Split out of the command so the DB/path logic is unit-testable
/// without triggering a real browser launch. Errors are user-facing strings
/// (the frontend surfaces them verbatim).
fn resolve_latest_artifact_html(
    conn: &Connection,
    root: &Path,
    thread_id: &str,
) -> Result<PathBuf, String> {
    let row = conn
        .query_row(
            "SELECT project_id, slug FROM threads WHERE id = ?1",
            [thread_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((project_id, slug)) = row else {
        return Err(format!("thread not found: {thread_id}"));
    };

    let has_versions: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM artifacts WHERE thread_id = ?1)",
            [thread_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !has_versions {
        return Err("this thread has no saved artifact yet".to_owned());
    }

    let path = artifacts::latest_copy_path(root, &project_id, &slug);
    if !path.is_file() {
        return Err(format!(
            "artifact file is missing on disk: {}",
            path.display()
        ));
    }
    Ok(path)
}

/// Open the thread's on-disk `artifact.html` with the system default browser
/// (FR-2.5 — the permanently-one-click portability guarantee). The frontend
/// never constructs filesystem paths: this command resolves the path from
/// the DB + artifacts root server-side and hands it to the opener plugin
/// (macOS: `/usr/bin/open`, which launches the `.html` default handler).
/// Returns the opened path (handy for logging/diagnostics).
#[tauri::command(rename_all = "snake_case")]
pub fn open_artifact_in_browser(db: State<DbHandle>, thread_id: String) -> Result<String, String> {
    let root = artifacts::artifacts_root().map_err(|e| e.to_string())?;
    // Resolve under the lock, open after releasing it — the launch can take
    // long enough that holding the shared connection would stall the API.
    let path = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        resolve_latest_artifact_html(&conn, &root, &thread_id)?
    };
    tauri_plugin_opener::open_path(&path, None::<&str>).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// A comment row for the shell's in-artifact comment layer (94m.3/94m.4) and
/// sidebar (94m.6). Mirrors the HTTP `CommentResponse` (docs/api.md "Comments")
/// field-for-field — the frontend commands and the CLI/skill HTTP surface stay
/// on the same shape. `anchor` is the stored FR-4.4 anchor JSON, `null` for a
/// direct follow-up.
#[derive(Serialize)]
pub struct CommentDto {
    pub id: String,
    pub thread_id: String,
    /// The root this comment replies to (epic conceptify-6xi), or `null` for a
    /// root. Carried separately from `comments::Comment` (see its `From` impl).
    pub parent_id: Option<String>,
    pub artifact_version: i64,
    pub anchor: Option<serde_json::Value>,
    pub body: String,
    pub status: String,
    pub answer_html: Option<String>,
    pub anchor_state: String,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

impl From<(comments::Comment, Option<String>)> for CommentDto {
    fn from((c, parent_id): (comments::Comment, Option<String>)) -> Self {
        CommentDto {
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
}

/// Create a comment against the artifact version currently in the viewer
/// (FR-4.1 text selection / FR-4.2 element click / FR-4.3 direct follow-up).
///
/// Thin wrapper over `comments::create_comment` — the same domain fn that backs
/// `POST /api/v1/comments`. `anchor` is the FR-4.4 anchor captured by the bridge
/// (`null`/absent for a direct follow-up); it is validated and stored verbatim.
/// The target thread and `artifact_version` must already exist (a comment always
/// anchors to a saved version), so a still-generating thread with no artifact
/// yields a clean error rather than an opaque composite-FK failure — the shell
/// only mounts the comment layer once an artifact is present.
///
/// Unlike the axum route this does **not** emit a `comment-created` event: the
/// shell that invoked it updates its own store directly (the established command
/// convention — events are for cross-surface CLI/API mutations).
///
/// `parent_id` (epic conceptify-6xi) makes this a threaded reply: it dispatches to
/// `comments::create_reply` (no anchor, inherits the parent's version, re-opens an
/// answered/applied root). The frontend reply composer (bead conceptify-6xi.3) is
/// the caller; this is the plumbing.
#[tauri::command(rename_all = "snake_case")]
pub fn create_comment(
    db: State<DbHandle>,
    thread_id: String,
    artifact_version: i64,
    anchor: Option<serde_json::Value>,
    body: String,
    parent_id: Option<String>,
) -> Result<CommentDto, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    let ctx = match parent_id.as_deref() {
        Some(pid) => comments::create_reply(&conn, &thread_id, pid, &body),
        None => {
            comments::create_comment(&conn, &thread_id, artifact_version, anchor.as_ref(), &body)
        }
    }
    .map_err(|e| e.to_string())?;
    Ok((ctx.comment, ctx.parent_id).into())
}

/// List a thread's comments, oldest first, optionally filtered to one status
/// (`open` | `answered` | `applied`). Thin wrapper over `comments::list_comments`
/// (the same domain fn behind `GET /api/v1/comments`). An unknown thread yields
/// an empty list; an unknown `status` value is a clean error.
#[tauri::command(rename_all = "snake_case")]
pub fn list_comments(
    db: State<DbHandle>,
    thread_id: String,
    status: Option<String>,
) -> Result<Vec<CommentDto>, String> {
    let status =
        match status.as_deref() {
            None | Some("") => None,
            Some(s) => Some(comments::CommentStatus::parse(s).ok_or_else(|| {
                format!("invalid status \"{s}\" (expected open|answered|applied)")
            })?),
        };
    let conn = db.lock().map_err(|e| e.to_string())?;
    let rows = comments::list_comments_with_parent(&conn, &thread_id, status)
        .map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(CommentDto::from).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives `list_projects` through Tauri's real IPC dispatch (not just a
    /// direct fn call): proves the `#[tauri::command(rename_all = "snake_case")]`
    /// arg mapping accepts the `include_archived` key the frontend sends, that
    /// the command reads through managed `DbHandle` state, and that the returned
    /// DTO carries the derived `root_exists` flag the FR-1.3 badge depends on.
    /// This is the automated stand-in for the (headlessly-unavailable) webview:
    /// it exercises the exact `invoke("list_projects", { include_archived })`
    /// path the shell uses.
    #[test]
    fn list_projects_command_returns_dtos_with_root_exists() {
        let db_path = std::env::temp_dir().join(format!(
            "conceptify-test-cmd-projects-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db_handle = crate::db::init_at(&db_path).expect("test db should init and migrate");

        // One project whose mapped directory exists on disk (the temp dir), one
        // pointing at a path that does not — the two `root_exists` outcomes.
        let existing_dir = std::env::temp_dir();
        let existing_dir_str = existing_dir.to_string_lossy().to_string();
        {
            let conn = db_handle.lock().unwrap();
            conn.execute(
                "INSERT INTO projects (id, name, root_path) VALUES ('p-exists', 'Exists', ?1)",
                [&existing_dir_str],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO projects (id, name, root_path)
                 VALUES ('p-missing', 'Missing', '/nonexistent/conceptify/xyz-should-not-exist')",
                [],
            )
            .unwrap();
        }

        let app = tauri::test::mock_builder()
            .manage(db_handle)
            .invoke_handler(tauri::generate_handler![list_projects])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("failed to build mock app");

        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("failed to build mock webview");

        let response = tauri::test::get_ipc_response(
            &webview,
            tauri::webview::InvokeRequest {
                cmd: "list_projects".into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: "tauri://localhost".parse().unwrap(),
                body: tauri::ipc::InvokeBody::Json(
                    serde_json::json!({ "include_archived": false }),
                ),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.to_string(),
            },
        )
        .expect("list_projects command should succeed over IPC");

        let value: serde_json::Value = response
            .deserialize()
            .expect("response should deserialize as a JSON array of projects");
        let arr = value.as_array().expect("response is a JSON array");
        assert_eq!(arr.len(), 2, "both non-archived projects should be listed");

        let by_id = |id: &str| {
            arr.iter()
                .find(|p| p["id"] == serde_json::json!(id))
                .unwrap_or_else(|| panic!("project {id} missing from response"))
        };
        assert_eq!(by_id("p-exists")["root_exists"], serde_json::json!(true));
        assert_eq!(by_id("p-missing")["root_exists"], serde_json::json!(false));
        assert_eq!(by_id("p-exists")["thread_count"], serde_json::json!(0));

        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
        let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
    }

    /// Shared fixture for the artifact-facing command tests: a real-migration
    /// DB with one project + one thread, and a throwaway artifacts root.
    fn artifact_fixture(
        tag: &str,
    ) -> (
        crate::db::DbHandle,
        std::path::PathBuf,
        String,
        std::path::PathBuf,
    ) {
        let db_path = std::env::temp_dir().join(format!(
            "conceptify-test-cmd-artifacts-{tag}-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = std::env::temp_dir().join(format!(
            "conceptify-test-cmd-artifacts-root-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let db_handle = crate::db::init_at(&db_path).expect("test db should init and migrate");
        let thread_id = {
            let conn = db_handle.lock().unwrap();
            conn.execute(
                "INSERT INTO projects (id, name, root_path) VALUES ('p1', 'Proj', '/tmp/p1')",
                [],
            )
            .unwrap();
            crate::threads::create_thread(&conn, "p1", "Viewer thread", "q")
                .unwrap()
                .id
        };
        (db_handle, root, thread_id, db_path)
    }

    fn cleanup(db_path: &std::path::Path, root: &std::path::Path) {
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_file(db_path);
        let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
        let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
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

    /// Drives `list_artifact_versions` through Tauri's real IPC dispatch,
    /// exactly like the frontend's `invoke("list_artifact_versions",
    /// { thread_id })`: proves the snake_case arg mapping, the managed-state
    /// read, and the ascending version order the switcher relies on. The
    /// versions are saved through the real `artifacts::save_artifact`
    /// pipeline so the DTO reflects genuine rows.
    #[test]
    fn list_artifact_versions_over_ipc_is_ascending() {
        let (db_handle, root, thread_id, db_path) = artifact_fixture("list");
        {
            let conn = db_handle.lock().unwrap();
            for v in 1..=3 {
                crate::artifacts::save_artifact(
                    &conn,
                    &root,
                    &thread_id,
                    artifact_html(v).as_bytes(),
                )
                .unwrap_or_else(|e| panic!("save v{v}: {e:?}"));
            }
        }

        let app = tauri::test::mock_builder()
            .manage(db_handle)
            .invoke_handler(tauri::generate_handler![list_artifact_versions])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("failed to build mock app");
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("failed to build mock webview");

        let response = tauri::test::get_ipc_response(
            &webview,
            tauri::webview::InvokeRequest {
                cmd: "list_artifact_versions".into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: "tauri://localhost".parse().unwrap(),
                body: tauri::ipc::InvokeBody::Json(serde_json::json!({ "thread_id": thread_id })),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.to_string(),
            },
        )
        .expect("list_artifact_versions should succeed over IPC");

        let value: serde_json::Value = response.deserialize().expect("JSON array of versions");
        let arr = value.as_array().expect("array");
        assert_eq!(arr.len(), 3);
        assert_eq!(
            arr.iter().map(|v| v["version"].as_i64().unwrap()).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "versions must come back ascending"
        );
        assert_eq!(arr[0]["created_by"], serde_json::json!("initial"));
        assert_eq!(arr[2]["created_by"], serde_json::json!("follow_up"));
        assert!(arr[0]["created_at"].as_str().is_some_and(|s| !s.is_empty()));

        // Unknown thread → empty list, not an error.
        let response = tauri::test::get_ipc_response(
            &webview,
            tauri::webview::InvokeRequest {
                cmd: "list_artifact_versions".into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: "tauri://localhost".parse().unwrap(),
                body: tauri::ipc::InvokeBody::Json(serde_json::json!({ "thread_id": "ghost" })),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.to_string(),
            },
        )
        .expect("unknown thread should still succeed");
        let value: serde_json::Value = response.deserialize().unwrap();
        assert_eq!(value.as_array().map(Vec::len), Some(0));

        cleanup(&db_path, &root);
    }

    /// Drives `create_comment` then `list_comments` through Tauri's real IPC
    /// dispatch, exactly like the shell's `invoke("create_comment", { thread_id,
    /// artifact_version, anchor, body })` and `invoke("list_comments", {
    /// thread_id })` (94m.3/94m.4). Proves the snake_case arg mapping (including
    /// the nested `anchor` JSON and the `Option` `status` filter), that the
    /// created comment starts `open`/`anchored` with the FR-4.4 anchor stored
    /// verbatim, and that the just-created comment lists back. A real artifact
    /// v1 is saved through the genuine pipeline so the composite-FK create path
    /// is exercised, not stubbed.
    #[test]
    fn comment_commands_over_ipc_create_and_list() {
        let (db_handle, root, thread_id, db_path) = artifact_fixture("comments");
        {
            let conn = db_handle.lock().unwrap();
            crate::artifacts::save_artifact(&conn, &root, &thread_id, artifact_html(1).as_bytes())
                .expect("save artifact v1");
        }

        let app = tauri::test::mock_builder()
            .manage(db_handle)
            .invoke_handler(tauri::generate_handler![create_comment, list_comments])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("failed to build mock app");
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("failed to build mock webview");

        // A text-selection anchor (FR-4.1): primary cfy_id + offsets, fallback
        // quote — the exact snake_case shape the bridge emits.
        let anchor = serde_json::json!({
            "v": 1,
            "type": "text",
            "cfy_id": "sec-t",
            "start": 0,
            "end": 9,
            "quote": { "exact": "Version 1", "prefix": "", "suffix": "" }
        });

        let response = tauri::test::get_ipc_response(
            &webview,
            tauri::webview::InvokeRequest {
                cmd: "create_comment".into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: "tauri://localhost".parse().unwrap(),
                body: tauri::ipc::InvokeBody::Json(serde_json::json!({
                    "thread_id": thread_id,
                    "artifact_version": 1,
                    "anchor": anchor,
                    "body": "why this heading?"
                })),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.to_string(),
            },
        )
        .expect("create_comment should succeed over IPC");
        let created: serde_json::Value = response.deserialize().expect("comment JSON");
        assert_eq!(created["status"], serde_json::json!("open"));
        assert_eq!(created["anchor_state"], serde_json::json!("anchored"));
        assert_eq!(created["artifact_version"], serde_json::json!(1));
        assert_eq!(created["anchor"], anchor, "anchor stored + returned verbatim");
        assert!(created["answer_html"].is_null());
        assert!(created["resolved_at"].is_null());
        let created_id = created["id"].as_str().expect("id string").to_owned();

        // The just-created comment lists back (no status filter → all comments).
        let response = tauri::test::get_ipc_response(
            &webview,
            tauri::webview::InvokeRequest {
                cmd: "list_comments".into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: "tauri://localhost".parse().unwrap(),
                body: tauri::ipc::InvokeBody::Json(serde_json::json!({ "thread_id": thread_id })),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.to_string(),
            },
        )
        .expect("list_comments should succeed over IPC");
        let listed: serde_json::Value = response.deserialize().expect("array JSON");
        let arr = listed.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], serde_json::json!(created_id));
        assert_eq!(arr[0]["body"], serde_json::json!("why this heading?"));

        // The `open` status filter matches; `applied` does not.
        let response = tauri::test::get_ipc_response(
            &webview,
            tauri::webview::InvokeRequest {
                cmd: "list_comments".into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: "tauri://localhost".parse().unwrap(),
                body: tauri::ipc::InvokeBody::Json(
                    serde_json::json!({ "thread_id": thread_id, "status": "applied" }),
                ),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.to_string(),
            },
        )
        .expect("list_comments with filter should succeed over IPC");
        let listed: serde_json::Value = response.deserialize().unwrap();
        assert_eq!(listed.as_array().map(Vec::len), Some(0));

        cleanup(&db_path, &root);
    }

    /// Drives `create_comment` with a `parent_id` through Tauri's real IPC
    /// dispatch (epic conceptify-6xi): proves the snake_case `parent_id` arg maps,
    /// that it dispatches to the reply path (null anchor, inherited version), and
    /// that the returned DTO carries `parent_id`. The plumbing 6xi.3's composer uses.
    #[test]
    fn create_reply_command_over_ipc_carries_parent_id() {
        let (db_handle, root, thread_id, db_path) = artifact_fixture("reply");
        {
            let conn = db_handle.lock().unwrap();
            crate::artifacts::save_artifact(&conn, &root, &thread_id, artifact_html(1).as_bytes())
                .expect("save artifact v1");
        }

        let app = tauri::test::mock_builder()
            .manage(db_handle)
            .invoke_handler(tauri::generate_handler![create_comment])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("failed to build mock app");
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("failed to build mock webview");

        let invoke = |body: serde_json::Value| {
            tauri::test::get_ipc_response(
                &webview,
                tauri::webview::InvokeRequest {
                    cmd: "create_comment".into(),
                    callback: tauri::ipc::CallbackFn(0),
                    error: tauri::ipc::CallbackFn(1),
                    url: "tauri://localhost".parse().unwrap(),
                    body: tauri::ipc::InvokeBody::Json(body),
                    headers: Default::default(),
                    invoke_key: tauri::test::INVOKE_KEY.to_string(),
                },
            )
        };

        // A root comment (no parent).
        let root_val: serde_json::Value = invoke(serde_json::json!({
            "thread_id": thread_id, "artifact_version": 1, "body": "root q"
        }))
        .expect("create root over IPC")
        .deserialize()
        .unwrap();
        assert!(root_val["parent_id"].is_null());
        let root_id = root_val["id"].as_str().unwrap().to_owned();

        // A reply (parent_id set; anchor omitted).
        let reply: serde_json::Value = invoke(serde_json::json!({
            "thread_id": thread_id, "artifact_version": 1, "body": "reply", "parent_id": root_id
        }))
        .expect("create reply over IPC")
        .deserialize()
        .unwrap();
        assert_eq!(reply["parent_id"], serde_json::json!(root_id));
        assert!(reply["anchor"].is_null(), "reply carries no anchor");
        assert_eq!(reply["status"], serde_json::json!("open"));
        assert_eq!(reply["artifact_version"], serde_json::json!(1));

        cleanup(&db_path, &root);
    }

    /// The open-in-browser resolution logic (everything except the actual
    /// browser launch, which is not exercisable headlessly): resolves the
    /// always-latest `artifact.html`, and errors cleanly for an unknown
    /// thread, a thread with no versions, and a missing file.
    #[test]
    fn resolve_latest_artifact_html_covers_happy_and_error_paths() {
        let (db_handle, root, thread_id, db_path) = artifact_fixture("resolve");
        let conn = db_handle.lock().unwrap();

        // No versions yet → clear error.
        let err = resolve_latest_artifact_html(&conn, &root, &thread_id).unwrap_err();
        assert!(err.contains("no saved artifact"), "{err}");

        // Unknown thread → clear error.
        let err = resolve_latest_artifact_html(&conn, &root, "ghost").unwrap_err();
        assert!(err.contains("thread not found"), "{err}");

        // Two saves → resolves to the always-latest copy with v2 content.
        for v in 1..=2 {
            crate::artifacts::save_artifact(&conn, &root, &thread_id, artifact_html(v).as_bytes())
                .unwrap();
        }
        let path = resolve_latest_artifact_html(&conn, &root, &thread_id).unwrap();
        assert!(path.ends_with("artifact.html"), "{}", path.display());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), artifact_html(2));

        // DB rows present but the file vanished → clear error, no panic.
        std::fs::remove_file(&path).unwrap();
        let err = resolve_latest_artifact_html(&conn, &root, &thread_id).unwrap_err();
        assert!(err.contains("missing on disk"), "{err}");

        drop(conn);
        cleanup(&db_path, &root);
    }

    /// `remove_thread_artifact_dir` (the best-effort dir removal behind
    /// `delete_thread`): removes an existing thread dir and its contents, and
    /// treats a missing dir as success (idempotent — the thread may never have
    /// saved an artifact).
    #[test]
    fn remove_thread_artifact_dir_removes_then_tolerates_missing() {
        let root = std::env::temp_dir().join(format!(
            "conceptify-test-rm-thread-dir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dir = crate::artifacts::thread_dir(&root, "p1", "how-does-x-work");
        std::fs::create_dir_all(dir.join("runs")).unwrap();
        std::fs::write(dir.join("artifact.html"), b"<html></html>").unwrap();
        std::fs::write(dir.join("artifact.v1.html"), b"<html></html>").unwrap();

        // Existing dir + contents are removed.
        remove_thread_artifact_dir(&root, "p1", "how-does-x-work").unwrap();
        assert!(!dir.exists());

        // A now-missing dir is success (best-effort / idempotent).
        remove_thread_artifact_dir(&root, "p1", "how-does-x-work").unwrap();

        let _ = std::fs::remove_dir_all(&root);
    }
}

// ---------------------------------------------------------------------------
// Agent settings (PRD §5.5, FR-7.1–7.4) — bead conceptify-b12.1.
//
// Thin command wrappers over the `crate::settings` domain module (which owns
// the storage, defaults, and resolution logic). The M6 Settings UI
// (`conceptify-959.4`) is the caller. Types are fully qualified so this block
// stays self-contained (appended at EOF to avoid colliding with concurrent
// edits higher in this file). Domain logic + substitution safety are tested in
// `crate::settings`; these wrappers only marshal the DB handle and stringify
// errors, following the pattern of the commands above.
// ---------------------------------------------------------------------------

/// Read the agent settings (stored overrides merged over code defaults, or pure
/// defaults when nothing has been saved — FR-7.4 zero-config).
#[tauri::command(rename_all = "snake_case")]
pub fn get_agent_settings(db: State<DbHandle>) -> Result<crate::settings::AgentSettings, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    crate::settings::get_settings(&conn).map_err(|e| e.to_string())
}

/// Persist the agent settings and emit `settings-changed` so any live view (or
/// a future settings-aware surface) refreshes — consistent with the app's
/// event-driven live-update pattern. Validation (a `default_adapter` that names
/// an existing adapter) happens in the domain layer before the write, so a
/// broken config is rejected rather than stored.
#[tauri::command(rename_all = "snake_case")]
pub fn set_agent_settings<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    db: State<DbHandle>,
    settings: crate::settings::AgentSettings,
) -> Result<(), String> {
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        crate::settings::update_settings(&conn, &settings).map_err(|e| e.to_string())?;
    }
    use tauri::Emitter;
    let _ = app.emit("settings-changed", &());
    Ok(())
}

/// Reset agent settings to the code defaults (FR-7.4 — the Settings "Reset to
/// defaults" action): delete the stored override row so `get_agent_settings`
/// returns pure defaults, exactly as a fresh install. Emits `settings-changed`
/// like `set_agent_settings`, and returns the now-default settings so the UI
/// can repaint without a second round-trip. Restores the true zero-config
/// baseline rather than writing a "defaults" blob.
#[tauri::command(rename_all = "snake_case")]
pub fn reset_agent_settings<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    db: State<DbHandle>,
) -> Result<crate::settings::AgentSettings, String> {
    let defaults = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        crate::settings::clear_settings(&conn).map_err(|e| e.to_string())?;
        crate::settings::get_settings(&conn).map_err(|e| e.to_string())?
    };
    use tauri::Emitter;
    let _ = app.emit("settings-changed", &());
    Ok(defaults)
}

/// Per-purpose configured models (the fallback used when a run carries no model
/// override), UI-friendly. Mirrors `settings::PurposeModels` in camelCase.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelsDto {
    pub follow_up: String,
    pub artifact_update: String,
    pub in_app_ask: String,
}

/// A UI-friendly view of the run-selection options a per-ask override picker
/// (bead conceptify-e7m.4) needs (epic conceptify-e7m): the configured adapter
/// KEYS (not the full command/args templates `get_agent_settings` returns), the
/// default adapter, and the per-purpose default models. Distinct from the
/// live model *catalog* (bead e7m.6): this is the settings-derived fallback
/// baseline. Additive — `get_agent_settings` still returns the full blob.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentOptionsDto {
    /// Configured adapter keys (sorted; `adapters` is a BTreeMap), e.g.
    /// `["claude"]` — the escape-hatch adapter list.
    pub adapters: Vec<String>,
    /// The adapter used when a run carries no `{adapter}` override.
    pub default_adapter: String,
    /// The per-purpose default models (the `{model}`-override fallback).
    pub models: AgentModelsDto,
    /// Whether an OpenRouter API key is stored (bead conceptify-e7m.7) — the
    /// ONLY key-derived fact that ever reaches the frontend. The Settings UI
    /// uses it to show "key set / not set"; the pickers use it to know whether
    /// openrouter-runnable models can actually run. The key itself is stored
    /// outside the settings blob and is never returned by any command.
    pub open_router_key_set: bool,
    pub local_endpoint_key_set: bool,
}

/// Expose the available adapters + per-purpose default models to the frontend
/// in a UI-friendly shape (epic conceptify-e7m, for the point-of-ask override
/// picker). Reads the same merged settings as `get_agent_settings` but projects
/// only what a picker needs. Never mutates settings.
#[tauri::command(rename_all = "snake_case")]
pub fn get_agent_options(db: State<DbHandle>) -> Result<AgentOptionsDto, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    let s = crate::settings::get_settings(&conn).map_err(|e| e.to_string())?;
    let open_router_key_set =
        crate::settings::has_openrouter_api_key(&conn).map_err(|e| e.to_string())?;
    let local_endpoint_key_set = crate::settings::get_local_endpoint_api_key(&conn)
        .map_err(|e| e.to_string())?.is_some();
    Ok(AgentOptionsDto {
        adapters: s.adapters.keys().cloned().collect(),
        default_adapter: s.default_adapter,
        models: AgentModelsDto {
            follow_up: s.models.follow_up,
            artifact_update: s.models.artifact_update,
            in_app_ask: s.models.in_app_ask,
        },
        open_router_key_set,
        local_endpoint_key_set,
    })
}

/// Store (or clear, with `null`/blank) the OpenRouter API key (bead
/// conceptify-e7m.7). Write-only by design: no command ever returns the key —
/// the frontend learns only the `openRouterKeySet` boolean from
/// `get_agent_options`. Stored in its own settings row (never inside the
/// `agent_settings` blob — see the storage decision recorded in
/// `settings::OPENROUTER_KEY_SETTINGS_KEY`'s docs), so `reset_agent_settings`
/// leaves it intact. Emits `settings-changed` so option readers refresh.
#[tauri::command(rename_all = "snake_case")]
pub fn set_openrouter_api_key<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    db: State<DbHandle>,
    key: Option<String>,
) -> Result<(), String> {
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        crate::settings::set_openrouter_api_key(&conn, key.as_deref())
            .map_err(|e| e.to_string())?;
    }
    use tauri::Emitter;
    let _ = app.emit("settings-changed", &());
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn set_local_endpoint_api_key<R: tauri::Runtime>(
    app: tauri::AppHandle<R>, db: State<DbHandle>, key: Option<String>,
) -> Result<(), String> {
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        crate::settings::set_local_endpoint_api_key(&conn, key.as_deref()).map_err(|e| e.to_string())?;
    }
    use tauri::Emitter;
    let _ = app.emit("settings-changed", &());
    Ok(())
}

// ---------------------------------------------------------------------------
// Follow-up flows (PRD FR-4.6/4.7/4.8/4.9) — beads b12.4/b12.5/b12.6.
//
// Thin command wrappers over the `crate::flows` layer (which owns the prompt
// assembly, child PATH preparation, thread-status policy, and the apply
// ordering contract — see flows.rs module docs). Following the established
// pattern: wrappers marshal managed state and stringify errors; the strings
// are shown verbatim in the sidebar. `cancel_run` (the FR-4.8 cancel button)
// lives in `crate::runs` and is registered alongside these in lib.rs.
// ---------------------------------------------------------------------------

/// A started flow run: what the sidebar needs to render the FR-4.8 run block
/// and compute per-comment progress. `target_comment_ids` is only available
/// here (targets are not persisted) — a UI re-attaching to an in-flight run
/// via `get_active_run` gets an indeterminate spinner instead.
#[derive(Serialize)]
pub struct RunStartedDto {
    pub run_id: String,
    pub thread_id: String,
    /// `answer` (FR-4.6) or `apply` (FR-4.7).
    pub mode: String,
    pub target_comment_ids: Vec<String>,
}

impl From<crate::flows::FlowStarted> for RunStartedDto {
    fn from(s: crate::flows::FlowStarted) -> Self {
        RunStartedDto {
            run_id: s.run_id,
            thread_id: s.thread_id,
            mode: s.mode.as_str().to_owned(),
            target_comment_ids: s.target_comment_ids,
        }
    }
}

/// Start the FR-4.6 "Ask follow-ups" batch run: ONE headless agent answers
/// every open comment individually via `resolve-comment` (sidebar-first; the
/// artifact is never modified in this mode). Concurrent submissions are
/// accepted into the durable provider queue.
#[tauri::command(rename_all = "snake_case")]
pub async fn ask_follow_ups<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    thread_id: String,
    run_override: Option<crate::settings::RunOverride>,
) -> Result<RunStartedDto, String> {
    crate::flows::ask_follow_ups(&app, &thread_id, run_override)
        .await
        .map(RunStartedDto::from)
        .map_err(|e| e.to_string())
}

/// Start the FR-4.7 "Apply to artifact" run for `comment_ids` (empty = every
/// `answered` comment). The agent publishes ONE new artifact version via
/// `save-artifact` after marking the targets `applied` (ordering contract in
/// flows.rs). The thread shows `updating` for the duration.
#[tauri::command(rename_all = "snake_case")]
pub async fn apply_to_artifact<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    thread_id: String,
    comment_ids: Vec<String>,
    run_override: Option<crate::settings::RunOverride>,
) -> Result<RunStartedDto, String> {
    crate::flows::apply_to_artifact(&app, &thread_id, comment_ids, run_override)
        .await
        .map(RunStartedDto::from)
        .map_err(|e| e.to_string())
}

/// The newest non-terminal run for a thread, if any. `status` may be queued,
/// starting, running, throttled, or cancelling.
#[derive(Serialize)]
pub struct ActiveRunDto {
    pub run_id: String,
    pub thread_id: String,
    pub mode: String,
    pub status: String,
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_active_run(
    db: State<DbHandle>,
    registry: State<crate::runs::RunRegistry>,
    thread_id: String,
) -> Result<Option<ActiveRunDto>, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    let summary = crate::flows::active_run_summary(&conn, &registry, &thread_id)
        .map_err(|e| e.to_string())?;
    Ok(summary.map(|s| ActiveRunDto {
        run_id: s.run_id,
        thread_id: s.thread_id,
        mode: s.mode,
        status: s.status,
    }))
}

#[tauri::command(rename_all = "snake_case")]
pub fn list_run_activity(db: State<DbHandle>) -> Result<Vec<crate::runs::RunActivity>, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    crate::runs::list_activity(&conn).map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub fn dismiss_run_activity(db: State<DbHandle>, run_id: String) -> Result<bool, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    crate::runs::dismiss_activity(&conn, &run_id).map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub fn mark_run_activity_seen(db: State<DbHandle>, run_ids: Vec<String>) -> Result<usize, String> {
    let mut conn = db.lock().map_err(|e| e.to_string())?;
    crate::runs::mark_activity_seen(&mut conn, &run_ids).map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub fn claim_system_run_notification(
    db: State<DbHandle>,
    run_id: String,
) -> Result<Option<crate::runs::SystemRunNotification>, String> {
    let mut conn = db.lock().map_err(|e| e.to_string())?;
    crate::runs::claim_system_notification(&mut conn, &run_id).map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct ConflictReviewDto {
    pub run_id: String,
    pub thread_id: String,
    pub project_id: String,
    pub project_name: String,
    pub thread_title: String,
    pub agent: String,
    pub model: String,
    pub route: Option<String>,
    pub base_version: Option<i64>,
    pub current_version: i64,
    pub resolution: String,
    pub kind: String,
    pub target_cfy_ids: Vec<String>,
    pub diff: conceptify_types::ArtifactVersionDiffResponse,
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_conflict_review(
    db: State<DbHandle>,
    run_id: String,
) -> Result<ConflictReviewDto, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    let row = conn
        .query_row(
            "SELECT r.thread_id, p.id, p.name, t.title, r.agent, r.model, r.route,
                    r.base_artifact_version, r.conflict_current_version,
                    COALESCE(r.conflict_resolution, 'pending'), r.status_reason,
                    r.candidate_path, a.file_path
             FROM follow_up_runs r
             JOIN threads t ON t.id = r.thread_id
             JOIN projects p ON p.id = t.project_id
             JOIN artifacts a ON a.thread_id = t.id
                 AND a.version = r.conflict_current_version
             WHERE r.id = ?1 AND r.status = 'conflicted'",
            [&run_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?, r.get::<_, String>(4)?, r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?, r.get::<_, Option<i64>>(7)?,
                    r.get::<_, i64>(8)?, r.get::<_, String>(9)?, r.get::<_, Option<String>>(10)?,
                    r.get::<_, String>(11)?, r.get::<_, String>(12)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "conflict candidate not found".to_owned())?;
    let reason = row.10.as_deref().unwrap_or("stale_base");
    let preview_comment_id = reason.strip_prefix("preview_required:")
        .or_else(|| reason.strip_prefix("stale_preview:"));
    let target_cfy_ids = preview_comment_id
        .and_then(|id| {
            conn.query_row("SELECT anchor FROM comments WHERE id = ?1", [id], |r| r.get::<_, Option<String>>(0))
                .optional().ok().flatten().flatten()
        })
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|anchor| anchor.get("target")?.get("cfy_ids")?.as_array().cloned())
        .map(|ids| ids.into_iter().filter_map(|id| id.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    let current_html = std::fs::read_to_string(&row.12).map_err(|e| e.to_string())?;
    let candidate_html = std::fs::read_to_string(&row.11).map_err(|e| e.to_string())?;
    let diff = crate::artifact_diff::diff_html(&row.0, row.8, row.8 + 1, &current_html, &candidate_html);
    Ok(ConflictReviewDto {
        run_id,
        thread_id: row.0,
        project_id: row.1,
        project_name: row.2,
        thread_title: row.3,
        agent: row.4,
        model: row.5,
        route: row.6,
        base_version: row.7,
        current_version: row.8,
        resolution: row.9,
        kind: if reason.starts_with("preview_required:") { "revision" } else { "stale_base" }.to_owned(),
        target_cfy_ids,
        diff,
    })
}

#[tauri::command(rename_all = "snake_case")]
pub fn publish_conflict_candidate<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    db: State<DbHandle>,
    run_id: String,
) -> Result<i64, String> {
    let root = artifacts::artifacts_root().map_err(|e| e.to_string())?;
    let (saved, applied_comment) = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        let row: (String, String, Option<String>) = conn
            .query_row(
                "SELECT thread_id, candidate_path, status_reason FROM follow_up_runs
                 WHERE id = ?1 AND status = 'conflicted'
                   AND COALESCE(conflict_resolution, 'pending') = 'pending'",
                [&run_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|_| "unresolved conflict candidate not found".to_owned())?;
        let bytes = std::fs::read(&row.1).map_err(|e| e.to_string())?;
        let saved = artifacts::save_artifact_for_run(
            &conn,
            &root,
            &row.0,
            &bytes,
            Some(&run_id),
            Some("separate"),
        )
        .map_err(|e| e.to_string())?;
        let applied = row.2.as_deref().and_then(|reason| {
            reason.strip_prefix("preview_required:").or_else(|| reason.strip_prefix("stale_preview:"))
        })
            .map(|comment_id| {
                crate::comments::update_comment(
                    &conn,
                    comment_id,
                    Some(crate::comments::CommentStatus::Applied),
                    Some("Applied after explicit revision preview."),
                    None,
                ).map_err(|e| e.to_string())
            }).transpose()?;
        (saved, applied)
    };
    use tauri::Emitter;
    let _ = app.emit(
        "artifact-updated",
        serde_json::json!({
            "project_id": saved.project_id,
            "thread_id": saved.thread_id,
            "version": saved.version,
        }),
    );
    for comment in &saved.reattached {
        let _ = app.emit(
            "comment-updated",
            serde_json::json!({
                "project_id": saved.project_id,
                "thread_id": comment.thread_id,
                "comment_id": comment.id,
                "status": comment.status.as_str(),
            }),
        );
    }
    if let Some(comment) = &applied_comment {
        let _ = app.emit("comment-updated", serde_json::json!({
            "project_id": saved.project_id,
            "thread_id": comment.comment.thread_id,
            "comment_id": comment.comment.id,
            "status": comment.comment.status.as_str(),
        }));
    }
    Ok(saved.version)
}

#[tauri::command(rename_all = "snake_case")]
pub fn reject_conflict_candidate(
    db: State<DbHandle>,
    run_id: String,
) -> Result<bool, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    let changed = conn.execute(
        "UPDATE follow_up_runs
         SET conflict_resolution = 'rejected',
             activity_dismissed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE id = ?1 AND status = 'conflicted'
           AND COALESCE(conflict_resolution, 'pending') = 'pending'",
        [&run_id],
    ).map_err(|e| e.to_string())?;
    Ok(changed == 1)
}

#[tauri::command(rename_all = "snake_case")]
pub fn restore_artifact_version<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    db: State<DbHandle>,
    thread_id: String,
    version: i64,
    run_id: Option<String>,
) -> Result<i64, String> {
    let root = artifacts::artifacts_root().map_err(|e| e.to_string())?;
    let (saved, reopened_comment_id) = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        let source: (String, Option<String>, Option<String>) = conn.query_row(
            "SELECT file_path, response_intent_json, selected_skills_json
             FROM artifacts WHERE thread_id = ?1 AND version = ?2",
            rusqlite::params![thread_id, version],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).map_err(|_| "artifact version to restore was not found".to_owned())?;
        let bytes = std::fs::read(&source.0).map_err(|e| e.to_string())?;
        let saved = artifacts::save_artifact_for_run(&conn, &root, &thread_id, &bytes, None, None)
            .map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE artifacts SET response_intent_json = ?3, selected_skills_json = ?4
             WHERE thread_id = ?1 AND version = ?2",
            rusqlite::params![thread_id, saved.version, source.1, source.2],
        ).map_err(|e| e.to_string())?;
        let reopened = run_id.as_deref().and_then(|id| {
            conn.query_row("SELECT status_reason FROM follow_up_runs WHERE id = ?1", [id], |r| r.get::<_, Option<String>>(0))
                .optional().ok().flatten().flatten()
        }).and_then(|reason| {
            reason.strip_prefix("preview_required:")
                .or_else(|| reason.strip_prefix("stale_preview:"))
                .map(str::to_owned)
        });
        if let Some(comment_id) = &reopened {
            conn.execute(
                "UPDATE comments SET status = 'open', answer_html = NULL, resolved_at = NULL WHERE id = ?1",
                [comment_id],
            ).map_err(|e| e.to_string())?;
            // The request was `applied` while the restoring artifact was saved,
            // so the normal save-time pass correctly skipped it as frozen
            // history. Reopen first, then run the same reattachment algorithm
            // against the restored latest content so Undo returns an actionable
            // anchor on the new version rather than a cross-version orphan.
            let restored_html = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
            crate::anchoring::reattach_thread_comments(
                &conn,
                restored_html,
                &thread_id,
                saved.version,
            ).map_err(|e| e.to_string())?;
        }
        (saved, reopened)
    };
    use tauri::Emitter;
    let _ = app.emit("artifact-updated", serde_json::json!({
        "project_id": saved.project_id,
        "thread_id": saved.thread_id,
        "version": saved.version,
    }));
    for comment in &saved.reattached {
        let _ = app.emit("comment-updated", serde_json::json!({
            "project_id": saved.project_id,
            "thread_id": comment.thread_id,
            "comment_id": comment.id,
            "status": comment.status.as_str(),
        }));
    }
    if let Some(comment_id) = reopened_comment_id {
        let _ = app.emit("comment-updated", serde_json::json!({
            "project_id": saved.project_id,
            "thread_id": saved.thread_id,
            "comment_id": comment_id,
            "status": "open",
        }));
    }
    Ok(saved.version)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn rebase_conflict<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    run_id: String,
) -> Result<RunStartedDto, String> {
    crate::flows::rebase_conflict(&app, &run_id)
        .await
        .map(RunStartedDto::from)
        .map_err(|e| e.to_string())
}

/// The tail of a run's transcript log (FR-4.8 failure surfacing). `log_path`
/// is always returned (the full log is retained on disk for debugging); a
/// missing/unreadable file degrades to a single explanatory line rather than
/// an error, so the failure panel can always render the path.
#[derive(Serialize)]
pub struct RunLogTailDto {
    pub run_id: String,
    pub log_path: String,
    pub lines: Vec<String>,
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_run_log_tail(
    db: State<DbHandle>,
    run_id: String,
    max_lines: Option<usize>,
) -> Result<RunLogTailDto, String> {
    let log_path: String = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT log_path FROM follow_up_runs WHERE id = ?1",
            [&run_id],
            |r| r.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => format!("run not found: {run_id}"),
            other => other.to_string(),
        })?
    };

    let max = max_lines.unwrap_or(crate::flows::DEFAULT_LOG_TAIL_LINES);
    let lines = crate::flows::tail_lines(Path::new(&log_path), max)
        .unwrap_or_else(|e| vec![format!("(could not read log: {e})")]);
    Ok(RunLogTailDto {
        run_id,
        log_path,
        lines,
    })
}

// ---------------------------------------------------------------------------
// In-app ask (PRD §7.5, UC5, FR-5.1/5.2/5.3) — beads 959.1 / 959.2.
//
// Thin command wrappers over `crate::flows`' in-app ask layer (which owns thread
// creation, the ask prompt, the generation run, and the FR-5.3 status policy).
// The composer (src/components/NewThreadComposer.tsx) invokes `ask_from_app`;
// the thread-view error state invokes `get_latest_run` (to resolve the failed
// run's id for the log tail) and `retry_ask`.
// ---------------------------------------------------------------------------

/// What a started in-app ask hands the composer: the new (or retried) thread and
/// the generation run now authoring its artifact.
#[derive(Serialize)]
pub struct AskStartedDto {
    pub run_id: String,
    pub thread_id: String,
}

impl From<crate::flows::AskStarted> for AskStartedDto {
    fn from(s: crate::flows::AskStarted) -> Self {
        AskStartedDto {
            run_id: s.run_id,
            thread_id: s.thread_id,
        }
    }
}

/// Start an FR-5.1 in-app ask: create a thread in `project_id` (status
/// `generating`) and spawn a headless generation run that authors an artifact
/// per the skill and publishes it via `conceptify save-artifact`. `title` is
/// optional (derived from the question when blank). Rejects (user-facing string)
/// on an empty question, an unknown project, or a missing CLI/agent binary.
#[tauri::command(rename_all = "snake_case")]
#[allow(clippy::too_many_arguments)]
pub async fn ask_from_app<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    project_id: String,
    title: Option<String>,
    question: String,
    run_override: Option<crate::settings::RunOverride>,
    response_intent: crate::skill_catalog::ResponseIntentInput,
    skill_mode: String,
    selected_skill_ids: Vec<String>,
) -> Result<AskStartedDto, String> {
    let response_metadata = crate::skill_catalog::resolve_run_metadata(
        &question,
        response_intent,
        &skill_mode,
        &selected_skill_ids,
    )?;
    crate::flows::ask_from_app(
        &app,
        &project_id,
        title.as_deref(),
        &question,
        run_override,
        Some(response_metadata),
    )
    .await
    .map(AskStartedDto::from)
    .map_err(|e| e.to_string())
}

/// Retry a failed in-app ask (FR-5.3): re-spawn the same question into the same
/// thread and move it back to `generating`. Backs the thread-view "Retry"
/// button on the generation-error state.
#[tauri::command(rename_all = "snake_case")]
pub async fn retry_ask<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    thread_id: String,
) -> Result<AskStartedDto, String> {
    crate::flows::retry_ask(&app, &thread_id)
        .await
        .map(AskStartedDto::from)
        .map_err(|e| e.to_string())
}

/// The most recent run for a thread (any mode/status), or `null`. The FR-5.3
/// generation-error state uses it to resolve the failed run's id (for the log
/// tail via `get_run_log_tail`) — this works after an app restart too, unlike
/// `get_active_run` which only reports live runs.
#[derive(Serialize)]
pub struct LatestRunDto {
    pub run_id: String,
    pub mode: String,
    pub status: String,
    /// Resolved model the run actually used (epic e7m: retry-surface display).
    pub model: String,
    /// Route tag recorded on the row (`anthropic|openai|openrouter|manual`);
    /// `null` on pre-routing rows.
    pub route: Option<String>,
    /// True iff a per-run override was recorded — Retry re-applies it
    /// verbatim; when false, Retry re-derives the current defaults.
    pub overridden: bool,
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_latest_run(
    db: State<DbHandle>,
    thread_id: String,
) -> Result<Option<LatestRunDto>, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    let latest = crate::flows::latest_run_for_thread(&conn, &thread_id).map_err(|e| e.to_string())?;
    Ok(latest.map(|r| LatestRunDto {
        run_id: r.run_id,
        mode: r.mode,
        status: r.status,
        model: r.model,
        route: r.route,
        overridden: r.overridden,
    }))
}

// ---------------------------------------------------------------------------
// Ask now: single-comment answer run (epic conceptify-6xi, bead 6xi.2).
// ---------------------------------------------------------------------------

/// Start an "Ask now" answer run for ONE root comment (epic conceptify-6xi):
/// the same sidebar-only answer-mode run as [`ask_follow_ups`], but fired for a
/// single root without gathering the whole batch. The prompt carries that
/// root's full exchange history (root + prior answer + replies in order) and
/// directs the agent at the LATEST unanswered message — the reply row when the
/// root was re-opened by a reply, the root itself for a fresh comment. Returns
/// the same [`RunStartedDto`] shape as `ask_follow_ups`, with
/// `target_comment_ids` the single root id (the actual resolve may land on a
/// reply row). Concurrent answers are accepted into the provider queue.
///
/// Errors (user-facing strings): no artifact; comment not found on this thread;
/// the target is a reply (reply to its root instead); the target root is not
/// open; missing agent/CLI.
#[tauri::command(rename_all = "snake_case")]
pub async fn ask_single_comment<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    thread_id: String,
    root_comment_id: String,
    run_override: Option<crate::settings::RunOverride>,
) -> Result<RunStartedDto, String> {
    crate::flows::ask_single_comment(&app, &thread_id, &root_comment_id, run_override)
        .await
        .map(RunStartedDto::from)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Model catalog (epic conceptify-e7m, bead e7m.6).
//
// Thin command wrappers over `crate::catalog`. `get_model_catalog` serves the
// cached/snapshot catalog filtered to the enabled provider suites (no network);
// `refresh_model_catalog` forces a live re-fetch. Both project the catalog
// through the shared `enabled_providers` setting. The Settings UI (bead e7m.3)
// and the point-of-ask picker (e7m.4) are the callers. Appended at EOF to avoid
// colliding with concurrent edits higher in this file.
// ---------------------------------------------------------------------------

/// The current model catalog filtered to the enabled provider suites, plus the
/// full provider list with counts (for the settings toggles). Reads the disk
/// cache (or bundled snapshot) — never the network — so it is instant and always
/// succeeds. The background startup refresh (see `lib.rs`) keeps the cache warm.
#[tauri::command(rename_all = "snake_case")]
pub fn get_model_catalog(db: State<DbHandle>) -> Result<conceptify_types::CatalogResponse, String> {
    let settings = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        crate::settings::get_settings(&conn)
            .map_err(|e| e.to_string())?
    };
    let (cat, source) = crate::catalog::load_for_serving();
    let mut response = crate::catalog::build_response(&cat, source, &settings.enabled_providers);
    crate::catalog::add_local_endpoint(&mut response, settings.local_endpoint.as_ref(), &settings.enabled_providers);
    Ok(response)
}

/// Force a live re-fetch of the model catalog (the Settings "refresh now"
/// action), update the on-disk cache, and return the fresh catalog filtered to
/// the enabled providers. Failure-silent: a network error degrades to the
/// cache/snapshot rather than failing. Emits `catalog-refreshed` so live views
/// repaint.
#[tauri::command(rename_all = "snake_case")]
pub async fn refresh_model_catalog<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<conceptify_types::CatalogResponse, String> {
    let (cat, source) = crate::catalog::refresh_now().await;
    let settings = {
        use tauri::Manager;
        let db = app.state::<DbHandle>();
        let conn = db.lock().map_err(|e| e.to_string())?;
        crate::settings::get_settings(&conn)
            .map_err(|e| e.to_string())?
    };
    use tauri::Emitter;
    let _ = app.emit("catalog-refreshed", &());
    let mut response = crate::catalog::build_response(&cat, source, &settings.enabled_providers);
    crate::catalog::add_local_endpoint(&mut response, settings.local_endpoint.as_ref(), &settings.enabled_providers);
    Ok(response)
}
