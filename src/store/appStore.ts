// Central app-shell store: projects, threads, and the current selection.
//
// A single module-level observable (not per-component state) so the whole shell
// shares one source of truth and so bead conceptify-qxr.5 (live list updates)
// has a stable seam to drive. That bead should NOT re-implement fetching — it
// only needs to translate Tauri events into calls on this store, e.g. in a
// top-level effect:
//
//   import { appStore } from "./store/appStore";
//   import { listen } from "@tauri-apps/api/event";
//   listen("projects-changed", () => appStore.refetchProjects());
//   listen<{ project_id: string; thread_id: string }>("thread-created", (e) => {
//     appStore.refetchProjects();                       // counts + ordering
//     appStore.refetchThreads(e.payload.project_id);    // no-op unless it's open
//   });
//
// `refetchProjects` / `refetchThreads` are the public seams; both are safe to
// call at any time and guard against out-of-order results and stale selections.

import { useEffect, useState } from "preact/hooks";
import * as api from "../lib/api";
import type { ArtifactVersion, Comment, Project, RunActivity, RunMode, RunOverride, Thread } from "../lib/api";

/** Which artifact version the viewer shows: a concrete number (read-only
 *  history view) or `"latest"` (tracks new saves live, FR-2.4). */
export type ViewerVersion = number | "latest";

/**
 * A root comment with its ordered reply chain (epic conceptify-6xi threaded
 * replies). The comments slice stays flat internally (the single source of
 * truth for highlights + list); this is the *derived* view the sidebar renders
 * one item per — see {@link groupComments}.
 */
export interface CommentChain {
  root: Comment;
  replies: Comment[];
}

/**
 * Group a flat comment list (as `list_comments` returns and the store holds)
 * into root chains for threaded rendering (bead conceptify-6xi.3).
 *
 * Roots (`parent_id == null`) keep their incoming order — the API returns
 * oldest-first, which is the order the sidebar wants. Each root's replies nest
 * beneath it, ordered by `created_at` then `id` as a stable tiebreak (the API
 * already returns oldest-first, so this only disambiguates same-timestamp
 * rows; it is not a re-sort of anything the server ordered differently).
 *
 * Chains are linear server-side (reply-to-reply is rejected), so every non-null
 * `parent_id` names a root that is present in the same list. Defensive: a reply
 * whose parent is somehow absent is appended as its own single-row chain at the
 * end rather than dropped, so nothing silently disappears.
 */
export function groupComments(comments: Comment[]): CommentChain[] {
  const chains: CommentChain[] = [];
  const byRootId = new Map<string, CommentChain>();

  // First pass: roots, preserving the incoming (oldest-first) order.
  for (const c of comments) {
    if (c.parent_id == null) {
      const chain: CommentChain = { root: c, replies: [] };
      chains.push(chain);
      byRootId.set(c.id, chain);
    }
  }

  // Second pass: attach replies to their root (or collect orphans).
  const orphans: Comment[] = [];
  for (const c of comments) {
    if (c.parent_id == null) continue;
    const chain = byRootId.get(c.parent_id);
    if (chain != null) chain.replies.push(c);
    else orphans.push(c);
  }

  // Stable order within each chain: created_at, then id.
  for (const chain of chains) {
    chain.replies.sort((a, b) => {
      if (a.created_at !== b.created_at) return a.created_at < b.created_at ? -1 : 1;
      return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
    });
  }

  // Defensive orphan handling: render flat at the end as their own chains.
  for (const orphan of orphans) chains.push({ root: orphan, replies: [] });

  return chains;
}

/**
 * A follow-up run the sidebar is tracking (FR-4.8). `targetIds` is the set of
 * comments the run was started for — per-comment progress is *derived* from
 * the store's comment statuses against this set as `comment-updated` events
 * land (no separate counter to drift). `null` when the run was re-attached
 * via `get_active_run` (targets aren't persisted): render an indeterminate
 * spinner. `lastProgress` is the most recent *displayable* `run-progress` line
 * (filtered/formatted by {@link formatRunProgressLine} — non-actionable noise
 * like "allowed" rate-limit heartbeats never lands here).
 */
export interface ActiveRunState {
  runId: string;
  threadId: string;
  mode: RunMode;
  targetIds: string[] | null;
  lastProgress: string | null;
  /** A small rolling log of the most recent parsed `run-progress` lines
   *  (`kind` + `detail`), newest last. Drives the FR-5.2 in-app generation
   *  progress panel's activity feed; the sidebar run block only uses
   *  `lastProgress`. Capped at {@link MAX_RUN_PROGRESS_LINES}. */
  recentProgress: string[];
  /** Ephemeral Claude text deltas for the answer currently being composed.
   * Never persisted; cleared when a comment update or terminal event lands. */
  liveAnswer: string;
}

/** How many `run-progress` lines the rolling activity feed keeps (FR-5.2). */
const MAX_RUN_PROGRESS_LINES = 8;

/** A terminal failure (`failed`/`timeout`) of the latest run on the selected
 *  thread (FR-4.8): drives the inline failure panel with the on-demand log
 *  tail. Cleared on dismiss, thread switch, or a new run start.
 *
 *  `targetRootId` is the single root comment a failed "Ask now" run was
 *  targeting (epic conceptify-6xi), so the sidebar can highlight that root's
 *  chain alongside the failure panel. `null` for a batch answer/apply run, a
 *  run re-attached after reload (targets aren't persisted), or a generation
 *  run — those aren't scoped to one root. */
export interface RunFailureState {
  runId: string;
  threadId: string;
  status: "failed" | "timeout";
  targetRootId: string | null;
}

export interface AskQuestionDraft {
  id: string;
  title: string;
  question: string;
  modelOverride: string | null;
  responseIntent: api.ResponseIntent;
  skillMode: "auto" | "none" | "manual";
  selectedSkillIds: string[];
  responseOrigins: Record<"depth" | "language" | "visuals" | "shape" | "visual_purpose", api.ResponsePreferenceOrigin>;
  responseIntentTouched: boolean;
}

export type AskSubmissionStatus =
  | "submitting"
  | "queued"
  | "running"
  | "throttled"
  | "cancelling"
  | "completed"
  | "cancelled"
  | "failed";

