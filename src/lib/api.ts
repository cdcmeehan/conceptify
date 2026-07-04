// Typed wrappers over the app-shell `#[tauri::command]`s (src-tauri/src/commands.rs).
//
// The shell talks to the Rust core through Tauri's `invoke` (the M0-established
// pattern, alongside `listen` for events) rather than the embedded HTTP API:
// the webview is cross-origin to 127.0.0.1 so the bearer-auth'd routes fail CORS
// preflight, and only Rust can read the token/port files or stat the filesystem
// for the "missing directory" badge. The commands use `rename_all = "snake_case"`,
// so argument keys here are snake_case and match the Rust parameter + DB field
// names exactly.
//
// Types are declared locally on purpose — `crates/conceptify-types` is owned by a
// parallel worker and must not be imported from the frontend.

import { invoke } from "@tauri-apps/api/core";

export type ThreadStatus = "generating" | "ready" | "updating" | "error";

export interface Project {
  id: string;
  name: string;
  root_path: string;
  /** Whether `root_path` still resolves on disk (drives the FR-1.3 badge). */
  root_exists: boolean;
  created_at: string;
  archived: boolean;
  thread_count: number;
  last_activity: string;
}

export interface Thread {
  id: string;
  project_id: string;
  title: string;
  slug: string;
  initial_question: string;
  status: ThreadStatus;
  created_at: string;
  updated_at: string;
  open_comment_count: number;
}

export function listProjects(includeArchived: boolean): Promise<Project[]> {
  return invoke<Project[]>("list_projects", { include_archived: includeArchived });
}

export function listThreads(projectId: string): Promise<Thread[]> {
  return invoke<Thread[]>("list_threads", { project_id: projectId });
}

export function renameProject(id: string, name: string): Promise<void> {
  return invoke<void>("rename_project", { id, name });
}

export function setProjectArchived(id: string, archived: boolean): Promise<void> {
  return invoke<void>("set_project_archived", { id, archived });
}

export function remapProject(id: string, rootPath: string): Promise<void> {
  return invoke<void>("remap_project", { id, root_path: rootPath });
}

/** One saved artifact version (FR-2.4). Lists come back ascending by version. */
export interface ArtifactVersion {
  version: number;
  created_at: string;
  /** `initial` (v1) or `follow_up` (v2+). */
  created_by: string;
}

export function listArtifactVersions(threadId: string): Promise<ArtifactVersion[]> {
  return invoke<ArtifactVersion[]>("list_artifact_versions", { thread_id: threadId });
}

export type CommentStatus = "open" | "answered" | "applied";

/**
 * A comment on an artifact version (PRD §7.4, FR-4.1–FR-4.5). Mirrors the Rust
 * `CommentDto` / documented `CommentResponse` shape. `anchor` is the FR-4.4
 * anchor JSON (captured by the bridge, stored verbatim) or `null` for a direct
 * follow-up question (94m.5). Types are declared locally on purpose — see the
 * module header (`crates/conceptify-types` is not imported from the frontend).
 */
export interface Comment {
  id: string;
  thread_id: string;
  artifact_version: number;
  /** FR-4.4 anchor; `null` for a direct follow-up (94m.5). Opaque here — the
   *  bridge (`src/lib/bridge.ts`) owns the typed anchor shapes. */
  anchor: Record<string, unknown> | null;
  body: string;
  status: CommentStatus;
  answer_html: string | null;
  anchor_state: "anchored" | "moved";
  created_at: string;
  resolved_at: string | null;
}

/**
 * Create a comment against the artifact version currently in the viewer
 * (FR-4.1/4.2/4.3). `anchor` is the bridge-captured FR-4.4 anchor, or `null`
 * for a direct follow-up. The target thread and `artifactVersion` must already
 * exist (a comment always anchors to a saved version). Resolves to the created
 * comment (status `open`, `anchored`), whose id/anchor drive the immediate
 * in-artifact highlight.
 */
export function createComment(input: {
  threadId: string;
  artifactVersion: number;
  anchor: Record<string, unknown> | null;
  body: string;
}): Promise<Comment> {
  return invoke<Comment>("create_comment", {
    thread_id: input.threadId,
    artifact_version: input.artifactVersion,
    anchor: input.anchor,
    body: input.body,
  });
}

/** List a thread's comments, oldest first (FR-4.5). `status` optionally filters
 *  to one state; omit for all. */
export function listComments(threadId: string, status?: CommentStatus): Promise<Comment[]> {
  return invoke<Comment[]>("list_comments", { thread_id: threadId, status: status ?? null });
}

/**
 * Open the thread's on-disk `artifact.html` with the system default browser
 * (FR-2.5). Path resolution happens entirely in Rust — the frontend never
 * constructs filesystem paths. Resolves to the opened path.
 */
export function openArtifactInBrowser(threadId: string): Promise<string> {
  return invoke<string>("open_artifact_in_browser", { thread_id: threadId });
}
