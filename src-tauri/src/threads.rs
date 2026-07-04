//! Threads domain logic (PRD §7.2, FR-2.1, FR-2.2).
//!
//! Implements create-thread (title → filesystem-safe, per-project-unique slug;
//! status starts `generating`) and list-threads (per project, sorted by last
//! activity, with status + open-comment counts). Status transitions past the
//! initial `generating` (→ `ready`/`updating`/`error`) are owned by
//! save-artifact and the follow-up run lifecycle in later milestones — this
//! module only defines the enum and sets the initial state.

use rusqlite::{Connection, OptionalExtension};

/// The thread status machine (PRD §4). Only `Generating` is produced here (the
/// initial state on create); the remaining variants are the target states of
/// transitions owned by later milestones (save-artifact, run lifecycle) and
/// are constructed when reading a thread back from the DB (`from_db_str`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadStatus {
    Generating,
    Ready,
    Updating,
    Error,
}

impl ThreadStatus {
    /// The exact text stored in `threads.status` (matches the DB CHECK
    /// constraint in `db::migrations`).
    pub fn as_str(&self) -> &'static str {
        match self {
            ThreadStatus::Generating => "generating",
            ThreadStatus::Ready => "ready",
            ThreadStatus::Updating => "updating",
            ThreadStatus::Error => "error",
        }
    }

    /// Parse the stored text back into the enum. The DB CHECK constraint
    /// guarantees only the four known values are ever persisted, so an unknown
    /// string is unreachable in practice; it falls back to `Generating` rather
    /// than erroring, keeping read paths total.
    pub fn from_db_str(s: &str) -> ThreadStatus {
        match s {
            "ready" => ThreadStatus::Ready,
            "updating" => ThreadStatus::Updating,
            "error" => ThreadStatus::Error,
            _ => ThreadStatus::Generating,
        }
    }
}

/// A thread row (mirrors the schema).
#[derive(Debug, Clone)]
pub struct Thread {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub slug: String,
    pub initial_question: String,
    pub status: ThreadStatus,
    pub created_at: String,
    pub updated_at: String,
}

/// A thread plus the derived stats `list_threads` returns.
#[derive(Debug, Clone)]
pub struct ThreadWithStats {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub slug: String,
    pub initial_question: String,
    pub status: ThreadStatus,
    pub created_at: String,
    pub updated_at: String,
    /// Comments on this thread still in the `open` state.
    pub open_comment_count: i64,
}

/// Errors specific to threads operations. Variants map to HTTP status codes
/// in the route handlers (see server::threads_routes).
#[derive(Debug, thiserror::Error)]
pub enum ThreadError {
    #[error("title must not be empty")]
    EmptyTitle,

    #[error("project not found: {0}")]
    ProjectNotFound(String),

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
}

/// Create a thread (PRD FR-2.1). Derives a filesystem-safe slug from `title`,
/// deduped to be unique within the project, and inserts with status
/// `generating`. Returns the stored row (with DB-generated timestamps).
///
/// The whole function runs under the caller's single connection lock, so the
/// project-existence check, slug dedupe, and insert are one atomic unit — no
/// window for a concurrent create to slip a duplicate slug past the dedupe.
pub fn create_thread(
    conn: &Connection,
    project_id: &str,
    title: &str,
    initial_question: &str,
) -> Result<Thread, ThreadError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(ThreadError::EmptyTitle);
    }

    // Explicit existence check gives a clean 404 instead of an opaque FOREIGN
    // KEY constraint error from the insert.
    let project_exists = conn
        .query_row(
            "SELECT 1 FROM projects WHERE id = ?1",
            [project_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !project_exists {
        return Err(ThreadError::ProjectNotFound(project_id.to_owned()));
    }

    let base_slug = slugify(title);
    let slug = dedupe_slug(conn, project_id, &base_slug)?;

    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO threads (id, project_id, title, slug, initial_question, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            id,
            project_id,
            title,
            slug,
            initial_question,
            ThreadStatus::Generating.as_str()
        ],
    )?;

    get_thread(conn, &id)
}

