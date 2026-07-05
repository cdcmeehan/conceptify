// Single Tauri-event subscription layer (PRD §5.3, bead conceptify-qxr.5).
//
// The Rust core emits events (documented in docs/api.md "Events") whenever a
// mutation happens over the HTTP API / CLI. This module is the one place that
// translates those events into calls on `appStore`, so the shell updates lists
// live instead of polling. Keeping it centralized (not scattered per-component)
// is deliberate: every later live feature (viewer refresh, sidebar answers)
// reuses this seam and these event names.
//
// The frontend's own mutations refetch after themselves (see appStore), so this
// wiring only ever fires for CLI/API-originated changes.

import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { appStore } from "../store/appStore";

interface ThreadCreatedPayload {
  project_id: string;
  thread_id: string;
}

interface NavigatePayload {
  project_id: string;
  thread_id: string | null;
}

interface ArtifactUpdatedPayload {
  project_id: string;
  thread_id: string;
  version: number;
}

interface CommentEventPayload {
  project_id: string;
  thread_id: string;
  comment_id: string;
}

interface ThreadUpdatedPayload {
  project_id: string;
  thread_id: string;
  status: string;
}

interface RunProgressPayload {
  run_id: string;
  thread_id: string;
  kind: string;
  detail: string;
}

interface RunFinishedPayload {
  run_id: string;
  thread_id: string;
  status: string;
}

/**
 * Subscribe to the core's Tauri events and drive `appStore`. Call once at
 * startup; returns a cleanup function that removes every listener.
 *
 * This is a single-window app so the listeners effectively live for the whole
 * session — the cleanup exists for correctness (HMR, and so a caller can tear
 * the wiring down without leaking Tauri subscriptions). `listen()` resolves
 * asynchronously, so cleanup awaits each pending registration before unlistening
 * (handles teardown that races an in-flight subscribe).
 */
export function initEventListeners(): () => void {
  const pending: Promise<UnlistenFn>[] = [
    // A project was created / renamed / archived elsewhere (CLI or API).
    listen("projects-changed", () => {
      void appStore.refetchProjects();
    }),

    // A thread was created (CLI or API). Refresh the project list (thread counts
    // + last-activity ordering) and, when that project is on screen, its threads.
    listen<ThreadCreatedPayload>("thread-created", (event) => {
      void appStore.refetchProjects();
      void appStore.refetchThreads(event.payload.project_id);
    }),

    // An artifact version was saved (save-artifact endpoint). Drives the
    // viewer's live refresh (< 500ms, PRD N2) plus list/status updates —
    // all the logic lives on the store seam.
    listen<ArtifactUpdatedPayload>("artifact-updated", (event) => {
      appStore.handleArtifactUpdated(event.payload);
    }),

    // A comment was created via the API/CLI (M5 headless flows, or another
    // surface). Refresh the affected thread's comments — a no-op unless it's the
    // thread on screen — so the sidebar/highlights pick it up live (FR-4.5); and
    // its project's threads for the open-comment badge. The shell's own
    // create (94m.3/94m.4) updates the store directly and emits no event, so
    // this only ever fires for cross-surface mutations.
    listen<CommentEventPayload>("comment-created", (event) => {
      void appStore.refetchComments(event.payload.thread_id);
      void appStore.refetchThreads(event.payload.project_id);
    }),

    // A comment mutated (answered/applied/anchor moved — M5 resolve flow). Same
    // scoped refresh: the sidebar re-renders the new status/answer, highlights
    // drop a no-longer-open comment, and the badge count updates.
    listen<CommentEventPayload>("comment-updated", (event) => {
      void appStore.refetchComments(event.payload.thread_id);
      void appStore.refetchThreads(event.payload.project_id);
    }),

    // A thread's status changed under the run lifecycle (M5 flows: apply-mode
    // `updating` → `ready`, PRD §4). Refresh the thread list so status chips
    // go live, and the project list for last-activity ordering.
    listen<ThreadUpdatedPayload>("thread-updated", (event) => {
      void appStore.refetchThreads(event.payload.project_id);
      void appStore.refetchProjects();
    }),

    // A headless follow-up run emitted a stdout line (FR-4.8). Feeds the run
    // status block's activity line; also lets the store re-attach to a run it
    // isn't tracking yet.
    listen<RunProgressPayload>("run-progress", (event) => {
      appStore.handleRunProgress(event.payload);
    }),

    // A headless follow-up run reached a terminal state (FR-4.8): clear the
    // run block, surface failures, reconcile lists.
    listen<RunFinishedPayload>("run-finished", (event) => {
      appStore.handleRunFinished(event.payload);
    }),

    // The user asked to open a specific project/thread (e.g. `conceptify open`).
    // The window is focused server-side; the frontend routes to the target here.
    listen<NavigatePayload>("navigate", (event) => {
      void navigateTo(event.payload);
    }),
  ];

  return () => {
    for (const registration of pending) {
      void registration.then((unlisten) => unlisten());
    }
  };
}

/**
 * Route the shell to a project (and optionally a thread) named by a `navigate`
 * event. Ordering matters: the target may have just been created via the CLI, so
 * refresh the project list first, then ensure the project's threads are loaded
 * before selecting the thread — otherwise `selectThread` would point at a thread
 * that isn't in the list yet and the selection wouldn't stick.
 */
async function navigateTo(payload: NavigatePayload): Promise<void> {
  await appStore.refetchProjects();
  appStore.selectProject(payload.project_id);

  if (payload.thread_id != null) {
    // `refetchThreads` is a no-op unless this project is the selected one, which
    // it now is. Awaiting it guarantees the target thread is present (covers both
    // a project switch and an already-selected project with a brand-new thread).
    await appStore.refetchThreads(payload.project_id);
    appStore.selectThread(payload.thread_id);
  }
}
