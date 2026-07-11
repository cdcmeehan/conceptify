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
  Read it before step 6; it is the FR-6.3 gate on every artifact.
- **`references/follow-ups.md`** — the house rules for headless follow-up
  runs (answering reader comments via `resolve-comment`, and apply-mode
  updates that publish a new artifact version). Read it when a run's prompt
  points you here; not needed for initial authoring.
- **`scripts/highlight.mjs`** — Shiki v4 dual-theme code highlighting
  helper (run it; no need to read it).
- **`scripts/postprocess-svg.mjs`** — post-processes d2/dot SVG for
  inlining (prolog/style stripping + `data-cfy-id` stamping; see
  rendering.md — run it, no need to read it).
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

### 4. Choose the detail level

**First, size the question.** Before anything else, classify it — this is
the difference between a two-minute answer and an eight-minute diagram tour
for a one-line question. This classification does double duty: it is the
artifact's target depth *and* the recommendation you show the user. Three
tiers:

- **COMPACT** — a single concept, a bit of syntax, a definition, a "what
  does X mean / do" question. Target **300–800 words**. Diagrams **only**
  if structure genuinely beats prose — usually **none**, occasionally one
  small bespoke SVG; Shiki-highlighted code blocks are fine and often
  carry the answer. No task-list ceremony, no multi-diagram tour, and a
  **lightweight** pre-save review (see step 6). A compact artifact should
  be authored, reviewed, and saved in a couple of minutes.
- **STANDARD** — a subsystem, a flow, a "how does X work". The default
  treatment (the structure and quality guidance in step 5): ~1,000–2,500
  words with 2–5 visuals.
- **DEEP** — an architecture tour or a multi-system walkthrough. The full
  treatment: more diagrams, longer, several code walkthroughs.

**Bias to COMPACT when in doubt** between compact and standard for a short
question. Under-building is cheap — the reader can comment to interrogate
any point deeper, which is the whole point of the product loop;
over-building a small question into a slow, diagram-heavy artifact is the
failure this step exists to prevent. This bias *is* the auto-classification;
it stands whenever the user doesn't override it below.

**Then decide who picks the depth — the user, or your auto-classification.**

**Interactive sessions ask once.** When a human is at the keyboard (a
normal Claude Code session where the user typed "explain X in conceptify")
**and** the invocation does not already state a depth preference, ask —
once, before authoring — using the **AskUserQuestion** tool. One question,
three options, Balanced recommended:

- Header: `Detail level`. Question text surfaces your auto-classification
  as a hint, e.g. *"How much detail? (For this one I'd suggest **Balanced**
  — it reads as a STANDARD question.)"* — substitute the tier you classified.
- Options **in this order** — AskUserQuestion has no "default" field, so you
  recommend by putting Balanced **first** and saying so in its description:
  1. **Balanced** — *"Recommended. My suggested depth
     (<COMPACT|STANDARD|DEEP> for this question) at balanced effort."*
  2. **Quick & simple** — *"A short, to-the-point artifact. Fast."*
  3. **Very detailed** — *"A thorough, diagram-rich deep dive. Slower."*

Map the answer to a **sizing tier** and an **authoring model**:

| Choice | Sizing | Authoring model |
|---|---|---|
| **Quick & simple** | **COMPACT** (overrides a higher auto-class) | fast / haiku-class |
| **Balanced** (default) | the **auto-classified** tier above (bias-to-COMPACT stands) | the session model |
| **Very detailed** | **DEEP** | top / opus- or fable-class |

If the user dismisses the question without choosing, treat it as **Balanced**.

**Explicit depth in the invocation → skip the question.** If the request
already states how much detail it wants, honor it and do **not** ask:
- "quick", "quick answer", "just briefly", "simple", "tl;dr", "short",
  "one-liner" → **Quick & simple**.
- "in depth", "in detail", "very detailed", "thorough", "deep dive",
  "comprehensive", "full walkthrough" → **Very detailed**.
- no depth signal, interactive session → ask (above).

**Headless / non-interactive runs never ask.** If no human is present to
answer — every path in `references/follow-ups.md` (answer mode, apply mode,
in-app asks) and any run launched by the app or another agent — **do not
call AskUserQuestion and do not delegate**: author at the **auto-classified**
tier on the **session model**, exactly as this flow did before this step
existed. The detail-level question is an interactive-only affordance; a
headless run must stay prompt-free end to end (it has no one to prompt). If
you are ever unsure whether a human is present, assume headless and skip.

