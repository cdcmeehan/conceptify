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
import type { Project, Thread } from "../lib/api";

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
};

class AppStore {
  private state: AppState = initialState;
  private listeners = new Set<Listener>();
  /** Monotonic token so a slow thread fetch can't clobber a newer one. */
  private threadFetchToken = 0;

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
      });
    } catch (e) {
      if (token !== this.threadFetchToken) return;
      this.set({ threadsLoading: false, threadsError: String(e) });
    }
  }

  // ---- selection ----

  selectProject(id: string): void {
    if (id === this.state.selectedProjectId) return;
    this.set({
      selectedProjectId: id,
      selectedThreadId: null,
      threads: [],
      threadsError: null,
    });
    void this.refetchThreads(id);
  }

  selectThread(id: string): void {
    if (id === this.state.selectedThreadId) return;
    this.set({ selectedThreadId: id });
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
      this.set({ selectedProjectId: null, selectedThreadId: null, threads: [] });
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