export interface AskSubmission extends AskQuestionDraft {
  status: AskSubmissionStatus;
  runId: string | null;
  threadId: string | null;
  error: string | null;
}

export interface AskComposerWorkspace {
  mode: "single" | "multi";
  draft: AskQuestionDraft;
  staged: AskQuestionDraft[];
  submissions: AskSubmission[];
  preferences: api.ResolvedResponsePreferences | null;
}

export interface AppState {
  projects: Project[];
  projectsLoading: boolean;
  projectsError: string | null;
  showArchived: boolean;
  selectedProjectId: string | null;
  threads: Thread[];
  threadsLoading: boolean;
  threadsError: string | null;
  selectedThreadId: string | null;
  /** Saved versions for the selected thread, ascending (FR-2.4). */
  artifactVersions: ArtifactVersion[];
  artifactVersionsLoading: boolean;
  artifactVersionsError: string | null;
  viewerVersion: ViewerVersion;
  /** Comments for the selected thread, oldest first (FR-4.5). The source of
   *  truth for the in-artifact highlights (94m.3/94m.4) and the sidebar
   *  (94m.6). Empty when no thread is selected. */
  comments: Comment[];
  commentsLoading: boolean;
  commentsError: string | null;
  /** The in-flight follow-up run for the selected thread, if any (FR-4.8). */
  activeRun: ActiveRunState | null;
  /** The latest failed/timed-out run on the selected thread (FR-4.8). */
  runFailure: RunFailureState | null;
  /** Whether the Settings overlay is open (FR-7.x). App-level UI state. */
  settingsOpen: boolean;
  /** Project-keyed composer state. It intentionally outlives component mounts
   * so partially written and staged questions survive navigation. */
  askComposerByProject: Record<string, AskComposerWorkspace>;
  runActivity: RunActivity[];
  runActivityLoading: boolean;
  activityTrayOpen: boolean;
  conflictReviewRunId: string | null;
  pendingConceptEvidence: { threadId: string; cfyId: string } | null;
  pendingSearchTarget: { threadId: string; kind: "artifact" | "comment"; targetId: string } | null;
  searchNotice: string | null;
}

type Listener = () => void;

const initialState: AppState = {
  projects: [],
  projectsLoading: false,
  projectsError: null,
  showArchived: false,
  selectedProjectId: null,
  threads: [],
  threadsLoading: false,
  threadsError: null,
  selectedThreadId: null,
  artifactVersions: [],
  artifactVersionsLoading: false,
  artifactVersionsError: null,
  viewerVersion: "latest",
  comments: [],
  commentsLoading: false,
  commentsError: null,
  activeRun: null,
  runFailure: null,
  settingsOpen: false,
  askComposerByProject: {},
  runActivity: [],
  runActivityLoading: false,
  activityTrayOpen: false,
  conflictReviewRunId: null,
  pendingConceptEvidence: null,
  pendingSearchTarget: null,
  searchNotice: null,
};

let askDraftSequence = 0;

function newAskDraft(preferences?: api.ResolvedResponsePreferences | null): AskQuestionDraft {
  askDraftSequence += 1;
  return {
    id: `ask-draft-${Date.now()}-${askDraftSequence}`,
    title: "",
    question: "",
    modelOverride: null,
    responseIntent: preferences?.intent ?? {
      version: 1,
      depth: "balanced",
      language: "familiar",
      visuals: "auto",
      shape: "auto",
      visual_purpose: "auto",
    },
    skillMode: "auto",
    selectedSkillIds: [],
    responseOrigins: preferences?.origins ?? {
      depth: "product",
      language: "product",
      visuals: "product",
      shape: "product",
      visual_purpose: "product",
    },
    responseIntentTouched: false,
  };
}

function defaultAskWorkspace(): AskComposerWorkspace {
  return { mode: "single", draft: newAskDraft(), staged: [], submissions: [], preferences: null };
}

function runOverrideOfModel(model: string | null): RunOverride | null {
  return model == null ? null : { model };
}

function askSubmissionStatus(status: api.RunStatus): AskSubmissionStatus {
  switch (status) {
    case "queued":
      return "queued";
    case "starting":
    case "running":
      return "running";
    case "throttled":
      return "throttled";
    case "cancelling":
      return "cancelling";
    case "completed":
      return "completed";
    case "cancelled":
      return "cancelled";
    case "conflicted":
    case "failed":
    case "timeout":
      return "failed";
  }
}

/** Fresh viewer state, applied whenever the selected thread changes/vanishes.
 *  Comments (and run tracking) belong to a thread, so they clear on the same
 *  boundary. */
const clearedViewer = {
  artifactVersions: [] as ArtifactVersion[],
  artifactVersionsLoading: false,
  artifactVersionsError: null,
  viewerVersion: "latest" as ViewerVersion,
  comments: [] as Comment[],
  commentsLoading: false,
  commentsError: null,
  activeRun: null as ActiveRunState | null,
  runFailure: null as RunFailureState | null,
};

/** The nested payload a `rate_limit_event` forwards in `run-progress.detail`
 *  (compact JSON, produced by `runs.rs` classify_line). All fields optional —
 *  we only depend on `status` / `isUsingOverage` / `resetsAt`. */
interface RateLimitInfo {
  status?: string;
  resetsAt?: number; // unix seconds
  isUsingOverage?: boolean;
  overageStatus?: string;
  overageResetsAt?: number; // unix seconds
}

/**
 * Turn a raw `run-progress` event into a single display line for the activity
 * feed, or `null` to drop it entirely. This is the ONE place run-progress
 * display policy lives (bead conceptify-pri) — it feeds both the in-app
 * generation panel and the sidebar run block, and the full transcript stays on
 * disk in the run log regardless.
 *
 * The only filtered class today is `rate_limit_event`: the claude CLI emits an
 * informational heartbeat (`rate_limit_info.status === "allowed"`) that read as
 * a scary warning in the feed. Those are dropped; genuine limiting (a status
 * other than "allowed", or overage actually in use) is surfaced plainly with
 * the reset time.
 */
export function formatRunProgressLine(kind: string, detail: string): string | null {
  if (kind === "rate_limit_event") return formatRateLimit(detail);
  return detail ? `${kind}: ${detail}` : kind;
}

