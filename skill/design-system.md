# Conceptify design system (D1–D6)

The reading-experience layer for artifacts (PRD §6, goal G2). Ships as
`skill/design-system.css`, which the Claude Code skill embeds **verbatim** as
the first `<style>` block of every artifact (artifact-spec §1). Artifact-
specific styles always go in a second `<style>` block after it.

The reference rendering is `skill/examples/demo-artifact.html` — a fully
valid artifact exercising every component below; it is also the M2
checkpoint fixture.

## OQ1 — final typography decision (D1)

Constraint that shaped everything: artifacts must render offline forever
(N6) with **no font downloads** — the artifact spec does not allowlist web
fonts. So the "font picks" are system-stack picks, macOS-first, with the
PRD's named candidates (Newsreader/Fraunces/Source Serif 4, Inter,
JetBrains Mono) ruled out as primary faces because none is preinstalled.

| Role | Stack (token) | Primary face on macOS |
|---|---|---|
| Headings | `--cfy-font-serif`: `ui-serif, "New York", "Iowan Old Style", "Palatino Nova", Palatino, Georgia, "Times New Roman", serif` | **New York** (Apple's optically-sized serif) |
| Body | `--cfy-font-sans`: `-apple-system, BlinkMacSystemFont, system-ui, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif` | **SF Pro** |
| Code | `--cfy-font-mono`: `ui-monospace, "SF Mono", SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace` | **SF Mono** |
| Hand-drawn flavor | `--cfy-font-hand`: `"Bradley Hand", "Chalkboard SE", "Comic Sans MS", "Segoe Print", cursive` | Bradley Hand |

Rationale:

- **Headings — New York via `ui-serif`.** A true optical-size family (Small
  → Extra Large cuts chosen automatically), designed to pair with SF, and
  present on every macOS render target through WKWebView/Safari. Chromium
  doesn't support `ui-serif`, so **Iowan Old Style** (bundled with macOS
  since Mavericks; same warm, book-like Transitional character) is the
  deterministic named fallback; Georgia covers non-Apple platforms.
- **Body — the system sans, not Inter.** Inter can't ship (not
  preinstalled, no network fonts allowed) and SF Pro is at least its equal
  for UI-adjacent long-form reading: optical sizing, tabular figures,
  excellent hinting on Apple displays.
- **Code — SF Mono via `ui-monospace`, not JetBrains Mono.** JetBrains Mono
  is deliberately *not* listed: it is only present if the reader happens to
  have installed it, which would make the agent's self-review screenshots
  (FR-6.3) not match what the reader sees. Determinism wins; Menlo is the
  named fallback everywhere `ui-monospace` isn't.
- **Visual language:** "quiet print editorial" — warm paper white
  (`#fbf9f4`) / warm charcoal (`#17140f`), warm near-black ink, hairline
  rules (h2 top rules, figure caption rules, table rows), one terracotta
  accent (`#a34d24` light / `#e09863` dark), tracked small-caps labels for
  kickers/h4/captions/table headers, serif display headings with negative
  tracking at display sizes. Restraint over decoration; the accent appears
  in links, step numerals, code marks, and the highlighted-line bar — never
  as large fills.

## Tokens (D2)

Everything between `/* @cfy:tokens:start */` and `/* @cfy:tokens:end */` in
`design-system.css` is the shareable token block — the app shell imports or
extracts exactly that section for visual continuity (out of scope here; the
sentinels exist so the shell bead can consume it without taking the
artifact-only rules).

- Type scale: `--cfy-text-sm/-body/-lede/-h4/-h3/-h2/-h1` (17px body,
  clamp()-fluid h1/h2), `--cfy-leading` (1.65).
- Rhythm: `--cfy-space-1…8` (0.25 → 4.5 rem), `--cfy-measure` (68ch),
  `--cfy-radius`, `--cfy-radius-sm`.
- Palette: `--cfy-paper/-surface/-ink/-muted/-line/-accent/-mark`;
  callouts `--cfy-{insight,warning,definition}` + `-bg`; code
  `--cfy-code-bg/-hl/-hl-bar`; diagrams `--cfy-diagram-node/-node-stroke/
  -edge/-label/-accent/-accent-bg`.
