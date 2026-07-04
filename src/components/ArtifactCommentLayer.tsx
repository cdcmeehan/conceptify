// In-artifact comment layer (PRD FR-4.1 text-selection / FR-4.2 element-click;
// beads conceptify-94m.3 / 94m.4). Rides the postMessage bridge (src/lib/bridge.ts):
//
//  - Bridge `selection` / `element_click` → a small "Add comment" popover
//    positioned near the reported rect (converted from iframe-viewport coords to
//    shell viewport coords by adding the iframe element's own bounding rect).
//  - Save → `api.createComment` against the *currently displayed* artifact
//    version, then the returned comment is pushed into `appStore` and its
//    highlight is commanded onto the artifact immediately (`set_highlights`).
//  - The store's comment list is the single source of truth for highlights: on
//    every list change, every version change, and every bridge `ready` (an
//    iframe reload wipes decorations), we re-send the FULL open-comment
//    highlight set for the shown version (full-replacement semantics).
//
// Rendered only when an artifact exists (ThreadView's `hasArtifact` branch), so a
// still-generating thread can't accept anchored comments (the backend's composite
// FK would reject them anyway — see bead conceptify-94m.2 notes).
//
// Popover dismissal rules (the "mid-typing" question from the bead):
//  - The textarea autofocuses so you can type at once.
//  - `dirty` = the textarea has non-empty trimmed content. It is the single gate
//    for protecting an in-progress comment:
//      · a new `selection` / `element_click` REPLACES the popover only while not
//        dirty (so re-selecting retargets an untouched popover, but never eats a
//        half-written comment);
//      · `selection_cleared` dismisses a *selection* popover only while not dirty
//        (collapsing the selection after you've started writing keeps the popover).
//  - Escape and click-away (mousedown outside the popover) always cancel — they
//    are explicit user gestures.
//  - `element_click` popovers ignore `selection_cleared` (they aren't tied to a
//    live selection).

import { useEffect, useMemo, useRef, useState } from "preact/hooks";
import type { RefObject } from "preact";
import * as api from "../lib/api";
import type { Anchor, ElementAnchor, HighlightSpec, TextAnchor } from "../lib/bridge";
import { artifactBridge, type BridgeRect } from "../lib/bridge";
import { appStore, useAppStore } from "../store/appStore";

/** Fixed popover width (Tailwind `w-72`); used for viewport clamping. */
const POPOVER_WIDTH = 288;
/** Rough popover height for the below/above placement flip (measured layout
 *  isn't needed for a box this small). */
const POPOVER_HEIGHT_ESTIMATE = 150;
const GAP = 8;
const VIEWPORT_MARGIN = 8;

interface PopoverState {
  /** Bumped each time a popover opens/retargets — drives the autofocus effect
   *  without re-focusing on every keystroke. */
  openId: number;
  kind: "selection" | "element";
  anchor: TextAnchor | ElementAnchor;
  /** Final shell-viewport position of the popover box (px, for `position:fixed`). */
  left: number;
  top: number;
  body: string;
  saving: boolean;
  error: string | null;
}

/** Convert an iframe-viewport rect to a clamped shell-viewport popover position,
 *  placed just below the target (flipped above when it would overflow the
 *  bottom), never overlapping the target. */
function placePopover(iframe: HTMLIFrameElement, rect: BridgeRect): { left: number; top: number } {
  const frame = iframe.getBoundingClientRect();
  const anchorLeft = frame.left + rect.x;
  const anchorTop = frame.top + rect.y;

  const maxLeft = window.innerWidth - POPOVER_WIDTH - VIEWPORT_MARGIN;
  const left = Math.max(VIEWPORT_MARGIN, Math.min(anchorLeft, maxLeft));

  const below = anchorTop + rect.height + GAP;
  let top = below;
  if (below + POPOVER_HEIGHT_ESTIMATE > window.innerHeight - VIEWPORT_MARGIN) {
    const above = anchorTop - POPOVER_HEIGHT_ESTIMATE - GAP;
    top = above >= VIEWPORT_MARGIN ? above : Math.max(VIEWPORT_MARGIN, below);
  }
  return { left, top };
}

