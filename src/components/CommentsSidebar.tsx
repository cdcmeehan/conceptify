// Comments sidebar — the interrogation home base (PRD FR-4.5, §7.4; bead
// conceptify-94m.6). A collapsible panel beside the artifact viewer listing all
// comments for the selected thread, driven entirely by `appStore.comments`
// (the same single source of truth that feeds the in-artifact highlights, so a
// comment created/resolved via the API/CLI appears/updates here live via
// events.ts → refetchComments, with no bespoke wiring).
//
// Each row shows: the anchor excerpt (the quote text from the anchor JSON, or
// "Direct question" for a null-anchor follow-up, or the element id for a
// graphical element with no quote), the comment body, and — once resolved —
// the agent's answer rendered from `answer_html`. Rows are filterable by status
// (all / open / answered / applied).
//
// Clicking a row scrolls the artifact to its anchor and pulses it, via the
// bridge (`scrollToAnchor` with the comment id as `key`, so the pulse lands on
// the live highlight decoration when there is one). Scroll/highlight is only
// meaningful for anchored comments on the *currently viewed* artifact version:
//  - null-anchor (direct) comments have nothing to scroll to;
//  - comments on a DIFFERENT version are shown with a version tag but are not
//    clickable. Server-side re-attachment (FR-4.4, bead conceptify-94m.7)
//    advances comments onto each new version on save, so open/answered
//    comments follow the latest automatically; a row left on an older version
//    is either "reference moved" (its anchor only resolves there — switch the
//    viewer to that version to scroll to it) or frozen `applied` history.
//
// `anchor_state === "moved"` rows get a "reference moved" badge (FR-4.4), set
// by the save-time re-attachment pass when an anchor can't be re-located in
// the new version (and cleared if a later version restores the content).
//
// The persistent open-comment highlights themselves are owned by
// ArtifactCommentLayer (94m.3/94m.4), not here — this sidebar only reads the
// store and issues scroll commands.

import { useState } from "preact/hooks";
import * as api from "../lib/api";
import type { Comment, CommentStatus, RunLogTail } from "../lib/api";
import type { Anchor } from "../lib/bridge";
import { artifactBridge } from "../lib/bridge";
import type { ActiveRunState, RunFailureState } from "../store/appStore";
import { appStore } from "../store/appStore";
import { CommentComposer } from "./CommentComposer";

type Filter = "all" | CommentStatus;

const FILTERS: { key: Filter; label: string }[] = [
  { key: "all", label: "All" },
  { key: "open", label: "Open" },
  { key: "answered", label: "Answered" },
  { key: "applied", label: "Applied" },
];

interface Props {
  comments: Comment[];
  loading: boolean;
  error: string | null;
  threadId: string;
  /** Concrete artifact version currently in the viewer ("latest" already
   *  resolved), or `null` when the thread has no artifact yet. Drives
   *  cross-version tagging and whether a row can scroll-to-anchor. */
  viewerVersion: number | null;
  /** In-flight follow-up run for this thread, if any (FR-4.8). Gates the
   *  FR-4.6/4.7 action buttons (FR-4.9: one run per thread). */
  activeRun: ActiveRunState | null;
  /** Latest failed/timed-out run on this thread (FR-4.8 failure panel). */
  runFailure: RunFailureState | null;
  onClose: () => void;
}

/** The excerpt shown on a row: the anchored quote, a direct-question marker, or
 *  the element id for a graphical element with no quote text. */
function anchorExcerpt(
  anchor: Record<string, unknown> | null,
): { kind: "direct" | "quote" | "element"; text: string } {
  if (anchor == null) return { kind: "direct", text: "Direct question" };
  const quote = anchor.quote;
  if (quote != null && typeof quote === "object") {
    const exact = (quote as Record<string, unknown>).exact;
    if (typeof exact === "string" && exact.trim().length > 0) {
      return { kind: "quote", text: exact.trim() };
    }
  }
  if (typeof anchor.cfy_id === "string" && anchor.cfy_id.length > 0) {
    return { kind: "element", text: anchor.cfy_id };
  }
  return { kind: "quote", text: "Anchored comment" };
}

const STATUS_META: Record<CommentStatus, { label: string; chip: string }> = {
  open: {
    label: "Open",
    chip: "bg-sky-100 text-sky-700 dark:bg-sky-500/15 dark:text-sky-300",
  },
  answered: {
    label: "Answered",
    chip: "bg-emerald-100 text-emerald-700 dark:bg-emerald-500/15 dark:text-emerald-300",
  },
  applied: {
    label: "Applied",
    chip: "bg-violet-100 text-violet-700 dark:bg-violet-500/15 dark:text-violet-300",
  },
};