- Dark mode is **media-query driven** (`prefers-color-scheme`), never
  class-driven — artifacts must adapt standalone in any browser.

## Component vocabulary (D3) — what the skill teaches agents

| Class / element | Usage |
|---|---|
| `<p class="cfy-kicker">` | Small-caps eyebrow line above the `<h1>`. |
| `<p class="cfy-lede">` | Opening hook paragraph, set larger. |
| `<aside class="cfy-callout cfy-insight\|cfy-warning\|cfy-definition">` | Callout with edge bar + wash; optional first child `<p class="cfy-callout-title">`. |
| `<ol class="cfy-steps">` | Step sequence: serif numeral badges on a connecting rail; start each `<li>` with `<strong>Name.</strong>`. |
| `<dfn class="cfy-term">` (or `span`) | Key-term highlighter swipe on first use. |
| `<details class="cfy-details">` | Collapsible deep-dive; `<summary>` first. Works with JS disabled. |
| `<nav class="cfy-outline" aria-label="On this page">` | Sticky semantic-section outline for STANDARD/DEEP artifacts; use a `<ul>` of native hash links. The bridge marks the current location and restores disclosures on history navigation. |
| `<details class="cfy-details cfy-deep-dive" id="…">` | Optional generated depth. Keep orientation/core outside; print exposes the body even when closed. |
| `<ul class="cfy-next-questions">` | Two to four semantic next-question branches. Each `<li>` carries the `data-cfy-next-question` contract; show a specific question plus why it follows. |
| `<figure data-cfy-id="fig-…">` + `<figcaption>` | Figure + caption (caption gets a hairline rule; bold lead-in via `<strong>`). |
| `<figure class="cfy-diagram">` | Adds the tinted diagram frame; put inline SVG inside. Add `cfy-hand` for the hand-drawn flavor. |
| `<div class="cfy-table-wrap">` + `<table class="cfy-table">` | Print-style table (horizontal hairlines only); `<caption>` renders below. Add `cfy-compare` when the first column is `<th scope="row">` row headers. |
| `<figure class="cfy-listing">` | Code listing wrapper: `<figcaption class="cfy-code-title">` filename bar + the Shiki `<pre>` + optional `<ol class="cfy-code-notes">`. |
| `<span class="cfy-code-mark">1</span>` | Circled annotation marker inside a code line, explained by the matching `cfy-code-notes` item. |
| `pre.cfy-fallback` | Tier-2 offline fallback source (artifact-spec §7.2) — dashed frame, reads as content, not error. |
| `.cfy-reveal` (+ `--cfy-reveal-n: 0,1,2…`) | Staged rise-into-place on load (D6); intro elements only, ≤ ~6 items. |
| `.cfy-flow` (SVG) | Marching-dash edge flow; dashed stroke at rest. Gated on `prefers-reduced-motion`. |

Table gotcha: `cfy-compare` row headers are `white-space: nowrap`, so a
long unbreakable row header (e.g. a filesystem path in `<code>`) sets a
huge first-column width and pushes the other columns out of view even at
comfortable reading width. For path-keyed tables, override in the
artifact's second style block (`table-layout: fixed` on the table plus
`tbody th { white-space: normal; overflow-wrap: anywhere }`), or keep
row headers short.

## Code blocks (D4)

Pre-render with **Shiki v4 dual themes**: `themes: { light: "vitesse-light",
dark: "vitesse-dark" }` (default `--shiki-dark` variable prefix). The
scaffold flips spans under `prefers-color-scheme: dark` and overrides the
`.shiki` container background onto the paper palette. Styled hooks:
`.line.highlighted` (wash + accent edge bar), `.highlighted-word`,
`.line.diff.add` / `.line.diff.remove`. Lines are `inline-block; width:100%`
with the horizontal padding on the line, so highlights span the full
scroll width.

## Diagram styling (D5)

- **Hand-authored SVG**: use the scaffold's SVG classes — `cfy-node`,
  `cfy-node-accent`, `cfy-edge`, `cfy-arrow` (marker heads), `cfy-label`,
  `cfy-label-sub`, `cfy-edge-label`. `var()` does **not** work in SVG
  presentation attributes; classes (or `style=""`) do.
