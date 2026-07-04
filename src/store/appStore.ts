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
import type { ArtifactVersion, Project, Thread } from "../lib/api";

/** Which artifact version the viewer shows: a concrete number (read-only
 *  history view) or `"latest"` (tracks new saves live, FR-2.4). */
export type ViewerVersion = number | "latest";

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
};

/** Fresh viewer state, applied whenever the selected thread changes/vanishes. */
const clearedViewer = {
  artifactVersions: [] as ArtifactVersion[],
  artifactVersionsLoading: false,
  artifactVersionsError: null,
  viewerVersion: "latest" as ViewerVersion,
};

class AppStore {
  private state: AppState = initialState;
  private listeners = new Set<Listener>();
  /** Monotonic token so a slow thread fetch can't clobber a newer one. */
  private threadFetchToken = 0;
  /** Same guard for artifact-version fetches (viewer switcher data). */
  private versionFetchToken = 0;

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
