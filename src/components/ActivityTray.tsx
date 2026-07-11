import { useEffect, useState } from "preact/hooks";
import type { RunActivity, RunStatus } from "../lib/api";
import { appStore } from "../store/appStore";

const ACTIVE = new Set<RunStatus>(["queued", "starting", "running", "throttled", "cancelling"]);
const ATTENTION = new Set<RunStatus>(["failed", "timeout", "conflicted"]);

export function ActivityTray({
  activity,
  loading,
  open,
}: {
  activity: RunActivity[];
  loading: boolean;
  open: boolean;
}) {
  const [, tick] = useState(0);
  useEffect(() => {
    if (!open) return;
    const timer = window.setInterval(() => tick((value) => value + 1), 1000);
    return () => window.clearInterval(timer);
  }, [open]);

  const activeCount = activity.filter((item) => ACTIVE.has(item.status)).length;
  const attentionCount = activity.filter((item) => ATTENTION.has(item.status)).length;
  const unseenCount = activity.filter((item) => !ACTIVE.has(item.status) && !item.seen).length;
  const badgeCount = activeCount + unseenCount;
  const completedCount = activity.filter((item) =>
    item.status === "completed" || item.status === "cancelled",
  ).length;

  return (
    <>
      <div class="sr-only" aria-live="polite" aria-atomic="true">
        {activeCount} active, {attentionCount} need attention.
      </div>
      {open && (
        <aside
          class="fixed bottom-14 right-3 z-40 flex max-h-[min(70vh,620px)] w-[min(24rem,calc(100vw-1.5rem))] flex-col overflow-hidden rounded-card border border-line bg-paper shadow-xl"
          aria-label="Global activity"
        >
          <header class="flex items-center gap-2 border-b border-line px-3 py-2.5">
            <div class="min-w-0 flex-1">
              <h2 class="font-serif text-base font-semibold text-ink">Activity</h2>
              <p class="text-[11px] text-muted">Work across every project</p>
            </div>
            {completedCount > 0 && (
              <button
                type="button"
                onClick={() => void appStore.clearCompletedActivity()}
                class="cfy-btn cfy-btn-ghost text-[11px]"
              >
                Clear complete
              </button>
            )}
            <button
              type="button"
              onClick={() => appStore.closeActivityTray()}
              class="cfy-btn cfy-btn-ghost h-7 w-7 p-0 text-base"
              aria-label="Close activity"
            >
              ×
            </button>
          </header>
          <div class="min-h-0 flex-1 overflow-y-auto p-2">
            {loading && activity.length === 0 ? (
              <div class="flex flex-col gap-2 p-2" aria-hidden="true">
                <div class="cfy-skeleton w-11/12" />
                <div class="cfy-skeleton w-3/4" />
              </div>
            ) : activity.length === 0 ? (
              <div class="px-6 py-10 text-center">
                <p class="font-serif text-sm font-semibold text-ink">All quiet</p>
                <p class="mt-1 text-xs text-muted">Queued and recent work will appear here.</p>
              </div>
            ) : (
              <ul class="flex flex-col gap-1.5">
                {activity.map((item) => (
                  <ActivityRow key={item.run_id} item={item} />
                ))}
              </ul>
            )}
          </div>
        </aside>
      )}
      <button
        type="button"
        onClick={() => (open ? appStore.closeActivityTray() : appStore.openActivityTray())}
        class="fixed bottom-3 right-3 z-40 flex items-center gap-2 rounded-full border border-line bg-paper px-3 py-2 text-xs font-medium text-ink shadow-lg transition-colors hover:border-accent/50 hover:bg-hover"
        aria-expanded={open}
        aria-label={`Activity, ${activeCount} active, ${attentionCount} need attention, ${unseenCount} new`}
      >
        <span class={`h-2 w-2 rounded-full ${activeCount > 0 ? "animate-pulse bg-info" : attentionCount > 0 ? "bg-danger" : "bg-muted/50"}`} />
        Activity
        {badgeCount > 0 && (
          <span class="cfy-chip bg-accent-bg tabular-nums text-accent-ink">{badgeCount}</span>
        )}
      </button>
    </>
  );
}

