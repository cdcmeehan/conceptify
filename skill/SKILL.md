---
name: conceptify
description: Produce a beautiful, self-contained HTML explanation artifact and publish it into the local Conceptify macOS app. Use when the user says "use conceptify to explain X", "explain X in conceptify", "conceptify this", "create a conceptify artifact", or otherwise asks for a rich, visual, publishable explanation of a codebase, subsystem, or technical topic.
---

# Conceptify — author and publish explanation artifacts

Conceptify is a local macOS app that stores and renders **explanation
artifacts**: single self-contained HTML files that explain a codebase or
concept with editorial typography, inline SVG diagrams, and pre-rendered
code walkthroughs. The reader can anchor comments to any heading or diagram
element, so every artifact must follow the anchoring contract below.

Publishing happens through the `conceptify` CLI. The app launches
automatically (the CLI probes health and runs `open -a Conceptify` if
needed) — never ask the user to start the app.

## Files in this skill

Read these before authoring — they are the contract, not background:

- **`artifact-spec.md`** — the full artifact specification (MUST/MUST NOT
  rules, `data-cfy-id` grammar, embedded diagram sources, validation rule
  set). **Read it in full before writing any HTML.** (Snapshotted at
  install time from `docs/artifact-spec.md` in the conceptify repo, which
  is canonical.)
- **`design-system.css`** — the CSS scaffold. Embed its contents
  **verbatim and unmodified** as the *first* `<style>` block of every
  artifact. Artifact-specific CSS goes in a *second* `<style>` block.
- **`design-system.md`** — the component vocabulary (callouts, steps,
  figures, listings, tables, motion rules) and diagram theming tokens.
  Read it in full; use its classes instead of inventing new components.
- **`references/rendering.md`** — the tiered visual strategy with exact,
  verified render commands (d2, Graphviz, Shiki) and the SVG
  post-processing recipes. Read it before producing any diagram or code
  block.
- **`references/self-review.md`** — the pre-save visual self-review loop
  (headless render + screenshot inspection recipe + review checklist).
  Read it before step 5; it is the FR-6.3 gate on every artifact.
- **`scripts/highlight.mjs`** — Shiki v4 dual-theme code highlighting
  helper (run it; no need to read it).
- **`examples/demo-artifact.html`** — a complete valid artifact exercising
  every component. Skim it as a reference rendering when unsure how
  components compose.

## The flow

### 1. Check the CLI

```bash
command -v conceptify && conceptify status
```

`status` prints `{"service":"conceptify","status":"ok",...}` and launches
the app if it isn't running (allow ~10s). If the binary is missing, stop
and tell the user to run `just install-cli` in the conceptify repo, then
resume. If `status` exits non-zero after a launch attempt, surface its
stderr to the user and stop. For a fuller diagnostic (app bundle, CLI,
d2, graphviz, node, agent binary — with install hints), run
`conceptify doctor`; it is the first debugging step when anything in
this flow misbehaves.

### 2. Ensure the project

Run from the repo being explained, with its root directory:

```bash
conceptify ensure-project --dir "$(git rev-parse --show-toplevel)"
```

(For a non-repo directory, pass that directory instead.) Output:
`{"projectId":"<uuid>","created":true|false}`. Idempotent — safe to re-run.

### 3. Create the thread (before authoring)

```bash
conceptify create-thread --project <projectId> \
  --title "<short human title>" \
  --question "<the user's question, verbatim>"
```

Output: `{"threadId":"<uuid>","slug":"<thread-slug>"}`. Create the thread
*before* authoring — the app shows it as `generating`, which is the
intended UX. Keep the `--question` string: it must reappear verbatim in
the artifact's `<meta name="cfy:question">`.

### 4. Author the artifact

The bulk of the work. Write the file to a temp path (e.g. under
`$TMPDIR`), **never into the target repo** — the app copies it into its
own central storage on save.

**Research first.** Read the actual code before writing a word. The
artifact must be true of *this* codebase: real file paths, real type and
function names, real control flow. Never explain from generic knowledge
of how such systems usually work.

**Structure — hook → mental model → visuals → walkthrough → summary:**

1. **Hook**: `cfy-kicker` + `<h1>` + one `cfy-lede` paragraph that states
   why the question matters and previews the answer in one or two
   sentences.
2. **Mental model**: the single organizing idea the reader should hold,
   usually paired with the artifact's primary diagram. If the reader
   remembers one section, it's this one.
3. **Visuals**: a diagram wherever structure beats prose — flows,
   architectures, state machines, lifecycles, sequences. Comparisons go
   in `cfy-table` tables, processes in `cfy-steps`.
4. **Walkthrough**: the real code, as trimmed excerpts (see below), in
   the order a request/value/event actually travels.
5. **Summary**: a short "what to remember" close — the mental model
   restated plus the two or three load-bearing facts.

**Quality dos and don'ts:**

- Aim for genuine understanding, not a README: typically 1,000–2,500
  words of prose plus 2–5 visuals for a single question. Depth over
  breadth; cut anything that doesn't serve the question. Never pad.