function formatRateLimit(detail: string): string | null {
  let info: RateLimitInfo;
  try {
    info = JSON.parse(detail) as RateLimitInfo;
  } catch {
    // Not the structured payload (older core, or a truncated line): drop it
    // rather than surface raw JSON noise — the whole point of this filter.
    return null;
  }
  const limited =
    (info.status != null && info.status !== "allowed") || info.isUsingOverage === true;
  if (!limited) return null; // informational heartbeat — never surface

  const resetTs =
    info.isUsingOverage === true ? (info.overageResetsAt ?? info.resetsAt) : info.resetsAt;
  if (resetTs != null && Number.isFinite(resetTs)) {
    const t = new Date(resetTs * 1000);
    const hh = String(t.getHours()).padStart(2, "0");
    const mm = String(t.getMinutes()).padStart(2, "0");
    return `Rate limited — waiting for reset ${hh}:${mm}`;
  }
  return "Rate limited — waiting for reset";
}

class AppStore {
  private state: AppState = initialState;
  private listeners = new Set<Listener>();
  /** Monotonic token so a slow thread fetch can't clobber a newer one. */
  private threadFetchToken = 0;
  /** Same guard for artifact-version fetches (viewer switcher data). */
  private versionFetchToken = 0;
  /** Same guard for comment fetches (comment layer + sidebar data). */
  private commentFetchToken = 0;

  getSnapshot(): AppState {
    return this.state;
  }

  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  private set(patch: Partial<AppState>): void {
    this.state = { ...this.state, ...patch };
    for (const listener of this.listeners) listener();
  }

  private askWorkspace(projectId: string): AskComposerWorkspace {
    return this.state.askComposerByProject[projectId] ?? defaultAskWorkspace();
  }

  private setAskWorkspace(projectId: string, workspace: AskComposerWorkspace): void {
    this.set({
      askComposerByProject: {
        ...this.state.askComposerByProject,
        [projectId]: workspace,
      },
    });
  }

  ensureAskWorkspace(projectId: string): void {
    if (this.state.askComposerByProject[projectId] != null) return;
    this.setAskWorkspace(projectId, defaultAskWorkspace());
    void api.getResponsePreferences(projectId).then((preferences) => {
      const workspace = this.askWorkspace(projectId);
      this.setAskWorkspace(projectId, {
        ...workspace,
        preferences,
        draft: workspace.draft.responseIntentTouched
          ? workspace.draft
          : newAskDraft(preferences),
      });
    }).catch(() => undefined);
  }

  setAskComposerMode(projectId: string, mode: "single" | "multi"): void {
    this.setAskWorkspace(projectId, { ...this.askWorkspace(projectId), mode });
  }

  updateAskDraft(
    projectId: string,
    patch: Partial<Omit<AskQuestionDraft, "id">>,
  ): void {
    const workspace = this.askWorkspace(projectId);
    this.setAskWorkspace(projectId, {
      ...workspace,
      draft: {
        ...workspace.draft,
        ...patch,
        ...(patch.responseIntent == null
          ? {}
          : {
              responseIntentTouched: true,
              responseOrigins: {
                depth: "question",
                language: "question",
                visuals: "question",
                shape: "question",
                visual_purpose: "question",
              } as const,
            }),
      },
    });
  }

  async saveAskResponsePreference(
    projectId: string,
    scope: "user" | "project",
    intent: api.ResponseIntent,
  ): Promise<void> {
    const preferences = await api.saveResponsePreference(projectId, scope, intent);
    const workspace = this.askWorkspace(projectId);
    this.setAskWorkspace(projectId, {
      ...workspace,
      preferences,
      draft: {
        ...workspace.draft,
        responseIntent: preferences.intent,
        responseOrigins: preferences.origins,
        responseIntentTouched: false,
      },
    });
  }

  async resetAskResponsePreference(
    projectId: string,
    scope: "user" | "project",
  ): Promise<void> {
    const preferences = await api.resetResponsePreference(projectId, scope);
    const workspace = this.askWorkspace(projectId);
    this.setAskWorkspace(projectId, {
      ...workspace,
      preferences,
      draft: {
        ...workspace.draft,
        responseIntent: preferences.intent,
        responseOrigins: preferences.origins,
        responseIntentTouched: false,
      },
    });
  }

  resetAskDraftToInherited(projectId: string): void {
    const workspace = this.askWorkspace(projectId);
    const inherited = newAskDraft(workspace.preferences);
    this.setAskWorkspace(projectId, {
      ...workspace,
      draft: {
        ...workspace.draft,
        responseIntent: inherited.responseIntent,
        responseOrigins: inherited.responseOrigins,
        responseIntentTouched: false,
      },
    });
  }

  stageAskDraft(projectId: string): void {
    const workspace = this.askWorkspace(projectId);
    if (workspace.draft.question.trim().length === 0) return;
    this.setAskWorkspace(projectId, {
      ...workspace,
      staged: [...workspace.staged, { ...workspace.draft }],
      draft: newAskDraft(workspace.preferences),
    });
  }

  updateStagedAskDraft(
    projectId: string,
    draftId: string,
    patch: Partial<Omit<AskQuestionDraft, "id">>,
  ): void {
    const workspace = this.askWorkspace(projectId);
    this.setAskWorkspace(projectId, {
      ...workspace,
      staged: workspace.staged.map((draft) =>
        draft.id === draftId
          ? {
              ...draft,
              ...patch,
              ...(patch.responseIntent == null
                ? {}
                : {
                    responseIntentTouched: true,
                    responseOrigins: {
                      depth: "question",
                      language: "question",
                      visuals: "question",
                      shape: "question",
                      visual_purpose: "question",
                    } as const,
                  }),
            }
          : draft,
      ),
    });
  }

  removeStagedAskDraft(projectId: string, draftId: string): void {
    const workspace = this.askWorkspace(projectId);
    this.setAskWorkspace(projectId, {
      ...workspace,
      staged: workspace.staged.filter((draft) => draft.id !== draftId),
    });
  }

