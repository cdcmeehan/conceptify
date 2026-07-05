//! Projects domain logic (PRD §7.1, FR-1.1, FR-1.3).
//!
//! Implements ensure-project (canonicalize path → find or create with deduped
//! name), list (with thread counts + last activity), rename, and archive.

use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};

/// A project row (mirrors the schema, minus the derived fields list returns).
#[derive(Debug, Clone)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub created_at: String,
    pub archived: bool,
}

/// Returned by ensure_project to indicate whether the project was newly created.
#[derive(Debug)]
pub struct EnsureProjectResult {
    pub project: Project,
    pub created: bool,
}

/// Errors specific to projects operations. Variants map to HTTP status codes
/// in the route handlers (see routes::projects).
#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("path does not exist or is not accessible: {0}")]
    PathNotFound(String),

    #[error("failed to canonicalize path: {0}")]
    CanonicalizeFailed(#[source] std::io::Error),

    #[error("project not found: {0}")]
    NotFound(String),

    #[error("project name must not be empty")]
    EmptyName,

    #[error("failed to create project directory: {0}")]
    MkdirFailed(#[source] std::io::Error),

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
}

/// Given a root path and optional name, canonicalize the path and return the
/// existing project or create one. Name defaults to the directory name, deduped
/// with numeric suffix (name, name-2, name-3...) if another project uses it.
///
/// Path must exist on disk; nonexistent paths return `PathNotFound`.
/// Canonicalization resolves symlinks and trailing slashes to one identity
/// (UNIQUE root_path in schema).
pub fn ensure_project(
    conn: &Connection,
    root_path: &str,
    name_override: Option<&str>,
) -> Result<EnsureProjectResult, ProjectError> {
    let path = Path::new(root_path);

    // Reject nonexistent paths early with a clear error.
    if !path.exists() {
        return Err(ProjectError::PathNotFound(root_path.to_owned()));
    }

    let canonical = path
        .canonicalize()
        .map_err(ProjectError::CanonicalizeFailed)?;
    let canonical_str = canonical.to_string_lossy();

    // Try to find by canonicalized path (UNIQUE constraint).
    if let Some(existing) = find_by_root_path(conn, &canonical_str)? {
        return Ok(EnsureProjectResult {
            project: existing,
            created: false,
        });
    }

    // New project: default name to directory name or take the override.
    let base_name = name_override
        .map(str::to_owned)
        .or_else(|| {
            canonical
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "project".to_owned());

    let deduped_name = dedupe_name(conn, &base_name)?;

    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO projects (id, name, root_path) VALUES (?1, ?2, ?3)",
        rusqlite::params![id, deduped_name, canonical_str.as_ref()],
    )?;

    let project = Project {
        id,
        name: deduped_name,
        root_path: canonical_str.to_string(),
        created_at: now_iso8601(),
        archived: false,
    };

    Ok(EnsureProjectResult {
        project,
        created: true,
    })
}

/// Create a fresh project directory under `base_dir` for a non-codebase topic
/// (FR-1.2 / UC6 "create a folder for me"), then map it as a project.
///
/// `name` is the human topic (e.g. "Distributed Systems"); it is slugified for
/// the directory name and deduped against what already exists on disk under
/// `base_dir` (`slug`, `slug-2`, `slug-3`, …) so a fresh call always makes a
/// new directory. The directory is created (`mkdir -p`), then the existing
/// [`ensure_project`] canonicalization path maps it, with the human `name` as
/// the project's display name (itself deduped in the DB by `ensure_project`).
///
/// Because `ensure_project` keeps one project per canonical path and this dir
/// is brand-new, the result is always a freshly-created project — never a
/// collision with an existing mapping. Dir-name dedupe and project-name dedupe
/// are independent: the on-disk slug avoids clobbering a sibling topic folder,
/// while the DB name dedupe keeps the sidebar labels distinct.
pub fn create_auto_project(
    conn: &Connection,
    base_dir: &Path,
    name: &str,
) -> Result<EnsureProjectResult, ProjectError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ProjectError::EmptyName);
    }

    let dir = dedupe_dir(base_dir, &slugify(name));
    std::fs::create_dir_all(&dir).map_err(ProjectError::MkdirFailed)?;

    let dir_str = dir.to_string_lossy();
    ensure_project(conn, &dir_str, Some(name))
}

/// Pick a not-yet-existing directory under `base_dir`: `base_slug`, then
/// `base_slug-2`, `base_slug-3`, … The filesystem is the integrity backstop
/// (`create_dir_all` on a fresh path); this loop keeps two auto-created topics
/// with the same slug from landing in the same folder.
fn dedupe_dir(base_dir: &Path, base_slug: &str) -> PathBuf {
    let first = base_dir.join(base_slug);
    if !first.exists() {
        return first;
    }
    let mut suffix = 2;
    loop {
        let candidate = base_dir.join(format!("{base_slug}-{suffix}"));
        if !candidate.exists() {
            return candidate;
        }
        suffix += 1;
    }
}