export function CommentsSidebar({
  comments,
  loading,
  error,
  threadId,
  viewerVersion,
  activeRun,
  runFailure,
  onClose,
}: Props) {
  const [filter, setFilter] = useState<Filter>("all");
  // Inline error from a rejected run start (guard messages are user-facing);
  // `starting` bridges the click → activeRun gap so a double-click can't race
  // the FR-4.9 guard.
  const [actionError, setActionError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);

  const counts: Record<Filter, number> = {
    all: comments.length,
    open: 0,
    answered: 0,
    applied: 0,
  };
  for (const c of comments) counts[c.status] += 1;

  const visible = filter === "all" ? comments : comments.filter((c) => c.status === filter);

  // FR-4.6/4.7 preconditions: an artifact exists and no run is active (FR-4.9).
  const runIdle = activeRun == null && !starting;
  const canAsk = runIdle && viewerVersion != null && counts.open > 0;
  const canApply = runIdle && viewerVersion != null;

  async function startRunAction(action: () => Promise<void>) {
    setActionError(null);
    setStarting(true);
    try {
      await action();
    } catch (e) {
      setActionError(String(e));
    } finally {
      setStarting(false);
    }
  }

  function onAskFollowUps() {
    void startRunAction(() => appStore.askFollowUps(threadId));
  }

  function onApplyComments(commentIds: string[]) {
    void startRunAction(() => appStore.applyToArtifact(threadId, commentIds));
  }

  function scrollTo(comment: Comment) {
    // Only anchored comments on the viewed version can be located in the frame.
    if (comment.anchor == null) return;
    if (viewerVersion == null || comment.artifact_version !== viewerVersion) return;
    // `key` makes the pulse land exactly on the live highlight when the comment
    // is open (and thus decorated); otherwise the bridge resolves it fresh.
    artifactBridge.scrollToAnchor(comment.anchor as unknown as Anchor, comment.id);
  }

  return (
    <aside class="flex h-full w-80 shrink-0 flex-col border-l border-neutral-200 bg-white dark:border-neutral-800 dark:bg-neutral-950">
      <header class="flex items-center gap-2 border-b border-neutral-200 px-3 py-2.5 dark:border-neutral-800">
        <h2 class="flex-1 text-xs font-semibold uppercase tracking-wide text-neutral-500 dark:text-neutral-400">
          Comments
        </h2>
        <button
          type="button"
          onClick={onClose}
          title="Hide comments"
          aria-label="Hide comments"
          class="rounded p-0.5 text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-neutral-700 dark:hover:bg-neutral-900 dark:hover:text-neutral-200"
        >
          <svg viewBox="0 0 20 20" fill="none" class="h-4 w-4" aria-hidden="true">
            <path
              d="M12.5 5 7.5 10l5 5"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </button>
      </header>

      <div class="flex gap-1 border-b border-neutral-200 px-2 py-1.5 dark:border-neutral-800" role="tablist" aria-label="Filter comments">
        {FILTERS.map((f) => {
          const active = filter === f.key;
          return (
            <button
              key={f.key}
              type="button"
              role="tab"
              aria-selected={active}
              onClick={() => setFilter(f.key)}
              class={`flex items-center gap-1 rounded-md px-2 py-1 text-xs font-medium transition-colors ${
                active
                  ? "bg-blue-600/10 text-blue-700 dark:bg-blue-500/20 dark:text-blue-300"
                  : "text-neutral-500 hover:bg-neutral-100 dark:text-neutral-400 dark:hover:bg-neutral-900"
              }`}
            >
              {f.label}
              <span class="tabular-nums opacity-70">{counts[f.key]}</span>
            </button>
          );
        })}
      </div>

      {/* FR-4.6/4.7 actions + FR-4.8 run status. Exactly one of: action
          buttons (idle) or the live run block (FR-4.9: the disabled state IS
          the guard's UI half — the engine enforces it server-side too). */}
      <div class="flex flex-col gap-1.5 border-b border-neutral-200 px-2 py-1.5 dark:border-neutral-800">
        {activeRun != null ? (
          <RunStatusBlock run={activeRun} comments={comments} />
        ) : (
          <div class="flex gap-1.5">
            <button
              type="button"
              onClick={onAskFollowUps}
              disabled={!canAsk}
              title={
                viewerVersion == null
                  ? "Available once the thread has an artifact"
                  : counts.open === 0
                    ? "No open comments to answer"
                    : "Answer every open comment in the sidebar (the artifact is not modified)"
              }
              class="flex-1 rounded-md bg-blue-600 px-2.5 py-1.5 text-xs font-medium text-white transition-colors hover:bg-blue-700 disabled:cursor-not-allowed disabled:bg-neutral-200 disabled:text-neutral-400 dark:disabled:bg-neutral-800 dark:disabled:text-neutral-600"
            >
              Ask follow-ups
              {counts.open > 0 && (
                <span class="ml-1 tabular-nums opacity-80">({counts.open})</span>
              )}
            </button>
            {counts.answered > 0 && (
              <button
                type="button"
                onClick={() => onApplyComments([])}
                disabled={!canApply}
                title="Apply every answered comment to the artifact (saves a new version)"
                class="flex-1 rounded-md border border-violet-300 bg-violet-600/10 px-2.5 py-1.5 text-xs font-medium text-violet-700 transition-colors hover:bg-violet-600/20 disabled:cursor-not-allowed disabled:opacity-50 dark:border-violet-500/40 dark:bg-violet-500/15 dark:text-violet-300"
              >
                Apply all answered
                <span class="ml-1 tabular-nums opacity-80">({counts.answered})</span>
              </button>
            )}
          </div>
        )}
        {actionError != null && (
          <p class="break-words text-xs text-rose-600 dark:text-rose-400">{actionError}</p>
        )}
        {runFailure != null && activeRun == null && (
          <RunFailurePanel failure={runFailure} />
        )}
      </div>

      <div class="min-h-0 flex-1 overflow-y-auto p-2">
        {error != null ? (
          <p class="px-2 py-3 text-xs text-rose-600 dark:text-rose-400">{error}</p>
        ) : loading && comments.length === 0 ? (
          <p class="px-2 py-3 text-xs text-neutral-400">Loading…</p>
        ) : comments.length === 0 ? (
          <p class="px-2 py-3 text-xs text-neutral-400">
            No comments yet. Select text or a diagram element in the artifact, or ask a
            follow-up below.
          </p>
        ) : visible.length === 0 ? (
          <p class="px-2 py-3 text-xs text-neutral-400">No {filter} comments.</p>
        ) : (
          <ul class="flex flex-col gap-2">
            {visible.map((c) => (
              <CommentRow
                key={c.id}
                comment={c}
                viewerVersion={viewerVersion}
                onScroll={scrollTo}
                canApply={canApply && c.status === "answered"}
                onApply={(comment) => onApplyComments([comment.id])}
              />
            ))}
          </ul>
        )}
      </div>

      <CommentComposer threadId={threadId} viewerVersion={viewerVersion} />
    </aside>
  );
}