  restoreAskSubmission(projectId: string, submissionId: string): void {
    const workspace = this.askWorkspace(projectId);
    const submission = workspace.submissions.find((item) => item.id === submissionId);
    if (submission == null || submission.status !== "failed") return;
    this.setAskWorkspace(projectId, {
      ...workspace,
      staged: [
        ...workspace.staged,
        {
          id: newAskDraft().id,
          title: submission.title,
          question: submission.question,
          modelOverride: submission.modelOverride,
          responseIntent: submission.responseIntent,
          skillMode: submission.skillMode,
          selectedSkillIds: submission.selectedSkillIds,
          responseOrigins: submission.responseOrigins,
          responseIntentTouched: true,
        },
      ],
      submissions: workspace.submissions.filter((item) => item.id !== submissionId),
      mode: "multi",
    });
  }

  async submitAskQuestions(projectId: string, drafts: AskQuestionDraft[]): Promise<void> {
    const workspace = this.askWorkspace(projectId);
    const existing = new Set(workspace.submissions.map((item) => item.id));
    const unique = drafts.filter(
      (draft, index) =>
        draft.question.trim().length > 0 &&
        !existing.has(draft.id) &&
        drafts.findIndex((candidate) => candidate.id === draft.id) === index,
    );
    if (unique.length === 0) return;

    const launching: AskSubmission[] = unique.map((draft) => ({
      ...draft,
      title: draft.title.trim(),
      question: draft.question.trim(),
      status: "submitting",
      runId: null,
      threadId: null,
      error: null,
    }));
    const ids = new Set(unique.map((draft) => draft.id));
    this.setAskWorkspace(projectId, {
      ...workspace,
      draft: ids.has(workspace.draft.id) ? newAskDraft(workspace.preferences) : workspace.draft,
      staged: workspace.staged.filter((draft) => !ids.has(draft.id)),
      submissions: [...launching, ...workspace.submissions].slice(0, 12),
    });

    await Promise.all(
      launching.map(async (submission) => {
        try {
          const started = await api.askFromApp(
            projectId,
            submission.title === "" ? null : submission.title,
            submission.question,
            runOverrideOfModel(submission.modelOverride),
            submission.responseIntent,
            submission.skillMode,
            submission.selectedSkillIds,
          );
          const active = await api.getActiveRun(started.thread_id).catch(() => null);
          const latest = active == null
            ? await api.getLatestRun(started.thread_id).catch(() => null)
            : null;
          const observed = active?.run_id === started.run_id
            ? active.status
            : latest?.run_id === started.run_id
              ? latest.status
              : "queued";
          this.updateAskSubmission(projectId, submission.id, {
            runId: started.run_id,
            threadId: started.thread_id,
            status: askSubmissionStatus(observed),
          });
        } catch (error) {
          this.updateAskSubmission(projectId, submission.id, {
            status: "failed",
            error: String(error),
          });
        }
      }),
    );
    await this.refetchThreads(projectId);
  }

  async cancelAskSubmission(projectId: string, submissionId: string): Promise<void> {
    const workspace = this.askWorkspace(projectId);
    const submission = workspace.submissions.find((item) => item.id === submissionId);
    if (submission?.runId == null) return;
    this.updateAskSubmission(projectId, submissionId, { status: "cancelling" });
    try {
      await api.cancelRun(submission.runId);
    } catch (error) {
      this.updateAskSubmission(projectId, submissionId, {
        status: "failed",
        error: String(error),
      });
    }
  }

  private updateAskSubmission(
    projectId: string,
    submissionId: string,
    patch: Partial<AskSubmission>,
  ): void {
    const workspace = this.askWorkspace(projectId);
    this.setAskWorkspace(projectId, {
      ...workspace,
      submissions: workspace.submissions.map((item) =>
        item.id === submissionId ? { ...item, ...patch } : item,
      ),
    });
  }

  private updateSubmissionByRun(runId: string, patch: Partial<AskSubmission>): void {
    let changed = false;
    const next: Record<string, AskComposerWorkspace> = {};
    for (const [projectId, workspace] of Object.entries(this.state.askComposerByProject)) {
      const submissions = workspace.submissions.map((item) => {
        if (item.runId !== runId) return item;
        changed = true;
        return { ...item, ...patch };
      });
      next[projectId] = submissions === workspace.submissions ? workspace : { ...workspace, submissions };
    }
    if (changed) this.set({ askComposerByProject: next });
  }

  // ---- reads / refetch seams (qxr.5 event listeners call these) ----

  async refetchProjects(): Promise<void> {
    this.set({ projectsLoading: true, projectsError: null });
    try {
      const projects = await api.listProjects(this.state.showArchived);
      const stillSelected =
        this.state.selectedProjectId != null &&
        projects.some((p) => p.id === this.state.selectedProjectId);
      this.set({
        projects,
        projectsLoading: false,
        selectedProjectId: stillSelected ? this.state.selectedProjectId : null,
      });
    } catch (e) {
      this.set({ projectsLoading: false, projectsError: String(e) });
    }
  }

  async refetchRunActivity(): Promise<void> {
    this.set({ runActivityLoading: true });
    try {
      const runActivity = await api.listRunActivity();
      this.set({ runActivity, runActivityLoading: false });
    } catch {
      this.set({ runActivityLoading: false });
    }
  }

  openActivityTray(): void {
    this.set({ activityTrayOpen: true });
    void this.refetchRunActivity().then(() => this.markVisibleActivitySeen());
  }

  closeActivityTray(): void {
    this.set({ activityTrayOpen: false });
  }

  openConflictReview(runId: string): void {
    this.set({ conflictReviewRunId: runId, activityTrayOpen: false });
  }

  closeConflictReview(): void {
    this.set({ conflictReviewRunId: null });
  }

  async dismissRunActivity(runId: string): Promise<void> {
    await api.dismissRunActivity(runId);
    this.set({ runActivity: this.state.runActivity.filter((item) => item.run_id !== runId) });
  }

  async markVisibleActivitySeen(): Promise<void> {
    const terminal = this.state.runActivity.filter(
      (item) => !item.seen && !["queued", "starting", "running", "throttled", "cancelling"].includes(item.status),
    );
    if (terminal.length === 0) return;
    await api.markRunActivitySeen(terminal.map((item) => item.run_id));
    const ids = new Set(terminal.map((item) => item.run_id));
    this.set({
      runActivity: this.state.runActivity.map((item) =>
        ids.has(item.run_id) ? { ...item, seen: true } : item,
      ),
    });
  }

