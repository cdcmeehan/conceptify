//! Shared request/response types for the Conceptify HTTP API.
//!
//! This crate defines types used by both the server (src-tauri) and CLI
//! (conceptify-cli), avoiding duplication and keeping the contract in one
//! place.

use serde::{Deserialize, Serialize};

/// Response shape for `GET /health` (unauthenticated, mirrored at
/// `/api/v1/health`).
///
/// Used by the CLI's launch-and-wait contract (probe → spawn if unhealthy →
/// poll until ready) and by the server's occupant-detection logic when a port
/// is already taken.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub service: String,
    pub status: String,
    pub version: String,
}

// Projects API types (PRD §7.1, FR-1.1, FR-1.3)

/// Request to ensure-project or create a project explicitly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsureProjectRequest {
    /// Root directory path. Will be canonicalized; symlinks and trailing slashes
    /// resolve to one identity. Must exist on disk.
    pub root_path: String,
    /// Optional name override. If omitted, defaults to directory name,
    /// deduped with numeric suffix if another project uses it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Response from ensure-project or create.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsureProjectResponse {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub created_at: String,
    pub archived: bool,
    /// True if this call created a new project; false if it already existed.
    pub created: bool,
}

/// One project in a list response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectListItem {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub created_at: String,
    pub archived: bool,
    /// Number of threads in this project.
    pub thread_count: i64,
    /// Most recent activity (max(threads.updated_at) or project.created_at).
    pub last_activity: String,
}

/// Response from list projects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListProjectsResponse {
    pub projects: Vec<ProjectListItem>,
}

/// Request to rename a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenameProjectRequest {
    pub name: String,
}

/// Request to archive or unarchive a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveProjectRequest {
    /// True to archive, false to unarchive.
    pub archived: bool,
}

// Threads API types (PRD §7.2, FR-2.1, FR-2.2)

/// Request to create a thread (PRD FR-2.1). The slug for the artifact folder
/// (§5.6) is derived server-side from `title`, not supplied by the caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateThreadRequest {
    /// The project this thread belongs to.
    pub project_id: String,
    /// Human-readable title; the artifact-folder slug is derived from this.
    pub title: String,
    /// The question that seeds the thread's initial artifact.
    pub initial_question: String,
}

/// Response from create thread. Includes the generated `id` and `slug`
/// (filesystem-safe, unique within the project) that later beads use to lay
/// out the artifact folder under `~/Documents/conceptify/artifacts/...`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateThreadResponse {
    pub id: String,
    pub project_id: String,
    pub title: String,
    /// Filesystem-safe slug, unique within the project (§5.6 folder name).
    pub slug: String,
    pub initial_question: String,
    /// One of `generating` | `ready` | `updating` | `error`. Newly created
    /// threads start `generating` (OQ4: create early, status transitions
    /// owned by save-artifact / run lifecycle in later milestones).
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

/// One thread in a list response (PRD FR-2.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadListItem {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub slug: String,
    pub initial_question: String,
    /// One of `generating` | `ready` | `updating` | `error`.
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    /// Number of comments on this thread still in the `open` state.
    pub open_comment_count: i64,
}

/// Response from list threads, sorted by last activity (most recent first).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListThreadsResponse {
    pub threads: Vec<ThreadListItem>,
}

// Open / focus API types (PRD §5.2 `conceptify open`)

/// Request to focus the app on a project or thread (`POST /api/v1/open`).
///
/// Exactly one of `thread_id` / `project_id` should be set. If both are
/// present the server resolves the more specific `thread_id`; if neither is
/// present it returns `400`. The CLI enforces exactly-one before calling.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

/// Response from the `open` endpoint: confirms the resolved target the app was
/// focused and navigated to. `thread_id` is `null` when opening a project with
/// no specific thread. The same `{project_id, thread_id}` shape is emitted as
/// the `navigate` Tauri event for the frontend to act on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenResponse {
    pub ok: bool,
    pub project_id: String,
    pub thread_id: Option<String>,
}