/// Turn a human topic name into a filesystem-safe directory slug: lowercase
/// ASCII alphanumerics, runs of anything else collapsed to a single hyphen, no
/// leading/trailing hyphen, capped length. Falls back to `"project"` when the
/// name reduces to nothing (all punctuation / non-Latin script).
///
/// ASCII-only and dependency-free — the same approach as `threads::slugify`,
/// kept local (a small, private helper) so the project-directory and
/// thread-folder slug rules stay independently owned rather than coupling the
/// two domain modules through a shared util for ~20 lines.
fn slugify(name: &str) -> String {
    const MAX_SLUG_LEN: usize = 80;

    let mut slug = String::new();
    let mut pending_sep = false;

    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_sep && !slug.is_empty() {
                slug.push('-');
            }
            pending_sep = false;
            slug.push(ch.to_ascii_lowercase());
        } else {
            pending_sep = true;
        }
    }

    if slug.len() > MAX_SLUG_LEN {
        slug.truncate(MAX_SLUG_LEN);
        while slug.ends_with('-') {
            slug.pop();
        }
    }

    if slug.is_empty() {
        slug.push_str("project");
    }

    slug
}

/// Find a project by its canonicalized root_path (the UNIQUE column).
fn find_by_root_path(conn: &Connection, root_path: &str) -> Result<Option<Project>, ProjectError> {
    conn.query_row(
        "SELECT id, name, root_path, created_at, archived FROM projects WHERE root_path = ?1",
        [root_path],
        |row| {
            Ok(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                root_path: row.get(2)?,
                created_at: row.get(3)?,
                archived: row.get::<_, i64>(4)? != 0,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// Dedupe a project name: if `base_name` is taken, try `base_name-2`,
/// `base_name-3`, ... until finding an unused name.
fn dedupe_name(conn: &Connection, base_name: &str) -> Result<String, ProjectError> {
    let mut candidate = base_name.to_owned();
    let mut suffix = 2;

    loop {
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM projects WHERE name = ?1",
                [&candidate],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);

        if !exists {
            return Ok(candidate);
        }

        candidate = format!("{}-{}", base_name, suffix);
        suffix += 1;
    }
}

/// Item for list_projects with thread count + last activity.
#[derive(Debug, Clone)]
pub struct ProjectWithStats {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub created_at: String,
    pub archived: bool,
    pub thread_count: i64,
    pub last_activity: String,
}

/// List all projects with thread counts and last activity.
/// Excludes archived by default; pass `include_archived: true` to include them.
pub fn list_projects(
    conn: &Connection,
    include_archived: bool,
) -> Result<Vec<ProjectWithStats>, ProjectError> {
    let query = if include_archived {
        "
        SELECT
            p.id,
            p.name,
            p.root_path,
            p.created_at,
            p.archived,
            COALESCE(COUNT(t.id), 0) AS thread_count,
            COALESCE(MAX(t.updated_at), p.created_at) AS last_activity
        FROM projects p
        LEFT JOIN threads t ON t.project_id = p.id
        GROUP BY p.id
        ORDER BY last_activity DESC
        "
    } else {
        "
        SELECT
            p.id,
            p.name,
            p.root_path,
            p.created_at,
            p.archived,
            COALESCE(COUNT(t.id), 0) AS thread_count,
            COALESCE(MAX(t.updated_at), p.created_at) AS last_activity
        FROM projects p
        LEFT JOIN threads t ON t.project_id = p.id
        WHERE p.archived = 0
        GROUP BY p.id
        ORDER BY last_activity DESC
        "
    };

    let mut stmt = conn.prepare(query)?;
    let rows = stmt.query_map([], |row| {
        Ok(ProjectWithStats {
            id: row.get(0)?,
            name: row.get(1)?,
            root_path: row.get(2)?,
            created_at: row.get(3)?,
            archived: row.get::<_, i64>(4)? != 0,
            thread_count: row.get(5)?,
            last_activity: row.get(6)?,
        })
    })?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Rename a project.
pub fn rename_project(conn: &Connection, id: &str, new_name: &str) -> Result<(), ProjectError> {
    let rows_affected = conn.execute(
        "UPDATE projects SET name = ?1 WHERE id = ?2",
        rusqlite::params![new_name, id],
    )?;

    if rows_affected == 0 {
        return Err(ProjectError::NotFound(id.to_owned()));
    }

    Ok(())
}

/// Archive or unarchive a project.
pub fn set_archived(conn: &Connection, id: &str, archived: bool) -> Result<(), ProjectError> {
    let archived_int = if archived { 1 } else { 0 };
    let rows_affected = conn.execute(
        "UPDATE projects SET archived = ?1 WHERE id = ?2",
        rusqlite::params![archived_int, id],
    )?;

    if rows_affected == 0 {
        return Err(ProjectError::NotFound(id.to_owned()));
    }

    Ok(())
}

/// Return true if a project with `id` exists. Used by the `open` endpoint
/// (§5.2) to validate a `--project` target before focusing/navigating (→ 404
/// when absent).
pub fn project_exists(conn: &Connection, id: &str) -> Result<bool, ProjectError> {
    let exists = conn
        .query_row("SELECT 1 FROM projects WHERE id = ?1", [id], |_| Ok(()))
        .optional()?
        .is_some();
    Ok(exists)
}

/// Current timestamp as ISO-8601 UTC (matches the DB's strftime default).
fn now_iso8601() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dedupe_name_logic() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE projects (id TEXT PRIMARY KEY, name TEXT NOT NULL, root_path TEXT UNIQUE)",
            [],
        )
        .unwrap();

        // First call: "myproj" available.
        let name1 = dedupe_name(&conn, "myproj").unwrap();
        assert_eq!(name1, "myproj");

        // Claim it.
        conn.execute(
            "INSERT INTO projects (id, name, root_path) VALUES ('1', 'myproj', '/a')",
            [],
        )
        .unwrap();

        // Second call: "myproj" taken → "myproj-2".
        let name2 = dedupe_name(&conn, "myproj").unwrap();
        assert_eq!(name2, "myproj-2");

        conn.execute(
            "INSERT INTO projects (id, name, root_path) VALUES ('2', 'myproj-2', '/b')",
            [],
        )
        .unwrap();

        // Third call: both taken → "myproj-3".
        let name3 = dedupe_name(&conn, "myproj").unwrap();
        assert_eq!(name3, "myproj-3");
    }

    #[test]
    fn slugify_is_filesystem_safe_with_project_fallback() {
        assert_eq!(slugify("Distributed Systems"), "distributed-systems");
        assert_eq!(slugify("  Music Theory!  "), "music-theory");
        assert_eq!(slugify("a/b\\c"), "a-b-c");
        // Non-Latin / all-punctuation collapse to the "project" fallback (not
        // "thread", so the projects and threads slug rules stay distinct).
        assert_eq!(slugify("你好"), "project");
        assert_eq!(slugify("***"), "project");
        let s = slugify("Weird::Name");
        assert!(!s.contains('/') && !s.contains(' '));
        assert!(!s.starts_with('-') && !s.ends_with('-'));
    }

    /// Projects table matching the shipped schema columns `ensure_project`
    /// reads back (id, name, root_path, created_at, archived).
    fn projects_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE projects (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                root_path TEXT UNIQUE,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                archived INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )
        .unwrap();
        conn
    }

    fn unique_base(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "conceptify-autoproj-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn create_auto_project_makes_dir_dedupes_and_maps() {
        let conn = projects_conn();
        let base = unique_base("make");

        // First call: slug dir created + project mapped under the human name.
        let r1 = create_auto_project(&conn, &base, "Distributed Systems").unwrap();
        assert!(r1.created);
        assert_eq!(r1.project.name, "Distributed Systems");
        assert!(base.join("distributed-systems").is_dir());
        assert!(r1.project.root_path.ends_with("distributed-systems"));

        // Second call, same name: the DIR dedupes to `-2` and a distinct fresh
        // project is created (one project per canonical path); the NAME dedupes
        // in the DB independently.
        let r2 = create_auto_project(&conn, &base, "Distributed Systems").unwrap();
        assert!(r2.created);
        assert_ne!(r1.project.id, r2.project.id);
        assert!(base.join("distributed-systems-2").is_dir());
        assert_eq!(r2.project.name, "Distributed Systems-2");
        assert!(r2.project.root_path.ends_with("distributed-systems-2"));

        // Empty / whitespace name is rejected before touching the filesystem.
        assert!(matches!(
            create_auto_project(&conn, &base, "   ").unwrap_err(),
            ProjectError::EmptyName
        ));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn create_auto_project_reuses_existing_mapping_for_same_dir() {
        // If the slug dir already exists AND is already mapped, dedupe makes a
        // fresh sibling dir rather than colliding — but pointing ensure_project
        // at an already-mapped canonical path lands on the existing project.
        let conn = projects_conn();
        let base = unique_base("reuse");
        std::fs::create_dir_all(&base).unwrap();

        let dir = base.join("topic");
        std::fs::create_dir_all(&dir).unwrap();
        let first = ensure_project(&conn, &dir.to_string_lossy(), Some("Topic")).unwrap();
        assert!(first.created);

        // ensure_project on the same canonical path → existing project, no error.
        let again = ensure_project(&conn, &dir.to_string_lossy(), Some("Topic")).unwrap();
        assert!(!again.created);
        assert_eq!(again.project.id, first.project.id);

        let _ = std::fs::remove_dir_all(&base);
    }
}