  async clearCompletedActivity(): Promise<void> {
    const clearable = this.state.runActivity.filter((item) =>
      ["completed", "cancelled"].includes(item.status),
    );
    await Promise.all(clearable.map((item) => api.dismissRunActivity(item.run_id)));
    const ids = new Set(clearable.map((item) => item.run_id));
    this.set({ runActivity: this.state.runActivity.filter((item) => !ids.has(item.run_id)) });
  }

  async jumpToRunActivity(item: RunActivity): Promise<void> {
    await this.jumpToProjectThread(item.project_id, item.thread_id);
  }

  async jumpToProjectThread(projectId: string, threadId: string): Promise<void> {
    if (this.state.selectedProjectId !== projectId) {
      this.selectProject(projectId);
      await this.refetchThreads(projectId);
    }
    this.selectThread(threadId);
    this.openActivityTray();
  }

  async cancelRunActivity(item: RunActivity): Promise<void> {
    await api.cancelRun(item.run_id);
    await this.refetchRunActivity();
  }

  async retryRunActivity(item: RunActivity): Promise<void> {
    await this.jumpToRunActivity(item);
    if (item.mode === "ask") await this.retryAsk(item.thread_id);
    await this.refetchRunActivity();
  }

  /**
   * Refetch the thread list for `projectId` (defaults to the selected project).
   * A no-op when `projectId` isn't the project currently on screen, so an event
   * for a background project can't overwrite the visible list.
   */
  async refetchThreads(projectId?: string): Promise<void> {
    const target = projectId ?? this.state.selectedProjectId;
    if (!target || target !== this.state.selectedProjectId) return;

    const token = ++this.threadFetchToken;
    this.set({ threadsLoading: true, threadsError: null });
    try {
      const threads = await api.listThreads(target);
      // Selection moved on (or a newer fetch started) while awaiting → drop it.
      if (token !== this.threadFetchToken || this.state.selectedProjectId !== target) return;
      const stillSelected =
        this.state.selectedThreadId != null &&
        threads.some((t) => t.id === this.state.selectedThreadId);
      this.set({
        threads,
        threadsLoading: false,
        selectedThreadId: stillSelected ? this.state.selectedThreadId : null,
        // The selected thread vanished → its viewer state is stale too.
        ...(stillSelected ? null : clearedViewer),
      });
    } catch (e) {
      if (token !== this.threadFetchToken) return;
      this.set({ threadsLoading: false, threadsError: String(e) });
    }
  }

  /**
   * Refetch the saved artifact versions for `threadId` (defaults to the
   * selected thread). Mirrors `refetchThreads`' guards: a no-op unless the
   * thread is the one on screen, and token-guarded against slow results
   * landing after the selection moved on.
   */
  async refetchArtifactVersions(threadId?: string): Promise<void> {
    const target = threadId ?? this.state.selectedThreadId;
    if (!target || target !== this.state.selectedThreadId) return;

    const token = ++this.versionFetchToken;
    this.set({ artifactVersionsLoading: true, artifactVersionsError: null });
    try {
      const versions = await api.listArtifactVersions(target);
      if (token !== this.versionFetchToken || this.state.selectedThreadId !== target) return;
      this.set({ artifactVersions: versions, artifactVersionsLoading: false });
    } catch (e) {
      if (token !== this.versionFetchToken) return;
      this.set({ artifactVersionsLoading: false, artifactVersionsError: String(e) });
    }
  }

  /**
   * Refetch the selected thread's comments (defaults to the selected thread).
   * Mirrors `refetchArtifactVersions`' guards: a no-op unless the thread is the
   * one on screen, and token-guarded so a slow result can't land after the
   * selection moved on. Fetches every status (the comment layer filters to
   * open+current-version for highlights; the sidebar (94m.6) filters for its
   * tabs). Called by `selectThread`, by this store after its own create, and by
   * `events.ts` on a CLI/API `comment-created`/`comment-updated` for this thread.
   */
  async refetchComments(threadId?: string): Promise<void> {
    const target = threadId ?? this.state.selectedThreadId;
    if (!target || target !== this.state.selectedThreadId) return;

    const token = ++this.commentFetchToken;
    this.set({ commentsLoading: true, commentsError: null });
    try {
      const comments = await api.listComments(target);
      if (token !== this.commentFetchToken || this.state.selectedThreadId !== target) return;
      this.set({ comments, commentsLoading: false });
    } catch (e) {
      if (token !== this.commentFetchToken) return;
      this.set({ commentsLoading: false, commentsError: String(e) });
    }
  }

  /**
   * Record a comment the shell just created via `api.createComment` (94m.3/4).
   * The command returns the authoritative row, so we append it immediately — no
   * round-trip on the critical path (N2: the highlight must land instantly) —
   * de-duped by id, and dropped if the thread is no longer selected. Then a
   * fresh `refetchComments` reconciles the full list: its token bump also
   * invalidates any still-in-flight initial load (whose snapshot may predate
   * this write), so a comment created mid-load can't hide pre-existing ones.
   * `refetchThreads` refreshes the open-comment badge.
   */
  addComment(comment: Comment): void {
    if (comment.thread_id !== this.state.selectedThreadId) return;
    if (!this.state.comments.some((c) => c.id === comment.id)) {
      this.set({ comments: [...this.state.comments, comment] });
    }
    void this.refetchComments();
    void this.refetchThreads();
  }

  /**
   * React to a core `artifact-updated` event `{project_id, thread_id,
   * version}` (a save landed via the API/CLI/skill). Two jobs:
   *
   * 1. List data: the save flipped the thread's status to `ready` and moved
   *    its last-activity ordering — refetch the project list and, when the
   *    project is on screen, its threads.
   * 2. Live viewer refresh (PRD N2, < 500ms): when the saved thread is the
   *    one being viewed, record the new version *synchronously* so the
   *    iframe src flips to it in the same tick — no round-trip on the
   *    critical path. A refetch then reconciles the optimistic entry
   *    (correct `created_at`/`created_by`) in the background.
   *
   * The viewer only follows the new version while `viewerVersion` is
   * `"latest"`; pinned historical versions stay put (FR-2.4).
   */
  handleArtifactUpdated(payload: {
    project_id: string;
    thread_id: string;
    version: number;
  }): void {
    void this.refetchProjects();
    void this.refetchThreads(payload.project_id);

    if (payload.thread_id !== this.state.selectedThreadId) return;
    if (!this.state.artifactVersions.some((v) => v.version === payload.version)) {
      const optimistic = [
        ...this.state.artifactVersions,
        {
          version: payload.version,
          created_at: new Date().toISOString(),
          created_by: payload.version === 1 ? "initial" : "follow_up",
          response_intent: null,
          skills: [],
        },
      ].sort((a, b) => a.version - b.version);
      this.set({ artifactVersions: optimistic });
    }
    void this.refetchArtifactVersions(payload.thread_id);
  }

