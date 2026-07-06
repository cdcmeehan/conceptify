// In-app ask generation states for the main thread view (PRD §7.5, UC5 —
// FR-5.2 streaming progress, FR-5.3 error + retry).
//
// These render in the thread view's main area (where the artifact viewer will
// appear) while a thread has no saved artifact yet:
//  - `GenerationProgress` — the live progress panel for an in-flight `ask` run:
//    a spinner, a small rolling feed of parsed `run-progress` activity lines,
//    and a cancel button. The moment `save-artifact` lands, `artifact-updated`
//    records the version and the thread view swaps to the viewer (this panel
//    unmounts) — see ThreadView / appStore.handleArtifactUpdated.
//  - `GenerationError` — the FR-5.3 failure state: a message, the run log tail
//    (loaded on demand from the failed run resolved via `get_latest_run`), and
//    a one-click Retry that re-spawns the same question into the same thread.

import { useEffect, useState } from "preact/hooks";
import * as api from "../lib/api";
import type { LatestRun, RunLogTail } from "../lib/api";
import type { ActiveRunState } from "../store/appStore";
import { appStore } from "../store/appStore";

/**
 * Seconds elapsed since `key` (the run id) first appeared, ticking once a
 * second. Resets when the run changes (e.g. Retry spawns a new run id) so the
 * clock always reflects the current generation. Best-effort: for a run
 * re-attached after a thread switch we don't know the true start, so this
 * counts from when the panel began observing it — still enough to read as
 * "working", which is the point (FR-5.2, bead conceptify-pri). */
function useElapsedSeconds(key: string | undefined): number {
  const [elapsed, setElapsed] = useState(0);
  useEffect(() => {
    if (key == null) return;
    const start = Date.now();
    setElapsed(0);
    const id = setInterval(() => {
      setElapsed(Math.floor((Date.now() - start) / 1000));
    }, 1000);
    return () => clearInterval(id);
  }, [key]);
  return elapsed;
}

