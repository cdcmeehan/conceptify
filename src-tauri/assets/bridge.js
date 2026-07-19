/*
 * Conceptify in-artifact bridge (PRD §5.4, §9 S2; bead conceptify-94m.1).
 *
 * Injected at serve time by the artifact:// protocol handler
 * (src-tauri/src/artifact_protocol.rs) just before </body>; the on-disk
 * artifact file NEVER contains this script (G3). It runs inside the
 * artifact's opaque-origin sandboxed iframe, alongside the artifact's own
 * inline JS, and provides the interaction layer for comments:
 *
 *   artifact -> shell : ready | selection | selection_cleared | element_click
 *   shell -> artifact : set_highlights | set_diff_markers | scroll_to_anchor
 *                       | set_theme
 *
 * The full protocol (envelope, payloads, coordinate + offset conventions,
 * trust model) is documented in docs/api.md, section "Bridge protocol" —
 * that doc is the contract; change it first.
 *
 * Constraints honoured here:
 *  - No capabilities beyond reporting quotes/ids/rects upward: the bridge
 *    never relays arbitrary DOM HTML, and it holds no secrets (it cannot —
 *    connect-src 'none', opaque origin).
 *  - Non-destructive decorations: highlight wraps are plain inline <span>s
 *    that are fully unwrapped (and text nodes re-normalized) on clear;
 *    element decorations are attributes in the reserved data-cfy-* space.
 *  - Zero-specificity injected styles (:where()) so artifact CSS always wins.
 *  - Safari/WKWebView-compatible JS only.
 *  - This file MUST NOT contain a literal script tag open/close sequence
 *    (it is inlined into an HTML script element; a Rust test guards this).
 */
