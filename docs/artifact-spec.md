# Conceptify artifact specification

The contract between generating agents and the Conceptify app (PRD §7.3,
FR-3.1–3.6). This is the **single source of truth**: the `save-artifact`
validator is implemented from §8 of this doc, and the Claude Code skill
embeds this doc verbatim as its authoring rules. Neither may restate rules
in conflicting form — change them here first.

**How to read this doc.** Written for two consumers:

- **Authoring agents** — sections 1–7 are your dos and don'ts. The words
  MUST / MUST NOT / SHOULD / MAY are used in the RFC 2119 sense.
- **Validator implementers** — section 8 is the exact, testable rule set,
  with stable rule IDs (`E-*` = hard failure, `W-*` = warning). Every
  threshold is a concrete number.

Related PRD context: §5.4 (rendering & isolation), §6 (design system),
§8 (tiered visual strategy), §9 (security model), §10 (N2/N6),
Appendix A (WKWebView gotchas).

---

## 1. The artifact file (FR-3.1)

An artifact is a **single, self-contained `.html` file**.

- MUST be UTF-8 encoded, MUST begin with `<!doctype html>` (case-
  insensitive), and MUST declare `<meta charset="utf-8">` first in
  `<head>` — the file is also opened directly in browsers (FR-2.5), which
  need the declaration.
- All CSS MUST be inline (`<style>` blocks): the design-system scaffold
  (published with the skill; see D1–D6) first, artifact-specific styles
  after it. Dark mode via `prefers-color-scheme` MUST work (D2).
- All diagrams MUST be inline SVG (Tier 1, §8). Not `<img src>` — even to
  a data URI SVG is discouraged; inline SVG is required for anchorability
  (§4 below) and is the verified-safe path in WKWebView (Appendix A,
  wry #168).
- JS MUST be inline (`<script>` blocks) and used only when the explanation
  genuinely needs it. The artifact MUST be fully readable with JS disabled
  (D6, Tier 0).
- **No local file references, anywhere.** No relative URLs, no `file://`
  URLs, no references to files in the source repo. There is no "next to
  the file" — the artifact is copied into central storage (§5.6) and must
  survive alone, indefinitely (N6).
- **Network references** are permitted ONLY from the Tier-2 pinned-CDN
  allowlist (§7), and every network-dependent feature MUST degrade
  gracefully offline (§7.2). Tier-0/1 content MUST render correctly with
  zero network.
- Small raster images MAY be embedded as `data:` URIs; prefer SVG. Mind
  the size cap (§8: warn above 5 MiB, reject above 50 MiB).
- Explanatory visuals MUST encode a requested relationship, not decorate the
  page. Inline SVG uses `role="img"` and a useful `aria-label`; charts retain
  exact values in adjacent text/table form, and interactive models include a
  complete static fallback. If the fitting visual form is unsupported, the
  artifact states the fallback briefly and uses the closest textual structure.
- `<a href>` links to external websites MAY be used for further reading.
  They are inert inside the app's sandboxed viewer (no navigation
  permission) but work when the file is opened in a browser. Relative
  `href`s MUST NOT be used.
- Fonts: system-stack fallbacks only for now (D1). If the M2 design pass
  (OQ1) adopts web fonts, it will extend the §7 allowlist (e.g. with
  pinned `@fontsource/*` packages); until then artifacts MUST NOT load
  fonts over the network.

Reserved namespaces: the `data-cfy-*` attribute space, `cfy:*` meta names,
`<!--cfy:` comment openers, and `__cfy*` JS globals belong to Conceptify.
Artifacts MUST NOT invent their own uses of them beyond what this spec
defines, and MUST NOT assume the injected bridge script exists — the same
file opens bridge-less in a plain browser (§5.4).

### 1.1 Layered response structure

STANDARD and DEEP explanation artifacts MUST use progressive disclosure as an
authoring structure, never by clipping generated prose after the fact:

- A concise orientation and the core mental model are ordinary, always-visible
  sections. Together they MUST answer the question coherently without requiring
  a disclosure to be opened.
- Optional implementation detail, edge cases, derivations, and reference
  material MAY follow in native `<details class="cfy-details cfy-deep-dive">`
  elements. Each has a specific `<summary>` and remains part of the document
  text and accessibility tree through native HTML semantics.
- A multi-section artifact MUST provide `<nav class="cfy-outline"
  aria-label="On this page">` containing hash links to its semantic section
  ids. Each linked section/heading MUST carry a native `id` matching its stable
  `data-cfy-id`, so navigation also works without the bridge. Links MUST use
  stable descriptive ids, not positional fragments.