function ActivityRow({ item }: { item: RunActivity }) {
  const meta = activityMeta(item);
  const cancellable = ACTIVE.has(item.status) && item.status !== "cancelling";
  const retryable = ATTENTION.has(item.status) && item.mode === "ask";
  const reviewable = item.status === "conflicted";
  const dismissible = !ACTIVE.has(item.status) && item.status !== "conflicted";
  const timingStart = item.execution_started_at ?? item.queued_at;
  const timing = timingStart == null
    ? null
    : elapsed(timingStart, ACTIVE.has(item.status) ? null : item.finished_at);

  return (
    <li class={`rounded-ctl border px-2.5 py-2 ${meta.rowClass}`}>
      <div class="flex items-start gap-2">
        <span class={`mt-1.5 h-2 w-2 shrink-0 rounded-full ${meta.dotClass}`} aria-hidden="true" />
        <div class="min-w-0 flex-1">
          <div class="flex items-start justify-between gap-2">
            <p class="line-clamp-2 text-xs font-medium leading-snug text-ink">{item.thread_title}</p>
            <span class={`cfy-chip shrink-0 ${meta.chipClass}`}>{meta.label}</span>
          </div>
          <p class="mt-0.5 truncate text-[10px] text-muted">
            {item.project_name} · {item.model}
          </p>
          <p class="mt-1 text-[10px] tabular-nums text-muted">
            {meta.detail}
            {timing != null ? ` · ${timing}` : ""}
          </p>
          <div class="mt-1.5 flex items-center justify-end gap-2 text-[10px]">
            <button
              type="button"
              onClick={() => void appStore.jumpToRunActivity(item)}
              class="text-accent-ink hover:underline"
            >
              Jump to thread
            </button>
            {cancellable && (
              <button
                type="button"
                onClick={() => void appStore.cancelRunActivity(item)}
                class="text-muted hover:text-danger"
              >
                Cancel
              </button>
            )}
            {retryable && (
              <button
                type="button"
                onClick={() => void appStore.retryRunActivity(item)}
                class="text-accent-ink hover:underline"
              >
                Retry
              </button>
            )}
            {reviewable && (
              <button
                type="button"
                onClick={() => appStore.openConflictReview(item.run_id)}
                class="text-accent-ink hover:underline"
              >
                Review conflict
              </button>
            )}
            {dismissible && (
              <button
                type="button"
                onClick={() => void appStore.dismissRunActivity(item.run_id)}
                class="text-muted hover:text-ink"
              >
                Dismiss
              </button>
            )}
          </div>
        </div>
      </div>
    </li>
  );
}

function activityMeta(item: RunActivity): {
  label: string;
  detail: string;
  rowClass: string;
  chipClass: string;
  dotClass: string;
} {
  switch (item.status) {
    case "queued":
      return {
        label: item.queue_position != null ? `Queued · #${item.queue_position}` : "Queued",
        detail: "Waiting for provider capacity",
        rowClass: "border-line bg-well/45",
        chipClass: "bg-well text-muted",
        dotClass: "bg-muted/60",
      };
    case "starting":
      return activeMeta("Starting", "Preparing the agent");
    case "running":
      return activeMeta(
        item.mode === "apply" ? "Applying" : item.mode === "answer" ? "Answering" : "Generating",
        "Agent is working",
      );
    case "throttled":
      return {
        label: "Provider wait",
        detail: "Will retry automatically",
        rowClass: "border-warn/25 bg-warn-bg/50",
        chipClass: "bg-warn-bg text-warn",
        dotClass: "bg-warn",
      };
    case "cancelling":
      return activeMeta("Cancelling", "Stopping the agent safely");
    case "failed":
    case "timeout":
    case "conflicted":
      return {
        label: item.status === "conflicted" ? "Conflict" : "Needs attention",
        detail: item.status_reason ?? (item.status === "timeout" ? "Run timed out" : "Run failed"),
        rowClass: "border-danger/25 bg-danger-bg/45",
        chipClass: "bg-danger-bg text-danger",
        dotClass: "bg-danger",
      };
    case "completed":
      return terminalMeta("Complete", "Finished successfully");
    case "cancelled":
      return terminalMeta("Cancelled", "Stopped by you");
  }
}

function activeMeta(label: string, detail: string) {
  return {
    label,
    detail,
    rowClass: "border-info/25 bg-info-bg/35",
    chipClass: "bg-info-bg text-info",
    dotClass: "animate-pulse bg-info",
  };
}

function terminalMeta(label: string, detail: string) {
  return {
    label,
    detail,
    rowClass: "border-line bg-paper",
    chipClass: "bg-ok-bg text-ok",
    dotClass: "bg-ok",
  };
}

function elapsed(startIso: string, endIso: string | null): string {
  const end = endIso == null ? Date.now() : Date.parse(endIso);
  const ms = Math.max(0, end - Date.parse(startIso));
  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${seconds % 60}s`;
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}
