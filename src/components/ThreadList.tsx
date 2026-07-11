// Thread list for the selected project (FR-2.2). The core already returns
// threads sorted by last activity, so this renders them in order with a status
// chip and open-comment count. Arrow keys move the selection when focused.

import { useEffect, useState } from "preact/hooks";
import type { RunActivity, Thread } from "../lib/api";
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
  projectId: string | null;
  projectName: string | null;
  loading: boolean;
  error: string | null;
  runActivity: RunActivity[];
}

export function ThreadList({
  threads,
  selectedThreadId,
  projectSelected,
  projectId,
  projectName,
  loading,
  error,
  runActivity,
}: Props) {
  // FR-5.1 in-app ask composer, toggled by the "New thread" header button.
  const [composerOpen, setComposerOpen] = useState(false);
  // Thread delete (bead conceptify-0kt): id awaiting inline confirmation, plus
  // in-flight + error state for the delete request.
  const [confirmingDeleteId, setConfirmingDeleteId] = useState<string | null>(null);
  const [deleteBusy, setDeleteBusy] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const [, tick] = useState(0);
  useEffect(() => {
    if (!runActivity.some((item) => ["queued", "starting", "running", "throttled", "cancelling"].includes(item.status))) return;
    const timer = window.setInterval(() => tick((value) => value + 1), 1000);
    return () => window.clearInterval(timer);
  }, [runActivity]);

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
      class="flex h-full w-60 shrink-0 flex-col border-r border-line bg-paper outline-none lg:w-72"
      tabIndex={0}
      onKeyDown={onListKeyDown}
      aria-label="Threads"
    >
      <header class="flex items-center gap-2 px-3 py-2.5">
        <h2 class="cfy-label min-w-0 flex-1 truncate" title={projectName ?? undefined}>
          {projectName ?? "Threads"}
        </h2>
        {projectSelected && !composerOpen && (
          <button
            type="button"
            onClick={() => setComposerOpen(true)}
            title="Ask a new question in this project"
            class="cfy-btn cfy-btn-primary shrink-0 px-2 py-1"
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

      {projectSelected && projectId != null && composerOpen && (
        <div class="max-h-[68%] overflow-y-auto px-2 pb-2">
          <NewThreadComposer projectId={projectId} onClose={() => setComposerOpen(false)} />
        </div>
      )}

      <div class="min-h-0 flex-1 overflow-y-auto px-2 pb-2">
        {!projectSelected ? (
          <div class="px-3 py-10 text-center">
            <p class="text-xs leading-relaxed text-muted">
              Select a project to see its threads.
            </p>
          </div>
        ) : error != null ? (
          <p class="px-2 py-3 text-xs text-danger">{error}</p>
        ) : loading && threads.length === 0 ? (
          <div class="flex flex-col gap-2.5 px-2 py-3" aria-hidden="true">
            <div class="cfy-skeleton w-11/12" />
            <div class="cfy-skeleton w-2/3" />
            <div class="cfy-skeleton w-4/5" />
          </div>
        ) : threads.length === 0 ? (
          // Empty project (bead conceptify-vxc): a sentence + the next action.
          <div class="px-3 py-10 text-center">
            <p class="font-serif text-sm font-semibold text-ink">Nothing asked yet</p>
            <p class="mt-1 text-xs leading-relaxed text-muted">
              Every question becomes a thread with a visual artifact.
            </p>
            {!composerOpen && (
              <button
                type="button"
                onClick={() => setComposerOpen(true)}
                class="cfy-btn cfy-btn-primary mt-3"
              >
                Ask a question
              </button>
            )}
          </div>
        ) : (
          <ul class="flex flex-col gap-0.5">
            {threads.map((thread) => {
              const selected = thread.id === selectedThreadId;
              const run = runActivity.find(
                (item) =>
                  item.thread_id === thread.id &&
                  ["queued", "starting", "running", "throttled", "cancelling"].includes(item.status),
              );
              return (
                <li key={thread.id}>
                  <div
                    role="button"
                    tabIndex={-1}
                    onClick={() => appStore.selectThread(thread.id)}
                    class={`w-full rounded-ctl px-2 py-2 text-left transition-colors ${
                      selected ? "bg-accent-bg" : "hover:bg-hover"
                    }`}
                  >
                    <div class="flex items-start justify-between gap-2">
                      <span
                        class="line-clamp-2 text-[13px] font-medium text-ink"
                        title={thread.title}
                      >
                        {thread.title}
                      </span>
                      {thread.open_comment_count > 0 && (
                        <span
                          class="cfy-chip mt-0.5 shrink-0 bg-info-bg tabular-nums text-info"
                          title={`${thread.open_comment_count} open comment${thread.open_comment_count === 1 ? "" : "s"}`}
                        >
                          {thread.open_comment_count}
                        </span>
                      )}
                    </div>
                    <div class="mt-1.5 flex items-center justify-between gap-2">
                      {run != null ? (
                        <ThreadRunStage run={run} />
                      ) : (
                        <StatusChip status={thread.status} stalled={isStalled(thread)} />
                      )}
                      <span class="shrink-0 text-[11px] text-muted">
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
                          <div class="flex flex-col gap-1.5">
                            <span class="text-xs text-muted">
                              Delete this thread and all its data?
                            </span>
                            <div class="flex items-center gap-1.5">
                              <button
                                type="button"
                                disabled={deleteBusy}
                                onClick={() => confirmDelete(thread.id)}
                                class="cfy-btn cfy-btn-danger px-2 py-0.5 text-[11px]"
                              >
                                {deleteBusy ? "Deleting…" : "Delete"}
                              </button>
                              <button
                                type="button"
                                disabled={deleteBusy}
                                onClick={() => setConfirmingDeleteId(null)}
                                class="cfy-btn cfy-btn-ghost px-2 py-0.5 text-[11px]"
                              >
                                Cancel
                              </button>
                            </div>
                            {deleteError != null && (
                              <span class="text-[11px] text-danger">{deleteError}</span>
                            )}
                          </div>
                        ) : (
                          <button
                            type="button"
                            onClick={() => {
                              setDeleteError(null);
                              setConfirmingDeleteId(thread.id);
                            }}
                            class="rounded text-[11px] text-muted transition-colors hover:text-danger"
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

function ThreadRunStage({ run }: { run: RunActivity }) {
  const label =
    run.status === "queued"
      ? run.queue_position != null ? `Queued #${run.queue_position}` : "Queued"
      : run.status === "throttled"
        ? "Provider wait"
        : run.status === "cancelling"
          ? "Cancelling"
          : run.mode === "apply"
            ? "Applying"
            : run.mode === "answer"
              ? "Answering"
              : "Generating";
  const started = run.execution_started_at ?? run.queued_at;
  return (
    <span class="cfy-chip bg-info-bg text-info" title={`${label} · ${run.model}`}>
      <span class={`h-1.5 w-1.5 rounded-full ${run.status === "queued" ? "bg-muted/60" : "animate-pulse bg-info"}`} />
      {label}
      {started != null && <span class="tabular-nums opacity-80">{shortElapsed(started)}</span>}
    </span>
  );
}

function shortElapsed(iso: string): string {
  const seconds = Math.max(0, Math.floor((Date.now() - Date.parse(iso)) / 1000));
  if (seconds < 60) return `${seconds}s`;
  return `${Math.floor(seconds / 60)}m`;
}
