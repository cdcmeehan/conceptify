# Rendering visuals and code — tiered strategy + verified commands

How to produce every visual in a Conceptify artifact. Principle (PRD §8):
**render at generation time and inline the result; escalate to runtime
libraries only when genuine interactivity demands it.** You have a full
toolchain; the reader's browser gets finished, self-contained output.

Contents: [Tiers](#the-tiers) · [Tools](#tool-availability) ·
[D2](#d2--structured-diagrams-preferred) · [Graphviz](#graphviz-dot--pure-graphs) ·
[Post-processing script](#post-processing-generated-svg-postprocess-svgmjs) ·
[Embedding sources](#embedding-the-dsl-source-cfysrc) ·
[Hand-authored SVG](#hand-authored-svg--small-bespoke-visuals) ·
[Code / Shiki](#code-blocks--shiki-v4) ·
[Mermaid decision](#mermaid-pre-rendering--not-in-the-toolchain-oq5) ·
[Tier 2](#tier-2--runtime-libraries-opt-in-escalation)

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
  `<figure class="cfy-diagram cfy-hand">`. **When the active cfy theme is
  `sketchbook`** (SKILL.md step 1, `conceptify status` → `artifactTheme`),
  `--sketch` + `cfy-hand` is the **default** — a preference, not a
  requirement: override it for dense or precision-critical diagrams where
  roughened strokes hurt legibility (89k.1 decision). The hand feel is
  guaranteed at the theme level regardless (hand-face labels + 2px frames
  come from the theme CSS). After a `--sketch` render, re-check the SVG
  for extra `fill-*`/`stroke-*` classes the adapter below doesn't cover.
- Keep shape keys short, lowercase, and meaningful (`client`,
  `token-svc`) — `data-cfy-id`s are derived from them and must stay
  stable across regenerations (artifact-spec §4.3).
- Mind the aspect ratio: the scaffold scales diagrams to the text column,
  so a long `direction: right` chain (6+ nodes) renders small. Prefer
  `direction: down` for long chains, or break flows into ranks with
  containers, so the diagram stays tall enough to read.
- Cycle edges that re-enter a container (`walker.ignore -> core.haystack`
  when `core` already feeds `walker`) get routed across the container's
  centered title text. Point such back-edges at the **container itself**
  (`walker.ignore -> core: DirEntry`) — the arrow stops at the border and
  the title stays clear. Verified: `label.near: top-left` does *not*
  reliably dodge the edge.
- Sequence diagrams: repeated messages between the same actor pair derive
  `-2`, `-3`, … ids in message order (`searcher-to-matcher`,
  `searcher-to-matcher-2`). Those suffixes re-bind if you later insert an
  earlier duplicate — when a specific repeated message is likely to be
  commented on, prefer wording the flow so each pair's messages stay
  unique, or accept that follow-up edits must not reorder them.
- The scaffold scales every diagram to the full text column
  (`.cfy-diagram svg { width: 100%; height: auto }`), which *magnifies*
  tall, narrow output — a vertical elk layout is often only 300–500
  units wide, so at a 900px column it blows up ~2× and eats a full
  screen of scroll. Cap such figures at natural size in the artifact's
  second `<style>` block:
  `figure[data-cfy-id="fig-x"] svg { max-width: <viewBox-width>px;
  margin-inline: auto; }` (read the width off the SVG's `viewBox`).
- A labeled fan-out to one rank (`api -> db: a` plus `api -> files: b`
  from the same node) can place both edge labels at the same midpoint,
  overlapping them into garbage. Label at most one edge of such a
  fan-out and let the caption or prose carry the rest.
- Long *edge* labels can lose their outermost characters: d2 computes
  canvas bounds with its embedded font, the post-processed SVG renders
  in the (wider) system font, and glyphs overhanging d2's bounds are
  clipped by the inner `viewBox`. Keep edge labels short or split them
  onto two lines with `\n`; node labels are safe (their boxes grow with
  the text).

**Post-processing (required)** — d2's raw SVG embeds base64 fonts and a
baked light-only palette; run the bundled script before inlining
([details + manual fallback](#post-processing-generated-svg-postprocess-svgmjs)):

```bash
node <skill-dir>/scripts/postprocess-svg.mjs --fig fig-request-flow \
  --input diagram.svg --write
```

It strips the prolog, deletes both `<style>` blocks (embedded
`@font-face` data + baked theme colors — the adapter CSS below replaces
them, and it cuts ~15 KB per diagram), stamps `data-cfy-id` on every
shape/container/connection group, and leaves the nested double-`<svg>`
structure and its `viewBox` as-is (the scaffold's
`.cfy-diagram svg { width:100%; height:auto }` handles scaling). Then
wrap the output in `<figure class="cfy-diagram" data-cfy-id="fig-…">` +
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

**Post-processing (required)** — same script
([details + manual fallback](#post-processing-generated-svg-postprocess-svgmjs)):

```bash
node <skill-dir>/scripts/postprocess-svg.mjs --fig fig-deps \
  --input graph.svg --write
```

It strips everything before `<svg` (prolog, DOCTYPE, generator
comments), removes the generated positional `id="…"` attributes
(`graph0`, `node1`, `edge1`, … — they collide when the artifact contains
several DOT diagrams), and stamps `data-cfy-id` on each
`<g class="node|edge|cluster">`. The inner `<title>` elements are kept —
they double as hover tooltips. Then wrap in
`<figure class="cfy-diagram" data-cfy-id="fig-…">` + `<figcaption>`,
preceded by the `cfy:src` comment.

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

## Post-processing generated SVG (`postprocess-svg.mjs`)

`scripts/postprocess-svg.mjs` is the **primary path** for both tools —
zero dependencies (node built-ins only), deterministic, and idempotent
(re-running on already-processed SVG is a no-op; already-stamped groups
are left untouched, which also preserves ids across regenerations):

```bash
# in place:
node <skill-dir>/scripts/postprocess-svg.mjs --fig fig-request-flow --input diagram.svg --write
# or streaming (tool autodetected from the SVG; --tool d2|dot to force):
d2 --layout=elk --pad 0 diagram.d2 - | node <skill-dir>/scripts/postprocess-svg.mjs --fig fig-request-flow > diagram.svg
```

`--fig` is the figure's `data-cfy-id`; every stamped id is namespaced
under it (`fig-request-flow.client`,
`fig-request-flow.client-to-middleware`). The script prints the stamped
ids on stderr — use that list when referencing diagram elements in prose
and to sanity-check coverage. It validates ids against the spec grammar
and warns past 64 chars (shorten the DSL key, don't truncate by hand).
It does **not** wrap the figure or write the `cfy:src` comment — those
stay yours. Mermaid SVG is rejected by design
([below](#mermaid-pre-rendering--not-in-the-toolchain-oq5)).

**Manual fallback / reference** — what the script implements, if you
must hand-edit or debug. The derivation algorithm (lowercase → `->`
becomes `-to-` → non-alphanumeric runs collapse to `-` → trim; collision
appends `-2`, `-3`, … in document order) and the grammar are normative
in artifact-spec §4.2/§4.4.

Where each tool exposes the DSL name (both verified):

- **d2**: every shape, container, and connection is a top-level
  `<g class="<base64>">` whose class is the **base64-encoded,
  HTML-escaped object key**: `Y2xpZW50` → `client`,
  `YXBpLnJvdXRlcg==` → `api.router`,
  `KGNsaWVudCAtJmd0OyBtaWRkbGV3YXJlKVswXQ==` →
  `(client -&gt; middleware)[0]`. Decode (`base64 -d`), unescape
  `-&gt;` to `->`, drop a `[0]` index (for parallel edges `[n]`, n ≥ 1,
  append `-{n+1}` instead — edges inside a container carry the container
  prefix outside the parens, `walk.(ignore -> globset)[0]`, treated the
  same way), then derive. Stamp the same `<g>`; ignore the inner
  `<g class="shape">`. In **sequence diagrams**, the actor box carries the
  plain actor key (stamp it); the actor's *lifeline* is an edge with an
  empty destination, `(walker -- )[0]`. Lifelines are not commentable
  concepts: leave them unstamped (the script skips them).
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
node <skill-dir>/scripts/highlight.mjs --lang rust --input excerpt.rs --highlight 2,7-9 --theme "$THEME"
# or from stdin:
sed -n '120,140p' src/server.rs | node <skill-dir>/scripts/highlight.mjs --lang rust --theme "$THEME"
```

`--theme` is the active cfy theme (SKILL.md step 1; default `manuscript`).
It selects the Shiki pair per the 89k.1 decision: manuscript & sketchbook
render `vitesse-light`/`vitesse-dark`; **blueprint** renders
`github-light`/`nord` (Vitesse's warm sand tokens clash on blueprint's ice
paper and navy code wash). Output is a dual-theme
`<pre class="shiki shiki-themes <light> <dark>">` block (`--shiki-dark`
variable prefix — exactly what the scaffold's dark-mode rules expect, D4).
Wrap it:

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
`.highlighted-word` are styled by the scaffold. When inserting
`cfy-code-mark` markers into the emitted HTML, string-replace on a single
distinctive identifier token and append the mark after its closing
`</span>` — Shiki splits tokens unpredictably (`async` and ` fn` are
separate spans), so multi-word match strings silently miss. If node is genuinely
unavailable, fall back to Tier-2 highlight.js
(`@highlightjs/cdn-assets@11` on the allowlist) with the mandatory
offline fallback pattern below.

## Mermaid pre-rendering — NOT in the toolchain (OQ5)

Decided empirically 2026-07 (conceptify-57l.5, PRD §12 OQ5): Mermaid
pre-rendering via `mermaid-cli`/Puppeteer does **not** earn a place at
generation time. Do not `npx @mermaid-js/mermaid-cli`. Measured on this
machine (mmdc 11.16.0, `look: neo`/`handDrawn`, ELK, themeVariables):

- **Weight/flakiness**: first `npx` run took ~70 s and downloaded full
  Chrome *plus* chrome-headless-shell (~550 MB) into
  `~/.cache/puppeteer`; every render boots headless Chrome (~3.5 s per
  diagram vs. ~0.3 s for d2 — and d2/dot need no network, ever).
- **Theming fragility**: `themeVariables` are *silently ignored* unless
  `theme: "base"`; even then, neo/handDrawn sequence actors bake literal
  `#eaeaea`/`#666` from variables outside the D5 token table
  (`actorBkg`, …) — staying on-system needs per-diagram-type variable
  maps.
- **Anchorability (the killer)**: state-diagram edges emit only
  positional ids (`edge0`, `edge1`, …) — exactly what artifact-spec §4.2
  forbids; sequence messages have no bounding `<g>` grouping line +
  label. Every mmdc SVG is `id="my-svg"` with an `#my-svg`-scoped
  `<style>` and `url(#my-svg-*)` marker refs, so two Mermaid diagrams in
  one artifact collide. Flowchart/state labels are HTML-in-
  `foreignObject`, which artifact CSS can bleed into.
- **What it would win**: denser sequence diagrams (notes, activations)
  and Mermaid-only types (gantt, ER, gitgraph, journey).

Verdict: d2 covers flow/architecture/sequence/state well — its sequence
labels are masked, not overlapped (the knockout is an SVG `mask`; some
thumbnailers drop it, browsers don't). For Mermaid-only types, first ask
whether a `cfy-table`/`cfy-steps` re-expression is clearer (usually yes
for gantt/ER in an explanation); if the reader genuinely needs the
diagram interactively, use Tier-2 runtime Mermaid below.
`postprocess-svg.mjs` rejects Mermaid SVG accordingly.

Revisit if: quality iteration shows d2 sequence output failing on real
content; artifacts repeatedly need gantt/ER at generation time; or a
browser-free Mermaid renderer ships.

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