function CommentRow({
  comment,
  viewerVersion,
  onScroll,
  canApply,
  onApply,
}: {
  comment: Comment;
  viewerVersion: number | null;
  onScroll: (c: Comment) => void;
  /** Whether the FR-4.7 per-comment "Apply to artifact" action is available
   *  (answered comment, artifact present, no active run). */
  canApply: boolean;
  onApply: (c: Comment) => void;
}) {
  const excerpt = anchorExcerpt(comment.anchor);
  const status = STATUS_META[comment.status] ?? STATUS_META.open;
  const crossVersion = viewerVersion != null && comment.artifact_version !== viewerVersion;
  const moved = comment.anchor_state === "moved";
  const scrollable = comment.anchor != null && !crossVersion && viewerVersion != null;

  const clickProps = scrollable
    ? {
        role: "button" as const,
        tabIndex: 0,
        onClick: () => onScroll(comment),
        onKeyDown: (e: KeyboardEvent) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onScroll(comment);
          }
        },
        title: "Scroll to this in the artifact",
      }
    : {};

  return (
    <li class="overflow-hidden rounded-lg border border-neutral-200 bg-white dark:border-neutral-800 dark:bg-neutral-900">
      <div
        {...clickProps}
        class={`px-2.5 py-2 outline-none ${
          scrollable
            ? "cursor-pointer hover:bg-neutral-50 focus-visible:bg-neutral-50 dark:hover:bg-neutral-800/60 dark:focus-visible:bg-neutral-800/60"
            : ""
        }`}
      >
        <div class="mb-1 flex items-start justify-between gap-2">
          {excerpt.kind === "direct" ? (
            <span class="text-[11px] font-medium uppercase tracking-wide text-neutral-400">
              Direct question
            </span>
          ) : excerpt.kind === "element" ? (
            <span class="line-clamp-1 min-w-0 font-mono text-[11px] text-neutral-500 dark:text-neutral-400">
              {excerpt.text}
            </span>
          ) : (
            <span class="line-clamp-2 min-w-0 text-xs italic text-neutral-500 dark:text-neutral-400">
              “{excerpt.text}”
            </span>
          )}
          <span
            class={`shrink-0 rounded-full px-1.5 py-0.5 text-[10px] font-medium ${status.chip}`}
          >
            {status.label}
          </span>
        </div>

        <p class="whitespace-pre-wrap break-words text-sm text-neutral-800 dark:text-neutral-100">
          {comment.body}
        </p>

        {(crossVersion || moved) && (
          <div class="mt-1.5 flex flex-wrap items-center gap-1">
            {crossVersion && (
              <span
                class="rounded-full bg-neutral-100 px-1.5 py-0.5 text-[10px] font-medium text-neutral-500 dark:bg-neutral-800 dark:text-neutral-400"
                title="This comment is anchored to a different artifact version"
              >
                v{comment.artifact_version}
              </span>
            )}
            {moved && (
              <span
                class="rounded-full bg-amber-100 px-1.5 py-0.5 text-[10px] font-medium text-amber-800 dark:bg-amber-500/15 dark:text-amber-300"
                title="The anchored reference could not be re-located in the current artifact version"
              >
                Reference moved
              </span>
            )}
          </div>
        )}
      </div>

      {comment.answer_html != null && comment.answer_html.length > 0 && (
        <AnswerHtml html={comment.answer_html} />
      )}

      {canApply && (
        <div class="border-t border-neutral-100 px-2.5 py-1.5 dark:border-neutral-800">
          <button
            type="button"
            onClick={() => onApply(comment)}
            title="Have the agent incorporate this clarification into the artifact (saves a new version)"
            class="rounded-md border border-violet-300 bg-violet-600/10 px-2 py-1 text-[11px] font-medium text-violet-700 transition-colors hover:bg-violet-600/20 dark:border-violet-500/40 dark:bg-violet-500/15 dark:text-violet-300"
          >
            Apply to artifact
          </button>
        </div>
      )}
    </li>
  );
}

