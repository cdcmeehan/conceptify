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
import type { ArtifactVersion, Comment, Project, RunMode, Thread } from "../lib/api";

/** Which artifact version the viewer shows: a concrete number (read-only
 *  history view) or `"latest"` (tracks new saves live, FR-2.4). */
export type ViewerVersion = number | "latest";

/**
 * A follow-up run the sidebar is tracking (FR-4.8). `targetIds` is the set of
 * comments the run was started for — per-comment progress is *derived* from
 * the store's comment statuses against this set as `comment-updated` events
 * land (no separate counter to drift). `null` when the run was re-attached
 * via `get_active_run` (targets aren't persisted): render an indeterminate
 * spinner. `lastProgress` is the most recent `run-progress` detail line.
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
}

/** How many `run-progress` lines the rolling activity feed keeps (FR-5.2). */
const MAX_RUN_PROGRESS_LINES = 8;

/** A terminal failure (`failed`/`timeout`) of the latest run on the selected
 *  thread (FR-4.8): drives the inline failure panel with the on-demand log
 *  tail. Cleared on dismiss, thread switch, or a new run start. */
export interface RunFailureState {
  runId: string;
  threadId: string;
  status: "failed" | "timeout";
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
};

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
  async askFollowUps(threadId: string): Promise<void> {
    const started = await api.askFollowUps(threadId);
    if (this.state.selectedThreadId !== started.thread_id) return;
    this.set({
      activeRun: {
        runId: started.run_id,
        threadId: started.thread_id,
        mode: started.mode,
        targetIds: started.target_comment_ids,
        lastProgress: null,
        recentProgress: [],
      },
      runFailure: null,
    });
  }

  /**
   * Start the FR-4.7 "Apply to artifact" run for the given comments (empty =
   * all answered). Same throw/record contract as `askFollowUps`. The thread's
   * `updating` status chip arrives via the `thread-updated` event.
   */
  async applyToArtifact(threadId: string, commentIds: string[]): Promise<void> {
    const started = await api.applyToArtifact(threadId, commentIds);
    if (this.state.selectedThreadId !== started.thread_id) return;
    this.set({
      activeRun: {
        runId: started.run_id,
        threadId: started.thread_id,
        mode: started.mode,
        targetIds: started.target_comment_ids,
        lastProgress: null,
        recentProgress: [],
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
  async askFromApp(title: string | null, question: string): Promise<void> {
    const projectId = this.state.selectedProjectId;
    if (projectId == null) throw new Error("select a project first");

    const started = await api.askFromApp(projectId, title, question);
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
    const run = this.state.activeRun;
    if (run != null && run.runId === payload.run_id) {
      const line = payload.detail ? `${payload.kind}: ${payload.detail}` : payload.kind;
      const recentProgress = [...run.recentProgress, line].slice(-MAX_RUN_PROGRESS_LINES);
      this.set({ activeRun: { ...run, lastProgress: payload.detail, recentProgress } });
      return;
    }
    if (run == null && payload.thread_id === this.state.selectedThreadId) {
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

    if (finishing != null) {
      this.set({ activeRun: null });
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
  async createProjectFromDir(rootPath: string): Promise<void> {
    const result = await api.ensureProject(rootPath, null);
    await this.refetchProjects();
    this.selectProject(result.id);
  }

  /**
   * Create a fresh topic folder under the auto-project base dir and map it
   * (FR-1.2 / UC6 "create a folder for me"), then refetch and select the new
   * project. Throws (empty name, unresolvable base dir, mkdir failure) so the
   * caller can surface the message inline.
   */
  async createProjectFolder(name: string): Promise<void> {
    const result = await api.createProjectFolder(name);
    await this.refetchProjects();
    this.selectProject(result.id);
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