- Every figure gets a `<figcaption>` that *interprets* ("Note the token
  never crosses this boundary"), never restates the title.
- Code excerpts: pick the load-bearing 5–30 lines, trim aggressively
  (`// …` for elisions), name the source file in the
  `cfy-code-title` bar, and use highlighted lines + `cfy-code-mark`
  annotation markers explained in `cfy-code-notes`. Never dump whole
  files. Render via `scripts/highlight.mjs` (see rendering.md).
- Use `cfy-callout` for the genuine asides (insight / warning /
  definition), `cfy-details` for optional deep-dives; define key terms
  once with `cfy-term`.
- Don't: walls of bullets where prose should carry an argument; filler
  ("In this document we will…"); decorative diagrams that encode no
  structure; invented APIs or paths.

**Assembly checklist** (details in `artifact-spec.md` — this is a
reminder, not a substitute):

- [ ] `<!doctype html>`, `<meta charset="utf-8">` first in `<head>`,
      viewport meta, non-empty `<title>`.
- [ ] `cfy:question` (verbatim from step 3), `cfy:version` (`1` for a new
      thread), `cfy:generated-by` (`claude-code/<model>`) metas.
- [ ] First `<style>` = `design-system.css` contents verbatim; second
      `<style>` = adapter CSS from rendering.md + artifact-specific rules.
- [ ] `data-cfy-id` on every `h1`–`h4`, every figure, and every
      meaningful diagram element — semantic kebab-case ids
      (`sec-mental-model`, `fig-auth-flow.token-service`), never
      positional. This is the comment-anchoring API; thin coverage
      triggers validator warnings.
- [ ] Every generated diagram has its DSL source in an adjacent
      `<!--cfy:src …-->` comment with the escaping rules from spec §5.
- [ ] Fully self-contained: no relative/`file://` refs, no network except
      the Tier-2 allowlist (rendering.md), readable with JS disabled.

**WKWebView constraints** (the in-app viewer is macOS WebKit):

- Safari-compatible CSS/JS only — if caniuse shows red/partial for
  current Safari, don't use it.
- Artifact JS runs in an opaque-origin sandbox with `connect-src 'none'`:
  no fetch, no storage, no `window.parent`, no `alert`. Embed all data.
- Animate `transform`/`opacity` only, inside
  `@media (prefers-reduced-motion: no-preference)`; design for 60fps.
- **Animation-suspension trap**: occluded WKWebViews suspend CSS
  animations indefinitely, so an animation's from-state can become the
  permanent rendered state. **No animation may hide content in its
  from-state** — no opacity-0 fade-ins, no draw-ins from invisible
  strokes. Transform-only reveals (like the scaffold's `.cfy-reveal`).

### 5. Pre-save review

Two passes, both required. Do not save until both are clean.

**Source review.** Re-read the finished HTML against the assembly
checklist above and `artifact-spec.md` §8's warning list (heading ids,
metas, diagram anchor coverage, orphaned `cfy:src` comments) so v1 saves
clean. For hand-authored SVG specifically, sanity-check text lengths
against shape widths and the `viewBox` against actual content extents.

**Visual review (`references/self-review.md`).** Source review cannot see
overlapping labels, clipped text, contrast, or narrow-pane overflow —
only a render can. Render the finished HTML headlessly, screenshot it at
**two widths (~460px and ~900px) in both light and dark**, **Read the
PNGs**, and judge them against the visual checklist. Fix, re-render, and
re-read until every frame is clean. The exact copy-pasteable
`agent-browser` recipe (with the mechanical horizontal-overflow check) and
the full checklist live in **`references/self-review.md`** — follow it.
This is the FR-6.3 safety net for hand-authored SVG, the highest-variance
tier; never skip it.

### 6. Save and verify

```bash
conceptify save-artifact --thread <threadId> --file <path>.html
```

Success prints `{"version":1,"warningsCount":N}` and the app focuses the
thread with the artifact on screen — done, zero manual steps. Warnings
appear on stderr as `warning: <CODE>: <message>`:

- Fix substantive warnings (`W-ANCHOR-*`, `W-META`, `W-SRC-*`,
  `W-EXTERNAL-REF`, `W-LOCAL-REF`) and save again — the re-save just
  becomes the next version; that's fine.
- A hard failure (`E-…`, exit 1) means nothing was stored: fix the
  reported rule and re-save.

Finally, tell the user the artifact is live in Conceptify (name the
thread title), and mention they can comment on any heading or diagram
element in the app.

## Scope notes

- This skill covers **initial artifact creation**. Guidance for follow-up
  runs (answering reader comments via `get-context`/`resolve-comment`,
  producing new artifact versions in apply mode) ships as
  `references/follow-ups.md` in a future revision — when updating an
  existing artifact meanwhile, obey artifact-spec.md §4.3: never rename
  existing `data-cfy-id`s, regenerate diagrams from their embedded
  sources, and bump `cfy:version`.
- Artifacts are stored centrally by the app
  (`~/Documents/conceptify/artifacts/…`) — they never touch the target
  repo, so there is nothing to gitignore.
