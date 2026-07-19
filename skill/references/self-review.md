# Visual self-review — render, screenshot, inspect (FR-6.3)

The last gate before `save-artifact`. Source review catches structural
problems; **only a render catches visual ones** — overlapping SVG labels,
clipped text, low contrast, dark-mode breakage, and narrow-pane overflow.

## How much to run — proportional to the artifact

Match the pass to what the artifact actually contains:

- **Hand-authored SVG or generated diagrams present → the full four-frame
  loop is mandatory.** Two widths (~460px and ~900px) × two schemes (light
  and dark), all four PNGs read and judged, looping until clean. Diagrams
  are the highest-variance tier — this is their FR-6.3 safety net and you
  **must not skip or shortcut it.** The recipe and checklist below are
  written for this case.
- **Text-and-Shiki-only artifact (no bespoke SVG, no rendered diagrams) →
  a single narrow-width dark render is enough.** Render once at **460px in
  dark mode** and run the mechanical `pixelWidth` overflow check. Those two
  cover the only bug classes a diagram-free artifact can have: **dark**
  surfaces any hardcoded color that breaks on the charcoal ground, and
  **narrow** surfaces horizontal overflow. Read that one PNG against the
  text-relevant checklist items (contrast, overflow, unstyled elements,
  animation-hidden content). If the render tooling is genuinely
  unavailable, a careful **source-only** review against the checklist is
  an acceptable substitute — for text-only artifacts, never for
  diagram-bearing ones.

When unsure which case you are in, run the full loop — it is never wrong,
only slower.

Do not skip the applicable pass. Loop it until every frame is clean.

## The mechanism: headless Chromium via `agent-browser`

Render the finished HTML file headlessly and screenshot it at two widths
in both color schemes, then **read the PNGs with the Read tool** and judge
them against the checklist below.

Use `agent-browser` (bundled headless Chromium, CDP-driven). It is the
proven path — it renders the exact same self-contained bytes the reader
sees, and it emulates `prefers-color-scheme` and viewport width reliably.
Do **not** try to screenshot the running app window: macOS blocks
screen-recording of app windows without a TCC grant. Do **not** depend on
Playwright-via-`npx` — it is frequently uninstalled. (Fallback at the
bottom if `agent-browser` is genuinely missing.)

Why both widths and both schemes:

- **Narrow (~460px)** — the app's thread pane can be dragged this narrow.
  Any element that sets a page-width floor (a `white-space:nowrap` run, a
  fixed-width table, an un-wrapped wide token) overflows *here* and looks
  fine at comfortable width. This is a real bug class.
- **Comfortable (~900px)** — the normal reading width; check overall
  composition, diagram sizing, and rhythm.
- **Light and dark** — the scaffold ships both. Hand-authored SVG is the
  usual dark-mode casualty: `var()` does not work in SVG presentation
  attributes, so a hardcoded hex fill that reads fine on paper can vanish
  on the dark charcoal ground.
- **In the artifact's ACTIVE theme** — screenshots must be taken from the
  finished file with its `data-cfy-theme` stamp in place (SKILL.md step 5),
  so the render shows the theme the reader will actually get — blueprint's
  navy dark ground and sketchbook's chalkboard are different contrast
  environments than manuscript's charcoal, in light AND dark. Rendering
  `file://` on the stamped file does this automatically; never strip the
  attribute "to simplify review", and judge contrast/legibility against
  the rendered theme, not the manuscript palette you may expect.

## The recipe

Render your finished artifact (the temp HTML file you are about to save).
Copy-paste, replacing `ART` with your file path:

```bash
ART="/absolute/path/to/your/artifact.html"   # the temp file, pre-save
OUT="${TMPDIR:-/tmp}/cfy-review"; mkdir -p "$OUT"
S="cfy-review"                                 # named session, kept off your other browser work

agent-browser --session "$S" open "file://$ART"

# narrow width (~460px) — the overflow-risk case; scale 2 = crisp retina text
agent-browser --session "$S" set viewport 460 900 2
agent-browser --session "$S" set media light
agent-browser --session "$S" screenshot --full "$OUT/w460-light.png"
agent-browser --session "$S" set media dark
agent-browser --session "$S" screenshot --full "$OUT/w460-dark.png"

# comfortable width (~900px)
agent-browser --session "$S" set viewport 900 900 2
agent-browser --session "$S" set media light
agent-browser --session "$S" screenshot --full "$OUT/w900-light.png"
agent-browser --session "$S" set media dark
agent-browser --session "$S" screenshot --full "$OUT/w900-dark.png"

# mechanical overflow check: a full-page PNG is exactly viewport-width × scale
# wide UNLESS content overflows horizontally. At scale 2 that's 920 (narrow)
# and 1800 (comfortable). Anything wider = horizontal overflow at that width.
for f in "$OUT"/w*.png; do printf '%s  ' "$(basename "$f")"; \
  sips -g pixelWidth "$f" 2>/dev/null | awk '/pixelWidth/{print $2}'; done

agent-browser --session "$S" close
```

Notes:

- `set viewport`/`set media` apply live to the next `screenshot` — no
  reload needed (verified). `--full` captures the entire scroll height,
  so the `900` height above is irrelevant; only width matters.