- The document MUST remain coherent with JavaScript disabled. Hash links use
  native browser history; Conceptify's injected bridge only enhances them by
  restoring disclosure state and active-location styling.
- Print/export MUST omit the navigation chrome and expose every deep-dive body.

COMPACT artifacts may omit the outline and deep-dive layer when the always-
visible answer is already short enough to scan.

### 1.2 Next-question branches

An artifact MAY end useful conceptual boundaries with two to four editable
next-question branches. Each branch is an element carrying a stable
`data-cfy-id`, `data-cfy-next-question`, `data-cfy-reason`, and
`data-cfy-branch`. Branch is one of `example`, `counterexample`, `mechanism`,
`tradeoff`, or `prerequisite`; generic “more detail” prompts are discouraged.
The reason says why the branch follows from this answer. Conceptify extracts
these hints on save for reuse on project home; generated markup never launches
work by itself.

## 2. Rendering targets (FR-3.2)

Every artifact MUST render correctly in **both**:

1. **WKWebView** (the in-app viewer — macOS system WebKit), and
2. **standalone browsers** (Safari and Chrome at minimum) via "Open in
   browser".

That means **Safari-compatible CSS/JS only — no Chromium-only features**.
Rule of thumb: if caniuse shows the feature red or partial for current
Safari, don't use it. Examples of things to avoid: Chromium-only CSS
(e.g. anchor positioning pre-Safari-26 behavior differences),
`showOpenFilePicker` and friends, non-standard `-webkit-`/`-blink-`
extensions.

WKWebView specifics (Appendix A) that MUST inform authoring:

- **60 fps rAF cap** pre-macOS-26: design animation for 60 fps; never
  assume 120.
- **Compositor-friendly animation only**: animate `transform` and
  `opacity`; avoid animating layout/paint properties (D6).