(function () {
  "use strict";

  // ---- Live theming (epic conceptify-89k, artifact-spec §1.5) -------------
  // The protocol handler stamps the app's CURRENT artifact-theme id on this
  // script tag at serve time. Apply it to the root element FIRST — this
  // script is synchronous at end-of-body, i.e. before first paint for
  // locally served documents — so the in-app view shows the live setting
  // (overriding whatever theme the artifact was authored under) with no
  // flash of the authored theme. Later theme changes arrive as `set_theme`
  // messages (dispatch below). Plain-browser opens have no bridge and keep
  // the authored data-cfy-theme stamp (or Manuscript if none).
  var VALID_THEMES = { manuscript: 1, blueprint: 1, sketchbook: 1 };
  function applyTheme(theme) {
    if (typeof theme === "string" && VALID_THEMES[theme] === 1) {
      document.documentElement.setAttribute("data-cfy-theme", theme);
    }
  }
  applyTheme(
    document.currentScript &&
      document.currentScript.getAttribute("data-cfy-theme")
  );

  // Idempotence at runtime (server-side injection is already idempotent via
  // the data-cfy-bridge marker; this guards pathological double-execution).
  if (window.__cfyBridge) return;
  window.__cfyBridge = Object.freeze({ v: 1 });

  // Opened outside the app shell (should be impossible — the script never
  // exists on disk — but stay inert rather than talking to ourselves).
  var shell = window.parent !== window ? window.parent : null;
  if (!shell) return;

  var CONTEXT_CHARS = 32; // quote prefix/suffix context length
  var ELEMENT_QUOTE_MAX = 300; // omit element quotes longer than this

  function post(msg) {
    // An opaque-origin frame cannot name the shell's origin, so "*" is the
    // only possible targetOrigin. Payloads carry only ids/quotes/rects.
    msg.cfy = 1;
    shell.postMessage(msg, "*");
  }

  function rectOf(r) {
    // Iframe-viewport CSS px (getBoundingClientRect coordinate space).
    return { x: r.x, y: r.y, width: r.width, height: r.height };
  }

  // -------------------------------------------------------------------------
  // Visible-text measurement (the anchor offset convention).
  //
  // "Visible text" of an element = the concatenation of its Text node data in
  // document order, EXCLUDING text inside script/style/noscript/template
  // subtrees. Offsets are UTF-16 code units into that string; whitespace is
  // NOT normalized. This is the convention `start`/`end` in text anchors use
  // (docs/api.md "Bridge protocol") — re-attachment (conceptify-94m.7) must
  // measure the same way. Excluding script/style makes offsets independent of
  // the (volatile) inline JS/CSS and of this injected script itself.
  // -------------------------------------------------------------------------

  function isSkipped(textNode) {
    for (var el = textNode.parentElement; el; el = el.parentElement) {
      var tag = el.tagName.toUpperCase();
      if (tag === "SCRIPT" || tag === "STYLE" || tag === "NOSCRIPT" || tag === "TEMPLATE") {
        return true;
      }
    }
    return false;
  }

  /** All non-skipped Text nodes under root, in document order. */
  function textNodesIn(root) {
    var walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, null);
    var nodes = [];
    var n;
    while ((n = walker.nextNode())) {
      if (!isSkipped(n)) nodes.push(n);
    }
    return nodes;
  }

  function visibleText(root) {
    var s = "";
    var nodes = textNodesIn(root);
    for (var i = 0; i < nodes.length; i++) s += nodes[i].data;
    return s;
  }

  /**
   * Visible-text offset of the DOM boundary point (node, offset) within
   * root, or null if the point cannot be interpreted.
   */
  function visibleOffset(root, node, offset) {
    var boundary = document.createRange();
    try {
      boundary.setStart(node, offset);
      boundary.collapse(true);
    } catch (e) {
      return null;
    }
    var acc = 0;
    var nodes = textNodesIn(root);
    for (var i = 0; i < nodes.length; i++) {
      var t = nodes[i];
      if (t === node) return acc + Math.min(offset, t.data.length);
      var cmp;
      try {
        // Where does the point (t, end-of-t) sit relative to the boundary?
        cmp = boundary.comparePoint(t, t.data.length);
      } catch (e) {
        return null;
      }
      if (cmp === 1) return acc; // t starts at/after the boundary
      acc += t.data.length; // t (incl. its end) is before/at the boundary
    }
    return acc;
  }

  /** Map visible-text offsets [start, end) within root back to a DOM Range. */
  function rangeFromOffsets(root, start, end) {
    if (!(start >= 0) || !(end > start)) return null;
    var range = document.createRange();
    var haveStart = false;
    var acc = 0;
    var nodes = textNodesIn(root);
    for (var i = 0; i < nodes.length; i++) {
      var t = nodes[i];
      var len = t.data.length;
      if (!haveStart && start < acc + len) {
        range.setStart(t, start - acc);
        haveStart = true;
      }
      if (haveStart && end <= acc + len) {
        range.setEnd(t, end - acc);
        return range;
      }
      acc += len;
    }
    return null; // offsets ran past the end of the text
  }

  function byCfyId(id) {
    if (typeof id !== "string" || id.length === 0) return null;
    // The id grammar is [a-z0-9.-], but escape defensively for the selector.
    var esc = id.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
    try {
      return document.querySelector('[data-cfy-id="' + esc + '"]');
    } catch (e) {
      return null;
    }
  }

  /** Nearest ancestor-or-self element bearing data-cfy-id. */
  function cfyAncestor(node) {
    var el = node && (node.nodeType === 1 ? node : node.parentElement);
    return el ? el.closest("[data-cfy-id]") : null;
  }

  // -------------------------------------------------------------------------
  // (a) Text-selection reporting
  //
  // Selection is reported on GESTURE COMPLETION, never mid-drag: while a
  // pointer is pressed the shell must not see a 'selection' (its action
  // popover would otherwise pop up while the user is still dragging — bead
  // conceptify-vu1.1). Two settle paths cover the two ways a selection ends:
  //
  //   - Pointer selections (mouse/trackpad/pen): every selectionchange is
  //     suppressed while a pointer is down; on release (pointerup, or
  //     pointercancel if the OS/gesture takes over) the final selection is
  //     posted exactly once. pointerdown/up (not mousedown/up) so trackpad
  //     and pen gestures count too.
  //   - Keyboard selections (shift+arrows, Cmd+A) have no pointerup, so a
  //     debounced settle path posts once the selection stops changing.
  //
  // 'selection_cleared' semantics are unchanged: reportSelection() emits it
  // whenever the selection resolves to nothing after having been non-empty
  // (so a drag that ends collapsed clears, and never posts a 'selection').
  // -------------------------------------------------------------------------

  var SETTLE_MS = 300; // keyboard-selection settle debounce (no pointer down)
  var selectionTimer = 0;
  var hadSelection = false;
  // Boolean, not a pointer counter: a missed pointerup (e.g. release outside
  // the frame) self-heals on the next gesture rather than wedging suppression.
  var pointerDown = false;
  // True while set_highlights mutates the DOM (wrap/unwrap/normalize fire
  // selectionchange for a live selection even though the user did nothing).
  // Without this guard, saving a comment re-reports the still-live selection
  // ~SETTLE_MS later and the shell pops its toolbar right back up over the
  // just-saved highlight (bead conceptify-vu1.4).
  var selfMutation = false;

  document.addEventListener("pointerdown", function () {
    // A fresh pointer gesture supersedes any pending keyboard settle.
    pointerDown = true;
    clearTimeout(selectionTimer);
  });

  // pointerup fires before the 'click' handler below, so element_click's
  // 'sel && !sel.isCollapsed' guard still sees the just-finalized selection.
  document.addEventListener("pointerup", endPointerGesture);
  document.addEventListener("pointercancel", endPointerGesture);

  function endPointerGesture() {
    if (!pointerDown) return; // stray release without a tracked press
    pointerDown = false;
    clearTimeout(selectionTimer);
    // Report the finalized selection once. A collapsed selection resolves to
    // range === null in reportSelection(), so a click or a drag that ends
    // collapsed posts no 'selection' (only selection_cleared if one was live).
    reportSelection();
  }

  document.addEventListener("selectionchange", function () {
    // Ignore selectionchange caused by our own highlight mutations — return
    // BEFORE touching the timer so a genuinely pending user settle survives.
    if (selfMutation) return;
    clearTimeout(selectionTimer);
    if (pointerDown) return; // mid-gesture: wait for release to report
    selectionTimer = setTimeout(reportSelection, SETTLE_MS);
  });

  // Escape cancels the selection (and with it the shell's action popover, via
  // the ordinary selection_cleared path). Necessary because after a drag the
  // artifact iframe holds keyboard focus, so the shell's own Escape handler
  // never sees the key; the bridge translates it into the one thing it owns:
  // clearing the live selection. The shell's dirty guard still applies — a
  // composer with typed content ignores selection_cleared (and while typing,
  // focus is in the shell textarea, so this handler doesn't run at all).
  document.addEventListener("keydown", function (ev) {
    try {
      if (ev.key !== "Escape") return;
      var sel = document.getSelection();
      if (!sel || sel.isCollapsed) return;
      sel.removeAllRanges();
      clearTimeout(selectionTimer);
      reportSelection(); // posts selection_cleared immediately (no settle lag)
    } catch (e) {
      /* never break the artifact */
    }
  });

  function reportSelection() {
    try {
      var sel = document.getSelection();
      var range =
        sel && sel.rangeCount > 0 && !sel.isCollapsed ? sel.getRangeAt(0) : null;
      var anchor = range ? captureTextAnchor(range) : null;
      if (!anchor) {
        if (hadSelection) {
          hadSelection = false;
          post({ type: "selection_cleared" });
        }
        return;
      }
      hadSelection = true;
      post({
        type: "selection",
        anchor: anchor,
        rect: rectOf(range.getBoundingClientRect()),
      });
    } catch (e) {
      /* the bridge must never break the artifact */
    }
  }

  /** Build a {v:1, type:"text", ...} anchor from a live Range, or null. */
  function semanticKind(el) {
    if (!el) return "text";
    if (el.closest("pre, code")) return "code";
    if (el.closest("svg, [role=img]")) return "diagram";
    if (el.closest("img, picture")) return "image";
    if (el.closest("figure")) return "figure";
    if (el.closest("p, li, blockquote, table, section, article")) return "block";
    return "text";
  }

  function semanticLabel(el, fallback) {
    if (!el) return fallback;
    var figure = el.closest("figure");
    var caption = figure && figure.querySelector("figcaption");
    var label = el.getAttribute("aria-label") || el.getAttribute("alt") || el.getAttribute("title") ||
      (caption && caption.textContent) || fallback;
    return (label || "Selected content").replace(/\s+/g, " ").trim().slice(0, 160);
  }

  function diagramRole(el) {
    if (!el || !el.closest("svg, [role=img]")) return null;
    var explicit = el.getAttribute("data-cfy-role");
    if (explicit) return explicit.replace(/\s+/g, " ").trim().slice(0, 80);
    var classes = " " + (el.getAttribute("class") || "").toLowerCase() + " ";
    if (classes.indexOf(" node ") !== -1) return "node";
    if (classes.indexOf(" edge ") !== -1 || classes.indexOf(" connection ") !== -1) return "connection";
    if (classes.indexOf(" cluster ") !== -1 || classes.indexOf(" group ") !== -1) return "group";
    var tag = el.tagName.toLowerCase();
    if (tag === "g") return "diagram element";
    if (["rect", "circle", "ellipse", "polygon", "path"].indexOf(tag) !== -1) return "shape";
    return "diagram";
  }

  function diagramRelationships(el) {
    if (!el || !el.closest("svg, [role=img]")) return [];
    var values = [];
    function add(value) {
      if (!value) return;
      var parts = String(value).split(/[|,;]/);
      for (var i = 0; i < parts.length && values.length < 8; i += 1) {
        var clean = parts[i].replace(/\s+/g, " ").trim().slice(0, 160);
        if (clean && values.indexOf(clean) === -1) values.push(clean);
      }
    }
    add(el.getAttribute("data-cfy-relationships"));
    add(el.getAttribute("data-cfy-rel"));
    var from = el.getAttribute("data-cfy-from");
    var to = el.getAttribute("data-cfy-to");
    if (from || to) add("Connects " + (from || "unknown") + " to " + (to || "unknown"));
    var describedBy = el.getAttribute("aria-describedby");
    if (describedBy) {
      var ids = describedBy.split(/\s+/);
      for (var j = 0; j < ids.length; j += 1) {
        var desc = document.getElementById(ids[j]);
        if (desc) add(desc.textContent);
      }
    }
    var title = el.querySelector && el.querySelector(":scope > title");
    if (title && (/->|--|→/.test(title.textContent || "") || diagramRole(el) === "connection")) {
      add(title.textContent);
    }
    return values;
  }

  function semanticTargetForElement(host, text) {
    var target = {
      kind: semanticKind(host),
      label: semanticLabel(host, text || host.getAttribute("data-cfy-id") || "Diagram element"),
      excerpt: text.slice(0, 240),
      cfy_ids: [host.getAttribute("data-cfy-id")],
      multi_block: false,
    };
    var role = diagramRole(host);
    var relationships = diagramRelationships(host);
    if (role) target.role = role;
    if (relationships.length) target.relationships = relationships;
    return target;
  }

  function semanticTargetForRange(range, exact) {
    var start = range.startContainer.nodeType === 1 ? range.startContainer : range.startContainer.parentElement;
    var ids = [];
    var nodes = document.querySelectorAll("[data-cfy-id]");
    for (var i = 0; i < nodes.length && ids.length < 8; i += 1) {
      try {
        if (range.intersectsNode(nodes[i])) ids.push(nodes[i].getAttribute("data-cfy-id"));
      } catch (_) {}
    }
    var excerpt = exact.replace(/\s+/g, " ").trim().slice(0, 240);
    return {
      kind: semanticKind(start),
      label: semanticLabel(start, excerpt),
      excerpt: excerpt,
      cfy_ids: ids,
      multi_block: ids.length > 1,
    };
  }

  function captureTextAnchor(range) {
    var body = document.body;
    if (!body) return null;
    var s = visibleOffset(body, range.startContainer, range.startOffset);
    var e = visibleOffset(body, range.endContainer, range.endOffset);
    if (s == null || e == null || e <= s) return null;
    var bodyText = visibleText(body);
    var exact = bodyText.slice(s, e);
    if (exact.replace(/\s/g, "").length === 0) return null; // whitespace-only

    var quote = { exact: exact };
    var prefix = bodyText.slice(Math.max(0, s - CONTEXT_CHARS), s);
    var suffix = bodyText.slice(e, e + CONTEXT_CHARS);
    if (prefix) quote.prefix = prefix;
    if (suffix) quote.suffix = suffix;

    var anchor = { v: 1, type: "text", quote: quote, target: semanticTargetForRange(range, exact) };
    var host = cfyAncestor(range.commonAncestorContainer);
    if (host) {
      var hs = visibleOffset(host, range.startContainer, range.startOffset);
      var he = visibleOffset(host, range.endContainer, range.endOffset);
      if (hs != null && he != null && he > hs) {
        anchor.cfy_id = host.getAttribute("data-cfy-id");
        anchor.start = hs;
        anchor.end = he;
      }
    }
    return anchor;
  }

  // -------------------------------------------------------------------------
  // (b) Click-to-comment on data-cfy-id elements
  // -------------------------------------------------------------------------

  function isInteractiveTarget(target) {
    return !!target.closest(
      "a[href], button, input, textarea, select, summary, label, [contenteditable]"
    );
  }

  function reportElement(host) {
    var anchor = { v: 1, type: "element", cfy_id: host.getAttribute("data-cfy-id") };
    var text = (host.textContent || "").replace(/\s+/g, " ").trim();
    if (text && text.length <= ELEMENT_QUOTE_MAX) anchor.quote = { exact: text };
    anchor.target = semanticTargetForElement(host, text);
    post({
      type: "element_click",
      anchor: anchor,
      rect: rectOf(host.getBoundingClientRect()),
    });
  }

  function reportSuggestion(suggestion) {
    post({
      type: "suggestion_click",
      cfy_id: suggestion.getAttribute("data-cfy-id"),
      question: suggestion.getAttribute("data-cfy-next-question") || "",
      reason: suggestion.getAttribute("data-cfy-reason") || "Builds on this explanation.",
      branch: suggestion.getAttribute("data-cfy-branch") || "mechanism",
      rect: rectOf(suggestion.getBoundingClientRect()),
    });
  }

  document.addEventListener("click", function (ev) {
    try {
      var sel = document.getSelection();
      if (sel && !sel.isCollapsed) return; // end of a selection drag, not a click
      var target = ev.target instanceof Element ? ev.target : null;
      if (!target) return;
      var suggestion = target.closest("[data-cfy-next-question][data-cfy-id]");
      if (suggestion) {
        ev.preventDefault();
        ev.stopPropagation();
        reportSuggestion(suggestion);
        return;
      }
      // Don't hijack genuinely interactive artifact elements.
      if (isInteractiveTarget(target)) return;
      var host = target.closest("[data-cfy-id]");
      if (!host) return;
      if (diagramRole(host)) {
        // A diagram inspection is not a slide/navigation gesture.
        ev.preventDefault();
        ev.stopPropagation();
      }
      reportElement(host);
    } catch (e) {
      /* never break the artifact */
    }
  }, true);

  var diagramNodes = document.querySelectorAll(
    "svg[data-cfy-id], svg [data-cfy-id], [role=img][data-cfy-id], [role=img] [data-cfy-id]"
  );
  for (var diagramIndex = 0; diagramIndex < diagramNodes.length; diagramIndex += 1) {
    var diagramNode = diagramNodes[diagramIndex];
    if (!diagramNode.hasAttribute("tabindex")) diagramNode.setAttribute("tabindex", "0");
    if (diagramNode.tagName.toLowerCase() !== "svg" && !diagramNode.hasAttribute("role")) {
      diagramNode.setAttribute("role", "button");
    }
    if (!diagramNode.hasAttribute("aria-label")) {
      var diagramText = (diagramNode.textContent || "").replace(/\s+/g, " ").trim();
      diagramNode.setAttribute(
        "aria-label",
        semanticLabel(diagramNode, diagramText || diagramNode.getAttribute("data-cfy-id"))
      );
    }
  }

  var suggestionNodes = document.querySelectorAll("[data-cfy-next-question][data-cfy-id]");
  for (var suggestionIndex = 0; suggestionIndex < suggestionNodes.length; suggestionIndex += 1) {
    var suggestionNode = suggestionNodes[suggestionIndex];
    if (!suggestionNode.hasAttribute("tabindex")) suggestionNode.setAttribute("tabindex", "0");
    if (!suggestionNode.hasAttribute("role")) suggestionNode.setAttribute("role", "button");
    if (!suggestionNode.hasAttribute("aria-label")) {
      suggestionNode.setAttribute("aria-label", suggestionNode.getAttribute("data-cfy-next-question"));
    }
  }

  document.addEventListener("keydown", function (ev) {
    try {
      var target = ev.target instanceof Element ? ev.target : null;
      var host = target && target.closest("[data-cfy-id]");
      if (!host) return;
      var suggestion = host.closest("[data-cfy-next-question][data-cfy-id]");
      if (suggestion && (ev.key === "Enter" || ev.key === " ")) {
        ev.preventDefault();
        ev.stopPropagation();
        reportSuggestion(suggestion);
        return;
      }
      if (!diagramRole(host) || isInteractiveTarget(target)) return;
      if (ev.key === "Enter" || ev.key === " ") {
        ev.preventDefault();
        ev.stopPropagation();
        reportElement(host);
        return;
      }
      if (["ArrowLeft", "ArrowUp", "ArrowRight", "ArrowDown"].indexOf(ev.key) === -1) return;
      var diagram = host.closest("svg, [role=img]");
      if (!diagram) return;
      var peers = Array.prototype.slice.call(diagram.querySelectorAll("[data-cfy-id][tabindex]"));
      if (diagram.matches("[data-cfy-id][tabindex]")) peers.unshift(diagram);
      var index = peers.indexOf(host);
      if (index === -1 || peers.length < 2) return;
      var delta = ev.key === "ArrowLeft" || ev.key === "ArrowUp" ? -1 : 1;
      var next = peers[(index + delta + peers.length) % peers.length];
      ev.preventDefault();
      ev.stopPropagation();
      next.focus();
    } catch (e) {
      /* never break the artifact */
    }
  }, true);

  // Hover affordance (always-on for v1 — see docs/api.md "Bridge protocol").
  // :where() pins specificity at zero so any artifact rule overrides these.
  var style = document.createElement("style");
  style.setAttribute("data-cfy-bridge", "style");
  style.textContent = [
    // :hover sits INSIDE :where so the whole selector stays specificity 0 —
    // otherwise it would out-rank the (also zero) highlight rules below.
    ":where([data-cfy-id]:hover) { outline: 1px dashed rgba(128,128,128,0.6); outline-offset: 2px; }",
    // Nicer, theme-following color where color-mix is supported (Safari 16.2+).
    ":where([data-cfy-id]:hover) { outline: 1px dashed color-mix(in srgb, currentColor 35%, transparent); }",
    ":where(svg [data-cfy-id], svg[data-cfy-id], [role=img] [data-cfy-id], [role=img][data-cfy-id]) { cursor: pointer; }",
    ":where(svg [data-cfy-id]:focus-visible, svg[data-cfy-id]:focus-visible, [role=img] [data-cfy-id]:focus-visible, [role=img][data-cfy-id]:focus-visible) { outline: 2px solid currentColor; outline-offset: 3px; }",

    // ---- Saved comment highlights (bead conceptify-vu1.3) ------------------
    // Warm terracotta family (coheres with the shell/artifact accent) tuned
    // for VISIBILITY first. Still zero-specificity :where() so the artifact's
    // own CSS wins. These are the LIGHT-theme values; the prefers-color-scheme
    // block at the very end re-tints them for dark (it must stay last so its
    // equal-specificity rules win the cascade in dark mode).
    //
    // Text: a translucent fill gives the at-a-glance highlighter read on
    // prose, and an OPAQUE bottom-border accent keeps the mark visible where
    // the fill is swallowed by a tinted ground (Shiki code blocks, callouts,
    // tables). The fill stays translucent so the underlying ink/syntax colour
    // shows through and highlighted text keeps ~AA legibility in both themes.
    // (border-bottom on an inline box paints per wrapped line and does not
    // change line-box height, so it is layout-neutral / non-destructive.)
    ":where(span[data-cfy-hl='text']) { background-color: rgba(232, 126, 52, 0.32); border-bottom: 2px solid rgba(138, 63, 28, 0.95); border-radius: 2px; }",
    // Element: two cheap, transform/opacity-free layers — a solid accent
    // outline offset off the box, plus a soft translucent ring (box-shadow,
    // which follows the element's own border-radius) so the mark separates
    // from any artifact border it sits beside instead of doubling it up.
    ":where([data-cfy-hl='element']) { outline: 2px solid rgba(163, 77, 36, 0.9); outline-offset: 3px; box-shadow: 0 0 0 6px rgba(232, 126, 52, 0.2); }",
    ":where(span[data-cfy-hl-state='answered']) { background-color: transparent; border-bottom: 2px dotted rgba(53, 112, 128, 0.9); }",
    ":where([data-cfy-hl-state='answered'][data-cfy-hl='element']) { outline: 2px dotted rgba(53, 112, 128, 0.9); box-shadow: none; }",

    // Live drag selection. ::selection is a pseudo-ELEMENT, so it cannot be
    // wrapped in :where() (pseudo-elements are invalid there) — this is the one
    // deliberate exception to this file's zero-specificity rule. We keep it as
    // minimal as possible (a bare, element-less ::selection = specificity
    // 0-0-1) and inject it last, so it colours the drag in artifacts that ship
    // no ::selection of their own, while an author who wants their own can
    // still win with any element-qualified rule (e.g. `body ::selection`).
    // Colour is the same terracotta family as the saved highlight, a shade
    // stronger so the active drag pops.
    "::selection { background-color: rgba(232, 126, 52, 0.42); }",

    // Scroll-to-anchor attention pulse. Opacity works for HTML *and* SVG and
    // is colour-agnostic, so it stays coherent with the new tints — the pulsed
    // target already carries the terracotta highlight above.
    "@keyframes cfy-pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.35; } }",
    ":where([data-cfy-pulse]) { animation: cfy-pulse 0.6s ease-in-out 2; }",

    // Version-diff gutters live in document-level overlay nodes rather than on
    // artifact elements. They cannot replace an artifact's border/background/
    // shadow and compose independently with terracotta comment highlights.
    ":where(.cfy-diff-marker) { position: absolute; z-index: 2147483646; width: 3px; border-radius: 3px; pointer-events: none; background: #5686a5; }",
    ":where(.cfy-diff-marker[data-kind='added']) { background: #4f8a68; }",
    ":where(.cfy-diff-marker[data-kind='moved']) { background: #8069a8; }",
    ":where(.cfy-diff-marker[data-kind='removed']) { width: 0; border-left: 3px dashed #b45b54; background: transparent; }",
    "@media (prefers-reduced-motion: reduce) { :where([data-cfy-pulse]) { animation: none; } }",

    // Dark re-tint (prefers-color-scheme resolves inside the artifact iframe —
    // artifacts are dual-theme by contract). Lighter terracotta, slightly less
    // fill (protects light syntax text over vitesse-dark), brighter accents for
    // punch on the warm-charcoal ground. LAST so these win in dark mode.
    "@media (prefers-color-scheme: dark) {" +
      " :where(span[data-cfy-hl='text']) { background-color: rgba(224, 152, 99, 0.26); border-bottom-color: rgba(232, 169, 124, 0.95); }" +
      " :where([data-cfy-hl='element']) { outline-color: rgba(224, 152, 99, 0.95); box-shadow: 0 0 0 6px rgba(224, 152, 99, 0.24); }" +
      " :where(span[data-cfy-hl-state='answered']) { background-color: transparent; border-bottom-color: rgba(115, 190, 207, 0.95); }" +
      " :where([data-cfy-hl-state='answered'][data-cfy-hl='element']) { outline-color: rgba(115, 190, 207, 0.95); box-shadow: none; }" +
      " ::selection { background-color: rgba(224, 152, 99, 0.4); }" +
      " }",
  ].join("\n");
  (document.head || document.documentElement).appendChild(style);

  // -------------------------------------------------------------------------
  // Layered artifact outline
  // -------------------------------------------------------------------------

  var outlineLinks = Array.prototype.slice.call(
    document.querySelectorAll('.cfy-outline a[href^="#"]')
  );

  function targetForOutlineLink(link) {
    var href = link && link.getAttribute("href");
    if (!href || href.charAt(0) !== "#" || href.length < 2) return null;
    try {
      var id = decodeURIComponent(href.slice(1));
      return document.getElementById(id) || byCfyId(id);
    } catch (_) {
      return null;
    }
  }

  function openContainingDetails(target) {
    for (var node = target; node; node = node.parentElement) {
      if (node.tagName && node.tagName.toLowerCase() === "details") node.open = true;
    }
  }

  function markOutlineLocation(target) {
    if (!target) return;
    for (var i = 0; i < outlineLinks.length; i += 1) {
      var active = targetForOutlineLink(outlineLinks[i]) === target;
      if (active) outlineLinks[i].setAttribute("aria-current", "location");
      else outlineLinks[i].removeAttribute("aria-current");
    }
  }

  function restoreOutlineLocation() {
    if (!window.location.hash) return;
    var link = { getAttribute: function () { return window.location.hash; } };
    var target = targetForOutlineLink(link);
    if (!target) return;
    openContainingDetails(target);
    markOutlineLocation(target);
    try { target.scrollIntoView({ block: "start" }); } catch (_) { target.scrollIntoView(); }
  }

  for (var outlineIndex = 0; outlineIndex < outlineLinks.length; outlineIndex += 1) {
    outlineLinks[outlineIndex].addEventListener("click", function () {
      var target = targetForOutlineLink(this);
      if (target) openContainingDetails(target);
    });
  }
  window.addEventListener("hashchange", restoreOutlineLocation);
  window.addEventListener("popstate", restoreOutlineLocation);
  document.addEventListener("beforematch", function (ev) {
    if (ev.target instanceof Element) openContainingDetails(ev.target);
  });

  if (outlineLinks.length > 0) {
    restoreOutlineLocation();
    if (!window.location.hash) markOutlineLocation(targetForOutlineLink(outlineLinks[0]));
    if ("IntersectionObserver" in window) {
      var outlineTargets = outlineLinks.map(targetForOutlineLink).filter(function (target) { return !!target; });
      var outlineObserver = new IntersectionObserver(function (entries) {
        var visible = entries.filter(function (entry) { return entry.isIntersecting; });
        if (visible.length > 0) {
          visible.sort(function (a, b) { return Math.abs(a.boundingClientRect.top) - Math.abs(b.boundingClientRect.top); });
          markOutlineLocation(visible[0].target);
        }
      }, { rootMargin: "-10% 0px -70% 0px", threshold: 0 });
      for (var targetIndex = 0; targetIndex < outlineTargets.length; targetIndex += 1) {
        outlineObserver.observe(outlineTargets[targetIndex]);
      }
    }
  }

  // -------------------------------------------------------------------------
  // (c) Highlight decorations
  // -------------------------------------------------------------------------

  // Active decorations: { key, element: Element|null, spans: Element[] }.
  // set_highlights is full-replacement: clear everything, apply the new set.
  var decorations = [];

  function clearHighlights() {
    for (var i = 0; i < decorations.length; i++) {
      var deco = decorations[i];
      if (deco.element) {
        deco.element.removeAttribute("data-cfy-hl");
        deco.element.removeAttribute("data-cfy-hl-key");
        deco.element.removeAttribute("data-cfy-hl-state");
        deco.element.removeAttribute("data-cfy-pulse");
      }
      for (var j = 0; j < deco.spans.length; j++) unwrap(deco.spans[j]);
    }
    decorations = [];
  }

  /** Remove a wrapper span, reparenting its children and re-merging text. */
  function unwrap(span) {
    var parent = span.parentNode;
    if (!parent) return;
    while (span.firstChild) parent.insertBefore(span.firstChild, span);
    parent.removeChild(span);
    parent.normalize(); // undo splitText fragmentation (offset-neutral)
  }

  function setHighlights(list) {
    clearHighlights();
    for (var i = 0; i < list.length; i++) {
      var item = list[i];
      if (!item || typeof item !== "object") continue;
      var anchor = item.anchor;
      if (!anchor || typeof anchor !== "object") continue;
      var key = typeof item.key === "string" ? item.key : "";
      var state = item.state === "answered" ? "answered" : "saved";
      try {
        var deco = applyHighlight(key, anchor, state);
        if (deco) decorations.push(deco);
      } catch (e) {
        /* one bad anchor must not break the rest */
      }
    }
  }

  // Diff markers are layout-neutral gutter overlays positioned in document
  // coordinates beside their target block. Removed content targets its nearest
  // surviving neighbor, chosen by the shell from the diff response.
  var diffMarkers = [];

  function clearDiffMarkers() {
    for (var i = 0; i < diffMarkers.length; i++) {
      var marker = diffMarkers[i].marker;
      if (marker.parentNode) marker.parentNode.removeChild(marker);
    }
    diffMarkers = [];
  }

  function findCfyElement(id) {
    var elements = document.querySelectorAll("[data-cfy-id]");
    for (var i = 0; i < elements.length; i++) {
      if (elements[i].getAttribute("data-cfy-id") === id) return elements[i];
    }
    return null;
  }

  function positionDiffMarkers() {
    for (var i = 0; i < diffMarkers.length; i++) {
      var item = diffMarkers[i];
      var rect = item.target.getBoundingClientRect();
      item.marker.style.left = Math.max(2, rect.left + window.scrollX - 8) + "px";
      item.marker.style.top = rect.top + window.scrollY + "px";
      item.marker.style.height = Math.max(12, rect.height) + "px";
    }
  }

  function setDiffMarkers(list) {
    clearDiffMarkers();
    for (var i = 0; i < list.length; i++) {
      var item = list[i];
      if (!item || typeof item.cfy_id !== "string") continue;
      var target = findCfyElement(item.cfy_id);
      if (!target) continue;
      var marker = document.createElement("span");
      marker.className = "cfy-diff-marker";
      marker.setAttribute("data-kind", typeof item.kind === "string" ? item.kind : "modified");
      marker.setAttribute("aria-hidden", "true");
      (document.body || document.documentElement).appendChild(marker);
      diffMarkers.push({ target: target, marker: marker });
    }
    positionDiffMarkers();
  }

  window.addEventListener("resize", positionDiffMarkers);

  function markElement(el, key, state) {
    el.setAttribute("data-cfy-hl", "element");
    if (key) el.setAttribute("data-cfy-hl-key", key);
    el.setAttribute("data-cfy-hl-state", state);
    return { key: key, element: el, spans: [] };
  }

  function applyHighlight(key, anchor, state) {
    if (anchor.type === "element") {
      var el = byCfyId(anchor.cfy_id);
      return el ? markElement(el, key, state) : null;
    }
    if (anchor.type === "text") {
      var range = resolveTextRange(anchor);
      if (range) {
        var spans = wrapRange(range, key, state);
        if (spans.length > 0) return { key: key, element: null, spans: spans };
        // Range resolved but nothing wrappable (e.g. text inside SVG):
        // decorate the nearest id-bearing ancestor instead.
        var near = cfyAncestor(range.startContainer);
        return near ? markElement(near, key, state) : null;
      }
      // Neither offsets nor quote resolved: coarse fallback to the host
      // element so the comment still has *some* visual anchor.
      var host = byCfyId(anchor.cfy_id);
      return host ? markElement(host, key, state) : null;
    }
    return null; // unknown anchor type: ignore (forward compatibility)
  }

  /** Resolve a text anchor to a Range: offsets first (verified against the
   *  quote when present), then document-wide quote search. */
  function resolveTextRange(anchor) {
    var quote =
      anchor.quote && typeof anchor.quote.exact === "string" ? anchor.quote : null;
    var host = byCfyId(anchor.cfy_id);
    if (
      host &&
      typeof anchor.start === "number" &&
      typeof anchor.end === "number" &&
      anchor.end > anchor.start
    ) {
      var hostText = visibleText(host);
      var slice = hostText.slice(anchor.start, anchor.end);
      var intact =
        slice.length === anchor.end - anchor.start &&
        (!quote || slice === quote.exact);
      if (intact) {
        var range = rangeFromOffsets(host, anchor.start, anchor.end);
        if (range) return range;
      }
    }
    return quote ? findQuote(quote) : null;
  }

  /** W3C-style text-quote search over the document's visible text; prefix/
   *  suffix disambiguate repeated matches. */
  function findQuote(quote) {
    var body = document.body;
    if (!body || !quote.exact) return null;
    var bodyText = visibleText(body);
    var exact = quote.exact;
    var candidates = [];
    for (var i = bodyText.indexOf(exact); i !== -1; i = bodyText.indexOf(exact, i + 1)) {
      candidates.push(i);
      if (candidates.length >= 200) break; // degenerate documents
    }
    if (candidates.length === 0) return null;
    var best = candidates[0];
    var bestScore = -1;
    for (var c = 0; c < candidates.length; c++) {
      var at = candidates[c];
      var score = 0;
      if (
        quote.prefix &&
        bodyText.slice(Math.max(0, at - quote.prefix.length), at) === quote.prefix
      ) {
        score += 1;
      }
      if (
        quote.suffix &&
        bodyText.slice(at + exact.length, at + exact.length + quote.suffix.length) ===
          quote.suffix
      ) {
        score += 1;
      }
      if (score > bestScore) {
        bestScore = score;
        best = at;
      }
    }
    return rangeFromOffsets(body, best, best + exact.length);
  }

  var XHTML_NS = "http://www.w3.org/1999/xhtml";

  /**
   * Wrap every text node covered by `range` in an inline highlight span.
   * Boundary text nodes are split first (end before start, so the start
   * split cannot invalidate the end boundary — live ranges are adjusted by
   * splitText per the DOM spec). Purely inline, background-only styling, so
   * layout and typography are untouched; unwrap() fully reverses it.
   */
  function wrapRange(range, key, state) {
    if (
      range.endContainer.nodeType === 3 &&
      range.endOffset < range.endContainer.data.length
    ) {
      range.endContainer.splitText(range.endOffset);
    }
    if (range.startContainer.nodeType === 3 && range.startOffset > 0) {
      var tail = range.startContainer.splitText(range.startOffset);
      range.setStart(tail, 0);
    }

    // Collect whole-node coverage first, then mutate.
    var scopeNode = range.commonAncestorContainer;
    var scope = scopeNode.nodeType === 3 ? scopeNode.parentNode : scopeNode;
    if (!scope) return [];
    var covered = [];
    var walker = document.createTreeWalker(scope, NodeFilter.SHOW_TEXT, null);
    var n;
    while ((n = walker.nextNode())) {
      if (n.data.length === 0 || isSkipped(n)) continue;
      var inside;
      try {
        inside =
          range.comparePoint(n, 0) === 0 &&
          range.comparePoint(n, n.data.length) === 0;
      } catch (e) {
        inside = false;
      }
      if (inside) covered.push(n);
    }

    var spans = [];
    for (var i = 0; i < covered.length; i++) {
      var node = covered[i];
      var parent = node.parentElement;
      if (!parent || parent.namespaceURI !== XHTML_NS) continue; // SVG text: skip
      var span = document.createElement("span");
      span.setAttribute("data-cfy-hl", "text");
      if (key) span.setAttribute("data-cfy-hl-key", key);
      span.setAttribute("data-cfy-hl-state", state);
      parent.insertBefore(span, node);
      span.appendChild(node);
      spans.push(span);
    }
    return spans;
  }

  // -------------------------------------------------------------------------
  // (d) Scroll-to-anchor
  // -------------------------------------------------------------------------

  function findDecoration(key) {
    if (typeof key !== "string" || !key) return null;
    for (var i = 0; i < decorations.length; i++) {
      if (decorations[i].key === key) return decorations[i];
    }
    return null;
  }

  function scrollToAnchor(msg) {
    // Prefer an existing decoration for the key (already resolved, and the
    // pulse then lands exactly on the highlight); otherwise resolve fresh.
    var targets = [];
    var deco = findDecoration(msg.key);
    if (deco) targets = deco.element ? [deco.element] : deco.spans.slice();

    var anchor = msg.anchor;
    if (targets.length === 0 && anchor && typeof anchor === "object") {
      if (anchor.type === "element") {
        var el = byCfyId(anchor.cfy_id);
        if (el) targets = [el];
      } else if (anchor.type === "text") {
        var range = resolveTextRange(anchor);
        var target = range
          ? range.startContainer.parentElement || cfyAncestor(range.startContainer)
          : byCfyId(anchor.cfy_id);
        if (target) targets = [target];
      }
    }
    if (targets.length === 0) {
      if (typeof msg.request_id === "string") post({ type: "scroll_result", request_id: msg.request_id, found: false });
      return;
    }

    try {
      var reduced = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
      targets[0].scrollIntoView({ behavior: reduced ? "auto" : "smooth", block: "center" });
    } catch (e) {
      targets[0].scrollIntoView();
    }
    for (var i = 0; i < targets.length; i++) {
      targets[i].removeAttribute("data-cfy-pulse");
      if (reduced) continue;
      void targets[i].offsetWidth; // restart a mid-flight animation
      targets[i].setAttribute("data-cfy-pulse", "");
    }
    setTimeout(function () {
      for (var i = 0; i < targets.length; i++) {
        targets[i].removeAttribute("data-cfy-pulse");
      }
    }, 1500);
    if (typeof msg.request_id === "string") post({ type: "scroll_result", request_id: msg.request_id, found: true });
  }

  // -------------------------------------------------------------------------
  // Shell -> artifact command dispatch
  // -------------------------------------------------------------------------

  window.addEventListener("message", function (ev) {
    // Only the embedding shell may command the bridge. Artifact JS posting to
    // its own window has source === window, not window.parent.
    if (ev.source !== shell) return;
    var d = ev.data;
    if (!d || typeof d !== "object" || d.cfy !== 1 || typeof d.type !== "string") return;
    try {
      if (d.type === "set_highlights") {
        // Suppress the selectionchange our own wrap/unwrap mutations fire for
        // a live selection (see selfMutation above). selectionchange is queued
        // as a task when the selection is perturbed (during the mutation), so
        // it runs before this timeout clears the flag; 50ms of slack covers
        // engines that coalesce/defer the event. Pointer-release reporting is
        // NOT gated by this flag, so a user mid-drag loses nothing.
        selfMutation = true;
        try {
          setHighlights(Array.isArray(d.highlights) ? d.highlights : []);
        } finally {
          setTimeout(function () {
            selfMutation = false;
          }, 50);
        }
      } else if (d.type === "set_diff_markers") {
        setDiffMarkers(Array.isArray(d.markers) ? d.markers : []);
      } else if (d.type === "scroll_to_anchor") {
        scrollToAnchor(d);
      } else if (d.type === "set_theme") {
        // Live retheme while open (settings-changed in the shell). Unknown
        // ids are ignored — applyTheme validates against the closed set.
        applyTheme(d.theme);
      }
      // Unknown types: silently ignored (forward compatibility).
    } catch (e) {
      /* never break the artifact */
    }
  });

  // Handshake: the document above this script is fully parsed (the tag is
  // injected at end-of-body), so the shell may command highlights as soon as
  // it sees this.
  post({ type: "ready" });
})();
