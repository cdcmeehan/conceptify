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