  /** Viewer switcher selection (FR-2.4): a concrete version or `"latest"`. */
  setViewerVersion(version: ViewerVersion): void {
    if (version === this.state.viewerVersion) return;
    this.set({ viewerVersion: version });
  }

  // ---- follow-up runs (FR-4.6/4.7/4.8/4.9) ----

  /**
   * Start the FR-4.6 "Ask follow-ups" batch run for `threadId`. Throws (so the
   * sidebar can surface the message inline) on guard failures — no artifact,
   * no open comments, or an already-active run (FR-4.9). On success records
   * the run (with its target ids, the basis for per-comment progress) and
   * clears any previous failure panel.
   */
  async askFollowUps(threadId: string, runOverride?: RunOverride | null): Promise<void> {
    const started = await api.askFollowUps(threadId, runOverride);
    if (this.state.selectedThreadId !== started.thread_id) return;
    this.set({
      activeRun: {
        runId: started.run_id,
        threadId: started.thread_id,
        mode: started.mode,
        targetIds: started.target_comment_ids,
        lastProgress: null,
        recentProgress: [],
        liveAnswer: "",
      },
      runFailure: null,
    });
  }

  /**
   * Start the "Ask now" single-comment answer run for `rootCommentId` (epic
   * conceptify-6xi). Mirrors `askFollowUps`: throws (so the sidebar surfaces the
   * guard message inline near the button) on no artifact / not-open / reply /
   * already-active-run, and on success records the run (`targetIds` = the single
   * root id — the basis for the inline single-run state that renders on that
   * root's chain instead of the header batch block) and clears any prior
   * failure panel.
   */
  async askSingleComment(
    threadId: string,
    rootCommentId: string,
    runOverride?: RunOverride | null,
  ): Promise<void> {
    const started = await api.askSingleComment(threadId, rootCommentId, runOverride);
    if (this.state.selectedThreadId !== started.thread_id) return;
    this.set({
      activeRun: {
        runId: started.run_id,
        threadId: started.thread_id,
        mode: started.mode,
        targetIds: started.target_comment_ids,
        lastProgress: null,
        recentProgress: [],
        liveAnswer: "",
      },
      runFailure: null,
    });
  }

  /**
   * Start the FR-4.7 "Apply to artifact" run for the given comments (empty =
   * all answered). Same throw/record contract as `askFollowUps`. The thread's
   * `updating` status chip arrives via the `thread-updated` event.
   */
  async applyToArtifact(
    threadId: string,
    commentIds: string[],
    runOverride?: RunOverride | null,
  ): Promise<void> {
    const started = await api.applyToArtifact(threadId, commentIds, runOverride);
    if (this.state.selectedThreadId !== started.thread_id) return;
    this.set({
      activeRun: {
        runId: started.run_id,
        threadId: started.thread_id,
        mode: started.mode,
        targetIds: started.target_comment_ids,
        lastProgress: null,
        recentProgress: [],
        liveAnswer: "",
      },
      runFailure: null,
    });
  }

  /**
   * Start an FR-5.1 in-app ask: create a thread + generation run, then navigate
   * to it (status `generating`, progress panel live). Reads `selectedProjectId`
   * for the target project. Throws (so the composer surfaces the message inline)
   * on an empty question / unknown project / missing agent. On success the new
   * thread is selected and its `ask` run tracked so progress lands immediately.
   */
  async askFromApp(
    title: string | null,
    question: string,
    runOverride?: RunOverride | null,
    responseIntent?: api.ResponseIntent,
  ): Promise<void> {
    const projectId = this.state.selectedProjectId;
    if (projectId == null) throw new Error("select a project first");

    const started = await api.askFromApp(projectId, title, question, runOverride, responseIntent);
    // Make the new thread appear, then select it. `selectThread` clears viewer
    // state (incl. activeRun), so record the ask run AFTER selecting.
    await this.refetchThreads(projectId);
    this.selectThread(started.thread_id);
    if (this.state.selectedThreadId === started.thread_id) {
      this.set({
        activeRun: {
          runId: started.run_id,
          threadId: started.thread_id,
          mode: "ask",
          targetIds: null,
          lastProgress: null,
          recentProgress: [],
          liveAnswer: "",
        },
        runFailure: null,
      });
    }
  }

  /**
   * Retry a failed in-app ask (FR-5.3): re-spawn the same question into the same
   * thread. The thread returns to `generating` (via the `thread-updated` event);
   * we record the new `ask` run so the progress panel shows immediately.
   */
  async retryAsk(threadId: string): Promise<void> {
    const started = await api.retryAsk(threadId);
    if (this.state.selectedThreadId !== threadId) return;
    this.set({
      activeRun: {
        runId: started.run_id,
        threadId: started.thread_id,
        mode: "ask",
        targetIds: null,
        lastProgress: null,
        recentProgress: [],
        liveAnswer: "",
      },
      runFailure: null,
    });
  }

  /**
   * React to a `run-progress` event: keep the tracked run's activity line
   * fresh, and — if we see progress for the selected thread with no tracked
   * run (a run started before this thread was selected, or before app focus) —
   * re-attach via `get_active_run`.
   */
  handleRunProgress(payload: { run_id: string; thread_id: string; kind: string; detail: string }): void {
    this.updateSubmissionByRun(payload.run_id, { status: "running" });
    const run = this.state.activeRun;
    if (run != null && run.runId === payload.run_id) {
      if (payload.kind === "assistant_text_delta") {
        if (run.mode === "answer") this.set({ activeRun: { ...run, liveAnswer: `${run.liveAnswer}${payload.detail}`.slice(-12000) } });
        return;
      }
      const line = formatRunProgressLine(payload.kind, payload.detail);
      if (line == null) return; // non-actionable noise (e.g. an "allowed" rate-limit heartbeat)
      const recentProgress = [...run.recentProgress, line].slice(-MAX_RUN_PROGRESS_LINES);
      this.set({ activeRun: { ...run, lastProgress: line, recentProgress } });
      return;
    }
    if (run == null && payload.thread_id === this.state.selectedThreadId) {
      void this.syncActiveRun(payload.thread_id);
    }
  }

