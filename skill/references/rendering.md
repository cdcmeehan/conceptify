# Rendering visuals and code — tiered strategy + verified commands

How to produce every visual in a Conceptify artifact. Principle (PRD §8):
**render at generation time and inline the result; escalate to runtime
libraries only when genuine interactivity demands it.** You have a full
toolchain; the reader's browser gets finished, self-contained output.

Contents: [Tiers](#the-tiers) · [Tools](#tool-availability) ·
[D2](#d2--structured-diagrams-preferred) · [Graphviz](#graphviz-dot--pure-graphs) ·
[Stamping ids](#stamping-data-cfy-id-on-generated-svg) ·
[Embedding sources](#embedding-the-dsl-source-cfysrc) ·
[Hand-authored SVG](#hand-authored-svg--small-bespoke-visuals) ·
[Code / Shiki](#code-blocks--shiki-v4) · [Tier 2](#tier-2--runtime-libraries-opt-in-escalation)

## The tiers

| Tier | What | When |
|---|---|---|
| 0 | Semantic HTML/CSS + design-system components, CSS-only motion | Always — the base layer; readable with JS off |
| 1 | **Inline SVG, rendered at generation time** (d2 / DOT) or hand-authored | **Default for every diagram** |
| 2 | Runtime libraries from the pinned CDN allowlist (artifact-spec §7) | Only when the reader must *interact* (zoom, scrub, toggle) |

Choosing within Tier 1:

- **d2** (preferred): flow, architecture, sequence, state — anything with
  boxes, containers, and labeled connections. Best-looking output.
- **Graphviz DOT**: pure graphs (dependency graphs, trees, dense
  digraphs) where automatic graph layout is the point.
- **Hand-authored SVG**: small bespoke visuals, ≤ ~10 elements — concept
  sketches, annotated pipelines, timelines with irregular structure.
- Comparison data is a `cfy-table`, not a diagram; a linear process is
  often better as `cfy-steps` than as a flowchart.

## Tool availability

```bash
command -v d2 dot node
```

All three are prerequisites (`brew install d2 graphviz`, node via any
installer); a `conceptify doctor` command that checks them is planned. If
a diagram tool is missing mid-task, fall back to hand-authored SVG (small
visuals) or a Tier-2 Mermaid block (complex ones) rather than shipping an
unrendered code block. Verified versions for the commands below: d2
0.7.1, Graphviz 15.1.0, node 22 / Shiki v4.

## D2 — structured diagrams (preferred)

Write the `.d2` source, then:

```bash
d2 --layout=elk --pad 0 diagram.d2 diagram.svg
```

- `--layout=elk` — always; markedly better layered layouts.
- `--pad 0` — the `cfy-diagram` frame already provides padding.
- `--sketch` — optional hand-drawn flavor; pair with
  `<figure class="cfy-diagram cfy-hand">`.
- Keep shape keys short, lowercase, and meaningful (`client`,
  `token-svc`) — `data-cfy-id`s are derived from them and must stay
  stable across regenerations (artifact-spec §4.3).
- Mind the aspect ratio: the scaffold scales diagrams to the text column,
  so a long `direction: right` chain (6+ nodes) renders small. Prefer
  `direction: down` for long chains, or break flows into ranks with
  containers, so the diagram stays tall enough to read.

**Post-processing (required)** — d2's raw SVG embeds base64 fonts and a
baked light-only palette; transform it before inlining:

1. Drop the `<?xml …?>` prolog.
2. Delete both `<style>…</style>` blocks inside the SVG (embedded
   `@font-face` data + baked theme colors). The adapter CSS below
   replaces them; this also cuts ~15 KB per diagram.
3. Stamp `data-cfy-id` on the shape/connection groups (next section).
4. Keep the nested double-`<svg>` structure and its `viewBox` as-is; the
   scaffold's `.cfy-diagram svg { width:100%; height:auto }` handles
   scaling.
5. Wrap in `<figure class="cfy-diagram" data-cfy-id="fig-…">` +
   `<figcaption>`, preceded by the `cfy:src` comment.

**d2 adapter CSS** — paste once into the artifact's second `<style>`
block whenever the artifact contains d2 output (mapping per
design-system.md D5; verified against d2 0.7.1 default theme 0):

```css
/* --- d2 SVG adapter --------------------------------------------------- */
.cfy-diagram .d2-svg { font-family: var(--cfy-font-sans); }
.cfy-diagram .d2-svg .shape { shape-rendering: geometricPrecision; stroke-linejoin: round; }
.cfy-diagram .d2-svg .connection { stroke-linecap: round; stroke-linejoin: round; }
.cfy-diagram .d2-svg .blend { mix-blend-mode: multiply; opacity: 0.5; }
.cfy-diagram .d2-svg .text-bold { font-weight: 600; }
.cfy-diagram .d2-svg .text-italic { font-style: italic; }
.cfy-diagram .d2-svg .text-mono { font-family: var(--cfy-font-mono); }
/* theme classes -> design tokens */
.cfy-diagram .d2-svg .fill-N1 { fill: var(--cfy-diagram-label); }
.cfy-diagram .d2-svg .fill-N2 { fill: var(--cfy-diagram-edge); }
.cfy-diagram .d2-svg .fill-N3 { fill: var(--cfy-muted); }
.cfy-diagram .d2-svg .fill-N4,
.cfy-diagram .d2-svg .fill-N5,
.cfy-diagram .d2-svg .fill-N6 { fill: var(--cfy-line); }
.cfy-diagram .d2-svg .fill-N7 { fill: transparent; } /* canvas background */
.cfy-diagram .d2-svg .fill-B1,
.cfy-diagram .d2-svg .fill-B2 { fill: var(--cfy-diagram-node-stroke); }
.cfy-diagram .d2-svg .fill-B3,
.cfy-diagram .d2-svg .fill-B4,
.cfy-diagram .d2-svg .fill-B5,
.cfy-diagram .d2-svg .fill-B6 { fill: var(--cfy-diagram-node); }
.cfy-diagram .d2-svg .fill-AA2 { fill: var(--cfy-diagram-accent); }
.cfy-diagram .d2-svg .fill-AA4,
.cfy-diagram .d2-svg .fill-AA5,
.cfy-diagram .d2-svg .fill-AB4,
.cfy-diagram .d2-svg .fill-AB5 { fill: var(--cfy-diagram-accent-bg); }
.cfy-diagram .d2-svg [class*="stroke-N"] { stroke: var(--cfy-diagram-edge); }
.cfy-diagram .d2-svg .stroke-B1,
.cfy-diagram .d2-svg .stroke-B2 { stroke: var(--cfy-diagram-node-stroke); }
.cfy-diagram .d2-svg .stroke-AA2 { stroke: var(--cfy-diagram-accent); }
.cfy-diagram .d2-svg .connection.stroke-B1,
.cfy-diagram .d2-svg .connection.stroke-B2 { stroke: var(--cfy-diagram-edge); }
.cfy-diagram .d2-svg .connection.fill-B1,
.cfy-diagram .d2-svg .connection.fill-B2 { fill: var(--cfy-diagram-edge); } /* arrowheads */
```

After rendering, grep the SVG for any `fill-*` / `stroke-*` class the
adapter doesn't cover (styled shapes, `--sketch` variants) and extend the
mapping using the D5 token table in design-system.md.

## Graphviz DOT — pure graphs

```bash
dot -Tsvg graph.dot -o graph.svg
```

Prefer `node [shape=box, style=rounded]` and `rankdir=LR` in the source —
default ellipses waste horizontal space. Keep node names lowercase and
meaningful (they become `data-cfy-id`s).

**Post-processing (required):**

1. Strip everything before `<svg` (XML prolog, DOCTYPE, generator
   comments).
2. Remove the generated `id="…"` attributes (`graph0`, `node1`,
   `edge1`, …) — they are positional and collide when the artifact
   contains several DOT diagrams.
3. Stamp `data-cfy-id` on each `<g class="node|edge|cluster">` (next
   section). Keep the inner `<title>` elements — they double as hover
   tooltips.
4. Wrap in `<figure class="cfy-diagram" data-cfy-id="fig-…">` +
   `<figcaption>`, preceded by the `cfy:src` comment.

**Graphviz adapter CSS** — paste once when the artifact contains DOT
output (verified against Graphviz 15.1.0 defaults):

```css
/* --- Graphviz SVG adapter --------------------------------------------- */
.cfy-diagram .graph > polygon { fill: transparent; stroke: none; } /* canvas */
.cfy-diagram .graph text { font-family: var(--cfy-font-sans); font-size: 13px; fill: var(--cfy-diagram-label); }
.cfy-diagram .node ellipse, .cfy-diagram .node polygon,
.cfy-diagram .node rect, .cfy-diagram .node path {
  fill: var(--cfy-diagram-node); stroke: var(--cfy-diagram-node-stroke); stroke-width: 1.25;
}
.cfy-diagram .edge > path { fill: none; stroke: var(--cfy-diagram-edge); stroke-width: 1.25; }
.cfy-diagram .edge polygon { fill: var(--cfy-diagram-edge); stroke: var(--cfy-diagram-edge); }
.cfy-diagram .edge text { fill: var(--cfy-diagram-edge); font-size: 11px; }
.cfy-diagram .cluster polygon, .cfy-diagram .cluster rect { fill: var(--cfy-surface); stroke: var(--cfy-line); }
```

## Stamping `data-cfy-id` on generated SVG

Renderers don't emit `data-cfy-id`; add it by editing the SVG text. The
derivation algorithm (lowercase → `->` becomes `-to-` → non-alphanumeric
runs collapse to `-` → trim) and the grammar are normative in
artifact-spec §4.2/§4.4; ids are namespaced under the figure:
`fig-request-flow.client`, `fig-request-flow.client-to-middleware`.

Where each tool exposes the DSL name (both verified):

- **d2**: every shape, container, and connection is a top-level
  `<g class="<base64>">` whose class is the **base64-encoded,
  HTML-escaped object key**: `Y2xpZW50` → `client`,
  `YXBpLnJvdXRlcg==` → `api.router`,
  `KGNsaWVudCAtJmd0OyBtaWRkbGV3YXJlKVswXQ==` →
  `(client -&gt; middleware)[0]`. Decode (`base64 -d`), unescape
  `-&gt;` to `->`, drop a `[0]` index (for parallel edges `[n]`, n ≥ 1,
  append `-{n+1}` instead), then derive. Stamp the same `<g>`; ignore the
  inner `<g class="shape">`.
- **Graphviz**: `<g class="node">` / `<g class="edge">` /
  `<g class="cluster">`, each with a child `<title>` holding the DOT name
  (`client`, `client&#45;&gt;middleware`, `cluster_x`). Entity-decode,
  then derive, and stamp the outer `<g>`.

Sanity check when done: every node and labeled edge in the figure carries
an id (the validator warns below 3 bearers on any SVG with ≥ 6 shapes),
and no `pointer-events: none` on stamped elements.

## Embedding the DSL source (`cfy:src`)

Every generated diagram MUST carry its source in an adjacent HTML comment
so follow-up agents regenerate instead of hand-editing SVG — exact format
and placement rules in artifact-spec §5:

```html
<!--cfy:src lang="d2" for="fig-request-flow" renderer="d2 v0.7.1 --layout=elk --pad 0"
direction: right
client -> middleware: attach session
-->
```

The comment must immediately precede the element carrying the matching
`data-cfy-id`. Escaping (spec §5.2): d2 and DOT sources normally need no
changes; Mermaid's `-->` arrows must be written `--\>` (and literal `\`
as `\\`).

## Hand-authored SVG — small bespoke visuals

For ≤ ~10 elements where no DSL fits. Rules:

- Use the scaffold's classes — `cfy-node`, `cfy-node-accent`, `cfy-edge`,
  `cfy-arrow`, `cfy-label`, `cfy-label-sub`, `cfy-edge-label` — never raw
  hex. `var()` does **not** work in SVG presentation attributes; classes
  or `style=""` do.
- Group each shape *with its label* in a `<g data-cfy-id="fig-x.concept">`
  from the start.
- Set a correct `viewBox` and `role="img"` + `aria-label`. No fixed
  `width`/`height` needed — the scaffold scales it.
- This is the highest-variance tier: before saving, re-check every text
  against its container width (~7.5px per character at 13px font-size),
  verify nothing crosses the `viewBox` edges, and keep generous gaps
  between elements. When a visual grows past ~10 elements, switch to d2.
- Light SMIL or CSS `stroke-dash*` accents are fine (`.cfy-flow`), but
  remember the suspension trap: nothing may be hidden in its from-state.

## Code blocks — Shiki v4

Always pre-render; never ship runtime highlighters when node is
available. Use the bundled helper (first run bootstraps `shiki@^4` into
`~/.cache/conceptify/shiki-env` via npm — needs network once):

```bash
# from a file (excerpt it first — never a whole file), highlighting lines 2 and 7-9:
node <skill-dir>/scripts/highlight.mjs --lang rust --input excerpt.rs --highlight 2,7-9
# or from stdin:
sed -n '120,140p' src/server.rs | node <skill-dir>/scripts/highlight.mjs --lang rust
```

Output is a dual-theme `<pre class="shiki shiki-themes vitesse-light
vitesse-dark">` block (`--shiki-dark` variable prefix — exactly what the
scaffold's dark-mode rules expect, D4). Wrap it:

```html
<figure class="cfy-listing" data-cfy-id="fig-launch-wait">
  <figcaption class="cfy-code-title">crates/conceptify-cli/src/main.rs</figcaption>
  <!-- highlight.mjs output goes here -->
  <ol class="cfy-code-notes">
    <li>Explains marker ① — insert <span class="cfy-code-mark">1</span> into the relevant line above.</li>
  </ol>
</figure>
```

`.line.highlighted` (via `--highlight`), `.line.diff.add` /
`.line.diff.remove` (add the classes by hand where useful), and
`.highlighted-word` are styled by the scaffold. If node is genuinely
unavailable, fall back to Tier-2 highlight.js
(`@highlightjs/cdn-assets@11` on the allowlist) with the mandatory
offline fallback pattern below.

## Tier 2 — runtime libraries (opt-in escalation)

Only when the reader must interact. Everything must come from the pinned
jsDelivr allowlist in artifact-spec §7.1 (mermaid@11 + ELK, motion@12,
animejs@4, gsap@3, d3@7, katex@0.17, markmap, highlight.js) — anything
else hard-fails validation and is blocked by the runtime CSP anyway.

Non-negotiables (spec §7.2):

- **Graceful offline degradation**: keep the source visible as a styled
  `<pre class="cfy-fallback">`, swap it only on successful render, and
  wrap the import in `try`/`catch`. A dead CDN must never leave a blank
  hole — the spec's §7.2 snippet is the template.
- Mermaid, when used, is always `look: handDrawn` or neo with
  `themeVariables` mapped to the design tokens (table in design-system.md
  D5) — never default styling. Note: runtime Mermaid renders nothing
  offline (the `<pre>` fallback shows instead), which is why d2/DOT at
  generation time is the default. Pin exact versions in URLs
  (`mermaid@11.15.0`, not `mermaid@11`) for reproducibility.
- Remember the runtime sandbox (spec §3): no fetch/storage/parent access;
  all data embedded in the file.
