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

export type ResponseDepth = "quick" | "balanced" | "deep";
export type ResponseLanguage = "plain" | "familiar" | "domain_native";
export type ResponseVisuals = "auto" | "prefer" | "avoid";
export type ResponseShape = "auto" | "walkthrough" | "comparison" | "reference";
export type VisualPurpose = "auto" | "compare" | "sequence" | "relationships" | "hierarchy" | "values" | "interactive";

export interface ResponseIntent {
  version: 1;
  depth: ResponseDepth;
  language: ResponseLanguage;
  visuals: ResponseVisuals;
  shape: ResponseShape;
  visual_purpose: VisualPurpose;
}

export interface SkillCapability {
  schema_version: number;
  id: string;
  name: string;
  outcome: string;
  supported_intents: string[];
  context_requirements: string[];
  expected_outputs: string[];
  latency_hint: "fast" | "moderate" | "extended";
  compatible_response_controls: {
    depth: ResponseDepth[];
    language: ResponseLanguage[];
    visuals: ResponseVisuals[];
    shape: ResponseShape[];
  };
  recommendation: {
    terms: string[];
    visual_preference_score: number;
    shape_scores: Partial<Record<ResponseShape, number>>;
    minimum_score: number;
  };
  manual_selectable: boolean;
  availability: { available: boolean; reason: string | null };
}

export interface SkillRecommendation {
  skill: SkillCapability;
  score: number;
  reason: string;
  selected_manually: boolean;
}

export type ResponsePreferenceOrigin = "product" | "user" | "project" | "question";
export interface ResolvedResponsePreferences {
  intent: ResponseIntent;
  origins: Record<"depth" | "language" | "visuals" | "shape" | "visual_purpose", Exclude<ResponsePreferenceOrigin, "question">>;
  user: Partial<Omit<ResponseIntent, "version">>;
  project: Partial<Omit<ResponseIntent, "version">>;
}

export function getResponsePreferences(projectId: string): Promise<ResolvedResponsePreferences> {
  return invoke<ResolvedResponsePreferences>("get_response_preferences", { project_id: projectId });
}

export function saveResponsePreference(
  projectId: string,
  scope: "user" | "project",
  intent: ResponseIntent,
): Promise<ResolvedResponsePreferences> {
  return invoke<ResolvedResponsePreferences>("save_response_preference", {
    project_id: projectId,
    scope,
    intent,
  });
}

export function resetResponsePreference(
  projectId: string,
  scope: "user" | "project",
): Promise<ResolvedResponsePreferences> {
  return invoke<ResolvedResponsePreferences>("reset_response_preference", {
    project_id: projectId,
    scope,
  });
}

export function listSkillCapabilities(): Promise<SkillCapability[]> {
  return invoke<SkillCapability[]>("list_skill_capabilities");
}

export function recommendSkills(
  question: string,
  intent: ResponseIntent,
  selectedSkillIds: string[] = [],
): Promise<SkillRecommendation[]> {
  return invoke<SkillRecommendation[]>("recommend_skills", {
    question,
    intent,
    selected_skill_ids: selectedSkillIds,
  });
}

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
  context: ProjectContextSummary | null;
}

export interface ProjectContextSummary {
  status: "scanning" | "ready" | "limited" | "error";
  repository: string;
  languages: Array<{ name: string; files: number }>;
  included_files: number;
  excluded_paths: string[];
  fingerprint: string;
  scanned_at: string;
  warning: string | null;
  unchanged: boolean;
}

export function scanProjectContext(projectId: string): Promise<ProjectContextSummary> {
  return invoke<ProjectContextSummary>("scan_project_context", { project_id: projectId });
}