/// Fetch a single thread by id.
fn get_thread(conn: &Connection, id: &str) -> Result<Thread, ThreadError> {
    conn.query_row(
        "SELECT id, project_id, title, slug, initial_question, status, created_at, updated_at
         FROM threads WHERE id = ?1",
        [id],
        |row| {
            Ok(Thread {
                id: row.get(0)?,
                project_id: row.get(1)?,
                title: row.get(2)?,
                slug: row.get(3)?,
                initial_question: row.get(4)?,
                status: ThreadStatus::from_db_str(&row.get::<_, String>(5)?),
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        },
    )
    .map_err(Into::into)
}

/// List a project's threads (PRD FR-2.2), sorted by last activity
/// (`updated_at`) descending, each with its open-comment count.
///
/// The `comments` table already exists (see `db::migrations`), so the count is
/// a real LEFT JOIN rather than a literal 0 — it stays correct once the
/// comments-backend bead starts inserting rows. An unknown `project_id` simply
/// yields an empty list (mirrors `list_projects`, which does no existence
/// gate); callers list threads for a project they already hold.
pub fn list_threads(
    conn: &Connection,
    project_id: &str,
) -> Result<Vec<ThreadWithStats>, ThreadError> {
    let mut stmt = conn.prepare(
        "
        SELECT
            t.id,
            t.project_id,
            t.title,
            t.slug,
            t.initial_question,
            t.status,
            t.created_at,
            t.updated_at,
            COALESCE(SUM(CASE WHEN c.status = 'open' THEN 1 ELSE 0 END), 0) AS open_comment_count
        FROM threads t
        LEFT JOIN comments c ON c.thread_id = t.id
        WHERE t.project_id = ?1
        GROUP BY t.id
        ORDER BY t.updated_at DESC, t.created_at DESC
        ",
    )?;

    let rows = stmt.query_map([project_id], |row| {
        Ok(ThreadWithStats {
            id: row.get(0)?,
            project_id: row.get(1)?,
            title: row.get(2)?,
            slug: row.get(3)?,
            initial_question: row.get(4)?,
            status: ThreadStatus::from_db_str(&row.get::<_, String>(5)?),
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
            open_comment_count: row.get(8)?,
        })
    })?;

    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Return the id of the project owning `thread_id`, or `None` if no such
/// thread exists. Used by the `open` endpoint (§5.2) to both validate the
/// target thread (→ 404 when absent) and resolve it to its project so the
/// frontend `navigate` event can carry both ids.
pub fn find_thread_project(
    conn: &Connection,
    thread_id: &str,
) -> Result<Option<String>, ThreadError> {
    conn.query_row(
        "SELECT project_id FROM threads WHERE id = ?1",
        [thread_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Transition a thread's status (PRD §4 status machine: generating | ready |
/// updating | error), bumping `updated_at` so the thread rises in the
/// last-activity sort. The caller owns the legality of the transition —
/// save-artifact owns `→ ready` (bead `conceptify-nsy.3`), the run lifecycle
/// owns `→ updating`/`→ error` (later beads). Returns
/// `ThreadError::ProjectNotFound`-style semantics via a plain rusqlite error
/// path: an unknown id simply updates zero rows, which callers that already
/// validated existence (as save-artifact does) can ignore.
pub fn set_thread_status(
    conn: &Connection,
    thread_id: &str,
    status: ThreadStatus,
) -> Result<(), rusqlite::Error> {
    conn.execute(
        "UPDATE threads
         SET status = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE id = ?1",
        rusqlite::params![thread_id, status.as_str()],
    )?;
    Ok(())
}

/// Turn a human title into a filesystem-safe slug: lowercase ASCII
/// alphanumerics, runs of any other character collapsed to a single hyphen,
/// no leading/trailing hyphen, capped at `MAX_SLUG_LEN`.
///
/// Deliberately ASCII-only and dependency-free (no transliteration crate, in
/// keeping with the project's lean deps): non-ASCII characters are dropped, so
/// a title that reduces to nothing (all punctuation/emoji/non-Latin script)
/// falls back to `"thread"`. Per-project uniqueness is handled separately by
/// `dedupe_slug`.
fn slugify(title: &str) -> String {
    const MAX_SLUG_LEN: usize = 80;

    let mut slug = String::new();
    let mut pending_sep = false;

    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            // A separator only becomes a hyphen once it's followed by more
            // content, so trailing separators never produce a trailing hyphen.
            if pending_sep && !slug.is_empty() {
                slug.push('-');
            }
            pending_sep = false;
            slug.push(ch.to_ascii_lowercase());
        } else {
            // Whitespace, punctuation, path separators, non-ASCII: all fold
            // into a single pending separator.
            pending_sep = true;
        }
    }

    if slug.len() > MAX_SLUG_LEN {
        slug.truncate(MAX_SLUG_LEN);
        // Truncation can leave a trailing hyphen; strip it. Slug is pure ASCII
        // here, so byte truncation lands on a char boundary.
        while slug.ends_with('-') {
            slug.pop();
        }
    }

    if slug.is_empty() {
        slug.push_str("thread");
    }

    slug
}

/// If `base` is already used within the project, append `-2`, `-3`, ... until
/// finding a free slug. Mirrors `projects::dedupe_name`. The `(project_id,
/// slug)` UNIQUE index is the integrity backstop; this loop is what actually
/// keeps us off it.
fn dedupe_slug(
    conn: &Connection,
    project_id: &str,
    base: &str,
) -> Result<String, ThreadError> {
    let mut candidate = base.to_owned();
    let mut suffix = 2;

    loop {
        let exists = conn
            .query_row(
                "SELECT 1 FROM threads WHERE project_id = ?1 AND slug = ?2",
                rusqlite::params![project_id, candidate],
                |_| Ok(()),
            )
            .optional()?
            .is_some();

        if !exists {
            return Ok(candidate);
        }

        candidate = format!("{}-{}", base, suffix);
        suffix += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an in-memory DB with the projects + threads + comments schema and
    /// one project, so domain functions can be exercised without the app.
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
                status TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            );
            CREATE UNIQUE INDEX idx_threads_project_slug ON threads(project_id, slug);
            CREATE TABLE comments (
                id TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL,
                artifact_version INTEGER NOT NULL,
                anchor TEXT,
                body TEXT NOT NULL,
                status TEXT NOT NULL,
                answer_html TEXT,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                resolved_at TEXT
            );
            INSERT INTO projects (id, name, root_path) VALUES ('p1', 'Proj One', '/a');
            INSERT INTO projects (id, name, root_path) VALUES ('p2', 'Proj Two', '/b');
            ",
        )
        .unwrap();
        conn
    }

    #[test]
    fn slugify_is_filesystem_safe() {
        assert_eq!(slugify("How does OAuth work?"), "how-does-oauth-work");
        assert_eq!(slugify("  Leading/trailing  "), "leading-trailing");
        assert_eq!(slugify("Multiple   spaces"), "multiple-spaces");
        assert_eq!(slugify("path/to/../thing"), "path-to-thing");
        assert_eq!(slugify("Under_score and-dash"), "under-score-and-dash");
        // Slugs never contain path separators, whitespace, or leading/trailing
        // hyphens, and are always lowercase.
        let s = slugify("!!!Weird::Title!!!");
        assert_eq!(s, "weird-title");
        assert!(!s.contains('/') && !s.contains(' '));
        assert!(!s.starts_with('-') && !s.ends_with('-'));
    }

    #[test]
    fn slugify_falls_back_when_empty() {
        assert_eq!(slugify("你好世界"), "thread");
        assert_eq!(slugify("***"), "thread");
        assert_eq!(slugify(""), "thread");
    }

    #[test]
    fn slugify_caps_length() {
        let long = "a".repeat(200);
        let s = slugify(&long);
        assert_eq!(s.len(), 80);
    }

    #[test]
    fn create_returns_id_and_slug() {
        let conn = test_conn();
        let t = create_thread(&conn, "p1", "How does OAuth work?", "explain oauth").unwrap();
        assert!(!t.id.is_empty());
        assert_eq!(t.slug, "how-does-oauth-work");
        assert_eq!(t.status, ThreadStatus::Generating);
        assert_eq!(t.project_id, "p1");
        assert!(!t.created_at.is_empty());
        assert_eq!(t.created_at, t.updated_at);
    }

    #[test]
    fn same_title_twice_yields_distinct_slugs() {
        let conn = test_conn();
        let a = create_thread(&conn, "p1", "Same Title", "q1").unwrap();
        let b = create_thread(&conn, "p1", "Same Title", "q2").unwrap();
        let c = create_thread(&conn, "p1", "Same Title", "q3").unwrap();
        assert_eq!(a.slug, "same-title");
        assert_eq!(b.slug, "same-title-2");
        assert_eq!(c.slug, "same-title-3");
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn same_slug_allowed_across_projects() {
        let conn = test_conn();
        let a = create_thread(&conn, "p1", "Shared", "q").unwrap();
        let b = create_thread(&conn, "p2", "Shared", "q").unwrap();
        // Uniqueness is scoped per project, so the same slug is fine in another.
        assert_eq!(a.slug, "shared");
        assert_eq!(b.slug, "shared");
    }

    #[test]
    fn create_rejects_empty_title() {
        let conn = test_conn();
        let err = create_thread(&conn, "p1", "   ", "q").unwrap_err();
        assert!(matches!(err, ThreadError::EmptyTitle));
    }

    #[test]
    fn create_rejects_unknown_project() {
        let conn = test_conn();
        let err = create_thread(&conn, "nope", "Title", "q").unwrap_err();
        assert!(matches!(err, ThreadError::ProjectNotFound(_)));
    }

    #[test]
    fn list_is_sorted_by_last_activity_with_open_comment_count() {
        let conn = test_conn();
        let first = create_thread(&conn, "p1", "First", "q").unwrap();
        let second = create_thread(&conn, "p1", "Second", "q").unwrap();
        // Thread in another project must not appear.
        create_thread(&conn, "p2", "Elsewhere", "q").unwrap();

        // Force a distinct, later updated_at on `first` so ordering is
        // deterministic (fresh rows can otherwise share a millisecond stamp).
        conn.execute(
            "UPDATE threads SET updated_at = '2999-01-01T00:00:00.000Z' WHERE id = ?1",
            [&first.id],
        )
        .unwrap();

        // Two open comments + one answered on `second`; the answered one must
        // not be counted.
        for (i, status) in [("c1", "open"), ("c2", "open"), ("c3", "answered")] {
            conn.execute(
                "INSERT INTO comments (id, thread_id, artifact_version, body, status)
                 VALUES (?1, ?2, 1, 'body', ?3)",
                rusqlite::params![i, second.id, status],
            )
            .unwrap();
        }

        let list = list_threads(&conn, "p1").unwrap();
        assert_eq!(list.len(), 2);
        // `first` was bumped to the far future → most recent activity → first.
        assert_eq!(list[0].id, first.id);
        assert_eq!(list[0].open_comment_count, 0);
        assert_eq!(list[1].id, second.id);
        assert_eq!(list[1].open_comment_count, 2);
    }

    #[test]
    fn set_status_updates_status_and_bumps_activity() {
        let conn = test_conn();
        let t = create_thread(&conn, "p1", "Status test", "q").unwrap();
        assert_eq!(t.status, ThreadStatus::Generating);

        // Backdate updated_at so the bump is observable.
        conn.execute(
            "UPDATE threads SET updated_at = '2000-01-01T00:00:00.000Z' WHERE id = ?1",
            [&t.id],
        )
        .unwrap();

        set_thread_status(&conn, &t.id, ThreadStatus::Ready).unwrap();

        let (status, updated_at): (String, String) = conn
            .query_row(
                "SELECT status, updated_at FROM threads WHERE id = ?1",
                [&t.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "ready");
        assert!(updated_at > "2000-01-01T00:00:00.000Z".to_owned());
    }

    #[test]
    fn list_unknown_project_is_empty() {
        let conn = test_conn();
        create_thread(&conn, "p1", "One", "q").unwrap();
        assert!(list_threads(&conn, "ghost").unwrap().is_empty());
    }
}
