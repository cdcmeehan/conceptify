// Shell side of the artifact bridge (PRD §5.4; bead conceptify-94m.1).
//
// Owns the postMessage channel to the sandboxed artifact iframe. The frame is
// opaque-origin (cross-scheme artifact:// + sandbox="allow-scripts", no
// allow-same-origin), which dictates both directions of the channel:
//
//  - Inbound: accepted ONLY when `event.origin === "null"` (how an opaque
//    origin serializes) AND `event.source === iframe.contentWindow` of the
//    currently attached iframe. Everything else is silently ignored.
//  - Outbound: `targetOrigin` must be `"*"` — an opaque origin cannot be
//    named. Outbound payloads carry only comment anchors/keys, never secrets.
//
// Trust model (PRD §9 S2 — containment, not adversarial): artifact JS shares
// the frame with the injected bridge and can post protocol-shaped messages
// itself. Every inbound message is therefore treated as UNTRUSTED input:
// shapes are validated here and unknown/malformed messages are dropped with a
// debug log. Nothing a spoofed message can trigger exceeds what the user
// could do by hand (open a comment popover).
//
// Protocol reference: docs/api.md "Bridge protocol" (envelope
// `{cfy: 1, type, ...}`; bridge script: src-tauri/assets/bridge.js).
//
// Consumers (94m.3 popover, 94m.4 element comments, 94m.6 sidebar) subscribe
// via `onMessage` and command via `setHighlights` / `scrollToAnchor`.
// Lifecycle contract: a `ready` message arrives after EVERY document load in
// the iframe (including version-switch reloads, which wipe all decorations),
// so consumers must (re)apply their highlights on every `ready`.

const PROTOCOL_VERSION = 1;

// ---- anchor shapes (the FR-4.4 schema; canonical def: conceptify-types) ----

export interface TextQuote {
  exact: string;
  prefix?: string;
  suffix?: string;
}

export interface SemanticTarget {
  kind: "text" | "block" | "code" | "figure" | "image" | "diagram";
  label: string;
  excerpt: string;
  cfy_ids: string[];
  multi_block: boolean;
}

export interface TextAnchor {
  v: number;
  type: "text";
  /** Nearest id-bearing ancestor of the selection (absent: quote-only). */
  cfy_id?: string;
  /** Selection offsets within `cfy_id`'s visible text (see docs/api.md). */
  start?: number;
  end?: number;
  quote: TextQuote;
  target?: SemanticTarget;
}

export interface ElementAnchor {
  v: number;
  type: "element";
  cfy_id: string;
  quote?: TextQuote;
  target?: SemanticTarget;
}

export type Anchor = TextAnchor | ElementAnchor;

/** A rect in the iframe's viewport coordinate space (CSS px). To position
 *  shell UI (94m.3 popover), add the iframe element's own
 *  getBoundingClientRect() offset. */
export interface BridgeRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

// ---- protocol messages ----

/** Artifact → shell. */
export type BridgeMessage =
  | { type: "ready" }
  | { type: "selection"; anchor: TextAnchor; rect: BridgeRect }
  | { type: "selection_cleared" }
  | { type: "element_click"; anchor: ElementAnchor; rect: BridgeRect };

/** One decoration for `setHighlights`; `key` is the comment id (used by
 *  `scrollToAnchor` to pulse the matching decoration). */
export interface HighlightSpec {
  key: string;
  anchor: Anchor;
}

export interface DiffMarkerSpec {
  key: string;
  cfy_id: string;
  kind: "modified" | "added" | "moved" | "removed";
}

type BridgeListener = (message: BridgeMessage) => void;

// ---- inbound validation (untrusted input) ----

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null;
}

function isRect(v: unknown): v is BridgeRect {
  return (
    isRecord(v) &&
    typeof v.x === "number" &&
    typeof v.y === "number" &&
    typeof v.width === "number" &&
    typeof v.height === "number"
  );
}

function isQuote(v: unknown): v is TextQuote {
  return (
    isRecord(v) &&
    typeof v.exact === "string" &&
    (v.prefix === undefined || typeof v.prefix === "string") &&
    (v.suffix === undefined || typeof v.suffix === "string")
  );
}

function isTextAnchor(v: unknown): v is TextAnchor {
  return (
    isRecord(v) &&
    v.v === 1 &&
    v.type === "text" &&
    isQuote(v.quote) &&
    (v.cfy_id === undefined || typeof v.cfy_id === "string") &&
    (v.start === undefined || typeof v.start === "number") &&
    (v.end === undefined || typeof v.end === "number") &&
    (v.target === undefined || isSemanticTarget(v.target))
  );
}

function isElementAnchor(v: unknown): v is ElementAnchor {
  return (
    isRecord(v) &&
    v.v === 1 &&
    v.type === "element" &&
    typeof v.cfy_id === "string" &&
    (v.quote === undefined || isQuote(v.quote)) &&
    (v.target === undefined || isSemanticTarget(v.target))
  );
}

