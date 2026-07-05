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
// Threaded replies (epic conceptify-6xi): the flat comment list is grouped into
// root chains (`groupComments`, in the store) — each root renders with its reply
// chain nested one level beneath (indented, quieter meta). Filters and counts
// operate on ROOT status only; a reply always renders with its root regardless
// of the active filter. A "Reply" affordance on a root that has begun an
// exchange (has an answer or replies) opens an inline composer whose submit
// creates a reply and re-opens the root server-side (its status chip flips back
// to `open`).  Anchor interactions and the cross-version tag stay root-only —
// replies carry no anchor and inherit the root's version.
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

import { useEffect, useRef, useState } from "preact/hooks";
import * as api from "../lib/api";
import type { Comment, CommentStatus, RunLogTail } from "../lib/api";
import type { Anchor } from "../lib/bridge";
import { artifactBridge } from "../lib/bridge";
import type { ActiveRunState, CommentChain, RunFailureState } from "../store/appStore";
import { appStore, groupComments } from "../store/appStore";
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

/** Comment-status chip families (bead conceptify-vxc token vocabulary):
 *  open = info (awaiting the agent), answered = ok, applied = accent (the
 *  artifact itself changed — the strongest state gets the accent). */
const STATUS_META: Record<CommentStatus, { label: string; chip: string }> = {
  open: { label: "Open", chip: "bg-info-bg text-info" },
  answered: { label: "Answered", chip: "bg-ok-bg text-ok" },
  applied: { label: "Applied", chip: "bg-accent-bg text-accent-ink" },
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

  // Threaded view (epic conceptify-6xi): group the flat list into root chains.
  // Filters + counts operate on ROOT status only — a reply always renders with
  // its root regardless of the active filter, and never counts on its own (a
  // reply's re-open flips its root back to `open`, so root status already
  // reflects the conversation state).
  const chains = groupComments(comments);

  const counts: Record<Filter, number> = {
    all: chains.length,
    open: 0,
    answered: 0,
    applied: 0,
  };
  for (const ch of chains) counts[ch.root.status] += 1;

  const visibleChains =
    filter === "all" ? chains : chains.filter((ch) => ch.root.status === filter);

  // FR-4.6/4.7 preconditions: an artifact exists and no run is active (FR-4.9).
  const runIdle = activeRun == null && !starting;
  const canAsk = runIdle && viewerVersion != null && counts.open > 0;
  const canApply = runIdle && viewerVersion != null;
  // The sidebar only owns answer/apply runs. An `ask` (in-app generation) run is
  // thread-scoped and surfaced by the main thread view's progress panel (FR-5.2),
  // never as a follow-up run block here.
  const sidebarRun = activeRun != null && activeRun.mode !== "ask" ? activeRun : null;

  // "Ask now" single-comment run (epic conceptify-6xi): an answer run targeting
  // exactly ONE root renders compactly INLINE on that root's chain, not as the
  // header batch block. Distinguished purely by target ids:
  //  - a batch "Ask follow-ups" that happened to have a single open comment is
  //    also length-1, and is correctly shown the same way (it *is* answering that
  //    one root — the inline treatment is coherent either way);
  //  - a run re-attached after a reload/thread-switch has no target ids (not
  //    persisted server-side) → we can't tell single from batch, so it falls
  //    back to the header block with its indeterminate spinner.
  const inlineRunRootId =
    sidebarRun != null && sidebarRun.mode === "answer" && sidebarRun.targetIds?.length === 1
      ? sidebarRun.targetIds[0]
      : null;
  // The inline single-run state can only render if its root is actually on
  // screen; if the active filter hides that root, fall back to the header block
  // so the run is never left with no visible indicator.
  const inlineRootVisible =
    inlineRunRootId != null && visibleChains.some((ch) => ch.root.id === inlineRunRootId);
  // The header shows the batch/apply run block for every sidebar run EXCEPT a
  // single-comment answer run whose root is visible (that one lives inline on
  // its root).
  const headerRun = inlineRootVisible ? null : sidebarRun;
  // The failure panel lives in the header (always visible regardless of the
  // active filter); a single-comment failure additionally highlights its root's
  // chain via `runFailure.targetRootId`.
  const failureRootId =
    runFailure != null && activeRun == null ? runFailure.targetRootId : null;

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

  function onAskNow(rootId: string) {
    void startRunAction(() => appStore.askSingleComment(threadId, rootId));
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
    // At narrow window widths the sidebar may shrink (to min-w-60) and the
    // artifact column compresses first — comment triage stays fully usable and
    // the sidebar is never clipped off-screen; collapse it to read the artifact.
    <aside class="flex h-full w-80 min-w-60 shrink flex-col border-l border-line bg-paper">
      <header class="flex items-center gap-2 border-b border-line px-3 py-2.5">
        <h2 class="cfy-label flex-1">Comments</h2>
        <button
          type="button"
          onClick={onClose}
          title="Hide comments"
          aria-label="Hide comments"
          class="rounded-ctl p-0.5 text-muted transition-colors hover:bg-hover hover:text-ink"
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

      <div
        class="flex flex-wrap gap-1 border-b border-line px-2 py-1.5"
        role="tablist"
        aria-label="Filter comments"
      >
        {FILTERS.map((f) => {
          const active = filter === f.key;
          return (
            <button
              key={f.key}
              type="button"
              role="tab"
              aria-selected={active}
              onClick={() => setFilter(f.key)}
              class={`flex items-center gap-1 rounded-ctl px-2 py-1 text-xs font-medium transition-colors ${
                active ? "bg-accent-bg text-accent-ink" : "text-muted hover:bg-hover hover:text-ink"
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
      <div class="flex flex-col gap-1.5 border-b border-line px-2 py-1.5">
        {headerRun != null ? (
          <RunStatusBlock run={headerRun} comments={comments} />
        ) : (
          <div class="flex flex-wrap gap-1.5">
            <button
              type="button"
              onClick={onAskFollowUps}
              disabled={!canAsk}
              title={
                !runIdle
                  ? "A run is already in progress"
                  : viewerVersion == null
                    ? "Available once the thread has an artifact"
                    : counts.open === 0
                      ? "No open comments to answer"
                      : "Answer every open comment in the sidebar (the artifact is not modified)"
              }
              class="cfy-btn cfy-btn-primary flex-1 py-1.5"
            >
              Ask follow-ups
              {counts.open > 0 && (
                <span class="tabular-nums opacity-80">({counts.open})</span>
              )}
            </button>
            {counts.answered > 0 && (
              <button
                type="button"
                onClick={() => onApplyComments([])}
                disabled={!canApply}
                title={
                  !runIdle
                    ? "A run is already in progress"
                    : "Apply every answered comment to the artifact (saves a new version)"
                }
                class="cfy-btn cfy-btn-accent flex-1 py-1.5"
              >
                Apply all answered
                <span class="tabular-nums opacity-80">({counts.answered})</span>
              </button>
            )}
          </div>
        )}
        {actionError != null && (
          <p class="break-words text-xs text-danger">{actionError}</p>
        )}
        {runFailure != null && activeRun == null && (
          <RunFailurePanel failure={runFailure} />
        )}
      </div>

      <div class="min-h-0 flex-1 overflow-y-auto p-2">
        {error != null ? (
          <p class="px-2 py-3 text-xs text-danger">{error}</p>
        ) : loading && comments.length === 0 ? (
          <div class="flex flex-col gap-2.5 px-2 py-3" aria-hidden="true">
            <div class="cfy-skeleton w-3/4" />
            <div class="cfy-skeleton w-11/12" />
            <div class="cfy-skeleton w-2/3" />
          </div>
        ) : comments.length === 0 ? (
          // No comments yet (bead conceptify-vxc): one quiet sentence; the
          // composer below is the standing affordance.
          <div class="px-3 py-10 text-center">
            <p class="font-serif text-sm font-semibold text-ink">No comments yet</p>
            <p class="mt-1 text-xs leading-relaxed text-muted">
              Select text or a diagram element in the artifact, or ask a follow-up
              below.
            </p>
          </div>
        ) : visibleChains.length === 0 ? (
          <p class="px-2 py-3 text-xs text-muted">No {filter} comments.</p>
        ) : (
          <ul class="flex flex-col gap-2">
            {visibleChains.map((chain) => (
              <ChainItem
                key={chain.root.id}
                chain={chain}
                threadId={threadId}
                viewerVersion={viewerVersion}
                onScroll={scrollTo}
                runIdle={runIdle}
                onApply={(comment) => onApplyComments([comment.id])}
                onAskNow={() => onAskNow(chain.root.id)}
                inlineRun={inlineRunRootId === chain.root.id ? sidebarRun : null}
                failedHighlight={failureRootId === chain.root.id}
              />
            ))}
          </ul>
        )}
      </div>

      <CommentComposer threadId={threadId} viewerVersion={viewerVersion} />
    </aside>
  );
}

/**
 * One conversation chain (epic conceptify-6xi): a root comment with its ordered
 * reply chain nested one level beneath. The root renders exactly as before
 * (anchor excerpt, body, answer, status chip, moved/cross-version badges, Apply
 * affordance); replies render quietly indented under it (smaller meta, body,
 * per-reply status, answer when answered).
 *
 * Anchor interactions (scroll/highlight) and the cross-version tag are
 * root-only — replies carry no anchor and inherit the root's version, so
 * there's nothing to scroll to or tag.
 *
 * A "Reply" affordance appears once the exchange has started (the root has an
 * answer or already has replies) — a fresh, unanswered root doesn't need one
 * (the agent hasn't spoken yet). Replying re-opens an answered/applied root
 * server-side; the flipped status chip lands live via `addComment`'s refetch.
 *
 * "Ask now" (epic conceptify-6xi.4): an OPEN root also carries an "Ask now"
 * button in the same action slot, firing a single-comment answer run for just
 * that root. While that run is active on THIS root, `inlineRun` is set and the
 * action slot is replaced by a compact inline run state (spinner + cancel).
 * FR-4.9: while ANY run is active on the thread (`!runIdle`), every per-root
 * action button (Ask now, Apply) is disabled with a "run in progress" tooltip.
 */
function ChainItem({
  chain,
  threadId,
  viewerVersion,
  onScroll,
  runIdle,
  onApply,
  onAskNow,
  inlineRun,
  failedHighlight,
}: {
  chain: CommentChain;
  threadId: string;
  viewerVersion: number | null;
  onScroll: (c: Comment) => void;
  /** Whether no run is active on the thread (FR-4.9): gates whether the per-root
   *  Ask now / Apply buttons are enabled. They still *render* during a run —
   *  disabled with a tooltip — so the FR-4.9 lockout is visible, not silent. */
  runIdle: boolean;
  onApply: (c: Comment) => void;
  /** Fire an FR-4.6-style "Ask now" answer run for this (open) root. */
  onAskNow: () => void;
  /** The single-comment answer run currently targeting THIS root, or `null`.
   *  When set, the action slot is replaced by a compact inline run state. */
  inlineRun: ActiveRunState | null;
  /** Whether a failed "Ask now" run on this root should highlight its chain
   *  (the failure panel itself lives in the sidebar header). */
  failedHighlight: boolean;
}) {
  const { root, replies } = chain;
  const [replyOpen, setReplyOpen] = useState(false);

  const excerpt = anchorExcerpt(root.anchor);
  const status = STATUS_META[root.status] ?? STATUS_META.open;
  const crossVersion = viewerVersion != null && root.artifact_version !== viewerVersion;
  const moved = root.anchor_state === "moved";
  const scrollable = root.anchor != null && !crossVersion && viewerVersion != null;

  const hasArtifact = viewerVersion != null;
  // Apply (FR-4.7) shows on answered roots; Ask now (6xi.4) on open roots. Both
  // need an artifact to exist. They render regardless of run state and disable
  // (not vanish) while a run is active, so the FR-4.9 lockout is visible.
  const showApply = hasArtifact && root.status === "answered";
  const showAskNow = hasArtifact && root.status === "open";
  // Reply shows once the exchange has started: the root has an answer, or it
  // already carries replies. A fresh unanswered root gets no Reply affordance.
  const hasAnswer = root.answer_html != null && root.answer_html.length > 0;
  const showReply = hasAnswer || replies.length > 0;
  // The action slot renders only when it has a visible button, and only when no
  // inline run occupies this root (the run state replaces the buttons). When the
  // reply composer is open the Reply button is hidden (the composer replaces it).
  const showActions =
    inlineRun == null && (showApply || showAskNow || (showReply && !replyOpen));

  const clickProps = scrollable
    ? {
        role: "button" as const,
        tabIndex: 0,
        onClick: () => onScroll(root),
        onKeyDown: (e: KeyboardEvent) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onScroll(root);
          }
        },
        title: "Scroll to this in the artifact",
      }
    : {};

  return (
    <li
      class={`cfy-card overflow-hidden ${
        failedHighlight ? "border-danger/50" : ""
      }`}
    >
      <div
        {...clickProps}
        class={`px-2.5 py-2 outline-none ${
          scrollable ? "hover:bg-hover focus-visible:bg-hover" : ""
        }`}
      >
        <div class="mb-1 flex items-start justify-between gap-2">
          {excerpt.kind === "direct" ? (
            <span class="text-[11px] font-medium uppercase tracking-wide text-muted">
              Direct question
            </span>
          ) : excerpt.kind === "element" ? (
            <span class="line-clamp-1 min-w-0 font-mono text-[11px] text-muted">
              {excerpt.text}
            </span>
          ) : (
            <span class="line-clamp-2 min-w-0 text-xs italic leading-snug text-muted">
              “{excerpt.text}”
            </span>
          )}
          <span class={`cfy-chip shrink-0 text-[10px] ${status.chip}`}>
            {status.label}
          </span>
        </div>

        <p class="select-text whitespace-pre-wrap break-words text-[13px] leading-relaxed text-ink">
          {root.body}
        </p>

        {(crossVersion || moved) && (
          <div class="mt-1.5 flex flex-wrap items-center gap-1">
            {crossVersion && (
              <span
                class="cfy-chip bg-hover text-[10px] text-muted"
                title="This comment is anchored to a different artifact version"
              >
                v{root.artifact_version}
              </span>
            )}
            {moved && (
              <span
                class="cfy-chip bg-warn-bg text-[10px] text-warn"
                title="The anchored reference could not be re-located in the current artifact version"
              >
                Reference moved
              </span>
            )}
          </div>
        )}
      </div>

      {hasAnswer && <AnswerHtml html={root.answer_html as string} />}

      {replies.length > 0 && (
        <ul class="flex flex-col">
          {replies.map((reply) => (
            <ReplyRow key={reply.id} reply={reply} />
          ))}
        </ul>
      )}

      {/* Per-root action slot (epic conceptify-6xi). Apply + Reply + 6xi.4's
          "Ask now" live in this row; while a single-comment run targets this
          root the compact inline run state replaces it. */}
      {inlineRun != null && <InlineRunState />}
      {showActions && (
        <div class="flex flex-wrap items-center gap-1.5 border-t border-line px-2.5 py-1.5">
          {showApply && (
            <button
              type="button"
              onClick={() => onApply(root)}
              disabled={!runIdle}
              title={
                !runIdle
                  ? "A run is already in progress"
                  : "Have the agent incorporate this clarification into the artifact (saves a new version)"
              }
              class="cfy-btn cfy-btn-accent px-2 py-1 text-[11px]"
            >
              Apply to artifact
            </button>
          )}
          {showAskNow && (
            <button
              type="button"
              onClick={onAskNow}
              disabled={!runIdle}
              title={
                !runIdle
                  ? "A run is already in progress"
                  : "Answer just this comment now (the artifact is not modified)"
              }
              class="cfy-btn cfy-btn-secondary px-2 py-1 text-[11px]"
            >
              Ask now
            </button>
          )}
          {showReply && !replyOpen && (
            <button
              type="button"
              onClick={() => setReplyOpen(true)}
              title="Ask a follow-up in this thread (re-opens the comment for the agent)"
              class="cfy-btn cfy-btn-ghost px-2 py-1 text-[11px]"
            >
              Reply
            </button>
          )}
        </div>
      )}

      {replyOpen && (
        <ReplyComposer
          threadId={threadId}
          root={root}
          onDone={() => setReplyOpen(false)}
        />
      )}
    </li>
  );
}

/**
 * Compact inline run state for an "Ask now" single-comment run (epic
 * conceptify-6xi.4), rendered in place of a root's action slot while its run is
 * live: a small spinner, "Answering…", and an icon cancel wired to `cancel_run`
 * (via `appStore.cancelActiveRun`, which cancels the one active run). Kept tiny
 * on purpose — the header batch block owns the fuller per-comment progress; a
 * single-target run needs only "working / cancel". It clears when the run
 * finishes (`run-finished` drops `activeRun` → the parent stops passing
 * `inlineRun`), at which point the chain shows the freshly-landed answer.
 */
function InlineRunState() {
  return (
    <div class="flex items-center gap-2 border-t border-info/20 bg-info-bg px-2.5 py-1.5">
      <svg
        viewBox="0 0 20 20"
        fill="none"
        class="h-3 w-3 shrink-0 animate-spin text-info"
        aria-hidden="true"
      >
        <circle cx="10" cy="10" r="7" stroke="currentColor" stroke-width="2" class="opacity-25" />
        <path d="M17 10a7 7 0 0 0-7-7" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
      </svg>
      <span class="flex-1 text-[11px] font-medium text-info">Answering…</span>
      <button
        type="button"
        onClick={() => appStore.cancelActiveRun()}
        title="Cancel this run (kills the agent process; answers already given are kept)"
        aria-label="Cancel this run"
        class="rounded-ctl p-0.5 text-info transition-colors hover:bg-info/10"
      >
        <svg viewBox="0 0 20 20" fill="currentColor" class="h-3.5 w-3.5" aria-hidden="true">
          <rect x="6" y="6" width="8" height="8" rx="1.5" />
        </svg>
      </button>
    </div>
  );
}

/**
 * One reply in a chain (epic conceptify-6xi): quietly indented beneath the root
 * with a left rule to signal nesting, smaller meta than the root, the reply
 * body, its own status chip (`open`/`answered` — `applied` is root-only), and
 * the agent's `answer_html` once answered.
 */
function ReplyRow({ reply }: { reply: Comment }) {
  const status = STATUS_META[reply.status] ?? STATUS_META.open;
  const answered = reply.answer_html != null && reply.answer_html.length > 0;

  return (
    <li class="ml-2.5 border-l border-line">
      <div class="px-2.5 py-1.5">
        <div class="mb-0.5 flex items-center justify-between gap-2">
          <span class="text-[10px] font-medium uppercase tracking-wide text-muted">
            Reply
          </span>
          <span class={`cfy-chip shrink-0 text-[10px] ${status.chip}`}>
            {status.label}
          </span>
        </div>
        <p class="select-text whitespace-pre-wrap break-words text-xs leading-relaxed text-ink">
          {reply.body}
        </p>
      </div>
      {answered && <AnswerHtml html={reply.answer_html as string} />}
    </li>
  );
}

/**
 * Inline reply composer (epic conceptify-6xi): a quiet textarea beneath a chain.
 * Same conventions as {@link CommentComposer} — Cmd/Ctrl+Enter submits, empty
 * input rejected — plus Escape to cancel (the composer is transient here, unlike
 * the always-mounted bottom composer). Submitting creates a reply
 * (`parent_id = root.id`, no anchor, the root's version) and reuses
 * `appStore.addComment`, whose refetch reconciles both the new reply and the
 * root's re-open (its status chip flips back to `open`).
 */
function ReplyComposer({
  threadId,
  root,
  onDone,
}: {
  threadId: string;
  root: Comment;
  onDone: () => void;
}) {
  const [body, setBody] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    textareaRef.current?.focus();
  }, []);

  function submit() {
    if (saving) return;
    const trimmed = body.trim();
    if (trimmed.length === 0) return;

    setSaving(true);
    setError(null);
    api
      .createComment({
        threadId,
        artifactVersion: root.artifact_version,
        anchor: null,
        body: trimmed,
        parentId: root.id,
      })
      .then((comment) => {
        // Same path as the popover/bottom composer: append + reconcile. The
        // refetch also picks up the root's server-side re-open (status → open).
        appStore.addComment(comment);
        onDone();
      })
      .catch((e) => {
        setError(String(e));
        setSaving(false);
      });
  }

  return (
    <div class="border-t border-line bg-well/60 px-2.5 py-2">
      <textarea
        ref={textareaRef}
        value={body}
        rows={2}
        disabled={saving}
        placeholder="Reply with a follow-up…"
        onInput={(e) => setBody((e.target as HTMLTextAreaElement).value)}
        onKeyDown={(e) => {
          // Cmd/Ctrl+Enter submits (matches the composer/popover); Escape cancels.
          if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
            e.preventDefault();
            submit();
          } else if (e.key === "Escape") {
            e.preventDefault();
            onDone();
          }
        }}
        class="cfy-input resize-none text-xs"
      />
      {error != null && (
        <p class="mt-1 text-[11px] text-danger">{error}</p>
      )}
      <div class="mt-1.5 flex items-center justify-end gap-1.5">
        <button
          type="button"
          onClick={onDone}
          class="cfy-btn cfy-btn-ghost px-2 py-0.5 text-[11px]"
        >
          Cancel
        </button>
        <button
          type="button"
          onClick={submit}
          disabled={saving || body.trim().length === 0}
          class="cfy-btn cfy-btn-primary px-2.5 py-0.5 text-[11px]"
        >
          {saving ? "Replying…" : "Reply"}
        </button>
      </div>
    </div>
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
    <div class="rounded-ctl border border-info/30 bg-info-bg px-2.5 py-2">
      <div class="flex items-center gap-2">
        <svg
          viewBox="0 0 20 20"
          fill="none"
          class="h-3.5 w-3.5 shrink-0 animate-spin text-info"
          aria-hidden="true"
        >
          <circle cx="10" cy="10" r="7" stroke="currentColor" stroke-width="2" class="opacity-25" />
          <path d="M17 10a7 7 0 0 0-7-7" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
        </svg>
        <span class="flex-1 text-xs font-medium text-info">{label}</span>
        {progress != null && (
          <span class="text-[11px] tabular-nums text-info/80">{progress}</span>
        )}
        <button
          type="button"
          onClick={() => appStore.cancelActiveRun()}
          title="Cancel this run (kills the agent process; answers already given are kept)"
          class="cfy-btn border border-info/40 px-1.5 py-0.5 text-[11px] text-info hover:bg-info/10"
        >
          Cancel
        </button>
      </div>
      {run.lastProgress != null && (
        <p class="mt-1 line-clamp-1 break-all font-mono text-[10px] text-muted">
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
    <div class="rounded-ctl border border-danger/30 bg-danger-bg px-2.5 py-2">
      <div class="flex items-center gap-2">
        <span class="flex-1 text-xs font-medium text-danger">{message}</span>
        {tail == null && (
          <button
            type="button"
            onClick={onShowLog}
            disabled={loadingTail}
            class="cfy-btn border border-danger/40 px-1.5 py-0.5 text-[11px] text-danger hover:bg-danger/10 disabled:opacity-50"
          >
            {loadingTail ? "Loading…" : "Show log"}
          </button>
        )}
        <button
          type="button"
          onClick={() => appStore.clearRunFailure()}
          title="Dismiss"
          aria-label="Dismiss run failure"
          class="rounded-ctl p-0.5 text-danger/70 transition-colors hover:bg-danger/10 hover:text-danger"
        >
          <svg viewBox="0 0 20 20" fill="none" class="h-3.5 w-3.5" aria-hidden="true">
            <path
              d="m5.5 5.5 9 9m0-9-9 9"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
            />
          </svg>
        </button>
      </div>
      {tailError != null && (
        <p class="mt-1 text-[11px] text-danger">{tailError}</p>
      )}
      {tail != null && (
        <div class="mt-1.5">
          <p class="select-text break-all font-mono text-[10px] text-muted">
            {tail.log_path}
          </p>
          <pre class="mt-1 max-h-48 select-text overflow-auto rounded-ctl bg-raised/70 p-1.5 font-mono text-[10px] leading-relaxed text-ink">
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
    <div class="border-t border-line bg-well/60 px-2.5 py-2">
      <p class="mb-1 text-[10px] font-semibold uppercase tracking-wide text-ok">
        Answer
      </p>
      <div
        class="cfy-answer select-text text-ink"
        // See the component docstring for the innerHTML / threat-model rationale.
        dangerouslySetInnerHTML={{ __html: html }}
      />
    </div>
  );
}
