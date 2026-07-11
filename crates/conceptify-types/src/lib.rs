//! Shared request/response types for the Conceptify HTTP API.
//!
//! This crate defines types used by both the server (src-tauri) and CLI
//! (conceptify-cli), avoiding duplication and keeping the contract in one
//! place.

use serde::{Deserialize, Serialize};

// Artifact version diffing (bead conceptify-3nn.1)

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactDiffKind {
    Unchanged,
    Modified,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextDiffKind {
    Equal,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextDiffHunk {
    pub kind: TextDiffKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactBlockDiff {
    /// `null` is the synthetic document fallback for changed visible text that
    /// lives outside every `data-cfy-id` block.
    pub cfy_id: Option<String>,
    pub kind: ArtifactDiffKind,
    /// Reordering is orthogonal to text classification. Insertion/deletion does
    /// not mark every later block moved; this is computed from the common-id LCS.
    pub moved: bool,
    pub old_index: Option<usize>,
    pub new_index: Option<usize>,
    pub previous_cfy_id: Option<String>,
    pub next_cfy_id: Option<String>,
    pub old_text: Option<String>,
    pub new_text: Option<String>,
    pub hunks: Vec<TextDiffHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactVersionDiffResponse {
    pub thread_id: String,
    pub from_version: i64,
    pub to_version: i64,
    /// Only changed/moved blocks. Identical versions therefore return `[]`.
    pub changes: Vec<ArtifactBlockDiff>,
    pub unchanged_count: usize,
    /// True when duplicate ids or id-less changed content required a fallback.
    pub degraded: bool,
    pub warnings: Vec<String>,
}

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
    /// When set, this is a threaded **reply** (epic conceptify-6xi): it attaches
    /// under the ROOT comment named here, in the same thread. A reply carries no
    /// `anchor` (rejected if present), inherits its parent's `artifact_version`
    /// (the supplied `artifact_version` is ignored), and — when its root was
    /// already answered/applied — re-opens that root. `null`/omitted creates a
    /// normal root comment. Chains are linear: the parent must itself be a root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}

/// A comment as returned by the API (the create response, and each list item).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentResponse {
    pub id: String,
    pub thread_id: String,
    /// The ROOT comment this is a reply to (epic conceptify-6xi), or `null` for a
    /// root comment. Chains are linear, so a reply's `parent_id` always names a
    /// root. Replies carry no anchor and inherit their parent's `artifact_version`.
    pub parent_id: Option<String>,
    pub artifact_version: i64,
    /// The stored anchor JSON (see `Anchor`), or `null` for a direct follow-up or
    /// any reply.
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

// Thread context API type (PRD §5.2 `get-context`, §5.5)
//
// The one-round-trip aggregate a headless follow-up run needs: the thread, its
// project, the latest artifact on disk, and the open comments (anchors carried
// verbatim). Serves both the `get-context` CLI and internal server-side prompt
// assembly (bead `conceptify-b12.2`), so it lives in shared types.

/// Response from `GET /api/v1/threads/:id/context`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadContextResponse {
    pub thread: ThreadContextThread,
    pub project: ThreadContextProject,
    /// The highest artifact version on disk, or `null` when the thread has no
    /// artifact yet (still `generating`).
    pub latest_artifact: Option<ThreadContextArtifact>,
    /// Open ROOT comments, oldest first — the questions the run must answer. Each
    /// carries its `anchor` verbatim (see `Anchor`) and its ordered `replies`
    /// chain nested (the exchange history a follow-up run builds on). See
    /// `ThreadContextComment`.
    pub open_comments: Vec<ThreadContextComment>,
}

/// One open ROOT comment plus its ordered reply chain, as nested under
/// `open_comments` in the get-context aggregate (epic conceptify-6xi). The root's
/// own fields are **flattened** in at the top level — so a JSON entry is exactly a
/// `CommentResponse` with a `replies` array appended — and `replies` is the linear
/// chain oldest-first (ordered `created_at`, then rowid). This is the exchange
/// history (original question + its prior answer + follow-up replies) that lets a
/// follow-up run answer build on what came before.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadContextComment {
    #[serde(flatten)]
    pub comment: CommentResponse,
    pub replies: Vec<CommentResponse>,
}

/// The thread fields a run needs for prompt assembly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadContextThread {
    pub id: String,
    pub title: String,
    /// The question that seeded the thread's initial artifact.
    pub initial_question: String,
    /// One of `generating` | `ready` | `updating` | `error`.
    pub status: String,
    /// Filesystem-safe artifact-folder slug (§5.6).
    pub slug: String,
}

/// The owning project's identity and on-disk root (the agent's `cwd`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadContextProject {
    pub id: String,
    pub name: String,
    pub root_path: String,
}