/**
 * The FR-4.8 live-run block: spinner + mode label, per-comment progress
 * (derived from the store's comment statuses against the run's target set —
 * each `comment-updated` refetch advances it), the latest agent activity
 * line, and the cancel button. A run re-attached after a thread switch has no
 * target set (not persisted) and shows an indeterminate spinner.
 */
function RunStatusBlock({ run, comments }: { run: ActiveRunState; comments: Comment[] }) {
  const label = run.mode === "answer" ? "Answering follow-ups…" : "Applying to artifact…";

  let progress: string | null = null;
  if (run.targetIds != null) {
    const done = run.targetIds.filter((id) => {
      const c = comments.find((x) => x.id === id);
      if (c == null) return false;
      // Answer runs advance targets out of `open`; apply runs land them on
      // `applied`.
      return run.mode === "answer" ? c.status !== "open" : c.status === "applied";
    }).length;
    const verb = run.mode === "answer" ? "answered" : "applied";
    progress = `${done} of ${run.targetIds.length} ${verb}`;
  }

  return (
    <div class="rounded-md border border-blue-200 bg-blue-50 px-2.5 py-2 dark:border-blue-500/30 dark:bg-blue-500/10">
      <div class="flex items-center gap-2">
        <svg
          viewBox="0 0 20 20"
          fill="none"
          class="h-3.5 w-3.5 shrink-0 animate-spin text-blue-600 dark:text-blue-400"
          aria-hidden="true"
        >
          <circle cx="10" cy="10" r="7" stroke="currentColor" stroke-width="2" class="opacity-25" />
          <path d="M17 10a7 7 0 0 0-7-7" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
        </svg>
        <span class="flex-1 text-xs font-medium text-blue-800 dark:text-blue-300">{label}</span>
        {progress != null && (
          <span class="tabular-nums text-[11px] text-blue-700/80 dark:text-blue-300/80">
            {progress}
          </span>
        )}
        <button
          type="button"
          onClick={() => appStore.cancelActiveRun()}
          title="Cancel this run (kills the agent process; answers already given are kept)"
          class="rounded border border-blue-300 px-1.5 py-0.5 text-[11px] font-medium text-blue-700 transition-colors hover:bg-blue-600/10 dark:border-blue-500/40 dark:text-blue-300"
        >
          Cancel
        </button>
      </div>
      {run.lastProgress != null && (
        <p class="mt-1 line-clamp-1 break-all font-mono text-[10px] text-blue-700/60 dark:text-blue-300/50">
          {run.lastProgress}
        </p>
      )}
    </div>
  );
}