export interface TopicContext { notes: string; links: string[]; files: string[]; }
export function getTopicContext(projectId: string): Promise<TopicContext> {
  return invoke<TopicContext>("get_topic_context", { project_id: projectId });
}
export function setTopicContext(projectId: string, context: TopicContext): Promise<TopicContext> {
  return invoke<TopicContext>("set_topic_context", { project_id: projectId, ...context });
}
export function getProjectGoal(projectId: string): Promise<string> {
  return invoke<string>("get_project_goal", { project_id: projectId });
}
export function setProjectGoal(projectId: string, goal: string): Promise<void> {
  return invoke<void>("set_project_goal", { project_id: projectId, goal });
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

/**
 * A project mapped or created via the "New project" affordance (FR-1.2 / UC6).
 * Mirrors the Rust `EnsuredProjectDto` (snake_case `root_path`). `created` is
 * `false` when an existing directory was already mapped (UC6: land on it, don't
 * error).
 */
export interface EnsuredProject {
  id: string;
  name: string;
  root_path: string;
  created: boolean;
}

/** Map an existing directory as a project (native dir-picker path, FR-1.2). */
export function ensureProject(rootPath: string, name?: string | null): Promise<EnsuredProject> {
  return invoke<EnsuredProject>("ensure_project", { root_path: rootPath, name: name ?? null });
}

/** Create a fresh topic folder under the auto-project base dir and map it
 *  (FR-1.2 "create a folder for me"). */
export function createProjectFolder(name: string): Promise<EnsuredProject> {
  return invoke<EnsuredProject>("create_project_folder", { name });
}

/** Delete a thread and all its data (bead conceptify-0kt hygiene): the DB row
 *  (cascading to artifacts/comments/runs) plus its on-disk artifact dir. */
export function deleteThread(threadId: string): Promise<void> {
  return invoke<void>("delete_thread", { thread_id: threadId });
}

/** One saved artifact version (FR-2.4). Lists come back ascending by version. */
export interface ArtifactVersion {
  version: number;
  created_at: string;
  /** `initial` (v1) or `follow_up` (v2+). */
  created_by: string;
  response_intent: ResponseIntent | null;
  skills: Array<{
    id: string;
    name: string;
    capability_version: number;
    selection: "recommended" | "manual";
  }>;
}

export function listArtifactVersions(threadId: string): Promise<ArtifactVersion[]> {
  return invoke<ArtifactVersion[]>("list_artifact_versions", { thread_id: threadId });
}

export type ArtifactDiffKind = "unchanged" | "modified" | "added" | "removed";
export type TextDiffKind = "equal" | "added" | "removed";

export interface TextDiffHunk {
  kind: TextDiffKind;
  text: string;
}

export interface ArtifactBlockDiff {
  cfy_id: string | null;
  kind: ArtifactDiffKind;
  moved: boolean;
  old_index: number | null;
  new_index: number | null;
  previous_cfy_id: string | null;
  next_cfy_id: string | null;
  old_text: string | null;
  new_text: string | null;
  hunks: TextDiffHunk[];
}

export interface ArtifactVersionDiff {
  thread_id: string;
  from_version: number;
  to_version: number;
  changes: ArtifactBlockDiff[];
  unchanged_count: number;
  degraded: boolean;
  warnings: string[];
}

export function diffVersions(
  threadId: string,
  fromVersion: number,
  toVersion: number,
): Promise<ArtifactVersionDiff> {
  return invoke<ArtifactVersionDiff>("diff_versions", {
    thread_id: threadId,
    from_version: fromVersion,
    to_version: toVersion,
  });
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
  /** The root this comment replies to (epic conceptify-6xi threaded replies), or
   *  `null` for a root comment. Chains are linear (reply-to-reply is rejected
   *  server-side), so a non-null `parent_id` always names a root. The sidebar
   *  groups the flat list by this (see `groupComments` in the store). */
  parent_id: string | null;
  artifact_version: number;
  /** FR-4.4 anchor; `null` for a direct follow-up (94m.5) or any reply (replies
   *  inherit the root's version and carry no anchor). Opaque here — the bridge
   *  (`src/lib/bridge.ts`) owns the typed anchor shapes. */
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
 *
 * Pass `parentId` (a root comment's id) to create a threaded reply instead
 * (epic conceptify-6xi): the backend dispatches to the reply path, which ignores
 * `anchor` (replies carry none), inherits the root's `artifact_version`, and
 * re-opens an answered/applied root. The reply composer (bead conceptify-6xi.3)
 * is the caller.
 */
export function createComment(input: {
  threadId: string;
  artifactVersion: number;
  anchor: Record<string, unknown> | null;
  body: string;
  parentId?: string | null;
}): Promise<Comment> {
  return invoke<Comment>("create_comment", {
    thread_id: input.threadId,
    artifact_version: input.artifactVersion,
    anchor: input.anchor,
    body: input.body,
    parent_id: input.parentId ?? null,
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

// ---------------------------------------------------------------------------
// Follow-up runs (PRD FR-4.6/4.7/4.8/4.9 — the interrogation loop)
// ---------------------------------------------------------------------------

/** `answer` = FR-4.6 sidebar answers only; `apply` = FR-4.7 new artifact
 *  version; `ask` = FR-5.1 in-app generation run (authors a thread's first
 *  artifact). */
export type RunMode = "answer" | "apply" | "ask";

/**
 * Per-invocation model/adapter override for a single run (epic conceptify-e7m,
 * beads e7m.1/e7m.4). camelCase to match the Rust `RunOverride` serde
 * (`settings.rs`). The point-of-ask picker only ever sets `model` — the adapter
 * is DERIVED from the model's provider by routing.rs; `adapter` stays available
 * as the backend's advanced escape hatch (bypasses routing) but no UI sets it.
 *
 * Passed as the flow commands' `run_override` argument. It must be sent ONLY
 * when it differs from the settings default: omitting it (or passing `null`)
 * leaves the spawned run override-free, so the run row keeps tracking settings
 * and a later retry re-derives the CURRENT defaults (bead e7m.1). The picker's
 * {@link runOverrideOf} helper enforces "null unless it truly differs".
 */
export interface RunOverride {
  adapter?: string;
  model?: string;
}

/** Terminal + initial run states (docs/api.md "Run events"). `failed` and
 *  `timeout` are the same error class for UI purposes; the distinction only
 *  changes the message. */
export type RunStatus =
  | "queued"
  | "starting"
  | "running"
  | "throttled"
  | "cancelling"
  | "completed"
  | "conflicted"
  | "failed"
  | "cancelled"
  | "timeout";

/**
 * A successfully started flow run. `target_comment_ids` are the comments this
 * run is expected to resolve — the basis for the FR-4.8 per-comment progress
 * ("n of m answered", derived from the store's comment statuses as
 * `comment-updated` events land). Targets are not persisted server-side, so
 * only the starting session has them (see `getActiveRun`).
 */
export interface RunStarted {
  run_id: string;
  thread_id: string;
  mode: RunMode;
  target_comment_ids: string[];
}

/**
 * Start the FR-4.6 "Ask follow-ups" batch run: one headless agent answers every
 * open comment individually via `resolve-comment`; the artifact is untouched.
 * Rejects when the thread has no artifact or no open comments. Concurrent
 * submissions are accepted into the durable provider queue.
 */
export function askFollowUps(
  threadId: string,
  runOverride?: RunOverride | null,
): Promise<RunStarted> {
  return invoke<RunStarted>("ask_follow_ups", {
    thread_id: threadId,
    // `null` deserializes to `Option::None` in Rust — identical to omitting it,
    // i.e. current behavior. Only a real override reaches the run row.
    run_override: runOverride ?? null,
  });
}

/**
 * Start the "Ask now" single-comment answer run (epic conceptify-6xi): one
 * headless agent answers just this one root comment's latest open message via
 * `resolve-comment`; the artifact is untouched. Same `RunStarted` shape and
 * same durable queue as {@link askFollowUps}, but scoped to a single root
 * (`target_comment_ids` is `[rootCommentId]`; the resolve may land
 * on a reply row when the root was re-opened by a reply). Rejects (with a
 * user-facing message) when the thread has no artifact, the comment isn't found,
 * the target is a reply, the target root isn't open, or the agent/CLI is missing.
 */
export function askSingleComment(
  threadId: string,
  rootCommentId: string,
  runOverride?: RunOverride | null,
): Promise<RunStarted> {
  return invoke<RunStarted>("ask_single_comment", {
    thread_id: threadId,
    root_comment_id: rootCommentId,
    run_override: runOverride ?? null,
  });
}

/**
 * Start the FR-4.7 "Apply to artifact" run for the given comments (empty array
 * = every answered comment). The agent saves ONE new artifact version; the
 * viewer refreshes live via `artifact-updated` and the comments transition to
 * `applied`. The thread shows `updating` while the run is in flight.
 */
export function applyToArtifact(
  threadId: string,
  commentIds: string[],
  runOverride?: RunOverride | null,
): Promise<RunStarted> {
  return invoke<RunStarted>("apply_to_artifact", {
    thread_id: threadId,
    comment_ids: commentIds,
    run_override: runOverride ?? null,
  });
}

/** The newest non-terminal run for a thread, or `null`. Status may be queued,
 *  starting, running, throttled, or cancelling. Carries no
 *  target ids (not persisted): a re-attached run renders indeterminate
 *  progress. */
export interface ActiveRun {
  run_id: string;
  thread_id: string;
  mode: RunMode;
  status: RunStatus;
}

export function getActiveRun(threadId: string): Promise<ActiveRun | null> {
  return invoke<ActiveRun | null>("get_active_run", { thread_id: threadId });
}

export interface RunActivity {
  run_id: string;
  project_id: string;
  project_name: string;
  thread_id: string;
  thread_title: string;
  mode: RunMode;
  status: RunStatus;
  model: string;
  provider_pool: string | null;
  queued_at: string | null;
  execution_started_at: string | null;
  finished_at: string | null;
  status_reason: string | null;
  queue_position: number | null;
  retry_of_run_id: string | null;
  seen: boolean;
}

export function listRunActivity(): Promise<RunActivity[]> {
  return invoke<RunActivity[]>("list_run_activity");
}

export function dismissRunActivity(runId: string): Promise<boolean> {
  return invoke<boolean>("dismiss_run_activity", { run_id: runId });
}

export function markRunActivitySeen(runIds: string[]): Promise<number> {
  return invoke<number>("mark_run_activity_seen", { run_ids: runIds });
}

export interface SystemRunNotification {
  run_id: string;
  project_id: string;
  project_name: string;
  thread_id: string;
  status: RunStatus;
  status_reason: string | null;
}

export function claimSystemRunNotification(
  runId: string,
): Promise<SystemRunNotification | null> {
  return invoke<SystemRunNotification | null>("claim_system_run_notification", {
    run_id: runId,
  });
}

export interface ConflictReview {
  run_id: string;
  thread_id: string;
  project_id: string;
  project_name: string;
  thread_title: string;
  agent: string;
  model: string;
  route: string | null;
  base_version: number | null;
  current_version: number;
  resolution: string;
  kind: "revision" | "stale_base";
  target_cfy_ids: string[];
  diff: ArtifactVersionDiff;
}

export function getConflictReview(runId: string): Promise<ConflictReview> {
  return invoke<ConflictReview>("get_conflict_review", { run_id: runId });
}

export function publishConflictCandidate(runId: string): Promise<number> {
  return invoke<number>("publish_conflict_candidate", { run_id: runId });
}

export function rejectConflictCandidate(runId: string): Promise<boolean> {
  return invoke<boolean>("reject_conflict_candidate", { run_id: runId });
}

export function restoreArtifactVersion(threadId: string, version: number, runId?: string): Promise<number> {
  return invoke<number>("restore_artifact_version", { thread_id: threadId, version, run_id: runId ?? null });
}

export function rebaseConflict(runId: string): Promise<RunStarted> {
  return invoke<RunStarted>("rebase_conflict", { run_id: runId });
}

/** Cancel a live run (FR-4.8): SIGKILLs the whole process tree; the run ends
 *  `cancelled` with partial answers preserved. */
export function cancelRun(runId: string): Promise<void> {
  return invoke<void>("cancel_run", { run_id: runId });
}

/** The tail of a run's transcript (FR-4.8 failure surfacing). `log_path` is
 *  the full on-disk log, always returned; `lines` degrades to a single
 *  explanatory entry if the file is unreadable. */
export interface RunLogTail {
  run_id: string;
  log_path: string;
  lines: string[];
}

export function getRunLogTail(runId: string, maxLines?: number): Promise<RunLogTail> {
  return invoke<RunLogTail>("get_run_log_tail", {
    run_id: runId,
    max_lines: maxLines ?? null,
  });
}

// ---------------------------------------------------------------------------
// In-app ask (PRD §7.5, UC5 — FR-5.1/5.2/5.3)
// ---------------------------------------------------------------------------

/** A started in-app ask: the new (or, on retry, the same) thread and the
 *  `ask`-mode generation run now authoring its artifact. */
export interface AskStarted {
  run_id: string;
  thread_id: string;
}

/**
 * Start an FR-5.1 in-app ask: create a thread in `projectId` (status
 * `generating`) and spawn a headless agent that authors an artifact per the
 * skill and publishes it via `conceptify save-artifact`. `title` is optional —
 * when blank the core derives one from the question. Rejects (user-facing
 * message) on an empty question, an unknown project, or a missing agent/CLI.
 */
export function askFromApp(
  projectId: string,
  title: string | null,
  question: string,
  runOverride?: RunOverride | null,
  responseIntent?: ResponseIntent,
  skillMode: "auto" | "none" | "manual" = "auto",
  selectedSkillIds: string[] = [],
): Promise<AskStarted> {
  return invoke<AskStarted>("ask_from_app", {
    project_id: projectId,
    title: title ?? null,
    question,
    run_override: runOverride ?? null,
    response_intent: responseIntent ?? {
      version: 1,
      depth: "balanced",
      language: "familiar",
      visuals: "auto",
      shape: "auto",
      visual_purpose: "auto",
    },
    skill_mode: skillMode,
    selected_skill_ids: selectedSkillIds,
  });
}

/** Retry a failed in-app ask (FR-5.3): re-spawn the same question into the same
 *  thread, which returns to `generating`. */
export function retryAsk(threadId: string): Promise<AskStarted> {
  return invoke<AskStarted>("retry_ask", { thread_id: threadId });
}

/** The most recent run for a thread (any mode/status), or `null`. The FR-5.3
 *  generation-error state uses it to resolve the failed run's id for the log
 *  tail — works after an app restart, unlike `getActiveRun` (live runs only). */
export interface LatestRun {
  run_id: string;
  mode: RunMode;
  status: RunStatus;
  /** Resolved model the run actually used (epic e7m retry-surface display). */
  model: string;
  /** Route tag recorded on the row; `null` on pre-routing rows. */
  route: "anthropic" | "openai" | "openrouter" | "manual" | null;
  /** True iff a per-run override was recorded — Retry re-applies it verbatim;
   *  false means Retry re-derives the current settings defaults. */
  overridden: boolean;
}

export function getLatestRun(threadId: string): Promise<LatestRun | null> {
  return invoke<LatestRun | null>("get_latest_run", { thread_id: threadId });
}

// ---------------------------------------------------------------------------
// Agent settings (PRD §5.5, FR-7.1–7.4) — the Settings UI surface.
//
// Mirrors the Rust `AgentSettings` (camelCase serde). Command templates edit
// the `adapters` map; per-purpose models live under `models`. `agentBinaryPath`
// / `autoProjectBaseDir` are `null` when unset (code defaults apply, FR-7.4).
// ---------------------------------------------------------------------------

export type Appearance = "system" | "light" | "dark";

/** One agent invocation template (§5.5). `args`/`cwd`/`command` may contain the
 *  `{prompt}`, `{model}`, `{project_root}` placeholders. */
export interface AgentAdapter {
  command: string;
  args: string[];
  cwd: string;
}

/** Per-purpose model ids (§5.5). */
export interface PurposeModels {
  followUp: string;
  artifactUpdate: string;
  inAppAsk: string;
}

/** Provider-pool execution limits for the durable run scheduler. Keys are
 * intentionally open-ended so new providers/local endpoints need no wire-shape
 * change. */
export interface RunConcurrency {
  default: number;
  pools: Record<string, number>;
}

export interface AgentSettings {
  /** name → adapter template. Phase 1 ships only `"claude"`. */
  adapters: Record<string, AgentAdapter>;
  /** Which adapter runs; must be a key of `adapters` (backend-validated). */
  defaultAdapter: string;
  models: PurposeModels;
  /** Agent run timeout in seconds (default 1800 = 30 min). */
  timeoutSecs: number;
  /** Absolute-path override for the agent binary; `null`/empty = auto (FR-7.3). */
  agentBinaryPath: string | null;
  /** App appearance (FR-7.2). */
  appearance: Appearance;
  /** Base dir for auto-created project folders; `null`/empty = default
   *  `~/Documents/conceptify/projects` (FR-7.3). */
  autoProjectBaseDir: string | null;
  /** Provider families whose models appear in the catalog pickers (epic
   *  conceptify-e7m, bead e7m.6). Serialized from a Rust `BTreeSet<String>`
   *  (sorted array). Serde-defaults to `["anthropic", "openai"]` when absent, so
   *  a settings blob written before this field existed still deserializes; we
   *  always send it back so the Settings suite toggles persist. */
  enabledProviders: string[];
  /** Preserved by the current settings form even before its dedicated editor
   * lands; changing unrelated settings must never reset scheduler capacity. */
  runConcurrency: RunConcurrency;
  /** Opt-in native completion/attention notifications. Permission is requested
   * only when the user turns this on; in-app activity remains available. */
  systemNotifications: boolean;
}

/** Read the agent settings (stored overrides merged over code defaults, or pure
 *  defaults when nothing is saved — FR-7.4). */
export function getAgentSettings(): Promise<AgentSettings> {
  return invoke<AgentSettings>("get_agent_settings");
}

/** Persist the agent settings. Rejects (user-facing message) when the config is
 *  invalid (e.g. `defaultAdapter` names no adapter). */
export function setAgentSettings(settings: AgentSettings): Promise<void> {
  return invoke<void>("set_agent_settings", { settings });
}

/** Reset to code defaults (FR-7.4): deletes the stored override and returns the
 *  now-default settings. The OpenRouter key lives in its own settings row and is
 *  deliberately NOT cleared by a reset (bead e7m.7). */
export function resetAgentSettings(): Promise<AgentSettings> {
  return invoke<AgentSettings>("reset_agent_settings");
}

// ---------------------------------------------------------------------------
// Model catalog + routing options (epic conceptify-e7m — beads e7m.6/e7m.7).
//
// The live, provider-grouped model catalog the Settings pickers (e7m.3) and the
// point-of-ask override (e7m.4) build on, plus the routing-adjacent options
// (adapter keys, per-purpose default models, whether an OpenRouter key is set).
// These mirror the Rust `conceptify_types::Catalog*` (camelCase serde) and the
// `AgentOptionsDto` (commands.rs) exactly — casing matches the Rust `#[serde]`
// attributes, so no runtime remapping is needed.
// ---------------------------------------------------------------------------

/** One normalized catalog model. `id` is the execution id (a bare native id for
 *  the claude/codex routes, an OpenRouter slug for the OpenRouter route);
 *  `provider` is the family used by the suite toggles; `openrouterRunnable` is
 *  true when OpenRouter lists this exact id. Mirrors `CatalogModel`. */
export interface CatalogModel {
  id: string;
  provider: string;
  displayName: string;
  /** Max input context window in tokens, when the source reports one
   *  (`skip_serializing_if` on the Rust side means it may be absent). */
  contextWindow?: number | null;
  openrouterRunnable: boolean;
}

/** One provider family with its whole-catalog model count and enabled flag —
 *  powers the settings suite toggles. Mirrors `CatalogProvider`. */
export interface CatalogProvider {
  provider: string;
  modelCount: number;
  enabled: boolean;
}

/** The catalog filtered to the enabled provider suites, plus every provider with
 *  counts, when it was fetched, and where the served copy came from. Mirrors
 *  `CatalogResponse`. */
export interface CatalogResponse {
  /** RFC3339 timestamp the served catalog was fetched from the network. */
  fetchedAt: string;
  /** `live` (fetched this call), `cache` (disk cache), or `snapshot` (bundled
   *  offline fallback). */
  source: string;
  /** Chat-capable models whose provider is enabled, sorted by provider then id. */
  models: CatalogModel[];
  /** Every provider in the full catalog, with counts + enabled flag. */
  providers: CatalogProvider[];
}

/** The current catalog filtered to the enabled provider suites (no network —
 *  reads the warm disk cache / bundled snapshot). Always succeeds. */
export function getModelCatalog(): Promise<CatalogResponse> {
  return invoke<CatalogResponse>("get_model_catalog");
}

/** Force a live re-fetch of the catalog and update the disk cache (the Settings
 *  "Refresh" action). Failure-silent server-side: a network error degrades to
 *  the cache/snapshot and still resolves (the returned `source` reveals which),
 *  so callers never show an error dialog. */
export function refreshModelCatalog(): Promise<CatalogResponse> {
  return invoke<CatalogResponse>("refresh_model_catalog");
}

/** UI-friendly view of the run-selection options (epic conceptify-e7m): the
 *  configured adapter KEYS, the default adapter, the per-purpose default models,
 *  and whether an OpenRouter key is stored. Mirrors `AgentOptionsDto`. */
export interface AgentOptions {
  adapters: string[];
  defaultAdapter: string;
  models: PurposeModels;
  /** Whether an OpenRouter API key is stored (bead e7m.7) — the only
   *  key-derived fact that ever reaches the frontend. */
  openRouterKeySet: boolean;
}

export function getAgentOptions(): Promise<AgentOptions> {
  return invoke<AgentOptions>("get_agent_options");
}

/** Store (`key`) or clear (`null`/blank) the OpenRouter API key (bead e7m.7).
 *  Write-only by design: no command ever returns the key — the frontend learns
 *  only `openRouterKeySet` from {@link getAgentOptions}. Stored outside the
 *  agent-settings blob, so {@link resetAgentSettings} leaves it intact. The
 *  backend rejects an empty/whitespace-only key. */
export function setOpenRouterApiKey(key: string | null): Promise<void> {
  return invoke<void>("set_openrouter_api_key", { key });
}
