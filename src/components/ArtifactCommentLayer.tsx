// In-artifact comment layer (PRD FR-4.1 text-selection / FR-4.2 element-click;
// beads conceptify-94m.3 / 94m.4; two-stage selection UX, bead conceptify-vu1.2).
// Rides the postMessage bridge (src/lib/bridge.ts):
//
//  - Bridge `selection` → a compact TWO-STAGE popover. After bead conceptify-vu1.1
//    `selection` arrives only on gesture completion (pointer release, or a ~300ms
//    keyboard settle), so it always represents a finished, deliberate selection.
//      · Stage 1 (toolbar): a horizontal action bar — "Comment" and "Copy" —
//        hovers just ABOVE the selection (flipped BELOW only when there's no room
//        above; note this inverts the composer's below-first preference), centered
//        on the selection rect's horizontal midpoint. It never steals focus, so a
//        keyboard-driven selection isn't disrupted.
//        - Copy writes the selection's `anchor.quote.exact` to the clipboard via
//          navigator.clipboard (shell context has clipboard access; the sandboxed
//          iframe does not), shows a brief "Copied" confirmation, then dismisses.
//        - Comment swaps the toolbar for the composer in place (stage 2).
//      · Stage 2 (composer): the textarea + Add comment/Cancel controls, Cmd+Enter
//        to save, autofocused on entry.
//  - Bridge `element_click` → the composer directly (single stage): clicking an
//    element is already an explicit intent to comment, and an element anchor has
//    no user-selected text to Copy, so the toolbar would add a redundant step.
//  - Save → `api.createComment` against the *currently displayed* artifact
//    version, then the returned comment is pushed into `appStore` and its
//    highlight is commanded onto the artifact immediately (`set_highlights`).
//  - The store's comment list is the single source of truth for highlights: on
//    every list change, every version change, and every bridge `ready` (an
//    iframe reload wipes decorations), we re-send the FULL open-comment
//    highlight set for the shown version (full-replacement semantics).
//
// Positioning: rects arrive in iframe-viewport coords and are converted to shell
// viewport coords by adding the iframe element's own bounding rect, then clamped
// to the viewport. The toolbar is intrinsic-width (placement uses an estimate);
// the composer is fixed-width (POPOVER_WIDTH). On the toolbar→composer transition
// the composer is re-placed from the retained selection rect with its own
// below-first preference, so it lands where the standard composer always would.
//
// Rendered only when an artifact exists (ThreadView's `hasArtifact` branch), so a
// still-generating thread can't accept anchored comments (the backend's composite
// FK would reject them anyway — see bead conceptify-94m.2 notes).
//
// Popover dismissal rules (the "mid-typing" question from the bead):
//  - The composer textarea autofocuses so you can type at once; the toolbar does
//    NOT take focus (its buttons stay reachable via Tab with the app focus ring).
//  - `dirty` = the composer textarea has non-empty trimmed content. A toolbar-stage
//    popover is never dirty (its body is always empty). Dirty is the single gate
//    for protecting an in-progress comment:
//      · a new `selection` / `element_click` REPLACES the popover only while not
//        dirty (so re-selecting retargets an untouched toolbar/composer, but never
//        eats a half-written comment);
//      · `selection_cleared` dismisses a *selection* popover (either stage) only
//        while not dirty (collapsing the selection after you've started writing
//        keeps the composer).
//  - Escape and click-away (mousedown outside the popover) always cancel — they
//    are explicit user gestures — in both stages.
//  - `element_click` popovers ignore `selection_cleared` (they aren't tied to a
//    live selection).

import { useEffect, useMemo, useRef, useState } from "preact/hooks";
import type { RefObject } from "preact";
import * as api from "../lib/api";
import type { Anchor, ElementAnchor, HighlightSpec, TextAnchor } from "../lib/bridge";
import { artifactBridge, type BridgeRect } from "../lib/bridge";
import { appStore, useAppStore } from "../store/appStore";

/** Fixed composer width (Tailwind `w-72`); used for viewport clamping. */
const POPOVER_WIDTH = 288;
/** Rough composer height for the below/above placement flip (measured layout
 *  isn't needed for a box this small). */
