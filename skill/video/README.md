# conceptify-video

Remotion composition templates for Conceptify artifact explainer videos
(bead conceptify-z9y.3). Rendered by `../scripts/render-video.mjs`; the
full authoring guide is **`../references/video.md`** — read that, not this.

## Setup (once; offline afterwards)

```bash
npm install
npm run ensure-browser   # downloads Remotion's headless Chromium
```

`render-video.mjs` also needs `ffmpeg` on PATH (`brew install ffmpeg`) to
normalize the encode to the artifact-spec §1.4 contract.

## Templates (compositions)

| id | shape | file |
|---|---|---|
| `step-sequence` | ordered process / pipeline | `src/compositions/StepSequence.tsx` |
| `state-machine` | state evolution | `src/compositions/StateMachine.tsx` |
| `data-flow` | payload travelling a system | `src/compositions/DataFlow.tsx` |

Each is parameterized by a storyboard JSON (`beats` array); see
`examples/`. Render target is fixed at 1280×720 / 30 fps (budget-safe).
Compositions are deterministic (frame-based, no wall-clock/randomness) and
use the light "Manuscript" `--cfy-*` palette (`src/theme.ts`); per-theme
palette support is a future extension (bead conceptify-89k.3).

## Preview

```bash
npm run studio   # opens Remotion Studio for interactive preview
```

## Render

```bash
node ../scripts/render-video.mjs --composition step-sequence \
  --props examples/step-sequence.json --out ./out
```
