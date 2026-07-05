// Thread list for the selected project (FR-2.2). The core already returns
// threads sorted by last activity, so this renders them in order with a status
// chip and open-comment count. Arrow keys move the selection when focused.

import { useState } from "preact/hooks";
import type { Thread } from "../lib/api";
import { appStore } from "../store/appStore";
import { relativeTime } from "../lib/time";
import { NewThreadComposer } from "./NewThreadComposer";
import { StatusChip } from "./StatusChip";

/** A `generating` thread idle this long is treated as stalled (bead
 *  conceptify-0kt option b-lite) — the authoring run likely died. Visual only. */
const STALL_MS = 30 * 60 * 1000;

/** Whether a thread's chip should render as "stalled": still `generating` well
 *  past the threshold (no artifact save has bumped `updated_at`). Cheap and
 *  time-based — it re-evaluates on each render (selection, refetch), no timer. */
function isStalled(thread: Thread): boolean {
  if (thread.status !== "generating") return false;
  const updated = Date.parse(thread.updated_at);
  return Number.isFinite(updated) && Date.now() - updated > STALL_MS;
}

interface Props {
  threads: Thread[];
  selectedThreadId: string | null;
  projectSelected: boolean;
  projectName: string | null;
  loading: boolean;
  error: string | null;
}