- **Generated SVG (d2 / dot / Mermaid)**: renderers bake literal hex, which
  would break dark mode. Render with a *sentinel palette*, then post-process
  the inlined SVG replacing each sentinel hex with a `var()`-based `style`
  (same pass that stamps `data-cfy-id`s, §4.4 of the artifact spec).
  Sentinel → token mapping:

  | Token | Mermaid themeVariables | d2 theme override |
  |---|---|---|
  | `--cfy-diagram-node` | `primaryColor` | shape fill (`B1`) |
  | `--cfy-diagram-node-stroke` | `primaryBorderColor` | shape stroke (`B2`) |
  | `--cfy-diagram-label` | `primaryTextColor`, `textColor` | label color (`N1`) |
  | `--cfy-diagram-edge` | `lineColor` | connection stroke (`N2`) |
  | `--cfy-diagram-accent` / `-accent-bg` | `secondaryBorderColor` / `secondaryColor` | accent shapes (`AA2`/`AB4`) |

- Hand-drawn flavor: `class="cfy-diagram cfy-hand"` (labels take
  `--cfy-font-hand`) + `d2 --sketch` / Mermaid `look: handDrawn`.

## Motion (D6)

Compositor-friendly only — `transform`/`opacity`, plus light SVG
`stroke-dash*` (WKWebView is 60 fps-capped pre-macOS-26). Every animation is
inside `@media (prefers-reduced-motion: no-preference)`, so the resting
document is complete and readable with JS disabled, reduced motion on, or
CSS animation unsupported. No scroll-driven animation (`animation-timeline`
is not Safari-safe).

**Hard rule (verified empirically during this design pass):** occluded or
off-screen WKWebViews *suspend* CSS animations indefinitely, so an
animation's from-state can become the permanent rendered state — this is
exactly the situation of FR-6.3 headless snapshot review. Therefore no
animation may hide content in its from-state: no opacity-0 fade-ins, no
stroke draw-ins from an invisible stroke. `.cfy-reveal` is transform-only
for this reason, and there is deliberately no `.cfy-draw` class. Artifact-
specific styles must follow the same rule.

## Themes (89k.1 design record)

Three explanation themes, selected per artifact (or per app setting —
89k.2). Each is a **complete override of the `@cfy:tokens` palette block**
plus a small, named set of component-level rules; type scale, spacing, and
rhythm are shared. `manuscript` is the default and is byte-identical to the
current scaffold values — zero visual change for existing artifacts.

Every text/background pairing below was contrast-checked against WCAG 2.1
AA with a Node implementation of the relative-luminance + contrast-ratio
formula (21 pairings × 6 variants = 126 checks, all ≥ 4.5:1 for text and
≥ 3:1 for UI strokes/indicators). Checked pairings per variant: ink/paper,
muted/paper, ink/surface, muted/surface, accent/paper, ink/mark, each
callout color vs its own bg, ink vs each callout bg, paper/accent
(code-mark badge), diagram-label/node, muted/node, ink/code-hl, plus 3:1
UI checks (node-stroke/node, edge/surface, diagram-accent/accent-bg,
code-hl-bar/code-hl, accent/surface). Worst cases: Sketchbook-light
muted/surface 4.68:1; Manuscript-dark node-stroke/node 4.11:1 (needs 3:1).

### `manuscript` — default (frozen baseline)

Quiet print editorial: warm paper, warm near-black ink, one terracotta
accent, New York serif display. These are the **current scaffold values,
frozen exactly** — the theme merely names them.