const POPOVER_HEIGHT_ESTIMATE = 150;
/** Estimated intrinsic size of the stage-1 toolbar (a two-button bar renders at
 *  ~130px; this leaves clamp headroom without visibly off-centering it). The
 *  toolbar is intrinsic-width, so centering/clamping uses an estimate —
 *  consistent with the composer's estimate-based above/below flip. */
const TOOLBAR_WIDTH_ESTIMATE = 310;
const TOOLBAR_HEIGHT_ESTIMATE = 66;
const GAP = 8;
const VIEWPORT_MARGIN = 8;
/** How long the "Copied" confirmation shows before the toolbar auto-dismisses. */
const COPIED_DISMISS_MS = 900;

interface PopoverState {
  /** Bumped each time a popover opens/retargets/advances a stage — drives the
   *  autofocus effect and guards async follow-ups (copy timeout) without
   *  re-focusing on every keystroke. */
  openId: number;
  kind: "selection" | "element";
  /** Stage 1 shows the action toolbar; stage 2 shows the comment composer.
   *  `element_click` opens straight into "composer". */
  stage: "toolbar" | "composer";
  anchor: TextAnchor | ElementAnchor;
  /** Iframe-viewport rect that opened this popover, retained so the composer can
   *  be re-placed (below-first) when advancing from the toolbar. */
  rect: BridgeRect;
  /** Final shell-viewport position of the popover box (px, for `position:fixed`). */
  left: number;
  top: number;
  body: string;
  saving: boolean;
  /** Transient toolbar-stage flag: the Copy action succeeded and is confirming. */
  copied: boolean;
  error: string | null;
  action: "explain" | "deepen" | "simplify" | "visualise" | "change" | null;
  destination: "inline" | "sidebar" | "thread";
  moreOpen: boolean;
}

/** Convert an iframe-viewport rect to a clamped shell-viewport composer
 *  position, placed just below the target (flipped above when it would overflow
 *  the bottom), never overlapping the target. */
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

/** Convert an iframe-viewport rect to a clamped shell-viewport toolbar position.
 *  Unlike the composer, the toolbar is centered horizontally on the selection
 *  and prefers ABOVE it (flipped below only when there's no room above) — the
 *  Medium/Notion selection-toolbar convention. */
function placeToolbar(iframe: HTMLIFrameElement, rect: BridgeRect): { left: number; top: number } {
  const frame = iframe.getBoundingClientRect();
  const anchorLeft = frame.left + rect.x;
  const anchorTop = frame.top + rect.y;
  const centerX = anchorLeft + rect.width / 2;

  const maxLeft = window.innerWidth - TOOLBAR_WIDTH_ESTIMATE - VIEWPORT_MARGIN;
  const left = Math.max(
    VIEWPORT_MARGIN,
    Math.min(centerX - TOOLBAR_WIDTH_ESTIMATE / 2, maxLeft),
  );

  const above = anchorTop - TOOLBAR_HEIGHT_ESTIMATE - GAP;
  let top = above;
  if (above < VIEWPORT_MARGIN) {
    const below = anchorTop + rect.height + GAP;
    const maxTop = window.innerHeight - TOOLBAR_HEIGHT_ESTIMATE - VIEWPORT_MARGIN;
    top = Math.max(VIEWPORT_MARGIN, Math.min(below, maxTop));
  }
  return { left, top };
}

/** The open-comment highlights for the shown version: every open comment on
 *  THIS artifact version that still carries an anchor (direct follow-ups have
 *  none). The version filter is deliberately exact: server-side re-attachment
 *  (FR-4.4, bead conceptify-94m.7) advances comments to each new version on
 *  save, so comments follow the latest automatically; a comment left on an
 *  older version is either "reference moved" (unresolvable here) or frozen
 *  `applied` history — neither should be decorated on this version. */
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
  onOpenSidebar: () => void;
}

const EXPLORATION_INTENTS: Record<NonNullable<PopoverState["action"]>, api.ResponseIntent> = {
  explain: { version: 1, depth: "balanced", language: "familiar", visuals: "auto", shape: "auto" },
  deepen: { version: 1, depth: "deep", language: "domain_native", visuals: "auto", shape: "walkthrough" },
  simplify: { version: 1, depth: "balanced", language: "plain", visuals: "avoid", shape: "walkthrough" },
  visualise: { version: 1, depth: "balanced", language: "familiar", visuals: "prefer", shape: "auto" },
  change: { version: 1, depth: "balanced", language: "familiar", visuals: "auto", shape: "auto" },
};