function isSemanticTarget(v: unknown): v is SemanticTarget {
  return isRecord(v) &&
    ["text", "block", "code", "figure", "image", "diagram"].includes(String(v.kind)) &&
    typeof v.label === "string" && typeof v.excerpt === "string" &&
    Array.isArray(v.cfy_ids) && v.cfy_ids.every((id) => typeof id === "string") &&
    typeof v.multi_block === "boolean";
}

/** Parse an untrusted `event.data` into a typed message, or null. */
function parseMessage(data: unknown): BridgeMessage | null {
  if (!isRecord(data) || data.cfy !== PROTOCOL_VERSION) return null;
  switch (data.type) {
    case "ready":
      return { type: "ready" };
    case "selection_cleared":
      return { type: "selection_cleared" };
    case "selection":
      if (isTextAnchor(data.anchor) && isRect(data.rect)) {
        return { type: "selection", anchor: data.anchor, rect: data.rect };
      }
      return null;
    case "element_click":
      if (isElementAnchor(data.anchor) && isRect(data.rect)) {
        return { type: "element_click", anchor: data.anchor, rect: data.rect };
      }
      return null;
    default:
      return null;
  }
}

// ---- the bridge singleton ----

class ArtifactBridge {
  private iframe: HTMLIFrameElement | null = null;
  /** True once the current attachment has seen its first `ready`. */
  private ready = false;
  /** Commands sent before the first `ready` of an attachment. */
  private queue: Record<string, unknown>[] = [];
  private listeners = new Set<BridgeListener>();
  private windowListenerInstalled = false;

  /**
   * Register the viewer iframe. Idempotent for the same element (safe to call
   * from a render-path ref). Attaching a different element replaces the
   * previous attachment (single-viewer app).
   */
  attach(iframe: HTMLIFrameElement): void {
    if (this.iframe === iframe) return;
    this.installWindowListener();
    this.iframe = iframe;
    this.ready = false;
    this.queue = [];
  }

  /**
   * Drop the attachment. Pass the element to make unmount-ordering safe: a
   * stale detach (for an iframe that has already been replaced) is a no-op.
   */
  detach(iframe?: HTMLIFrameElement): void {
    if (iframe !== undefined && this.iframe !== iframe) return;
    this.iframe = null;
    this.ready = false;
    this.queue = [];
  }

  /** Subscribe to validated inbound messages; returns an unsubscribe fn. */
  onMessage(listener: BridgeListener): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  /**
   * Replace the full decoration set in the artifact (empty array clears).
   * Decorations do not survive an iframe reload — re-send on every `ready`.
   */
  setHighlights(highlights: HighlightSpec[]): void {
    this.send({ type: "set_highlights", highlights });
  }

  /** Replace layout-neutral diff gutter markers. This is a separate bridge
   * channel so comment highlights and selections remain untouched. */
  setDiffMarkers(markers: DiffMarkerSpec[]): void {
    this.send({ type: "set_diff_markers", markers });
  }

  /** Smooth-scroll the anchored element/range into view with a brief pulse.
   *  `key` lets the bridge target an existing decoration exactly. */
  scrollToAnchor(anchor: Anchor, key?: string): void {
    this.send({ type: "scroll_to_anchor", anchor, ...(key != null ? { key } : null) });
  }

  private send(msg: Record<string, unknown>): void {
    if (this.iframe == null) return; // no viewer: drop silently
    if (!this.ready) {
      // The bridge script isn't listening yet; flushed on `ready`.
      this.queue.push(msg);
      return;
    }
    this.post(msg);
  }

  private post(msg: Record<string, unknown>): void {
    const target = this.iframe?.contentWindow;
    if (target == null) return;
    // "*" is required: an opaque-origin frame cannot be named. The payload
    // contains only anchors/keys — nothing sensitive (see module header).
    target.postMessage({ cfy: PROTOCOL_VERSION, ...msg }, "*");
  }

  private installWindowListener(): void {
    if (this.windowListenerInstalled) return;
    this.windowListenerInstalled = true;
    window.addEventListener("message", (event: MessageEvent) => {
      // S2 gate: only the attached, opaque-origin artifact frame gets through.
      if (this.iframe == null) return;
      if (event.origin !== "null") return;
      if (event.source !== this.iframe.contentWindow) return;

      const message = parseMessage(event.data);
      if (message == null) {
        // Unknown/malformed types are expected across versions (and artifact
        // JS can post junk): drop, but leave a trace for development.
        console.debug("[bridge] ignored message from artifact frame:", event.data);
        return;
      }

      if (message.type === "ready") {
        this.ready = true;
        const pending = this.queue;
        this.queue = [];
        for (const queued of pending) this.post(queued);
      }
      for (const listener of this.listeners) listener(message);
    });
  }
}

/** The app-wide bridge instance (single-viewer app, mirroring appStore). */
export const artifactBridge = new ArtifactBridge();