/// The latest artifact version and the absolute path of its immutable
/// `artifact.vN.html` file on disk (the file a run reads and edits).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadContextArtifact {
    pub version: i64,
    pub file_path: String,
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

// Model catalog API types (epic conceptify-e7m, bead e7m.6)
//
// The live, auto-refreshing model catalog the model-selection UI (settings
// dropdowns e7m.3, point-of-ask picker e7m.4) and execution routing (e7m.7)
// build on. Sourced from LiteLLM's model_prices_and_context_window.json and
// OpenRouter's public /api/v1/models, normalized in `src-tauri/src/catalog.rs`.
// Shared here so the CLI/frontend consume the same shape. All camelCase.

/// One normalized model in the catalog. `id` is the **execution id**: the value
/// handed to the agent — a bare native id (`claude-sonnet-5`, `gpt-5`) for the
/// claude/codex routes, or an OpenRouter model slug (`google/gemini-3-pro`) for
/// the OpenRouter route. `provider` is the model **family** used for the
/// provider-suite toggles (`anthropic`, `openai`, `google`, `mistralai`, ...),
/// derived from the source — never a backend-routing name like `bedrock`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    pub id: String,
    pub provider: String,
    /// Human-readable label for pickers (OpenRouter's `name`, else the id).
    pub display_name: String,
    /// Max input context window in tokens, when the source reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// True when this exact id is listed by OpenRouter's public `/models` — i.e.
    /// runnable via the OpenRouter route. Bead e7m.7 consumes this to pick the
    /// execution route (anthropic->claude CLI, openai->codex CLI, else->OpenRouter).
    pub openrouter_runnable: bool,
}

/// One provider family with its total model count and whether it is currently
/// enabled. Powers the settings suite toggles: `model_count` is over the WHOLE
/// catalog (so a disabled family still shows e.g. "google (29)").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogProvider {
    pub provider: String,
    pub model_count: usize,
    pub enabled: bool,
}

/// Response for `GET /api/v1/catalog/models`, its `POST .../refresh` sibling, and
/// the matching Tauri commands: the models filtered to the enabled providers,
/// the full provider list with counts, when the catalog was fetched, and where
/// the served copy came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogResponse {
    /// RFC3339 timestamp the served catalog was fetched from the network (the
    /// bundled snapshot carries its build-time stamp).
    pub fetched_at: String,
    /// Where the served catalog came from: `live` (fetched this call), `cache`
    /// (disk cache), or `snapshot` (bundled offline fallback).
    pub source: String,
    /// Chat-capable models whose provider is enabled, sorted by provider then id.
    pub models: Vec<CatalogModel>,
    /// Every provider in the full catalog, with counts + enabled flag.
    pub providers: Vec<CatalogProvider>,
}

// Full-text search API (epic conceptify-7x3).

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchHitKind {
    Project,
    Thread,
    Artifact,
    Comment,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub kind: SearchHitKind,
    pub entity_id: String,
    pub project_id: String,
    pub thread_id: Option<String>,
    pub artifact_version: Option<i64>,
    pub block_id: Option<String>,
    pub title: String,
    /// Safe marker-delimited text (`<mark>…</mark>` generated by SQLite).
    pub snippet: String,
    pub rank: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub projects: Vec<SearchHit>,
    pub threads: Vec<SearchHit>,
    pub artifacts: Vec<SearchHit>,
    pub comments: Vec<SearchHit>,
}
