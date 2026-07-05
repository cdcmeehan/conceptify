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
import type { Comment, CommentStatus } from "../lib/api";
import type { Anchor } from "../lib/bridge";
import { artifactBridge } from "../lib/bridge";
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
  onClose,
}: Props) {
  const [filter, setFilter] = useState<Filter>("all");

  const counts: Record<Filter, number> = {
    all: comments.length,
    open: 0,
    answered: 0,
    applied: 0,
  };
  for (const c of comments) counts[c.status] += 1;

  const visible = filter === "all" ? comments : comments.filter((c) => c.status === filter);

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
              <CommentRow key={c.id} comment={c} viewerVersion={viewerVersion} onScroll={scrollTo} />
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
}: {
  comment: Comment;
  viewerVersion: number | null;
  onScroll: (c: Comment) => void;
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
    </li>
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