/** `mm:ss`, zero-padded (e.g. 95s → "01:35"). */
function formatElapsed(totalSeconds: number): string {
  const m = Math.floor(totalSeconds / 60);
  const s = totalSeconds % 60;
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

/** Spinner shared by the progress panel and the retry button. */
function Spinner({ class: cls }: { class?: string }) {
  return (
    <svg
      viewBox="0 0 20 20"
      fill="none"
      class={`animate-spin ${cls ?? ""}`}
      aria-hidden="true"
    >
      <circle cx="10" cy="10" r="7" stroke="currentColor" stroke-width="2" class="opacity-25" />
      <path d="M17 10a7 7 0 0 0-7-7" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
    </svg>
  );
}

/**
 * FR-5.2 live progress panel for an in-app `ask` run. `run` is the tracked
 * active run (may be `null` briefly — e.g. a thread mid-generation from another
 * surface before re-attachment, or right after Retry): fall back to a plain
 * spinner then.
 */
export function GenerationProgress({ run }: { run: ActiveRunState | null }) {
  const activity = run?.recentProgress ?? [];
  const elapsed = useElapsedSeconds(run?.runId);
  return (
    <section class="rounded-card border border-info/30 bg-info-bg p-5">
      <div class="flex items-center gap-2.5">
        <Spinner class="h-4 w-4 shrink-0 text-info" />
        <p class="flex-1 text-[13px] font-medium text-info">Generating artifact…</p>
        {run != null && (
          <span class="shrink-0 text-xs font-medium tabular-nums text-info">
            {formatElapsed(elapsed)}
          </span>
        )}
        {run != null && (
          <button
            type="button"
            onClick={() => appStore.cancelActiveRun()}
            title="Cancel this run (kills the agent process)"
            class="cfy-btn shrink-0 border border-info/40 text-info hover:bg-info/10"
          >
            Cancel
          </button>
        )}
      </div>
      <p class="mt-1.5 text-xs leading-relaxed text-muted">
        The agent is authoring your artifact. It appears here the moment it is saved.
        Complex explanations can take several minutes.
      </p>
      {activity.length > 0 && (
        <ul class="mt-3 select-text space-y-0.5 rounded-ctl bg-raised/70 p-2 font-mono text-[11px] leading-relaxed text-muted">
          {activity.map((line, i) => (
            <li key={i} class="truncate">
              {line}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/**
 * FR-5.3 generation-error state: a crash/timeout/cancel left the thread in
 * `error` with no artifact. Resolves the failed run (for its id + status) via
 * `get_latest_run`, offers its log tail on demand, and a one-click Retry that
 * re-spawns the same question into the same thread.
 */
export function GenerationError({ threadId }: { threadId: string }) {
  const [latest, setLatest] = useState<LatestRun | null>(null);
  const [tail, setTail] = useState<RunLogTail | null>(null);
  const [loadingTail, setLoadingTail] = useState(false);
  const [retrying, setRetrying] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    setLatest(null);
    setTail(null);
    api
      .getLatestRun(threadId)
      .then((run) => {
        if (live) setLatest(run);
      })
      .catch(() => {
        // Non-fatal: Retry still works without the run id; just no log button.
      });
    return () => {
      live = false;
    };
  }, [threadId]);

  const message =
    latest?.status === "timeout"
      ? "Generation timed out and was stopped."
      : latest?.status === "cancelled"
        ? "Generation was cancelled."
        : "Generation failed — no artifact was produced.";

  // Epic e7m (checkpoint e7m.5): show what the failed run used, and — when a
  // per-run override was recorded — that Retry will reuse it. An override-free
  // run retries with the *current* settings defaults, so no promise is shown.
  const routeLabel =
    latest?.route === "anthropic"
      ? "via claude CLI"
      : latest?.route === "openai"
        ? "via codex CLI"
        : latest?.route === "openrouter"
          ? "via OpenRouter"
          : latest?.route === "manual"
            ? "via manual adapter"
            : null;

  function onShowLog() {
    if (latest == null) return;
    setLoadingTail(true);
    setError(null);
    api
      .getRunLogTail(latest.run_id)
      .then(setTail)
      .catch((e) => setError(String(e)))
      .finally(() => setLoadingTail(false));
  }

  function onRetry() {
    setRetrying(true);
    setError(null);
    appStore
      .retryAsk(threadId)
      .catch((e) => setError(String(e)))
      .finally(() => setRetrying(false));
  }

  return (
    <section class="rounded-card border border-danger/30 bg-danger-bg p-5">
      <p class="text-[13px] font-medium text-danger">{message}</p>
      {latest != null && (
        <p class="mt-1 text-xs text-muted">
          Model: <span class="select-text font-mono">{latest.model}</span>
          {routeLabel != null && <> · {routeLabel}</>}
          {latest.overridden && <> · Retry reuses this override</>}
        </p>
      )}
      <div class="mt-3 flex flex-wrap items-center gap-2">
        <button
          type="button"
          onClick={onRetry}
          disabled={retrying}
          class="cfy-btn cfy-btn-primary px-3 py-1.5"
        >
          {retrying && <Spinner class="h-3.5 w-3.5" />}
          {retrying ? "Retrying…" : "Retry"}
        </button>
        {latest != null && tail == null && (
          <button
            type="button"
            onClick={onShowLog}
            disabled={loadingTail}
            class="cfy-btn border border-danger/40 text-danger hover:bg-danger/10 disabled:opacity-50"
          >
            {loadingTail ? "Loading…" : "Show log"}
          </button>
        )}
      </div>
      {error != null && (
        <p class="mt-2 break-words text-xs text-danger">{error}</p>
      )}
      {tail != null && (
        <div class="mt-3">
          <p class="select-text break-all font-mono text-[10px] text-muted">
            {tail.log_path}
          </p>
          <pre class="mt-1 max-h-64 select-text overflow-auto rounded-ctl bg-raised/70 p-2 font-mono text-[10px] leading-relaxed text-ink">
            {tail.lines.join("\n")}
          </pre>
        </div>
      )}
    </section>
  );
}