**Influencing the model (Quick / Very detailed).** An interactive session
cannot switch its own model mid-run, so to honor the *fast-model* and
*top-model* halves of Quick and Very detailed, **delegate the authoring
step (step 5) to a subagent launched on the target model** — Claude Code's
Task/Agent tool takes a `model` override (`haiku`, `sonnet`, `opus`,
`fable`) plus `subagent_type: "general-purpose"` (full toolset). Delegate
**only when it actually changes the model**:

- **Quick & simple** → unless you are already a fast model, launch a
  `general-purpose` subagent with `model: "haiku"` to author. If you are
  already light, just author COMPACT yourself.
- **Balanced** → never delegate; author yourself on the session model.
- **Very detailed** → unless you are already top-tier, launch a
  `general-purpose` subagent with `model: "opus"` (or `"fable"`). If you
  are already top-tier, just author DEEP yourself.

Depth is applied in **every** case; delegation only changes *which model*
authors, so skipping it (because the session model already matches) still
produces the right-sized artifact.

The subagent starts fresh — its prompt must carry everything it needs:
- the **fixed sizing tier** (COMPACT or DEEP) and an explicit instruction to
  **author at that tier and NOT ask any detail-level question** (it is not
  interactive — this both keeps headless prompt-free and prevents recursion);
- that it should follow `~/.claude/skills/conceptify/SKILL.md` **from step 5
  (Author) through step 7 (Save)** — research, author, run the full step-6
  self-review, then `save-artifact` itself;
- the ids already created in steps 2–3: the **projectId**, the **threadId**
  (it saves version 1 into this *existing* thread — it must **not** create a
  new one), the **verbatim question** string (for `cfy:question`), and the
  **repo path** (its working directory and `ensure-project --dir`);
- the exact **`cfy:generated-by`** value to stamp — `claude-code/<model>`
  for the model you launched it on (e.g. `claude-code/haiku`). A subagent
  does not reliably know its own model, so you must tell it; this is what
  makes the meta reflect the *actual* authoring model.

The delegated artifact goes through the **same pipeline** — the step-6
self-review gate and `save-artifact` validation apply unchanged; delegation
weakens nothing. When the subagent returns, confirm it saved (the thread now
shows the artifact) before reporting to the user.

**If delegation is impractical or would degrade quality**, fall back to
**depth-only** influence: author the chosen tier yourself on the session
model, and stamp `cfy:generated-by` with your *actual* model — never claim a
model you did not run on. Say in your reply that you fell back and why.

### 5. Author the artifact

The bulk of the work. Write the file to a temp path (e.g. under
`$TMPDIR`), **never into the target repo** — the app copies it into its
own central storage on save. Author to the **sizing tier settled in step 4**
(COMPACT / STANDARD / DEEP) — its word/visual budget governs everything
below. (When step 4 delegated authoring to a subagent, that subagent runs
steps 5–7 on the chosen model, following exactly these instructions.)

**Research first.** Read the actual code before writing a word. The
artifact must be true of *this* codebase: real file paths, real type and
function names, real control flow. Never explain from generic knowledge
of how such systems usually work.

**Structure — orientation → core → optional depth:**

1. **Orientation**: `cfy-kicker` + `<h1>` + one `cfy-lede` paragraph that
   states why the question matters and previews the answer in one or two
   sentences. Add a short overview with the answer and key terms up front.
2. **Core explanation**: the single organizing mental model, primary visual,
   and load-bearing walkthrough. This always-visible layer must stand alone
   with the orientation; never make a reader open a disclosure to understand
   the answer.
3. **Optional depth**: implementation detail, edge cases, derivations, and
   reference material belong in specific native
   `<details class="cfy-details cfy-deep-dive">` disclosures. This material is
   already generated and needs no follow-up run; do not hide essential facts.
4. **Outline**: STANDARD and DEEP artifacts with multiple sections include a
   sticky `<nav class="cfy-outline" aria-label="On this page">` whose hash
   links point to stable semantic section ids. Put a native `id` matching
   `data-cfy-id` on every linked target so the outline works standalone.
   COMPACT artifacts may omit it.
5. **Visuals**: use a diagram wherever structure beats prose — flows,
   architectures, state machines, lifecycles, sequences. Comparisons go
   in `cfy-table` tables, processes in `cfy-steps`.
6. **Walkthrough**: present the real code, as trimmed excerpts (see below), in
   the order a request/value/event actually travels.
7. **Summary**: close with a short "what to remember" section — the mental model
   restated plus the two or three load-bearing facts.
