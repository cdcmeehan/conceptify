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

use serde::Serialize;
use tauri::State;

use crate::db::DbHandle;
use crate::{projects, threads};

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
pub fn set_project_archived(
    db: State<DbHandle>,
    id: String,
    archived: bool,
) -> Result<(), String> {
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
                body: tauri::ipc::InvokeBody::Json(serde_json::json!({ "include_archived": false })),
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
}
