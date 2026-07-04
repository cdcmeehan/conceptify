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

// Artifacts API types (PRD §7.3 FR-3.6, §5.6)

/// One validation-rule outcome from `save-artifact`. `code` is a stable rule
/// identifier from docs/artifact-spec.md §8 (`E-*` = hard failure, `W-*` =
/// warning); `message` is human-readable detail. The CLI prints warnings to
/// stderr as `warning: <code>: <message>` lines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactIssue {
    pub code: String,
    pub message: String,
}

/// Success response from `POST /api/v1/threads/{thread_id}/artifact`. The
/// request body is the raw artifact HTML bytes (not JSON) — see docs/api.md.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveArtifactResponse {
    pub thread_id: String,
    pub project_id: String,
    /// The server-assigned version (v1, v2, ...) — authoritative over the
    /// file's `cfy:version` meta.
    pub version: i64,
    /// `initial` (version 1) or `follow_up` (version ≥ 2); inferred
    /// server-side, never caller-supplied.
    pub created_by: String,
    /// Absolute path of the stored `artifact.vN.html` on disk.
    pub file_path: String,
    /// Spec §8.2 warnings — the artifact was stored despite these.
    pub warnings: Vec<ArtifactIssue>,
}

/// Error body when `save-artifact` rejects the file (spec §8.1 hard
/// failures). `error`/`code` carry the first violation (the shape the spec
/// promises); `errors` lists every violated rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveArtifactErrorResponse {
    pub error: String,
    pub code: String,
    #[serde(default)]
    pub errors: Vec<ArtifactIssue>,
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

// Comments API types (PRD §7.4, FR-4.1–FR-4.5/4.7)
//
// The anchor model (FR-4.4) is the load-bearing contract shared by the
// in-artifact bridge (`conceptify-94m.1`), the re-attachment logic
// (`conceptify-94m.7`), and the headless follow-up agents (M5). It is
// documented as prose in docs/api.md; the `Anchor` types below are its
// canonical machine-readable definition. Field naming is snake_case throughout,
// matching the rest of the API (the JS bridge emits snake_case too).

/// A W3C-Web-Annotation-style text-quote selector: the exact target text plus a
/// little surrounding context. It is the FR-4.4 **fallback** anchor — used to
/// re-locate a comment after the artifact is edited when the primary
/// `data-cfy-id` anchor no longer resolves. `prefix`/`suffix` disambiguate a
/// repeated `exact` string; both are optional (absent at document edges).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextQuote {
    /// The exact selected/target text.
    pub exact: String,
    /// A short run of text immediately preceding `exact` (context for
    /// disambiguation during re-attachment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// A short run of text immediately following `exact`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
}

/// A text-selection anchor (FR-4.1): the user highlighted a run of prose.
///
/// **Primary** anchor = the nearest ancestor `data-cfy-id` (`cfy_id`) plus the
/// `start`/`end` character offsets of the selection within that element's
/// normalized text content. **Fallback** = `quote`. When the selection has no
/// id-bearing ancestor, `cfy_id`/`start`/`end` are omitted and `quote` is the
/// sole anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextAnchor {
    /// Anchor schema version. Currently `1`.
    pub v: u32,
    /// `data-cfy-id` of the nearest id-bearing ancestor of the selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cfy_id: Option<String>,
    /// Selection start offset within `cfy_id`'s normalized text content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<u32>,
    /// Selection end offset within `cfy_id`'s normalized text content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<u32>,
    /// Text-quote fallback (always captured).
    pub quote: TextQuote,
}

/// A diagram/heading element anchor (FR-4.2): the user clicked a whole
/// `data-cfy-id`-bearing element.
///
/// **Primary** anchor = that `cfy_id`. Optional `quote` (the element's text
/// content) is a re-attachment fallback if the id later disappears — omitted
/// for purely graphical nodes that carry no text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElementAnchor {
    /// Anchor schema version. Currently `1`.
    pub v: u32,
    /// The clicked element's `data-cfy-id`.
    pub cfy_id: String,
    /// Optional text-quote fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<TextQuote>,
}

/// The FR-4.4 anchor model, stored as JSON on a comment. Internally tagged by
/// `type`; each variant carries a `v` schema version for forward compatibility.
/// A `null` anchor (the absence of this object) models a direct follow-up
/// question (FR-4.3) — it flows through the identical comment machinery.
///
/// The server stores a submitted anchor **verbatim** (so the bridge may add
/// capture hints as extra fields without a server change) after validating it
/// deserializes into one of these variants with a supported `v`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Anchor {
    Text(TextAnchor),
    Element(ElementAnchor),
}

/// Request to create a comment (PRD FR-4.1–FR-4.3).
///
/// `anchor` is `null` for a direct follow-up question (FR-4.3) and otherwise an
/// `Anchor` object (validated against the `Anchor` schema, then stored
/// verbatim). `artifact_version` is the version the anchor was captured
/// against; an artifact of that version must already exist for the thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateCommentRequest {
    pub thread_id: String,
    pub artifact_version: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<serde_json::Value>,
    pub body: String,
}

/// A comment as returned by the API (the create response, and each list item).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentResponse {
    pub id: String,
    pub thread_id: String,
    pub artifact_version: i64,
    /// The stored anchor JSON (see `Anchor`), or `null` for a direct follow-up.
    pub anchor: Option<serde_json::Value>,
    pub body: String,
    /// One of `open` | `answered` | `applied`.
    pub status: String,
    /// The agent's resolution, rendered HTML/markdown (FR-4.5); `null` until
    /// resolved.
    pub answer_html: Option<String>,
    /// One of `anchored` | `moved`. `moved` is FR-4.4's "reference moved" flag,
    /// driven by the re-attachment bead (`conceptify-94m.7`); `anchored` until
    /// then.
    pub anchor_state: String,
    pub created_at: String,
    /// When the comment first left `open` (was answered/applied); `null` while
    /// still open.
    pub resolved_at: Option<String>,
}

/// Response from list-comments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListCommentsResponse {
    pub comments: Vec<CommentResponse>,
}

/// Request to update a comment (PRD FR-4.6/4.7; drives the M5 `resolve-comment`
/// CLI). Every field is optional — supply the subset to change; an empty body
/// is rejected. `status` transitions are validated: status may only **advance**
/// `open` → `answered` → `applied`, never regress. `anchor_state` is driven by
/// the re-attachment bead (FR-4.4) and is independent of the status machine.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateCommentRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_html: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_state: Option<String>,
}