8. **Useful next branches**: for STANDARD/DEEP artifacts, end with two to four
   specific editable paths such as example, counterexample, mechanism,
   trade-off, or prerequisite—not generic “more details.” Use
   `<li data-cfy-id="next-…" data-cfy-next-question="…"
   data-cfy-reason="…" data-cfy-branch="example|counterexample|mechanism|tradeoff|prerequisite">`
   inside `<ul class="cfy-next-questions">`. State why each follows. These are
   inert suggestions until the reader explicitly edits and launches one.
9. **Concept evidence**: add `data-cfy-concepts="Concept A|Concept B"` to
   meaningful headings, figures/diagram nodes, and next-question branches.
   Use one to five reader-recognizable concepts per element. Tag only explicit
   evidence; never spray extracted keywords or pretend similarity is a fact.

**Quality dos and don'ts:**

- Aim for genuine understanding, not a README. The word/visual budget
  follows the sizing tier from step 4: a STANDARD question runs ~1,000–2,500
  words plus 2–5 visuals; a COMPACT one runs 300–800 words with few or no
  diagrams. Depth over breadth; cut anything that doesn't serve the
  question. Never pad — and never inflate a compact answer to hit the
  STANDARD budget.
- Every figure gets a `<figcaption>` that *interprets* ("Note the token
  never crosses this boundary"), never restates the title.
- Choose visuals by explanatory purpose, using the smallest supported form:
  compare → table/small multiples; sequence → steps/flow/sequence; relationships
  → node-link/concept map; hierarchy → tree/nesting; values → accessible chart
  plus exact values; interactive model → minimal controls plus a complete static
  fallback. If the fitting form is unsupported, say so briefly and use the
  closest textual structure. Every SVG uses `role="img"` with a useful
  `aria-label`; every chart retains exact values in a table or description.
  Never add a diagram merely to decorate prose.
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
      thread), `cfy:generated-by` (`claude-code/<model>` — the model that
      *actually* authored this artifact; when step 4 delegated authoring,
      it is the delegate's model, not the session's) metas.
- [ ] First `<style>` = `design-system.css` contents verbatim; second
      `<style>` = adapter CSS from rendering.md + artifact-specific rules.
- [ ] STANDARD/DEEP multi-section artifacts have an always-visible overview +
      core, optional native `cfy-deep-dive` disclosures, and a `cfy-outline`
      linking stable semantic section ids. The answer works with all details
      closed and prints coherently with all details exposed.
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

### 6. Pre-save review

Two passes, both required. Do not save until both are clean.

**Source review.** Re-read the finished HTML against the assembly
checklist above and `artifact-spec.md` §8's warning list (heading ids,
metas, diagram anchor coverage, orphaned `cfy:src` comments) so v1 saves
clean. For hand-authored SVG specifically, sanity-check text lengths
against shape widths and the `viewBox` against actual content extents.

**Visual review (`references/self-review.md`).** Source review cannot see
overlapping labels, clipped text, contrast, or narrow-pane overflow —
only a render can. **The depth of this pass is proportional to what the
artifact contains:**

- **Artifacts with hand-authored SVG or generated diagrams** — run the
  full loop: screenshot at **two widths (~460px and ~900px) in both light
  and dark**, **Read all four PNGs**, and judge them against the visual
  checklist. Fix, re-render, and re-read until every frame is clean. This
  is the FR-6.3 safety net for the highest-variance tier; **never skip or
  shortcut it.**
- **Text-and-Shiki-only artifacts** (the usual COMPACT shape) — a single
  **narrow-width (460px) dark-mode** render plus the mechanical
  `pixelWidth` overflow check suffices: dark catches hardcoded-color
  mistakes and narrow catches overflow — the only two real bug classes
  when there is no bespoke SVG. If the render tooling is unavailable, a
  careful source-only review is acceptable for these (never for
  diagram-bearing ones).

The exact copy-pasteable `agent-browser` recipe (with the mechanical
horizontal-overflow check) and the full checklist live in
**`references/self-review.md`** — follow it.

### 7. Save and verify

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
  producing new artifact versions in apply mode) lives in
  `references/follow-ups.md` — read it when a headless run's prompt points
  you there. The core rule for any update: obey artifact-spec.md §4.3 —
  never rename existing `data-cfy-id`s, regenerate diagrams from their
  embedded sources, and bump `cfy:version`.
- Artifacts are stored centrally by the app
  (`~/Documents/conceptify/artifacts/…`) — they never touch the target
  repo, so there is nothing to gitignore.
