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
 *   shell -> artifact : set_highlights | scroll_to_anchor
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
    clearTimeout(selectionTimer);
    if (pointerDown) return; // mid-gesture: wait for release to report
    selectionTimer = setTimeout(reportSelection, SETTLE_MS);
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

    var anchor = { v: 1, type: "text", quote: quote };
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

  document.addEventListener("click", function (ev) {
    try {
      var sel = document.getSelection();
      if (sel && !sel.isCollapsed) return; // end of a selection drag, not a click
      var target = ev.target instanceof Element ? ev.target : null;
      if (!target) return;
      // Don't hijack genuinely interactive artifact elements.
      if (
        target.closest(
          "a[href], button, input, textarea, select, summary, label, [contenteditable]"
        )
      ) {
        return;
      }
      var host = target.closest("[data-cfy-id]");
      if (!host) return;
      var anchor = { v: 1, type: "element", cfy_id: host.getAttribute("data-cfy-id") };
      // Optional re-attachment hint: the element's text, whitespace-collapsed.
      // Omitted when empty (purely graphical node) or implausibly long.
      var text = (host.textContent || "").replace(/\s+/g, " ").trim();
      if (text && text.length <= ELEMENT_QUOTE_MAX) anchor.quote = { exact: text };
      post({
        type: "element_click",
        anchor: anchor,
        rect: rectOf(host.getBoundingClientRect()),
      });
    } catch (e) {
      /* never break the artifact */
    }
  });

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
    // Highlight decorations for existing comment anchors (shell-commanded).
    ":where(span[data-cfy-hl='text']) { background-color: rgba(255, 196, 0, 0.28); border-radius: 2px; }",
    ":where([data-cfy-hl='element']) { outline: 2px solid rgba(255, 170, 0, 0.55); outline-offset: 3px; }",
    // Scroll-to-anchor attention pulse (opacity: works for HTML and SVG).
    "@keyframes cfy-pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.35; } }",
    ":where([data-cfy-pulse]) { animation: cfy-pulse 0.6s ease-in-out 2; }",
  ].join("\n");
  (document.head || document.documentElement).appendChild(style);

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
      try {
        var deco = applyHighlight(key, anchor);
        if (deco) decorations.push(deco);
      } catch (e) {
        /* one bad anchor must not break the rest */
      }
    }
  }

  function markElement(el, key) {
    el.setAttribute("data-cfy-hl", "element");
    if (key) el.setAttribute("data-cfy-hl-key", key);
    return { key: key, element: el, spans: [] };
  }

  function applyHighlight(key, anchor) {
    if (anchor.type === "element") {
      var el = byCfyId(anchor.cfy_id);
      return el ? markElement(el, key) : null;
    }
    if (anchor.type === "text") {
      var range = resolveTextRange(anchor);
      if (range) {
        var spans = wrapRange(range, key);
        if (spans.length > 0) return { key: key, element: null, spans: spans };
        // Range resolved but nothing wrappable (e.g. text inside SVG):
        // decorate the nearest id-bearing ancestor instead.
        var near = cfyAncestor(range.startContainer);
        return near ? markElement(near, key) : null;
      }
      // Neither offsets nor quote resolved: coarse fallback to the host
      // element so the comment still has *some* visual anchor.
      var host = byCfyId(anchor.cfy_id);
      return host ? markElement(host, key) : null;
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
  function wrapRange(range, key) {
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
    if (targets.length === 0) return;

    try {
      targets[0].scrollIntoView({ behavior: "smooth", block: "center" });
    } catch (e) {
      targets[0].scrollIntoView();
    }
    for (var i = 0; i < targets.length; i++) {
      targets[i].removeAttribute("data-cfy-pulse");
      void targets[i].offsetWidth; // restart a mid-flight animation
      targets[i].setAttribute("data-cfy-pulse", "");
    }
    setTimeout(function () {
      for (var i = 0; i < targets.length; i++) {
        targets[i].removeAttribute("data-cfy-pulse");
      }
    }, 1500);
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
        setHighlights(Array.isArray(d.highlights) ? d.highlights : []);
      } else if (d.type === "scroll_to_anchor") {
        scrollToAnchor(d);
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
