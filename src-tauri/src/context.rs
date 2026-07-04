//! Thread run-context aggregation (PRD §5.2 `get-context`, §5.5).
//!
//! Composes, in one connection lock, everything a headless follow-up run needs
//! to answer a thread's open comments without touching the DB directly: the
//! thread, its owning project, the latest artifact on disk, and the open
//! comments (anchors carried verbatim). One round-trip for the whole prompt.
//!
//! Deliberately a plain domain function over `&Connection` (not tied to the
//! HTTP layer) so it serves **both** the `get-context` CLI path (via
//! `server::threads_routes`) and internal server-side prompt assembly (bead
//! `conceptify-b12.2`, the headless spawner), keeping a single source of truth
//! for what "context" means.

use rusqlite::Connection;

use crate::artifacts::{self, LatestArtifact};
use crate::comments::{self, Comment, CommentStatus};
use crate::threads::{self, Thread};

/// The owning project's identity and on-disk root (the run's `cwd`).
#[derive(Debug, Clone)]
pub struct ProjectRef {
    pub id: String,
    pub name: String,
    pub root_path: String,
}

/// The aggregated run context for a thread. Reusable directly from Rust; the
/// route layer maps it to `conceptify_types::ThreadContextResponse`.
#[derive(Debug, Clone)]
pub struct ThreadContext {
    pub thread: Thread,
    pub project: ProjectRef,
    /// The highest artifact version on disk, or `None` when the thread has none
    /// yet (still `generating`).
    pub latest_artifact: Option<LatestArtifact>,
    /// Open comments only, oldest first — the questions the run must answer.
    pub open_comments: Vec<Comment>,
}

/// Errors from the aggregation. `ThreadNotFound` maps to a 404 in the route
/// layer; the rest are internal (500).
#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error("thread not found: {0}")]
    ThreadNotFound(String),

    #[error(transparent)]
    Comments(#[from] comments::CommentError),

    #[error(transparent)]
    Threads(#[from] threads::ThreadError),

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
}

/// Assemble the run context for `thread_id` (PRD §5.2 `get-context`).
///
/// Runs under the caller's single connection lock (via `db::with_conn_result`),
/// so the thread, project, artifact, and comment reads are one consistent
/// snapshot. An unknown thread is `ContextError::ThreadNotFound` (→ 404). The
/// project is resolved from the thread's FK, so it always exists for a valid
/// thread.
pub fn thread_context(conn: &Connection, thread_id: &str) -> Result<ThreadContext, ContextError> {
    let thread = threads::get_thread_opt(conn, thread_id)?
        .ok_or_else(|| ContextError::ThreadNotFound(thread_id.to_owned()))?;

    let project = conn.query_row(
        "SELECT id, name, root_path FROM projects WHERE id = ?1",
        [&thread.project_id],
        |row| {
            Ok(ProjectRef {
                id: row.get(0)?,
                name: row.get(1)?,
                root_path: row.get(2)?,
            })
        },
    )?;

    let latest_artifact = artifacts::latest_artifact(conn, thread_id)?;
    let open_comments = comments::list_comments(conn, thread_id, Some(CommentStatus::Open))?;

    Ok(ThreadContext {
        thread,
        project,
        latest_artifact,
        open_comments,
    })
}