  handleCommentUpdated(payload: { thread_id: string; comment_id: string }): void {
    const run = this.state.activeRun;
    if (run?.mode === "answer" && run.threadId === payload.thread_id) {
      this.set({ activeRun: { ...run, liveAnswer: "" } });
    }
    void this.refetchComments(payload.thread_id);
  }

  handleRunStateChanged(payload: {
    run_id: string;
    thread_id: string;
    status: api.RunStatus;
  }): void {
    void this.refetchRunActivity();
    this.updateSubmissionByRun(payload.run_id, {
      status: askSubmissionStatus(payload.status),
    });
    if (payload.thread_id === this.state.selectedThreadId) {
      void this.syncActiveRun(payload.thread_id);
    }
  }

  /**
   * React to a `run-finished` event: drop the tracked run, surface a failure
   * panel for `failed`/`timeout` (FR-4.8 — the two are the same error class,
   * the message just says why), and reconcile comments/threads (answers landed
   * per-comment already; this catches anything missed).
   */
  handleRunFinished(payload: { run_id: string; thread_id: string; status: string }): void {
    void this.refetchRunActivity();
    const submissionStatus: AskSubmissionStatus =
      payload.status === "completed"
        ? "completed"
        : payload.status === "cancelled"
          ? "cancelled"
          : "failed";
    this.updateSubmissionByRun(payload.run_id, { status: submissionStatus });
    void this.refetchComments(payload.thread_id);
    void this.refetchThreads();

    const finishing =
      this.state.activeRun?.runId === payload.run_id ? this.state.activeRun : null;
    // An `ask` (generation) run's failure surfaces in the MAIN thread view — via
    // the thread's `error` status + the generation-error panel (log + Retry) —
    // not the comments sidebar. Detect it by the tracked run's mode, or (for an
    // untracked run) by the thread having no artifact yet (answer/apply runs
    // always operate on an existing artifact).
    const isGenerationRun =
      finishing?.mode === "ask" ||
      (finishing == null &&
        payload.thread_id === this.state.selectedThreadId &&
        this.state.artifactVersions.length === 0);

    // An "Ask now" single-comment run (epic conceptify-6xi) is an answer run
    // with exactly one target root — carry that root id onto the failure so the
    // sidebar highlights its chain. Batch/apply/reload-restored runs (no or many
    // targets) stay unscoped (`null`).
    const targetRootId =
      finishing != null &&
      finishing.mode === "answer" &&
      finishing.targetIds != null &&
      finishing.targetIds.length === 1
        ? finishing.targetIds[0]
        : null;
    const isRevisionPreview =
      payload.status === "conflicted" &&
      finishing?.mode === "apply" &&
      finishing.targetIds?.some((id) => {
        const anchor = this.state.comments.find((comment) => comment.id === id)?.anchor;
        if (anchor == null || typeof anchor.exploration !== "object" || anchor.exploration == null) return false;
        return (anchor.exploration as Record<string, unknown>).action === "change";
      });

    if (finishing != null) {
      this.set({ activeRun: null });
    }
    if (isRevisionPreview) {
      this.set({ conflictReviewRunId: payload.run_id });
    } else if (payload.status === "conflicted") {
      // A terminal state event may have cleared `activeRun` just before the
      // finished event. Ask the durable review row so targeted previews still
      // open automatically after reload/event reordering; stale-base conflicts
      // remain in the activity tray.
      void api.getConflictReview(payload.run_id).then((review) => {
        if (review.kind === "revision") this.set({ conflictReviewRunId: payload.run_id });
      }).catch(() => {});
    }
    if (
      !isGenerationRun &&
      (payload.status === "failed" || payload.status === "timeout") &&
      payload.thread_id === this.state.selectedThreadId
    ) {
      this.set({
        runFailure: {
          runId: payload.run_id,
          threadId: payload.thread_id,
          status: payload.status,
          targetRootId,
        },
      });
    }
  }

  /**
   * Re-attach to a possibly in-flight run for `threadId` (called on thread
   * switch and when progress events arrive untracked). A re-attached run has
   * no target ids (not persisted server-side) → indeterminate progress UI.
   */
  async syncActiveRun(threadId: string): Promise<void> {
    try {
      const run = await api.getActiveRun(threadId);
      if (this.state.selectedThreadId !== threadId) return;
      if (run != null) {
        if (this.state.activeRun?.runId === run.run_id) return; // already tracked, keep targets
        this.set({
          activeRun: {
            runId: run.run_id,
            threadId: run.thread_id,
            mode: run.mode,
            targetIds: null,
            lastProgress: null,
            recentProgress: [],
            liveAnswer: "",
          },
        });
      } else if (this.state.activeRun?.threadId === threadId) {
        this.set({ activeRun: null }); // stale — run ended while we weren't looking
      }
    } catch {
      // Non-fatal: the run block just won't render; run-finished still lands.
    }
  }

  /** Cancel the tracked run (FR-4.8 cancel button). Fire-and-forget: the
   *  authoritative `cancelled` arrives via `run-finished`. */
  cancelActiveRun(): void {
    const run = this.state.activeRun;
    if (run == null) return;
    api.cancelRun(run.runId).catch(() => {
      // Already finished (NotActive) — run-finished will clear the block.
    });
  }

  /** Dismiss the FR-4.8 failure panel. */
  clearRunFailure(): void {
    if (this.state.runFailure != null) this.set({ runFailure: null });
  }

  // ---- selection ----

  selectProject(id: string): void {
    if (id === this.state.selectedProjectId) return;
    this.set({
      selectedProjectId: id,
      selectedThreadId: null,
      threads: [],
      threadsError: null,
      ...clearedViewer,
    });
    void this.refetchThreads(id);
  }

