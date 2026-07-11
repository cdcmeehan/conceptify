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

## Regenerating the demo

`skill/examples/demo-artifact.html` contains the scaffold spliced verbatim.
After editing `design-system.css`, re-splice by replacing the first
`<style>` block's contents with the CSS file (the M3 skill will do this
programmatically for real artifacts).