- `scale 2` gives retina-sharp text for contrast/overlap judgement. If a
  full-page PNG comes back too tall to read clearly (very long artifact),
  drop to scale `1`, or crop the region you need from the full-page PNG:
  `cp full.png crop.png && sips --cropOffset <y> 0 --cropToHeightWidth
  <h> <w> crop.png` (offsets in device pixels = CSS px × scale). Judging
  a diagram's labels needs this close-up pass — thumbnails hide overlap
  and contrast problems. Don't use `screenshot <selector>` element
  captures for this: they intermittently return blank frames on `file://`
  pages (observed with agent-browser 0.27).
- Optional determinism: append `reduced-motion`
  (`set media dark reduced-motion`) to force settled, animation-free
  frames.

Then **Read all four PNGs** and work the checklist. The `pixelWidth` line
is your overflow tripwire: read the narrow frames especially closely if it
prints anything above `920`.

## The review checklist

Judge every frame. A defect in one frame fails the whole review — fix and
re-render.

- [ ] **Overlapping / clipped SVG labels.** In every hand-authored and
      generated diagram, is each label fully inside its shape, and clear of
      neighboring labels and edges? Watch collisions between adjacent node
      labels and text running past the `viewBox` edge (clipped). This is
      the #1 hand-SVG failure.
- [ ] **Text contrast — both schemes.** Every run of text comfortably
      readable in light *and* dark, including **diagram text sitting on a
      filled shape** (label on an accent-filled node) and edge labels over
      lines. If a label is faint or invisible in one scheme only, suspect a
      hardcoded color (next item).
- [ ] **Dark-mode-specific breakage (hardcoded colors).** A label/shape
      that is fine in light but wrong (invisible, jarring, wrong ground) in
      dark almost always means a raw hex in an SVG attribute or inline
      `style`. Fix by using the scaffold's SVG classes (`cfy-label`,
      `cfy-node`, `cfy-edge`, …) or design tokens — never a literal hex.
- [ ] **Horizontal overflow at narrow width.** The `pixelWidth` check
      prints > `920` for a narrow frame, or the read image shows the body
      column squeezed into the left with an empty gutter / a horizontal
      scroll. Hunt the widest thing: a `white-space:nowrap` run, a long
      unbreakable token outside inline `<code>`, an un-wrapped wide
      `<table>` (wrap it in `.cfy-table-wrap`), or an SVG with a fixed
      pixel `width`.
- [ ] **Broken / missing figures.** Every `<figure>` actually renders —
      no blank frames, no raw SVG markup shown as text, no empty diagram
      box, no missing code block. A dead Tier-2 CDN must show its styled
      `<pre>` fallback, not a hole.
- [ ] **Oversized / undersized diagrams.** No diagram dominates the page
      or renders too small to read. A long `direction: right` d2 chain
      shrinks at column width — prefer `direction: down` (see
      rendering.md). Bespoke SVG should fill, not overspill, the measure.
- [ ] **Unstyled elements (missed component classes).** Anything that
      looks like raw browser default — an unstyled bullet list, a bare
      table, plain block-quote, default-blue link — means a component class
      was missed. Apply the `cfy-*` class from design-system.md.
- [ ] **Animation-hidden content (source cross-check).** Headless
      Chromium runs animations to completion, so it *cannot* reproduce the
      WKWebView occlusion-suspension trap where an animation freezes in its
      from-state. Verify this at the **source**: no artifact animation may
      hide content in its from-state (no `opacity:0` fade-ins, no
      stroke-draw-in from invisible). Transform-only reveals only (the
      scaffold's `.cfy-reveal`). See SKILL.md "Animation-suspension trap".

## The loop

1. Render (recipe above) → Read the four PNGs.
2. Work the checklist. If anything fails, fix the HTML/SVG at the source.
3. **Re-render and re-read** — never assume a fix worked without seeing it.
4. Repeat until all four frames pass every item. Only then `save-artifact`.

## Fallback (no `agent-browser`)

`agent-browser` is the supported tool — install it if missing:
`npm i -g agent-browser && agent-browser install`. If its daemon wedges
(every `screenshot` hangs or errors "Resource temporarily unavailable"
even on a trivial page), `pkill -f agent-browser` and retry once; if it
stays wedged, fall back rather than fight it. A reliable last resort on
macOS is a ~60-line Swift harness around `WKWebView.takeSnapshot`
(offscreen borderless window, `NSAppearance` `.darkAqua`/`.aqua` for the
color scheme, height from `document.documentElement.scrollHeight`,
compiled with `swiftc`) — it renders the *real* WKWebView target, and
evaluating `document.documentElement.scrollWidth` replaces the
pixelWidth overflow tripwire. If you truly cannot,
any headless Chromium with `prefers-color-scheme` + viewport emulation
works; e.g. best-effort Playwright CLI (unreliable — browsers may be
uninstalled):

```bash
npx playwright screenshot --full-page \
  --viewport-size=460,900 --color-scheme=dark \
  "file://$ART" w460-dark.png
```

The essentials are the same regardless of tool: **full-page PNGs at ~460
and ~900 CSS px, in light and dark, read and judged against the checklist,
looping until clean.**