  selectThread(id: string): void {
    if (id === this.state.selectedThreadId) return;
    this.set({ selectedThreadId: id, ...clearedViewer });
    void this.refetchArtifactVersions(id);
    void this.refetchComments(id);
    // Re-attach to a run already in flight on this thread (FR-4.8).
    void this.syncActiveRun(id);
  }

  /** Cold-safe navigation used by global search and external navigation. */
  async navigateTo(projectId: string, threadId: string | null): Promise<void> {
    if (projectId !== this.state.selectedProjectId) this.selectProject(projectId);
    if (threadId == null) return;
    await this.refetchThreads(projectId);
    this.selectThread(threadId);
  }

  openConceptEvidence(threadId: string, cfyId: string): void {
    this.set({ pendingConceptEvidence: { threadId, cfyId } });
    this.selectThread(threadId);
  }

  clearPendingConceptEvidence(): void {
    this.set({ pendingConceptEvidence: null });
  }

  async navigateToSearchHit(hit: api.SearchHit): Promise<void> {
    const target = hit.threadId == null ? null : hit.kind === "artifact" && hit.blockId != null
      ? { threadId: hit.threadId, kind: "artifact" as const, targetId: hit.blockId }
      : hit.kind === "comment"
        ? { threadId: hit.threadId, kind: "comment" as const, targetId: hit.entityId }
        : null;
    this.set({ pendingSearchTarget: target, searchNotice: null });
    await this.navigateTo(hit.projectId, hit.threadId);
  }

  finishSearchTarget(notice: string | null = null): void {
    this.set({ pendingSearchTarget: null, searchNotice: notice });
    if (notice != null) window.setTimeout(() => {
      if (this.state.searchNotice === notice) this.set({ searchNotice: null });
    }, 4200);
  }

  setShowArchived(showArchived: boolean): void {
    this.set({ showArchived });
    void this.refetchProjects();
  }

  // ---- mutations (refetch after so the UI reflects the change) ----

  async renameProject(id: string, name: string): Promise<void> {
    await api.renameProject(id, name);
    await this.refetchProjects();
  }

  async archiveProject(id: string, archived: boolean): Promise<void> {
    await api.setProjectArchived(id, archived);
    if (archived && this.state.selectedProjectId === id) {
      this.set({ selectedProjectId: null, selectedThreadId: null, threads: [], ...clearedViewer });
    }
    await this.refetchProjects();
  }

  /** Point a project at a new directory. Throws (invalid/missing path) so the
   *  caller can surface the message inline. */
  async remapProject(id: string, rootPath: string): Promise<void> {
    await api.remapProject(id, rootPath);
    await this.refetchProjects();
  }

  /**
   * Map an existing directory as a project (FR-1.2 / UC6, native dir-picker
   * path), then refetch and select it. Picking an already-mapped directory
   * lands on the existing project (`created: false`) rather than erroring, so
   * either way the sidebar ends up focused on the right project. Throws
   * (invalid/missing path) so the caller can surface the message inline.
   */
  async createProjectFromDir(rootPath: string): Promise<string> {
    const result = await api.ensureProject(rootPath, null);
    await this.refetchProjects();
    this.selectProject(result.id);
    this.set({
      projects: this.state.projects.map((project) =>
        project.id === result.id && project.context == null
          ? {
              ...project,
              context: {
                status: "scanning",
                repository: "Folder",
                languages: [],
                included_files: 0,
                excluded_paths: [],
                fingerprint: "",
                scanned_at: "",
                warning: null,
                unchanged: false,
              },
            }
          : project,
      ),
    });
    void api.scanProjectContext(result.id).then(() => this.refetchProjects()).catch(() => undefined);
    return result.id;
  }

  /**
   * Create a fresh topic folder under the auto-project base dir and map it
   * (FR-1.2 / UC6 "create a folder for me"), then refetch and select the new
   * project. Throws (empty name, unresolvable base dir, mkdir failure) so the
   * caller can surface the message inline.
   */
  async createProjectFolder(name: string, context?: api.TopicContext): Promise<string> {
    const result = await api.createProjectFolder(name);
    if (context != null && (context.notes !== "" || context.links.length > 0 || context.files.length > 0)) {
      await api.setTopicContext(result.id, context);
    }
    await this.refetchProjects();
    this.selectProject(result.id);
    return result.id;
  }

  async launchFirstQuestion(projectId: string, question: string, select = true): Promise<string | null> {
    const trimmed = question.trim();
    if (trimmed === "") return null;
    const preferences = await api.getResponsePreferences(projectId).catch(() => null);
    const started = await api.askFromApp(
      projectId,
      null,
      trimmed,
      null,
      preferences?.intent,
      "auto",
      [],
    );
    await this.refetchProjects();
    await this.refetchThreads(projectId);
    if (select) this.selectThread(started.thread_id);
    return started.thread_id;
  }

  /**
   * Delete a thread and all its data (bead conceptify-0kt hygiene valve): the
   * command removes the DB row (cascading to artifacts/comments/runs) and its
   * on-disk artifact dir. If the deleted thread was selected, its selection +
   * viewer state clear; then the thread list and project list (counts) refetch.
   * Throws (unknown thread / DB error) so the caller can surface the message.
   */
  async deleteThread(threadId: string): Promise<void> {
    const wasSelected = this.state.selectedThreadId === threadId;
    await api.deleteThread(threadId);
    if (wasSelected) {
      this.set({ selectedThreadId: null, ...clearedViewer });
    }
    await this.refetchThreads();
    await this.refetchProjects();
  }

  // ---- settings overlay (FR-7.x) ----

  openSettings(): void {
    if (!this.state.settingsOpen) this.set({ settingsOpen: true });
  }

  closeSettings(): void {
    if (this.state.settingsOpen) this.set({ settingsOpen: false });
  }
}

export const appStore = new AppStore();

/** Subscribe a component to the store; re-renders on every change. */
export function useAppStore(): AppState {
  const [snapshot, setSnapshot] = useState(appStore.getSnapshot());
  useEffect(() => {
    // Catch any change between the initial render and this subscription.
    setSnapshot(appStore.getSnapshot());
    return appStore.subscribe(() => setSnapshot(appStore.getSnapshot()));
  }, []);
  return snapshot;
}