- **Inline SVG, not `<img src="*.svg">`** — SVG-in-img via custom
  protocols is broken in wry (wry #168), and external refs are banned
  anyway (§1).
- **SMIL** (in-SVG animation) works in WebKit but runs on the main
  thread — fine for light accents (`stroke-dashoffset` flows, small
  reveals), not for heavy continuous animation.

## 3. Runtime environment — what artifact JS can and cannot do (§5.4, S2)

In the app, the artifact is served via the `artifact://` scheme into a
sandboxed iframe with `sandbox="allow-scripts"` and **no
`allow-same-origin`** — an *opaque origin* — behind a per-response CSP.
The response CSP is owned by the protocol-handler implementation, but it
will be **at least as restrictive as** this reference policy, which
authors MUST treat as the runtime contract:

```
default-src 'none';
script-src  'unsafe-inline' https://cdn.jsdelivr.net;
style-src   'unsafe-inline' https://cdn.jsdelivr.net;
font-src    data: https://cdn.jsdelivr.net;
img-src     data:;
connect-src 'none';
```

Consequences your inline JS lives under:

- **No network I/O of any kind**: `fetch`, `XMLHttpRequest`, `WebSocket`,
  `EventSource`, `sendBeacon` all fail (`connect-src 'none'`) — including
  to the allowlisted CDN. All data MUST be embedded in the file. Loading
  *code/styles* from the allowlist via `<script src>` / `<link>` /
  `import` is the only permitted network access.
- **No storage**: in an opaque origin, `localStorage`, `sessionStorage`,
  IndexedDB, and cookies throw or are unavailable. Don't touch them, and
  wrap any library that might (some do feature-probing) so a
  `SecurityError` can't break rendering.
- **No frame escape**: `window.parent` / `window.top` are cross-origin;
  don't touch them. The app's bridge script (injected at serve time, never
  present on disk) owns all shell communication.
- **No chrome**: `window.open`, top navigation, downloads, `alert` /
  `confirm` / `prompt`, and form submission are all blocked by the sandbox
  flags. Don't rely on any of them.
- External images/media are blocked by CSP (`img-src data:` only) — use
  inline SVG or `data:` URIs.

Standalone browsers apply none of this, so anything that works in-app
works in the browser too; author to the in-app constraints.

## 4. Anchorability — `data-cfy-id` (FR-3.3)

Stable `data-cfy-id` attributes are what make text/diagram comments and
anchor re-attachment work (§7.4). They are the artifact's public API to
the comment system.

### 4.1 What MUST carry an id

- **Every section heading** `h1`–`h4` (`h5`/`h6` SHOULD).
- **Every figure/diagram wrapper** (`<figure>` or the top-level `<svg>`).
- **Every meaningful diagram element**, meaning:
  - every **node** / shape-with-label (MUST);
  - every **named container / cluster / group** (MUST);
  - every **edge** that carries a label or represents a step in a flow
    (MUST); unlabeled edges in dense graphs (SHOULD).
  - Rule of thumb: *if a reader might point at it and ask "why this?", it
    needs an id.* Stamp the id on the bounding `<g>` (shape + label
    together), not on individual `<path>`/`<text>` fragments, and don't
    set `pointer-events: none` on anchored elements — they must be
    clickable (FR-4.2).
- Callouts, tables, and step-sequence items MAY carry ids where a
  question seems likely.

### 4.2 Id grammar

```
id       = segment *( "." segment )        ; ≤ 64 chars total
segment  = lower [ ( lower / digit / "-" )* ( lower / digit ) ]
```

i.e. dot-separated kebab-case segments: lowercase `a-z`, digits, hyphens;
each segment starts with a letter and doesn't end with a hyphen. Regex:

```
^[a-z](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z](?:[a-z0-9-]*[a-z0-9])?)*$
```

Ids MUST be **unique within the document** and MUST be **semantic, not
positional**: named after the content (`sec-mental-model`,
`fig-auth-flow.token-service`), never numbered by position
(`sec-3`, `node-7`) — positional ids silently rebind to different content
when sections are inserted, which corrupts existing comment anchors.

Conventions (SHOULD): `sec-` prefix for headings, `fig-` for
figures/diagrams; diagram elements are namespaced under their figure id
with a dot: `<figure-id>.<element>`, e.g. `fig-auth-flow.client`,
`fig-auth-flow.client-to-gateway` for the edge client→gateway.

### 4.3 Stability across versions

Comment anchors reference these ids across artifact versions (FR-4.4,
FR-6.4). Therefore, in any follow-up update:

- An id that still refers to the same conceptual content MUST keep exactly
  the same value. **Never rename existing ids.**
- If content is genuinely removed, its id disappears with it; an id value
  MUST NOT be reused later for different content.
- New content gets new ids.
- When a diagram is regenerated from its embedded source (§5), the same
  conceptual nodes MUST receive the same ids as before — which the
  deterministic derivation below guarantees if node names in the DSL are
  kept stable.

### 4.4 Generated SVG (D2 / DOT / Mermaid): post-process to add ids

Generation-time renderers don't emit `data-cfy-id`; the agent MUST
post-process the inlined SVG to add them. Derive each element's id
segment from its **DSL name** deterministically:

1. lowercase the DSL identifier/label key;
2. replace `->` / `→` with `-to-`;
3. replace every run of characters outside `[a-z0-9]` with a single `-`;
4. trim leading/trailing `-`;
5. on collision within the figure, append `-2`, `-3`, … in document
   order.

Where to stamp, per tool:

- **Graphviz `dot -Tsvg`**: nodes, edges, and clusters are emitted as
  `<g class="node">`, `<g class="edge">`, `<g class="cluster">`, each
  containing a `<title>` with the DOT name (`a`, `a->b`, `cluster_x`).
  Map the title through the derivation above and stamp the `<g>`.
- **D2 (`d2 --layout=elk`)**: one `<g>` per shape and per connection;
  identify by the shape's key/label and stamp that `<g>`.
- **Mermaid (pre-rendered)**: node groups carry generated DOM ids
  containing the node name (e.g. `flowchart-client-1`); map the node name
  and stamp the group.

Hand-authored SVG (Tier-1 bespoke visuals) MUST be structured with a
`<g data-cfy-id="…">` per concept from the start — group each shape with
its label.

## 5. Embedded diagram sources (FR-3.4)

Every diagram rendered from a DSL at generation time (D2, DOT,
Mermaid, …) MUST embed its source in an HTML comment adjacent to the
rendered SVG, so follow-up agents **regenerate from source instead of
hand-editing SVG** (FR-4.7, FR-6.4). Hand-authored SVG has no source
comment. Tier-2 *runtime*-rendered diagrams (§7.2) already carry their
source visibly in the mandatory `<pre>` fallback — that satisfies FR-3.4
for them; do not duplicate it in a comment.

### 5.1 Comment format

```html
<!--cfy:src lang="d2" for="fig-request-flow" renderer="d2 v0.7 --layout=elk"
direction: right
client -> middleware: attach session
middleware -> handler: authorized request
-->
```

Exactly, for machine findability:

- The comment MUST open with `<!--cfy:src` — the string `cfy:src`
  immediately after `<!--`, no leading whitespace. (Finder regex:
  `<!--cfy:src\s`.)
- The rest of the first line is space-separated `key="value"` attributes
  (parse: `([a-z]+)="([^"]*)"`):
  - `lang` (required): the DSL — `d2`, `dot`, or `mermaid` (other values
    MAY be used for other tools).
  - `for` (required): the `data-cfy-id` of the rendered figure/SVG this
    source produces.
  - `renderer` (optional, free-form): exact tool version + flags used,
    for reproducibility.
- The **body** is everything after the first newline up to the newline
  before the closing `-->`, encoded per §5.2.
- **Placement**: the comment MUST be an immediately preceding sibling of
  the element carrying the matching `data-cfy-id` (only whitespace text
  nodes may sit between them).

### 5.2 Body encoding (comment-safe escaping)

HTML comments terminate at `-->` (and the parser also closes on `--!>`),
and Mermaid arrows are literally `-->` — so raw embedding is not safe.
Encode the DSL source as follows, in this order:

1. `\`  → `\\`
2. `-->` → `--\>`
3. `--!>` → `--!\>`

Decoding is a single left-to-right pass: `\\` → `\`, `\>` → `>` (a `\`
followed by anything else is left as-is). D2 and DOT sources are usually
untouched by rules 2–3 (`->` and `--` are fine as-is); Mermaid flowchart
arrows become `--\>`. Example:

```html
<!--cfy:src lang="mermaid" for="fig-request-lifecycle"
flowchart LR
  Client --\> Gateway
  Gateway --\>|verified| Service
-->
```

Agents consuming a source MUST decode before editing and re-encode before
writing it back.

## 6. Head metadata (FR-3.5)

Three `<meta>` tags MUST be present in `<head>`, plus a non-empty
`<title>`:

```html
<title>How the auth middleware works</title>
<meta name="cfy:question" content="Explain how the auth middleware works in this codebase.">
<meta name="cfy:version" content="1">
<meta name="cfy:generated-by" content="claude-code/claude-sonnet-5">
```

- **`cfy:question`** — the thread's *initial* question, verbatim plain
  text (normal HTML attribute escaping). Follow-up updates keep the
  original question; they do not replace it with the follow-up text.
- **`cfy:version`** — the artifact version this file was authored as: a
  positive integer, `1` for the initial artifact, previous + 1 for
  updates (the previous version is available via `conceptify
  get-context`). Informational: the server-assigned version is
  authoritative; a mismatch is a warning (§8), never a failure.
- **`cfy:generated-by`** — `<agent>/<model>`, e.g.
  `claude-code/claude-sonnet-5`; free-form if that shape doesn't fit.

## 7. Tier-2 pinned CDN allowlist (§8)

Tier 2 is an **opt-in escalation** for genuinely interactive content;
the default for all diagrams is generation-time-rendered inline SVG
(Tier 1). When runtime libraries are justified, they MUST come from this
allowlist and nowhere else.

All entries are served from **one host, `cdn.jsdelivr.net`** (npm
mirror) — a deliberate choice so the runtime CSP allows exactly one CDN
host and the validator check is a pure URL-prefix match.

### 7.1 The allowlist

| Purpose | Package (pinned) | URL prefix |
|---|---|---|
| Interactive diagrams (only when interaction is needed; always Neo/handDrawn + themeVariables, never defaults) | `mermaid@11` | `https://cdn.jsdelivr.net/npm/mermaid@11` |
| Mermaid ELK layout | `@mermaid-js/layout-elk@0` | `https://cdn.jsdelivr.net/npm/@mermaid-js/layout-elk@0` |
| JS-sequenced explainers (light) | `motion@12` | `https://cdn.jsdelivr.net/npm/motion@12` |
| JS-sequenced explainers (alt) | `animejs@4` | `https://cdn.jsdelivr.net/npm/animejs@4` |
| Heavy timelines | `gsap@3` | `https://cdn.jsdelivr.net/npm/gsap@3` |
| Custom interactive viz | `d3@7` | `https://cdn.jsdelivr.net/npm/d3@7` |
| Math (KaTeX JS + CSS + fonts) | `katex@0.17` | `https://cdn.jsdelivr.net/npm/katex@0.17` |
| Interactive mind maps | `markmap-lib@0` | `https://cdn.jsdelivr.net/npm/markmap-lib@0` |
| | `markmap-view@0` | `https://cdn.jsdelivr.net/npm/markmap-view@0` |
| | `markmap-toolbar@0` | `https://cdn.jsdelivr.net/npm/markmap-toolbar@0` |
| Code-highlight fallback (only when Shiki pre-rendering is unavailable) | `@highlightjs/cdn-assets@11` | `https://cdn.jsdelivr.net/npm/@highlightjs/cdn-assets@11` |

**Match rule** (the validator implements exactly this): an external URL
is allowlisted iff, case-sensitively,

1. it starts with one of the URL prefixes above;
2. the character immediately after the prefix is `.` or `/` (so
   `mermaid@11`, `mermaid@11.15.0`, and `mermaid@11/dist/…` all match,
   while `mermaid@110` does not);
3. the remainder consists only of `[A-Za-z0-9@._/-]` and contains no `..`
   path segment — which structurally excludes query strings, fragments,
   and percent-escapes.

Pins are to a major line (`@11`) or, for 0.x packages, a `@0` line;
authors SHOULD pin the exact known-good version in URLs they emit (e.g.
`mermaid@11.15.0` — jsDelivr resolves bare `@11` to "latest 11.x" at
request time, so exact pins are more reproducible). Both forms match.

Anything not in this table — other packages, other CDNs, other hosts —
is a **hard validation failure** when referenced from a code/style
position (§8, `E-EXTERNAL-CODE`) and is blocked by the runtime CSP
regardless. Extending the allowlist means editing this table (which
implies updating the CSP and validator together).

### 7.2 Graceful offline degradation (required)

Every Tier-2 usage MUST degrade gracefully with no network (FR-3.1, N6):
the reader gets a styled, readable fallback, never a blank hole or a JS
error cascade. The standard pattern — keep the source visible as a
styled `<pre>`, replace it only on successful render:

```html
<figure data-cfy-id="fig-service-map">
  <pre class="cfy-fallback"><code>flowchart LR
  Client --> Gateway --> Service</code></pre>
  <div class="cfy-live" hidden></div>
  <script type="module">
    try {
      const { default: mermaid } = await import(
        "https://cdn.jsdelivr.net/npm/mermaid@11.15.0/dist/mermaid.esm.min.mjs"
      );
      // …render into .cfy-live, then unhide it and hide the <pre>…
    } catch { /* offline / CDN down: the <pre> stays visible */ }
  </script>
  <figcaption>Service map (interactive when online).</figcaption>
</figure>
```

The `try`/`catch` (or an equivalent `onerror` handler for classic
scripts) is mandatory: a failed CDN load MUST NOT throw uncaught or leave
`hidden` content as the only content.

## 8. Validation — the `save-artifact` rule set (FR-3.6, S4)

`save-artifact` runs these checks on the submitted file. There are two
severities and no others:

- **HARD FAILURES (`E-*`)**: the artifact is rejected — nothing is
  stored, no version is created, the endpoint returns a non-2xx with
  `{ "error": "<message>", "code": "E-…" }`, and the CLI exits non-zero.
- **WARNINGS (`W-*`)**: the artifact is accepted and stored; warnings are
  returned to the agent in the success JSON
  (`"warnings": [{ "code": "W-…", "message": "…" }, …]`) and printed by
  the CLI to **stderr** (one `warning: <code>: <message>` line each,
  exit code still 0). Warnings exist to steer the agent, not to block it
  (per FR-3.6, only allowlist violations and unusability hard-fail).

### 8.1 Hard failures

| ID | Condition |
|---|---|
| `E-UTF8` | File is not valid UTF-8. |
| `E-PARSE` | File is empty, or HTML parsing yields a document whose `<body>` contains no elements. (HTML5 parsers are error-recovering; "unparseable" in practice means "nothing survives parsing".) |
| `E-EXTERNAL-CODE` | Any code/style-loading reference whose URL fails the §7.1 match rule. Checked positions: `script[src]`; `link[href]` where `rel` contains `stylesheet`, `preload`, or `modulepreload`; and `@import` of an `http(s)` URL inside inline CSS. Relative and `file://` URLs in these positions fail by definition (they can't match the allowlist). |
| `E-SIZE-MAX` | File exceeds **52,428,800 bytes (50 MiB)**. (Not in the PRD's letter — added as a viewer-protection backstop; the PRD's "size sanity" intent is the 5 MiB warning below.) |

### 8.2 Warnings

| ID | Condition |
|---|---|
| `W-SIZE` | File exceeds **5,242,880 bytes (5 MiB)**. |
| `W-DOCTYPE` | File does not begin with `<!doctype html>` (case-insensitive; leading whitespace/BOM permitted) — quirks mode breaks the design system. |
| `W-CHARSET` | No `<meta charset="utf-8">` in `<head>`. |
| `W-TITLE` | Missing or empty `<title>`. |
| `W-META` | A required `cfy:*` meta (§6) is missing or has empty `content`. One warning per missing tag. |
| `W-VERSION-MISMATCH` | `cfy:version` present but ≠ the server-assigned version (server-side check at save time; file-only validators skip it). |
| `W-ANCHOR-HEADINGS` | An `h1`–`h4` element lacks `data-cfy-id`. One warning per heading, identifying it by text. |
| `W-ANCHOR-DIAGRAM` | "Thin diagram coverage": an inline `<svg>` containing **≥ 6 shape elements** (`path`, `rect`, `circle`, `ellipse`, `line`, `polyline`, `polygon` — counted across all descendants) has **< 3 elements** bearing `data-cfy-id` (the `<svg>` itself and all descendants count). Rationale: ≤ 5 shapes is a decorative accent; anything bigger is a diagram with at least a few commentable concepts. |
| `W-ANCHOR-NONE` | Zero `data-cfy-id` attributes in the entire document (comments would be text-quote-only). |
| `W-ID-FORMAT` | A `data-cfy-id` value violates the §4.2 grammar or exceeds 64 chars. |
| `W-ID-DUP` | The same `data-cfy-id` value appears on more than one element. |
| `W-SRC-MALFORMED` | A `<!--cfy:src` comment lacks a `lang` or `for` attribute. |
| `W-SRC-ORPHAN` | A `<!--cfy:src` comment's `for` value doesn't match the `data-cfy-id` of its next non-whitespace sibling element (missing, mismatched, or non-adjacent). |
| `W-EXTERNAL-REF` | An `http(s)` URL in a non-code resource position — `img[src]`, `srcset`, `video`/`audio`/`source`/`track` `src`, `iframe[src]`, `object`/`embed`, or CSS `url(...)` — regardless of host. These are blocked by the runtime CSP and break offline rendering (N6). |
| `W-LOCAL-REF` | A relative or `file://` URL in any resource position not already covered by `E-EXTERNAL-CODE`, including relative `<a href>`. These are broken by definition (§1). |

## 9. Minimal example artifact

```html
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>How the auth middleware works</title>
<meta name="cfy:question" content="Explain how the auth middleware works in this codebase.">
<meta name="cfy:version" content="1">
<meta name="cfy:generated-by" content="claude-code/claude-sonnet-5">
<style>
  /* 1. design-system scaffold, embedded verbatim from the skill (D1–D6) */
  /* 2. artifact-specific styles */
</style>
</head>
<body>
<article>
  <h1 data-cfy-id="sec-title">How the auth middleware works</h1>

  <h2 data-cfy-id="sec-mental-model">The mental model</h2>
  <p>Every request passes through one gate…</p>

  <!--cfy:src lang="d2" for="fig-request-flow" renderer="d2 v0.7 --layout=elk"
direction: right
client -> middleware: attach session
middleware -> handler: authorized request
  -->
  <figure data-cfy-id="fig-request-flow">
    <svg viewBox="0 0 640 200" role="img" aria-label="Request flow">
      <g data-cfy-id="fig-request-flow.client"><!-- shape + label --></g>
      <g data-cfy-id="fig-request-flow.middleware"><!-- … --></g>
      <g data-cfy-id="fig-request-flow.handler"><!-- … --></g>
      <g data-cfy-id="fig-request-flow.client-to-middleware"><!-- edge --></g>
      <g data-cfy-id="fig-request-flow.middleware-to-handler"><!-- edge --></g>
    </svg>
    <figcaption>The request path through the middleware.</figcaption>
  </figure>

  <h2 data-cfy-id="sec-walkthrough">Walkthrough</h2>
  <p>…</p>
</article>
</body>
</html>
```

Fully self-contained, zero network, dual-renders in WKWebView and any
browser, every commentable concept anchored, and the diagram
regenerable from its embedded D2 source.