| Token | Light | Dark |
|---|---|---|
| `--cfy-paper` | `#fbf9f4` | `#17140f` |
| `--cfy-surface` | `#f3f0e8` | `#221e17` |
| `--cfy-ink` | `#211d16` | `#eae4d8` |
| `--cfy-muted` | `#6d6759` | `#a89f8d` |
| `--cfy-line` | `#e4dfd2` | `#383226` |
| `--cfy-accent` | `#a34d24` | `#e09863` |
| `--cfy-mark` | `#f3e5c3` | `#4a3a1c` |
| `--cfy-insight` / `-bg` | `#7a5a12` / `#f5edd8` | `#e3c377` / `#322a15` |
| `--cfy-warning` / `-bg` | `#9b3a2a` / `#f7e8e0` | `#e89b85` / `#3a211a` |
| `--cfy-definition` / `-bg` | `#33586b` / `#e8eef1` | `#9dc2d6` / `#1d2b33` |
| `--cfy-code-bg` / `-hl` / `-hl-bar` | `#f4f1e9` / `#eae4d2` / `#a34d24` | `#201c15` / `#332c1e` / `#e09863` |
| `--cfy-diagram-node` / `-node-stroke` | `#f3f0e8` / `#58513f` | `#2a251c` / `#8d8471` |
| `--cfy-diagram-edge` / `-label` | `#6d6759` / `#211d16` | `#a89f8d` / `#eae4d8` |
| `--cfy-diagram-accent` / `-accent-bg` | `#a34d24` / `#f0ddcf` | `#e09863` / `#46301f` |

Fonts: unchanged (`--cfy-font-serif` headings). Shiki pair:
`vitesse-light` / `vitesse-dark` (unchanged).

### `blueprint` — cool, precise, drafted-on-vellum

For systems/infra topics. Ice-blue near-white paper, graphite blue-black
ink, prussian/steel-blue accent, pale ice-blue highlighter, cooler code
wash. Dark variant sits on deep navy, not neutral black. Headings switch
to the sans stack; H4 tracked labels and code-listing title bars go mono
for a drafting-table register.

| Token | Light | Dark |
|---|---|---|
| `--cfy-paper` | `#f7f9fb` | `#101923` |
| `--cfy-surface` | `#eaeff4` | `#1a2634` |
| `--cfy-ink` | `#1c2430` | `#dce6ef` |
| `--cfy-muted` | `#55606e` | `#94a5b6` |
| `--cfy-line` | `#d3dce4` | `#2b3a4a` |
| `--cfy-accent` | `#2b5f8a` | `#6aa5d8` |
| `--cfy-mark` | `#d8e8f5` | `#1f3a55` |
| `--cfy-insight` / `-bg` | `#74591a` / `#f0eddc` | `#d9c069` / `#2b2a17` |
| `--cfy-warning` / `-bg` | `#a13434` / `#f8e8e6` | `#e89a8f` / `#382223` |
| `--cfy-definition` / `-bg` | `#1e6a80` / `#e0eef3` | `#85c7dd` / `#16303c` |
| `--cfy-code-bg` / `-hl` / `-hl-bar` | `#eef2f6` / `#dde7f0` / `#2b5f8a` | `#0d151e` / `#1c2f42` / `#6aa5d8` |
| `--cfy-diagram-node` / `-node-stroke` | `#eaeff4` / `#46566a` | `#1a2634` / `#7e93a8` |
| `--cfy-diagram-edge` / `-label` | `#55606e` / `#1c2430` | `#94a5b6` / `#dce6ef` |
| `--cfy-diagram-accent` / `-accent-bg` | `#2b5f8a` / `#d9e5f0` | `#6aa5d8` / `#21405c` |

Non-token rules (part of the theme block, 89k.3):
`--cfy-font-serif` reassigned to the sans stack (headings, blockquote,
step numerals, compare row headers all follow); `h4` and `.cfy-code-title`
take `var(--cfy-font-mono)`. Shiki pair: **`github-light` / `nord`**
(see decision below).

### `sketchbook` — warm, hand-drawn

For conceptual/teaching explanations. Creamier paper, deep-ochre accent
(darkened until it passes 4.5:1 as link text — raw ochre does not),
ink-blue reserved for the definition callout, chunky saturated-yellow
highlighter. Headings take the existing `--cfy-font-hand` stack (Bradley
Hand / Chalkboard SE — installed macOS faces, no network). Dark variant is
chalkboard green-black with chalk-ochre accent.