function explorationMeta(anchor: Record<string, unknown> | null): Record<string, unknown> | null {
  if (anchor == null || typeof anchor.exploration !== "object" || anchor.exploration == null) return null;
  return anchor.exploration as Record<string, unknown>;
}

export function ArtifactCommentLayer({ threadId, artifactVersion, iframeRef, onOpenSidebar }: Props) {
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
    // Stage 1: the action toolbar for a fresh text selection (prefers above,
    // never focuses — a keyboard selection must not be disrupted).
    const openToolbar = (anchor: TextAnchor, rect: BridgeRect) => {
      const iframe = iframeRef.current;
      if (iframe == null) return;
      const { left, top } = placeToolbar(iframe, rect);
      openIdRef.current += 1;
      setPopover({
        openId: openIdRef.current,
        kind: "selection",
        stage: "toolbar",
        anchor,
        rect,
        left,
        top,
        body: "",
        saving: false,
        copied: false,
        error: null,
        action: null,
        destination: "inline",
        moreOpen: false,
      });
    };

    // element_click opens the composer directly (see module header): clicking an
    // element is an explicit comment intent, and it has no selected text to Copy.
    const openElementComposer = (anchor: ElementAnchor, rect: BridgeRect) => {
      const iframe = iframeRef.current;
      if (iframe == null) return;
      const { left, top } = placePopover(iframe, rect);
      openIdRef.current += 1;
      setPopover({
        openId: openIdRef.current,
        kind: "element",
        stage: "composer",
        anchor,
        rect,
        left,
        top,
        body: "",
        saving: false,
        copied: false,
        error: null,
        action: null,
        destination: "sidebar",
        moreOpen: false,
      });
    };

    const unsubscribe = artifactBridge.onMessage((msg) => {
      switch (msg.type) {
        case "ready":
          // The iframe (re)loaded and dropped all decorations — re-apply them.
          artifactBridge.setHighlights(highlightsRef.current);
          break;
        case "selection":
          // Protect an in-progress comment; otherwise (re)target the toolbar.
          if (!isDirty(popoverRef.current)) openToolbar(msg.anchor, msg.rect);
          break;
        case "element_click":
          if (!isDirty(popoverRef.current)) openElementComposer(msg.anchor, msg.rect);
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

  // Stage 1 → 2: swap the toolbar for the composer in place. Re-places from the
  // retained selection rect (composer's below-first preference) and bumps openId
  // so the autofocus effect focuses the freshly rendered textarea.
  function goToComposer(action: PopoverState["action"] = null) {
    const current = popoverRef.current;
    const iframe = iframeRef.current;
    if (current == null || current.stage !== "toolbar" || iframe == null) return;
    const { left, top } = placePopover(iframe, current.rect);
    openIdRef.current += 1;
    setPopover({
      ...current,
      stage: "composer",
      left,
      top,
      openId: openIdRef.current,
      copied: false,
      error: null,
      action,
      moreOpen: false,
    });
  }

  // Copy the exact selected text to the clipboard from the shell (the sandboxed
  // iframe has no clipboard access). Async with error handling; on success shows
  // a brief "Copied" confirmation then dismisses. The openId guard keeps a
  // retarget/dismiss that landed mid-copy from being clobbered by this closure.
  function copySelection() {
    const current = popoverRef.current;
    if (current == null || current.stage !== "toolbar") return;
    const exact = current.anchor.quote?.exact ?? "";
    const id = current.openId;
    void (async () => {
      try {
        await navigator.clipboard.writeText(exact);
        setPopover((prev) => (prev != null && prev.openId === id ? { ...prev, copied: true, error: null } : prev));
        window.setTimeout(() => {
          setPopover((prev) => (prev != null && prev.openId === id ? null : prev));
        }, COPIED_DISMISS_MS);
      } catch (e) {
        setPopover((prev) => (prev != null && prev.openId === id ? { ...prev, error: String(e) } : prev));
      }
    })();
  }

  function save() {
    const current = popoverRef.current;
    if (current == null || current.saving) return;
    const visualTarget = ["figure", "image", "diagram"].includes(current.anchor.target?.kind ?? "");
    const defaultQuestions: Record<NonNullable<PopoverState["action"]>, string> = {
      explain: "Explain this selection.",
      deepen: "Go deeper on this selection.",
      simplify: "Explain this selection more simply.",
      visualise: "Visualise this selection.",
      change: visualTarget ? "Redraw this visual." : "Change this part of the artifact.",
    };
    const body = current.body.trim() || (current.action == null ? "" : defaultQuestions[current.action]);
    if (body.length === 0) return;

    setPopover({ ...current, saving: true, error: null });
    const target = current.anchor.target;
    const intent = current.action == null ? EXPLORATION_INTENTS.explain : EXPLORATION_INTENTS[current.action];

    if (current.destination === "thread" && current.action != null && current.action !== "change") {
      const excerpt = target?.excerpt || current.anchor.quote?.exact || target?.label || "Selected artifact content";
      const question = `${body}\n\nSource excerpt from an existing artifact (thread ${threadIdRef.current}, version ${artifactVersionRef.current}):\n${excerpt}`;
      void appStore
        .askFromApp(target?.label ? `Explore: ${target.label}`.slice(0, 80) : null, question, null, intent)
        .catch((e) => {
          setPopover((prev) => (prev != null ? { ...prev, saving: false, error: String(e) } : prev));
        });
      return;
    }

    const anchor = {
      ...current.anchor,
      exploration: current.action == null ? undefined : {
        action: current.action,
        destination: current.action === "change" ? "revision" : current.destination,
        response_intent: intent,
        source_thread_id: threadIdRef.current,
        source_artifact_version: artifactVersionRef.current,
      },
    } as unknown as Record<string, unknown>;

    api.createComment({
        threadId: threadIdRef.current,
        artifactVersion: artifactVersionRef.current,
        anchor,
        body,
      })
      .then(async (comment) => {
        // Store update recomputes `highlights` → the new comment's highlight is
        // commanded onto the artifact in the same tick. Dismiss the popover
        // (clears the shell's pending-selection state).
        appStore.addComment(comment);
        if (current.action == null) {
          setPopover(null);
          return;
        }
        if (current.action === "change") {
          setPopover(null);
          await appStore.applyToArtifact(threadIdRef.current, [comment.id]);
          return;
        }
        if (current.destination === "sidebar") onOpenSidebar();
        // The comment is already durable at this point. Dismiss before starting
        // the answer run so a rejected run cannot leave a resubmittable composer
        // that would create a duplicate; the persisted row exposes retry in the
        // Comments sidebar.
        setPopover(null);
        await appStore.askSingleComment(threadIdRef.current, comment.id);
      })
      .catch((e) => {
        setPopover((prev) => (prev != null ? { ...prev, saving: false, error: String(e) } : prev));
      });
  }

  const inlineAnswers = state.comments.filter((comment) => {
    const meta = explorationMeta(comment.anchor);
    return comment.parent_id == null && comment.artifact_version === artifactVersion && meta?.destination === "inline";
  }).slice(-3);
  const inlineRight = iframeRef.current == null
    ? 16
    : Math.max(16, window.innerWidth - iframeRef.current.getBoundingClientRect().right + 16);

  if (popover == null && inlineAnswers.length === 0) return null;

  // Keep mousedowns inside the popover from bubbling out to the artifact / list
  // panes. (The capture-phase click-away listener already ignores clicks whose
  // target is inside the popover element.)
  const positionStyle = popover == null ? undefined : { left: `${popover.left}px`, top: `${popover.top}px` };

  // Stage 1: the compact action toolbar for a fresh selection.
  if (popover?.stage === "toolbar") {
    const preview = popover.anchor.target?.label || popover.anchor.target?.excerpt || popover.anchor.quote?.exact?.trim() || "Selected content";
    return (
      <div
        ref={popoverElRef}
        role="toolbar"
        aria-label="Selection actions"
        class="cfy-toolbar fixed z-50 flex w-[310px] flex-col items-stretch"
        style={positionStyle}
        onMouseDown={(e) => e.stopPropagation()}
      >
        {popover.copied ? (
          <span class="cfy-toolbar-status" role="status">
            Copied
          </span>
        ) : popover.error != null ? (
          <span class="cfy-toolbar-status" role="status" data-tone="danger">
            Copy failed
          </span>
        ) : (
          <>
            <p class="max-w-full truncate px-2 pt-1 text-[9px] text-muted" title={preview}>“{preview}”</p>
            <div class="flex items-center px-1 pb-1">
              <button type="button" onClick={() => goToComposer("explain")} class="cfy-btn cfy-btn-ghost">Explain</button>
              <button type="button" onClick={() => goToComposer("deepen")} class="cfy-btn cfy-btn-ghost">Deepen</button>
              <button type="button" onClick={() => goToComposer(null)} class="cfy-btn cfy-btn-ghost">Comment</button>
              <div class="relative">
                <button type="button" aria-expanded={popover.moreOpen} onClick={() => setPopover((current) => current == null ? null : { ...current, moreOpen: !current.moreOpen })} class="cfy-btn cfy-btn-ghost">More</button>
                {popover.moreOpen && <div class="cfy-popover absolute right-0 top-full z-10 mt-1 w-36 p-1" role="menu" aria-label="More selection actions">
                  <button type="button" role="menuitem" onClick={() => goToComposer("simplify")} class="cfy-btn cfy-btn-ghost w-full justify-start">Simplify</button>
                  <button type="button" role="menuitem" onClick={() => goToComposer("visualise")} class="cfy-btn cfy-btn-ghost w-full justify-start">Visualise</button>
                  <button type="button" role="menuitem" onClick={() => goToComposer("change")} class="cfy-btn cfy-btn-ghost w-full justify-start">{["figure", "image", "diagram"].includes(popover.anchor.target?.kind ?? "") ? "Redraw" : "Change"}</button>
                  <button type="button" role="menuitem" onClick={copySelection} class="cfy-btn cfy-btn-ghost w-full justify-start">Copy</button>
                </div>}
              </div>
            </div>
          </>
        )}
      </div>
    );
  }

  // Stage 2: the comment composer.
  if (popover == null) return <InlineExplorationCards comments={inlineAnswers} activeRun={state.activeRun} right={inlineRight} />;

  const actionLabels = { explain: "Explain selection", deepen: "Deepen selection", simplify: "Simplify selection", visualise: "Visualise selection", change: "Change selection" } as const;
  const label = popover.action == null
    ? (popover.kind === "selection" ? "Comment on selection" : "Comment on element")
    : popover.action === "change" && ["figure", "image", "diagram"].includes(popover.anchor.target?.kind ?? "")
      ? "Redraw selected visual"
      : actionLabels[popover.action];

  return (
    <div
      ref={popoverElRef}
      role="dialog"
      aria-label="Add comment"
      class="cfy-popover fixed z-50 w-72 p-2.5"
      style={positionStyle}
      onMouseDown={(e) => e.stopPropagation()}
    >
      <p class="cfy-label mb-1.5">{label}</p>
      <div class="mb-2 rounded-ctl border border-line bg-well px-2 py-1.5">
        <p class="truncate text-[11px] font-medium text-ink">
          {popover.anchor.target?.label || popover.anchor.quote?.exact || popover.anchor.cfy_id || "Selected content"}
        </p>
        <p class="mt-0.5 line-clamp-2 text-[10px] leading-snug text-muted">
          Included from artifact v{artifactVersion}
          {popover.anchor.target?.kind ? ` · ${popover.anchor.target.kind}` : ""}
        </p>
      </div>
      <textarea
        ref={textareaRef}
        value={popover.body}
        rows={3}
        placeholder={popover.action == null ? "Add a comment…" : "Add detail or ask with the suggested request…"}
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
        class="cfy-input resize-none"
      />
      {popover.action != null && popover.action !== "change" && (
        <fieldset class="mt-2">
          <legend class="cfy-label mb-1">Answer in</legend>
          <div class="grid grid-cols-3 gap-1" aria-label="Answer destination">
            {([
              ["inline", "Here"],
              ["sidebar", "Sidebar"],
              ["thread", "New thread"],
            ] as const).map(([value, text]) => (
              <button
                key={value}
                type="button"
                aria-pressed={popover.destination === value}
                onClick={() => setPopover((prev) => prev == null ? null : { ...prev, destination: value })}
                class={`cfy-btn px-1 py-1 text-[10px] ${popover.destination === value ? "cfy-btn-accent" : "cfy-btn-secondary"}`}
              >
                {text}
              </button>
            ))}
          </div>
          <p class="mt-1 text-[9px] leading-snug text-muted">
            {popover.destination === "inline"
              ? "A compact answer stays linked to this target."
              : popover.destination === "sidebar"
                ? "Continue the anchored exchange in Comments."
                : "Create a durable exploration thread from this context."}
          </p>
        </fieldset>
      )}
      {popover.action === "change" && (
        <p class="mt-2 rounded-ctl border border-warn/30 bg-warn-bg px-2 py-1.5 text-[10px] leading-snug text-warn">
          This generates a scoped candidate and diff. Nothing changes until you review and apply it.
        </p>
      )}
      {popover.error != null && (
        <p class="mt-1 text-[11px] text-danger">{popover.error}</p>
      )}
      <div class="mt-1.5 flex items-center justify-end gap-1.5">
        <button
          type="button"
          onClick={() => setPopover(null)}
          class="cfy-btn cfy-btn-ghost px-2 py-0.5"
        >
          Cancel
        </button>
        <button
          type="button"
          onClick={save}
          disabled={popover.saving || (popover.action == null && popover.body.trim().length === 0)}
          class="cfy-btn cfy-btn-primary px-2.5 py-0.5"
        >
          {popover.saving ? "Adding…" : popover.action == null ? "Add comment" : popover.action === "change" ? "Generate preview" : "Add request"}
        </button>
      </div>
    </div>
  );
}

function InlineExplorationCards({
  comments,
  activeRun,
  right,
}: {
  comments: api.Comment[];
  activeRun: import("../store/appStore").ActiveRunState | null;
  right: number;
}) {
  return (
    <aside class="fixed bottom-4 z-40 flex w-80 max-w-[calc(100vw-2rem)] flex-col gap-2" style={{ right: `${right}px` }} aria-label="Inline exploration answers">
      {comments.map((comment) => {
        const anchor = comment.anchor as unknown as Anchor;
        const target = anchor.target;
        const answering = activeRun?.mode === "answer" && activeRun.targetIds?.includes(comment.id);
        return (
          <article key={comment.id} class="cfy-card overflow-hidden shadow-lg">
            <button
              type="button"
              onClick={() => artifactBridge.scrollToAnchor(anchor, comment.id)}
              class="flex w-full items-start justify-between gap-2 border-b border-line bg-well px-3 py-2 text-left hover:bg-hover"
              title="Show the anchored target"
            >
              <span class="min-w-0">
                <span class="cfy-label block">{String(explorationMeta(comment.anchor)?.action || "Explore")}</span>
                <span class="mt-0.5 block truncate text-[11px] text-muted">{target?.label || target?.excerpt || comment.body}</span>
              </span>
              <span class="cfy-chip shrink-0 bg-accent-bg text-[9px] text-accent-ink">Show target</span>
            </button>
            <div class="px-3 py-2">
              {comment.answer_html ? (
                <div class="cfy-answer select-text text-xs leading-relaxed text-ink" dangerouslySetInnerHTML={{ __html: comment.answer_html }} />
              ) : (
                <div class="flex items-center gap-2 text-[11px] text-muted">
                  {answering && <span class="h-2 w-2 animate-pulse rounded-full bg-info" aria-hidden="true" />}
                  <span>{answering ? "Answering…" : comment.status === "open" ? "Waiting to answer" : "Answer unavailable"}</span>
                  {answering && (
                    <button type="button" onClick={() => appStore.cancelActiveRun()} class="cfy-btn cfy-btn-ghost ml-auto px-1.5 py-0.5 text-[10px]">Cancel</button>
                  )}
                </div>
              )}
            </div>
          </article>
        );
      })}
    </aside>
  );
}
