# Explainer videos — storyboard, render, embed

How to produce a short explainer clip and place it in an artifact as a
`cfy-video` figure (artifact-spec §1.4). Videos are **strictly additive**:
the artifact must remain a complete explanation with the clip absent, and
the clip's narration must also live, verbatim, in the mandatory
transcript. Use one **only for genuinely temporal concepts** — state
evolution, request lifecycles, pipelines, protocol rounds — never as
decoration (design-system.md). WHEN to reach for a video is a separate
decision (bead conceptify-z9y.5); this guide is the mechanical HOW.

Contents: [Workflow](#the-workflow) · [Templates](#pick-a-template) ·
[Storyboard JSON](#the-storyboard-json) · [Rendering](#rendering) ·
[Budgets](#budgets) · [Assembling the figure](#assembling-the-cfy-video-figure) ·
[Notes](#notes)

## The workflow

1. **Storyboard first — script, then motion.** Write **4–8 beats**, one
   sentence each. Each beat is (a) one narration line and (b) one segment
   of the animation. The beats ARE the video: their sentences become the
   transcript and the WebVTT cues, and their count/order drive the
   animation. Keep sentences short and declarative.
2. **Pick the template** whose shape matches the concept (below).
3. **Write the storyboard JSON** (one `beats` array + a little structure).
4. **Render** with `scripts/render-video.mjs` → an `.mp4`, a `.poster.jpg`,
   and a `.vtt`.
5. **Upload** the mp4 with `conceptify save-asset` to get its
   `cfy-asset://` URL (the clip is never embedded in the HTML).
6. **Assemble** the `cfy-video` figure: the `cfy-asset` src, the poster as
   a `data:` URI, the transcript = the narration **verbatim**, a
   `<figcaption>`.

## Pick a template

Three templates live in `skill/video/` (React/Remotion compositions).
Match the concept's *shape*, not its topic:

| Template | Concept shape | Reach for it when… |
|---|---|---|
| `step-sequence` | Ordered process / pipeline | stages happen in sequence: a request lifecycle, a build/deploy pipeline, an algorithm's phases. Visual echo of the `cfy-steps` component. |
| `state-machine` | State evolution | the subject IS in a state and transitions between named states: a connection/socket lifecycle, a retry/backoff loop, a handshake. |
| `data-flow` | Something travels | a payload/token moves through a fixed system and is transformed or inspected per hop: a read trace, an ETL pipeline, message passing. |

If none fits, the concept is probably not temporal — use a static diagram
(references/rendering.md) instead.

## The storyboard JSON

Every template takes a `beats` array; each beat has a `narration`
(string) and `seconds` (number > 0). The clip's duration is exactly the
sum of `seconds`. Fixed render target: **1280×720, 30 fps** (budget-safe;
do not try to change it). Example props live in `skill/video/examples/`.

**step-sequence** — one beat per step (`label`, optional `detail`):

```json
{
  "title": "One request, start to finish",
  "beats": [
    { "label": "Request", "detail": "The viewer asks for the artifact.",
      "narration": "A request leaves the viewer.", "seconds": 5 },
    { "label": "Resolve", "detail": "The handler maps the URL to a file.",
      "narration": "The handler resolves that URL to a file.", "seconds": 5 }
  ]
}
```

**state-machine** — a `nodes` list, then one beat per transition (`to` =
the state entered; optional `edgeLabel`):

```json
{
  "title": "A connection's lifecycle",
  "nodes": [ { "id": "idle", "label": "Idle" }, { "id": "open", "label": "Open" } ],
  "beats": [
    { "to": "idle", "narration": "It starts idle.", "seconds": 4 },
    { "to": "open", "edgeLabel": "connect()", "narration": "Then it opens.", "seconds": 5 }
  ]
}
```

**data-flow** — a `stages` list and a default `payload`, then one beat per
hop (`note` = the per-hop annotation; optional `payload` overrides the
token label to show a transform). There is one fewer beat than stages:

```json
{
  "title": "A read, traced end to end",
  "payload": "GET /doc",
  "stages": [ { "label": "Viewer" }, { "label": "Handler" }, { "label": "Core" } ],
  "beats": [
    { "note": "The viewer emits the request.", "narration": "The viewer emits a read request.", "seconds": 5 },
    { "note": "The handler validates the path.", "payload": "thread/sha", "narration": "The handler validates the path.", "seconds": 5 }
  ]
}
```

Optional top-level `posterFrame` (integer) picks the poster frame;
default is ~0.6 s in (a settled opening frame, not a blank one).

## Rendering

Prereq once (offline afterwards; `conceptify doctor` reports this and
hints the command):

```bash
cd skill/video && npm install && npm run ensure-browser
```

`ensure-browser` downloads Remotion's headless Chromium; `render-video.mjs`
also requires **ffmpeg** on PATH (`brew install ffmpeg`) — it normalizes
the encode to the exact §1.4 contract and adds faststart. Then:

```bash
node scripts/render-video.mjs --composition step-sequence \
  --props video/examples/step-sequence.json --out ./out
```

Outputs land in `--out` (default `./out`) as `<name>.mp4`,
`<name>.poster.jpg`, `<name>.vtt` (`--name` sets the basename; default =
composition id). Inline props also work: `--props-json '{"beats":[…]}'`.

**Rendering is CPU-heavy** — expect roughly a second of compute per second
of video, plus bundling. Tell the user a render is running and roughly how
long. The compositions are deterministic (frame-based; no `Date.now`/
`Math.random`), so the same props always yield the same bytes.

## Budgets

The script enforces the §1.4 encoding budgets so an out-of-spec clip is
caught at render time, not later at upload. It produces MP4 / H.264 High ≤
L4.0 / 8-bit `yuv420p`, silent, faststart.

| | Hard (render **fails**) | Should (**warns**) |
|---|---|---|
| Size | ≤ 20 MiB | — |
| Duration | ≤ 120 s (also refused pre-render) | 30–90 s |
| Resolution / fps | — | ≤ 1280×720 / ≤ 30 fps |
| Poster | — | ≤ 150 KiB |

A too-long storyboard is refused **before** rendering (in milliseconds).
If the script prints `[FAIL]`, do **not** upload the clip — trim beats and
re-render. Flat vector graphics compress tiny (these examples are well
under 1 MiB), so size is rarely a concern; duration discipline is.

## Assembling the `cfy-video` figure

1. Upload the clip and capture its URL:
   `conceptify save-asset --thread <thread-id> --file out/<name>.mp4`
   → prints `cfy-asset://localhost/<thread-id>/<sha256>.mp4`.
2. Base64 the poster into a `data:` URI (JPEG, SHOULD ≤ 150 KiB).
3. Emit the figure exactly per §1.4 — `<video controls preload="metadata"
   playsinline>`, **no autoplay/loop**, poster present, transcript
   immediately after, figcaption present:

```html
<figure class="cfy-video" data-cfy-id="vid-request-lifecycle">
  <video controls preload="metadata" playsinline
         poster="data:image/jpeg;base64,…"
         src="cfy-asset://localhost/<thread-id>/<sha256>.mp4"></video>
  <details class="cfy-details cfy-video-transcript">
    <summary>Transcript</summary>
    <!-- every beat.narration, in order, verbatim -->
    <p>A request leaves the viewer. The handler resolves that URL to a file. …</p>
  </details>
  <figcaption><strong>One request, start to finish.</strong> A short
  motion rendering of the serve path; the transcript carries the full
  narration, so nothing is lost when the clip cannot play.</figcaption>
</figure>
```

**The transcript body MUST be the narration script, word for word** (the
beats' `narration` lines, concatenated in order). This is how the
mandatory-transcript rule (`W-VIDEO-TRANSCRIPT`, §1.4) is satisfied in
practice, and it keeps the video's content readable, searchable, and
present offline/in a plain browser (where `cfy-asset://` shows only the
poster). A `<track kind="captions">` with a `data:`-URI copy of the `.vtt`
MAY be added, but the transcript is the floor regardless.

## Notes

- **Palette.** Compositions hardcode the light "Manuscript" `--cfy-*`
  defaults (`skill/video/src/theme.ts`) so clips match the artifact's
  default register. Per-theme/dark palette support is a natural extension
  once the theme-integration bead (conceptify-89k.3) lands — not wired up
  yet.
- **At most 2 video figures per artifact** (§1.4), and never as the sole
  carrier of information — adjacent prose must cover the same content.
- **Remotion licensing.** Remotion is free for individuals and companies
  of ≤ 3 people; larger companies need a paid license
  (remotion.dev/license). Fine for this personal project — recorded here
  so it is not forgotten if Conceptify ever ships commercially.