| Token | Light | Dark |
|---|---|---|
| `--cfy-paper` | `#faf6ec` | `#141a15` |
| `--cfy-surface` | `#f1ead9` | `#1e2620` |
| `--cfy-ink` | `#292118` | `#e6e3d5` |
| `--cfy-muted` | `#6f6754` | `#a3a892` |
| `--cfy-line` | `#ddd3ba` | `#333e30` |
| `--cfy-accent` | `#8a5c12` | `#d9b25f` |
| `--cfy-mark` | `#f2da8f` | `#4c451d` |
| `--cfy-insight` / `-bg` | `#75570f` / `#f4e9c8` | `#dcc274` / `#2e2f18` |
| `--cfy-warning` / `-bg` | `#9c4423` / `#f6e4d8` | `#e5a08a` / `#3a2620` |
| `--cfy-definition` / `-bg` | `#41597d` / `#e8ecf2` | `#a7c6e0` / `#202e3a` |
| `--cfy-code-bg` / `-hl` / `-hl-bar` | `#f2ecda` / `#e8dfc2` / `#8a5c12` | `#101711` / `#253023` / `#d9b25f` |
| `--cfy-diagram-node` / `-node-stroke` | `#f1ead9` / `#5c5138` | `#1e2620` / `#8b937c` |
| `--cfy-diagram-edge` / `-label` | `#6f6754` / `#292118` | `#a3a892` / `#e6e3d5` |
| `--cfy-diagram-accent` / `-accent-bg` | `#8a5c12` / `#efe0b8` | `#d9b25f` / `#3d3a20` |

Non-token rules (part of the theme block, 89k.3):
`--cfy-font-serif` reassigned to the hand stack; `--cfy-radius: 14px`,
`--cfy-radius-sm: 8px`; headings drop the negative tracking
(`letter-spacing: 0` on h1/h2 — hand faces are not optically sized);
`.cfy-term` gradient stop moves 58% → 45% (chunkier swipe); callout edge
3px → 4px; `.cfy-diagram`, `.cfy-details`, `.cfy-next-questions > li`
borders 1px → 2px; `.cfy-diagram svg` labels take `var(--cfy-font-hand)`
(every diagram gets the `cfy-hand` treatment without needing the class).
No fake roughen filters on text. Shiki pair: `vitesse-light` /
`vitesse-dark` (Vitesse's warm earth tokens already suit cream and
chalkboard).

### Theme decisions

- **Shiki pairing is per-theme, chosen at generation time** — but only
  Blueprint actually diverges: Manuscript and Sketchbook use
  `vitesse-light`/`vitesse-dark`; Blueprint uses `github-light`/`nord`.
  Rationale: Vitesse's warm sand-and-umber token palette visibly clashes
  on Blueprint's ice paper and navy code-bg, and no single pair can be
  warm enough for Manuscript/Sketchbook and cool enough for Blueprint
  dark. Nord's frost-blue tokens were designed for a navy ground.
  Live-retheme caveat: Shiki colors are baked per-span, so retheming an
  existing artifact keeps its original code colors. This stays safe
  because all three themes' `--cfy-code-bg` values sit in tight luminance
  bands (light ≈ Y 0.83–0.89, dark ≈ Y 0.006–0.013), so any pair remains
  AA-legible on any theme's code wash — the mismatch is aesthetic only
  and self-heals on regeneration.
- **Sketchbook *prefers* d2 sketch mode; it does not force it.** The skill
  defaults to `d2 --sketch` (+ Mermaid `look: handDrawn` where the
  adapter supports theming) when the theme is `sketchbook`, but may
  override for dense or precision-critical diagrams where roughened
  strokes hurt legibility. Forcing is also unenforceable: hand-authored
  SVG has no sketch switch, and some Mermaid handDrawn looks bake literal
  hex (see rendering.md). The hand *feel* is guaranteed at the theme
  level regardless — hand-face diagram labels and 2px frames come from
  the theme block, not from the renderer.
- **Callout semantics keep their hue family in every theme** (insight =
  gold/olive, warning = red, definition = blue) so callout recognition
  transfers across themes; each theme re-tunes temperature/saturation to
  sit on its ground. In Blueprint, definition shifts to cyan-teal
  (`#1e6a80`/`#85c7dd`) so it stays distinct from the steel-blue accent.

## Regenerating the demo

`skill/examples/demo-artifact.html` contains the scaffold spliced verbatim.
After editing `design-system.css`, re-splice by replacing the first
`<style>` block's contents with the CSS file (the M3 skill will do this
programmatically for real artifacts).