/**
 * The FR-4.8 failure panel: names the failure class (`failed` vs `timeout` —
 * same handling, different message), loads the log tail on demand via
 * `get_run_log_tail`, and always shows the full log path (the transcript is
 * retained on disk for debugging).
 */
function RunFailurePanel({ failure }: { failure: RunFailureState }) {
  const [tail, setTail] = useState<RunLogTail | null>(null);
  const [tailError, setTailError] = useState<string | null>(null);
  const [loadingTail, setLoadingTail] = useState(false);

  const message =
    failure.status === "timeout"
      ? "The follow-up run timed out and was stopped."
      : "The follow-up run failed.";

  function onShowLog() {
    setLoadingTail(true);
    setTailError(null);
    api
      .getRunLogTail(failure.runId)
      .then(setTail)
      .catch((e) => setTailError(String(e)))
      .finally(() => setLoadingTail(false));
  }

  return (
    <div class="rounded-md border border-rose-200 bg-rose-50 px-2.5 py-2 dark:border-rose-500/30 dark:bg-rose-500/10">
      <div class="flex items-center gap-2">
        <span class="flex-1 text-xs font-medium text-rose-700 dark:text-rose-300">{message}</span>
        {tail == null && (
          <button
            type="button"
            onClick={onShowLog}
            disabled={loadingTail}
            class="rounded border border-rose-300 px-1.5 py-0.5 text-[11px] font-medium text-rose-700 transition-colors hover:bg-rose-600/10 disabled:opacity-50 dark:border-rose-500/40 dark:text-rose-300"
          >
            {loadingTail ? "Loading…" : "Show log"}
          </button>
        )}
        <button
          type="button"
          onClick={() => appStore.clearRunFailure()}
          title="Dismiss"
          aria-label="Dismiss run failure"
          class="rounded px-1 text-[11px] font-medium text-rose-400 transition-colors hover:text-rose-600 dark:hover:text-rose-300"
        >
          ✕
        </button>
      </div>
      {tailError != null && (
        <p class="mt-1 text-[11px] text-rose-600 dark:text-rose-400">{tailError}</p>
      )}
      {tail != null && (
        <div class="mt-1.5">
          <p class="break-all font-mono text-[10px] text-rose-700/70 dark:text-rose-300/60">
            {tail.log_path}
          </p>
          <pre class="mt-1 max-h-48 overflow-auto rounded bg-white/60 p-1.5 font-mono text-[10px] leading-relaxed text-neutral-700 dark:bg-neutral-950/60 dark:text-neutral-300">
            {tail.lines.join("\n")}
          </pre>
        </div>
      )}
    </div>
  );
}

/**
 * Render the agent's `answer_html` fragment. It is authored by our own trusted
 * follow-up agent and returned through our backend (PRD §9 threat model:
 * single-user machine, artifact/agent authors are already trusted with shell
 * access — this is containment/hygiene, not adversarial defense). We assign it
 * with `innerHTML` (via Preact's `dangerouslySetInnerHTML`), which does NOT
 * execute `<script>` tags. Inline event handlers in the fragment are
 * theoretically possible and are accepted under this threat model; if that ever
 * changes, sanitize here (there is no DOMPurify dependency today, by design).
 */
function AnswerHtml({ html }: { html: string }) {
  return (
    <div class="border-t border-neutral-100 bg-neutral-50 px-2.5 py-2 dark:border-neutral-800 dark:bg-neutral-900/60">
      <p class="mb-1 text-[10px] font-semibold uppercase tracking-wide text-emerald-600 dark:text-emerald-400">
        Answer
      </p>
      <div
        class="cfy-answer text-neutral-700 dark:text-neutral-300"
        // See the component docstring for the innerHTML / threat-model rationale.
        dangerouslySetInnerHTML={{ __html: html }}
      />
    </div>
  );
}
