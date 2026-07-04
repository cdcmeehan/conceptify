//! Projects domain logic (PRD §7.1, FR-1.1, FR-1.3).
//!
//! Implements ensure-project (canonicalize path → find or create with deduped
//! name), list (with thread counts + last activity), rename, and archive.

use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

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
}