export function ThreadList({
  threads,
  selectedThreadId,
  projectSelected,
  projectName,
  loading,
  error,
}: Props) {
  // FR-5.1 in-app ask composer, toggled by the "New thread" header button.
  const [composerOpen, setComposerOpen] = useState(false);
  // Thread delete (bead conceptify-0kt): id awaiting inline confirmation, plus
  // in-flight + error state for the delete request.
  const [confirmingDeleteId, setConfirmingDeleteId] = useState<string | null>(null);
  const [deleteBusy, setDeleteBusy] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);

  function confirmDelete(threadId: string) {
    setDeleteBusy(true);
    setDeleteError(null);
    appStore
      .deleteThread(threadId)
      .then(() => setConfirmingDeleteId(null))
      .catch((e) => setDeleteError(String(e)))
      .finally(() => setDeleteBusy(false));
  }

  function onListKeyDown(e: KeyboardEvent) {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    if (threads.length === 0) return;
    e.preventDefault();

    const index = threads.findIndex((t) => t.id === selectedThreadId);
    const delta = e.key === "ArrowDown" ? 1 : -1;
    const next = index < 0 ? (delta === 1 ? 0 : threads.length - 1) : index + delta;
    const clamped = Math.max(0, Math.min(threads.length - 1, next));
    appStore.selectThread(threads[clamped].id);
  }

  return (
    <section
      class="flex h-full w-72 shrink-0 flex-col border-r border-neutral-200 bg-white outline-none dark:border-neutral-800 dark:bg-neutral-950"
      tabIndex={0}
      onKeyDown={onListKeyDown}
      aria-label="Threads"
    >
      <header class="flex items-center gap-2 px-3 py-2.5">
        <h2 class="min-w-0 flex-1 truncate text-xs font-semibold uppercase tracking-wide text-neutral-500 dark:text-neutral-400">
          {projectName ?? "Threads"}
        </h2>
        {projectSelected && !composerOpen && (
          <button
            type="button"
            onClick={() => setComposerOpen(true)}
            title="Ask a new question in this project"
            class="inline-flex shrink-0 items-center gap-1 rounded-md bg-blue-600 px-2 py-1 text-xs font-medium text-white transition-colors hover:bg-blue-700"
          >
            <svg viewBox="0 0 20 20" fill="none" class="h-3.5 w-3.5" aria-hidden="true">
              <path
                d="M10 4.5v11M4.5 10h11"
                stroke="currentColor"
                stroke-width="1.75"
                stroke-linecap="round"
              />
            </svg>
            New thread
          </button>
        )}
      </header>

      {projectSelected && composerOpen && (
        <div class="px-2 pb-2">
          <NewThreadComposer onClose={() => setComposerOpen(false)} />
        </div>
      )}

      <div class="min-h-0 flex-1 overflow-y-auto px-2 pb-2">
        {!projectSelected ? (
          <p class="px-2 py-3 text-xs text-neutral-400">Select a project.</p>
        ) : error != null ? (
          <p class="px-2 py-3 text-xs text-rose-600 dark:text-rose-400">{error}</p>
        ) : loading && threads.length === 0 ? (
          <p class="px-2 py-3 text-xs text-neutral-400">Loading…</p>
        ) : threads.length === 0 ? (
          <p class="px-2 py-3 text-xs text-neutral-400">No threads in this project yet.</p>
        ) : (
          <ul class="flex flex-col gap-0.5">
            {threads.map((thread) => {
              const selected = thread.id === selectedThreadId;
              return (
                <li key={thread.id}>
                  <div
                    role="button"
                    tabIndex={-1}
                    onClick={() => appStore.selectThread(thread.id)}
                    class={`w-full cursor-pointer rounded-md px-2 py-2 text-left transition-colors ${
                      selected
                        ? "bg-blue-600/10 dark:bg-blue-500/20"
                        : "hover:bg-neutral-100 dark:hover:bg-neutral-900"
                    }`}
                  >
                    <div class="flex items-start justify-between gap-2">
                      <span class="line-clamp-2 text-sm font-medium text-neutral-800 dark:text-neutral-100">
                        {thread.title}
                      </span>
                      {thread.open_comment_count > 0 && (
                        <span
                          class="mt-0.5 shrink-0 rounded-full bg-blue-100 px-1.5 text-xs font-medium tabular-nums text-blue-700 dark:bg-blue-500/20 dark:text-blue-300"
                          title={`${thread.open_comment_count} open comment${thread.open_comment_count === 1 ? "" : "s"}`}
                        >
                          {thread.open_comment_count}
                        </span>
                      )}
                    </div>
                    <div class="mt-1.5 flex items-center justify-between gap-2">
                      <StatusChip status={thread.status} stalled={isStalled(thread)} />
                      <span class="shrink-0 text-xs text-neutral-400">
                        {relativeTime(thread.updated_at)}
                      </span>
                    </div>

                    {/* Delete affordance (bead conceptify-0kt): the hygiene
                        valve for a thread stuck in generating (also useful for
                        any unwanted thread). Shown on the selected thread with
                        an inline confirm — deletes the thread and all its data
                        (comments/artifacts/runs cascade + artifact dir). */}
                    {selected && (
                      <div class="mt-1.5" onClick={(e) => e.stopPropagation()}>
                        {confirmingDeleteId === thread.id ? (
                          <div class="flex flex-col gap-1">
                            <span class="text-xs text-neutral-500 dark:text-neutral-400">
                              Delete this thread and all its data?
                            </span>
                            <div class="flex items-center gap-2">
                              <button
                                type="button"
                                disabled={deleteBusy}
                                onClick={() => confirmDelete(thread.id)}
                                class="rounded bg-rose-600 px-2 py-0.5 text-xs font-medium text-white hover:bg-rose-700 disabled:opacity-50"
                              >
                                {deleteBusy ? "Deleting…" : "Delete"}
                              </button>
                              <button
                                type="button"
                                disabled={deleteBusy}
                                onClick={() => setConfirmingDeleteId(null)}
                                class="rounded px-2 py-0.5 text-xs text-neutral-500 hover:text-neutral-800 disabled:opacity-50 dark:hover:text-neutral-200"
                              >
                                Cancel
                              </button>
                            </div>
                            {deleteError != null && (
                              <span class="text-[11px] text-rose-600 dark:text-rose-400">
                                {deleteError}
                              </span>
                            )}
                          </div>
                        ) : (
                          <button
                            type="button"
                            onClick={() => {
                              setDeleteError(null);
                              setConfirmingDeleteId(thread.id);
                            }}
                            class="text-xs text-neutral-500 hover:text-rose-600 dark:text-neutral-400 dark:hover:text-rose-400"
                          >
                            Delete
                          </button>
                        )}
                      </div>
                    )}
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </section>
  );
}