/** The open-comment highlights for the shown version: every open comment on
 *  THIS artifact version that still carries an anchor (direct follow-ups have
 *  none). Anchors made on other versions aren't decorated here — cross-version
 *  re-attachment is bead conceptify-94m.7. */
function computeHighlights(
  comments: api.Comment[],
  artifactVersion: number,
): HighlightSpec[] {
  const specs: HighlightSpec[] = [];
  for (const c of comments) {
    if (c.status !== "open") continue;
    if (c.artifact_version !== artifactVersion) continue;
    if (c.anchor == null) continue;
    specs.push({ key: c.id, anchor: c.anchor as unknown as Anchor });
  }
  return specs;
}

interface Props {
  threadId: string;
  /** The concrete version currently in the viewer ("latest" already resolved). */
  artifactVersion: number;
  iframeRef: RefObject<HTMLIFrameElement | null>;
}

export function ArtifactCommentLayer({ threadId, artifactVersion, iframeRef }: Props) {
  const state = useAppStore();
  const [popover, setPopover] = useState<PopoverState | null>(null);

  // Refs so the single, stably-registered bridge subscription always reads
  // current values (no re-subscribe churn on version switch / re-render).
  const threadIdRef = useRef(threadId);
  threadIdRef.current = threadId;
  const artifactVersionRef = useRef(artifactVersion);
  artifactVersionRef.current = artifactVersion;
  const popoverRef = useRef<PopoverState | null>(popover);
  popoverRef.current = popover;
  const openIdRef = useRef(0);

  const highlights = useMemo(
    () => computeHighlights(state.comments, artifactVersion),
    [state.comments, artifactVersion],
  );
  const highlightsRef = useRef(highlights);
  highlightsRef.current = highlights;

  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const popoverElRef = useRef<HTMLDivElement>(null);

  const isDirty = (p: PopoverState | null): boolean => p != null && p.body.trim().length > 0;

  // Re-send the full highlight set whenever it changes (new comment saved, list
  // refetched, version switched). Full-replacement, so always the complete set.
  useEffect(() => {
    artifactBridge.setHighlights(highlights);
  }, [highlights]);

  // Single bridge subscription for the layer's lifetime (remounts per thread —
  // ThreadView keys the layer by thread id). Handlers read refs for freshness.
  useEffect(() => {
    const openPopover = (
      kind: "selection" | "element",
      anchor: TextAnchor | ElementAnchor,
      rect: BridgeRect,
    ) => {
      const iframe = iframeRef.current;
      if (iframe == null) return;
      const { left, top } = placePopover(iframe, rect);
      openIdRef.current += 1;
      setPopover({ openId: openIdRef.current, kind, anchor, left, top, body: "", saving: false, error: null });
    };

    const unsubscribe = artifactBridge.onMessage((msg) => {
      switch (msg.type) {
        case "ready":
          // The iframe (re)loaded and dropped all decorations — re-apply them.
          artifactBridge.setHighlights(highlightsRef.current);
          break;
        case "selection":
          // Protect an in-progress comment; otherwise (re)target the popover.
          if (!isDirty(popoverRef.current)) openPopover("selection", msg.anchor, msg.rect);
          break;
        case "element_click":
          if (!isDirty(popoverRef.current)) openPopover("element", msg.anchor, msg.rect);
          break;
        case "selection_cleared": {
          const current = popoverRef.current;
          if (current?.kind === "selection" && !isDirty(current)) setPopover(null);
          break;
        }
      }
    });
    return unsubscribe;
    // iframeRef is a stable ref object; subscribe once per mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Escape + click-away cancel (explicit gestures — win over the dirty guard).
  useEffect(() => {
    if (popover == null) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        setPopover(null);
      }
    };
    const onMouseDown = (e: MouseEvent) => {
      const el = popoverElRef.current;
      if (el != null && e.target instanceof Node && !el.contains(e.target)) setPopover(null);
    };
    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("mousedown", onMouseDown, true);
    return () => {
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("mousedown", onMouseDown, true);
    };
  }, [popover != null]);

  // Autofocus the textarea when a popover opens/retargets (not on every keystroke).
  useEffect(() => {
    if (popover != null) textareaRef.current?.focus();
  }, [popover?.openId]);

  function save() {
    const current = popoverRef.current;
    if (current == null || current.saving) return;
    const body = current.body.trim();
    if (body.length === 0) return;

    setPopover({ ...current, saving: true, error: null });
    api
      .createComment({
        threadId: threadIdRef.current,
        artifactVersion: artifactVersionRef.current,
        anchor: current.anchor as unknown as Record<string, unknown>,
        body,
      })
      .then((comment) => {
        // Store update recomputes `highlights` → the new comment's highlight is
        // commanded onto the artifact in the same tick. Dismiss the popover
        // (clears the shell's pending-selection state).
        appStore.addComment(comment);
        setPopover(null);
      })
      .catch((e) => {
        setPopover((prev) => (prev != null ? { ...prev, saving: false, error: String(e) } : prev));
      });
  }

  if (popover == null) return null;

  const label = popover.kind === "selection" ? "Comment on selection" : "Comment on element";

  return (
    <div
      ref={popoverElRef}
      role="dialog"
      aria-label="Add comment"
      class="fixed z-50 w-72 rounded-lg border border-neutral-200 bg-white p-2.5 shadow-lg dark:border-neutral-700 dark:bg-neutral-900"
      style={{ left: `${popover.left}px`, top: `${popover.top}px` }}
      // Keep clicks inside from bubbling out to the artifact / list panes.
      onMouseDown={(e) => e.stopPropagation()}
    >
      <p class="mb-1.5 text-[11px] font-semibold uppercase tracking-wide text-neutral-400">
        {label}
      </p>
      <textarea
        ref={textareaRef}
        value={popover.body}
        rows={3}
        placeholder="Add a comment…"
        disabled={popover.saving}
        onInput={(e) =>
          setPopover((prev) =>
            prev != null ? { ...prev, body: (e.target as HTMLTextAreaElement).value } : prev,
          )
        }
        onKeyDown={(e) => {
          // Cmd/Ctrl+Enter saves (matches the direct-composer, 94m.5); plain
          // Escape cancels (handled by the window listener above too, but stop
          // it reaching the artifact).
          if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
            e.preventDefault();
            save();
          }
        }}
        class="w-full resize-none rounded border border-neutral-300 bg-white px-2 py-1 text-sm text-neutral-900 outline-none focus:border-blue-400 disabled:opacity-50 dark:border-neutral-700 dark:bg-neutral-950 dark:text-neutral-100"
      />
      {popover.error != null && (
        <p class="mt-1 text-[11px] text-rose-600 dark:text-rose-400">{popover.error}</p>
      )}
      <div class="mt-1.5 flex items-center justify-end gap-1.5">
        <button
          type="button"
          onClick={() => setPopover(null)}
          class="rounded px-2 py-0.5 text-xs text-neutral-500 hover:text-neutral-800 dark:hover:text-neutral-200"
        >
          Cancel
        </button>
        <button
          type="button"
          onClick={save}
          disabled={popover.saving || popover.body.trim().length === 0}
          class="rounded bg-blue-600 px-2.5 py-0.5 text-xs font-medium text-white hover:bg-blue-700 disabled:opacity-50"
        >
          {popover.saving ? "Adding…" : "Add comment"}
        </button>
      </div>
    </div>
  );
}
