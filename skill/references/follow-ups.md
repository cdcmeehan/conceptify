# Follow-up runs — answering and applying comments (FR-6.4)

House rules for a **headless follow-up run**: the Conceptify app spawned
you with a run-specific prompt to handle reader comments on an artifact
you did not write. Two modes — **answer** (reply in the sidebar) and
**apply** (publish a new artifact version). Your prompt already carries
the load-bearing contract; this file *deepens* it — answer style, how to
read anchors, per-renderer diagram re-generation, and the run's toolset
limits. **Where this file and your prompt ever seem to disagree, the
prompt wins** — it is the exact per-run contract, and the app tests it
verbatim.

Contents: [How a run works](#how-a-follow-up-run-works) ·
[Reading anchors](#reading-anchors) · [Answer mode](#answer-mode) ·
[Apply mode](#apply-mode) · [Toolset limits](#toolset-limits) ·
[When you cannot finish](#when-you-cannot-finish)

## How a follow-up run works

- You are launched **headless, inside the project the artifact explains**
  — your working directory is the project root. The real code is your
  ground truth; read it before answering or editing anything.
- Your prompt embeds the run context: the thread question, the artifact
  file path, and every target comment with its anchor. If you need to
  re-fetch it fresh (or lost it), run
  `conceptify get-context --thread <id>` — it returns the same context in
  one round-trip (thread question, `projectRoot`, `artifactPath`, and the
  `openComments` array). Top-level keys are camelCase; **each comment's
  `anchor` is passed through verbatim** (see below).
- The artifact path is the **app-owned file**. Read it; never write to it.
  In apply mode you edit a *copy* and publish via the CLI (never a direct
  file write) — the app copies your working file into its own storage on
  save.

## Reading anchors

An anchor says *where in the artifact* a comment points. Its inner keys
stay **`snake_case`** (`cfy_id`, `quote`, `start`, `end`, `type`, `v`) —
`get-context` passes the stored object through byte-for-byte, so read it
as-is and never rewrite it.

- `cfy_id` — the `data-cfy-id` of the target element (heading, figure, or
  a diagram node like `fig-auth-flow.token-service`). This is your
  fastest way to find the anchored spot in the HTML.
- `quote.exact` — the exact text the reader selected (with optional
  `prefix`/`suffix` context). For text anchors, `start`/`end` are
  character offsets *within* the `cfy_id` element's visible text.
- `type: "text"` = a text selection; `type: "element"` = a whole clicked
  element (diagram node / heading).
- **A `null` anchor is a direct question about the artifact as a whole** —
  not tied to any element. Answer it holistically.

The `cfy_id`/`quote` pair is the durable cross-version contract that keeps
comments attached after edits (re-attachment, below) — treat it as
load-bearing, not incidental.

## Answer mode

Reply to each comment in the sidebar; the artifact is **never modified**.

- **Per-comment granularity.** One `conceptify resolve-comment` call per
  comment — never combine several, never skip one. Resolve each the moment
  its answer is ready so answers stream into the sidebar one by one.
- **Answer what you can; leave what you can't — never fabricate.** If the
  code doesn't support a confident answer, say what you *can* establish and
  name the uncertainty. A comment you genuinely can't address is better
  left unanswered than answered with invention.
- **The answer file is a sidebar fragment, not a document.** It may be an
  **HTML fragment or Markdown** — the sidebar renders it verbatim into a
  narrow pane. Style it accordingly:
  - Compact: a sentence to two short paragraphs. This is a margin note,
    not an essay.
  - Simple markup only — `<p>`, `<ul>`/`<ol>`, `<strong>`/`<em>`, `<a>`,
    and `<code>`/`<pre>` for code. Small, load-bearing code snippets are
    welcome; keep them short.
  - **No** `<html>`/`<head>`/`<body>` wrapper, **no** `<script>`, **no**
    `<style>` or design-system classes (the artifact's CSS isn't loaded in
    the sidebar), **no** full-page scaffold.
- **Never** `save-artifact` and **never** pass `--applied` in answer mode.
  Answering and applying are deliberately separate steps; this run only
  answers.

```bash
ANSWERS=$(mktemp -d)                                   # scratch, outside the repo
# ...write $ANSWERS/<comment-id>.html per comment...
conceptify resolve-comment --id <comment-id> --answer-file "$ANSWERS/<comment-id>.html"
```

## Apply mode

Edit a **working copy** of the artifact so every target comment is
addressed, then publish exactly one new version. **The order is a hard
contract:** all edits first, then `resolve-comment --applied` for **every**
target, then `save-artifact` **once, last**.

**Why the order.** `--applied` freezes a comment at the artifact version it
was written against and excludes it from the save-time re-attachment pass;
saving first would make the app try to re-anchor the very text you just
rewrote (noise, not corruption). This is the FR-4.4 freeze-before-save
property — the re-attachment pass runs inside the same transaction as the
version insert and migrates only the comments you did *not* touch (full
semantics: `docs/api.md → Re-attachment across versions` in the Conceptify
repo).

**Editing the working copy:**

- **Edit surgically.** Change only what the comments ask for; leave the
  rest of the file byte-stable. You are amending an artifact, not
  rewriting it.
- **Never rename, repurpose, or delete an existing `data-cfy-id`.** Other
  comments' anchors point at those ids, and so does re-attachment.
  Re-attachment is the *safety net* for content that genuinely moved — not
  a license to churn ids (`artifact-spec.md §4.3`). New elements get new
  ids; keep every existing one exactly.
- **Regenerate diagrams from source — never hand-edit rendered SVG.** Each
  generated diagram carries its DSL in a `<!--cfy:src lang="…" for="…"
  renderer="…"-->` comment immediately before the figure. To change a
  diagram: **decode** the source (`\\`→`\`, `\>`→`>`), edit the DSL,
  re-render it with the recorded renderer, re-run `postprocess-svg.mjs` to
  re-stamp `data-cfy-id`s, replace the rendered element, and update the
  `cfy:src` comment (**re-encode**: `\`→`\\`, `-->`→`--\>`, `--!>`→`--!\>`).
  Exact render/post-process commands per tool are in `references/rendering.md`;
  the escaping rules are `artifact-spec.md §5.2`.
  - **Keep DSL node keys stable.** Ids are derived deterministically from
    the DSL keys, so an unchanged node keeps its id across regeneration
    (`postprocess-svg.mjs` is idempotent and preserves already-stamped
    ids). Renaming a key silently renames its id — which breaks anchors.
    If the fix is to a node the reader commented on, edit its *label*, not
    its key.
- **Bump `<meta name="cfy:version" content="…">`** to the next version
  (your prompt states the exact number).
- **Keep the scaffold intact.** The first `<style>` block is the
  design-system CSS, verbatim; leave it and the design-system component
  classes as they are. The file must stay fully self-contained.
- **Run the visual self-review before saving** whenever your change
  altered anything visual (a diagram, hand-authored SVG, layout, new
  components) — render, screenshot at two widths in both schemes, and
  Read the PNGs per `references/self-review.md`. It is the FR-6.3 gate and
  it applies to edits, not just first drafts.

```bash
WORK=$(mktemp -d)/artifact.html          # working copy, outside the repo
cp "<artifact-path>" "$WORK"
# ...edit $WORK until every comment is addressed; bump cfy:version...
# THEN, only once the file is final — mark every target applied:
conceptify resolve-comment --id <comment-id> --answer-file <note-file> --applied
# ...one --applied call per target...
# THEN publish, exactly once, as the very last CLI call:
conceptify save-artifact --thread <thread-id> --file "$WORK"
```

If you cannot complete the edits, do **not** `save-artifact` and do **not**
mark any comment `--applied` — an honest failure beats publishing a broken
version or claiming an unpublished change was applied.

## Toolset limits

Your run is deliberately scoped (OQ3; recorded in `docs/api.md →
Permission scoping`). Every follow-up and in-app mode, and every future
adapter, obeys the same principle:

- **The target repo is read-only.** Your Edit/Write tools are denied under
  the project root. Read the code freely; write only under your own temp
  directory. Artifact writes go through the CLI (`save-artifact`), never a
  direct file write.
- **Temp dirs are writable.** Do all scratch and working-copy work in
  `mktemp -d` directories outside the repo.
- **No web.** `WebFetch`/`WebSearch` are disabled — ground everything in
  the local code and the artifact, not the internet.
- **No mutating git.** `commit`, `push`, `add`, `rebase`, `merge`, `reset`,
  `checkout`, `switch`, `restore`, `stash`, `clean` are denied; git *reads*
  (`log`, `diff`, `blame`, `grep`) stay available for grounding.

If a tool call you actually need is denied, **don't flail** — retrying a
denied command wastes the run. Note the limitation in the affected
comment's answer and move on.

## When you cannot finish

Fail loudly and honestly, not silently. If the artifact is missing, the
context is broken, or a needed capability is denied, **say so through
`resolve-comment` answers** on the comments you can still reach, rather
than exiting with no trace. Your full transcript is retained at
`runs/<run-id>.log` for the user to inspect, so a clear explanation in the
answer plus an honest non-completion is always better than a fabricated
success.
