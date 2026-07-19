#!/usr/bin/env node
// Conceptify skill — render a storyboard into an artifact-ready explainer
// clip. Drives the Remotion project in `skill/video/` (compositions +
// pinned deps) to produce, from one storyboard JSON, the three files a
// `cfy-video` figure needs (artifact-spec §1.4):
//
//   <name>.mp4         H.264 High / yuv420p 8-bit, faststart, silent
//   <name>.poster.jpg  a designated opening frame (the offline/print face)
//   <name>.vtt         WebVTT captions, one cue per storyboard beat
//
// The MP4 is NOT embedded in the artifact: it is uploaded first via
// `conceptify save-asset` and referenced through cfy-asset:// (see
// references/video.md). This script only produces conformant files.
//
// Budgets (artifact-spec §1.4) are checked here so an out-of-spec clip is
// caught at render time, not rejected later at upload:
//   HARD (exit non-zero): > 20 MiB, > 120 s, or not H.264/yuv420p mp4.
//   SHOULD (warn):        outside 30–90 s, > 1280x720, or > 30 fps.
// The duration cap is also pre-flighted from the storyboard BEFORE
// rendering, so a too-long clip is refused in milliseconds rather than
// after minutes of CPU.
//
// Fully offline once `skill/video` is installed and the Remotion browser
// is fetched (see `conceptify doctor` / references/video.md):
//   cd skill/video && npm install && npx remotion browser ensure
//
// Usage:
//   node render-video.mjs --composition step-sequence \
//     --props ../video/examples/step-sequence.json --out ./out
//   node render-video.mjs --composition data-flow \
//     --props-json '{"stages":[...],"beats":[...]}' --out /tmp/clip
//
// Flags:
//   --composition <id>   (required) step-sequence | state-machine | data-flow
//   --props <file>       storyboard JSON file (or --props-json)
//   --props-json <str>   inline storyboard JSON
//   --out <dir>          output directory (default: ./out)
//   --name <basename>    output basename (default: the composition id)
//   --poster-frame <n>   frame for the poster (default: ~0.6 s in)

import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';

const FPS = 30; // must match skill/video/src/theme.ts RENDER.fps
const HARD_BYTES = 20 * 1024 * 1024; // artifact-spec §1.4: <= 20 MiB
const HARD_SECONDS = 120; // artifact-spec §1.4: <= 120 s
const SHOULD_MIN_S = 30;
const SHOULD_MAX_S = 90;

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const VIDEO_DIR = path.resolve(scriptDir, '..', 'video');

function arg(name) {
  const i = process.argv.indexOf(`--${name}`);
  return i !== -1 ? process.argv[i + 1] : undefined;
}
function die(msg, code = 1) {
  console.error(`render-video: ${msg}`);
  process.exit(code);
}

// ---- resolve inputs --------------------------------------------------------
const composition = arg('composition');
if (!composition) die('missing --composition (step-sequence | state-machine | data-flow)', 2);

let propsRaw = arg('props-json');
const propsFile = arg('props');
if (!propsRaw && propsFile) {
  if (!fs.existsSync(propsFile)) die(`props file not found: ${propsFile}`, 2);
  propsRaw = fs.readFileSync(propsFile, 'utf8');
}
if (!propsRaw) die('missing --props <file> or --props-json <str>', 2);

let inputProps;
try {
  inputProps = JSON.parse(propsRaw);
} catch (e) {
  die(`storyboard JSON is not valid: ${e.message}`, 2);
}
const beats = inputProps.beats;
if (!Array.isArray(beats) || beats.length === 0) {
  die('storyboard must have a non-empty "beats" array', 2);
}
for (const [i, b] of beats.entries()) {
  if (typeof b.narration !== 'string' || !b.narration.trim()) {
    die(`beat ${i} is missing a "narration" string (needed for the transcript + captions)`, 2);
  }
  if (typeof b.seconds !== 'number' || b.seconds <= 0) {
    die(`beat ${i} is missing a positive "seconds" duration`, 2);
  }
}

const outDir = path.resolve(arg('out') ?? './out');
const name = arg('name') ?? composition;
fs.mkdirSync(outDir, { recursive: true });

// ---- beat frame geometry (mirrors skill/video/src/timing.ts) ---------------
function beatFrameRanges(bs) {
  let acc = 0;
  return bs.map((b) => {
    const start = acc;
    const dur = Math.max(1, Math.round(b.seconds * FPS));
    acc += dur;
    return { start, dur, end: acc };
  });
}
const ranges = beatFrameRanges(beats);
const totalFrames = ranges[ranges.length - 1].end;
const plannedSeconds = totalFrames / FPS;

// ---- pre-flight: refuse a too-long clip BEFORE spending CPU ----------------
if (plannedSeconds > HARD_SECONDS) {
  die(
    `storyboard runs ${plannedSeconds.toFixed(1)} s, over the ${HARD_SECONDS} s hard cap ` +
      `(artifact-spec §1.4). Trim beats before rendering — refusing without rendering.`,
    3,
  );
}

// ---- load Remotion from the vendored project -------------------------------
if (!fs.existsSync(path.join(VIDEO_DIR, 'node_modules'))) {
  die(
    `Remotion project is not installed. Run:\n` +
      `  cd ${VIDEO_DIR} && npm install && npx remotion browser ensure\n` +
      `(or run \`conceptify doctor\` for the same hint).`,
    4,
  );
}
const requireFromVideo = createRequire(path.join(VIDEO_DIR, 'package.json'));
const { bundle } = requireFromVideo('@remotion/bundler');
const { selectComposition, renderMedia, renderStill, ensureBrowser } =
  requireFromVideo('@remotion/renderer');

// ---- time formatting for WebVTT --------------------------------------------
function vttStamp(frames) {
  const totalMs = Math.round((frames / FPS) * 1000);
  const ms = totalMs % 1000;
  const s = Math.floor(totalMs / 1000) % 60;
  const m = Math.floor(totalMs / 60000) % 60;
  const h = Math.floor(totalMs / 3600000);
  const p = (n, w = 2) => String(n).padStart(w, '0');
  return `${p(h)}:${p(m)}:${p(s)}.${p(ms, 3)}`;
}
function buildVtt(bs, rgs) {
  const lines = ['WEBVTT', ''];
  bs.forEach((b, i) => {
    lines.push(String(i + 1));
    lines.push(`${vttStamp(rgs[i].start)} --> ${vttStamp(rgs[i].end)}`);
    lines.push(b.narration.trim());
    lines.push('');
  });
  return lines.join('\n');
}

// ---- ffmpeg / ffprobe helpers (faststart + budget verification) ------------
function has(bin) {
  try {
    execFileSync(bin, ['-version'], { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
}
function ffprobe(file) {
  const out = execFileSync(
    'ffprobe',
    [
      '-v', 'error',
      '-select_streams', 'v:0',
      '-show_entries', 'stream=codec_name,pix_fmt,width,height,avg_frame_rate',
      '-show_entries', 'format=duration',
      '-of', 'json',
      file,
    ],
    { encoding: 'utf8' },
  );
  const j = JSON.parse(out);
  const st = (j.streams && j.streams[0]) || {};
  const [num, den] = String(st.avg_frame_rate || '0/1').split('/').map(Number);
  return {
    codec: st.codec_name,
    pixFmt: st.pix_fmt,
    width: st.width,
    height: st.height,
    fps: den ? num / den : 0,
    duration: parseFloat((j.format && j.format.duration) || '0'),
  };
}

// ---- render ----------------------------------------------------------------
const mp4Out = path.join(outDir, `${name}.mp4`);
const posterOut = path.join(outDir, `${name}.poster.jpg`);
const vttOut = path.join(outDir, `${name}.vtt`);

const posterFrame = Number.isInteger(Number(arg('poster-frame')))
  ? Number(arg('poster-frame'))
  : Number.isInteger(inputProps.posterFrame)
    ? inputProps.posterFrame
    : Math.min(totalFrames - 1, Math.round(FPS * 0.6));

console.error(
  `render-video: ${composition} — ${beats.length} beats, ` +
    `${plannedSeconds.toFixed(1)} s @ ${FPS}fps. Rendering (this is CPU-heavy; ` +
    `expect ~seconds per second of video)…`,
);

await ensureBrowser();

const serveUrl = await bundle({
  entryPoint: path.join(VIDEO_DIR, 'src', 'index.ts'),
  webpackOverride: (c) => c,
});

const comp = await selectComposition({ serveUrl, id: composition, inputProps });

const rawMp4 = has('ffmpeg') ? path.join(outDir, `.${name}.raw.mp4`) : mp4Out;
await renderMedia({
  composition: comp,
  serveUrl,
  codec: 'h264',
  muted: true, // silent clip — no audio track (budget allows silent)
  x264Preset: 'medium',
  outputLocation: rawMp4,
  inputProps,
});

// Normalize to the exact §1.4 encoding contract in one ffmpeg pass:
//   - pix_fmt yuv420p (LIMITED range): Remotion/libx264 tags its output
//     yuvj420p (full range) because the Chromium frames are full-range
//     RGB; limited-range yuv420p is the safe floor every player decodes
//     and is what the app-side E-ASSET-TYPE sniff expects.
//   - profile High, level 4.0: pins the MUST ceiling regardless of size.
//   - +faststart: moov before mdat (SHOULD) so the clip seeks over Range.
//   - -an: drop any audio (the clip is silent).
// The source is flat vector motion graphics, so this transcode is fast
// and visually lossless at crf 18.
if (has('ffmpeg')) {
  execFileSync(
    'ffmpeg',
    [
      '-y', '-i', rawMp4,
      // remap full-range (pc) sample values to limited (tv) range so the
      // output is true yuv420p, not the full-range yuvj420p variant.
      '-vf', 'scale=in_range=pc:out_range=tv',
      '-c:v', 'libx264',
      '-profile:v', 'high',
      '-level', '4.0',
      '-pix_fmt', 'yuv420p',
      '-preset', 'medium',
      '-crf', '18',
      '-an',
      '-movflags', '+faststart',
      mp4Out,
    ],
    { stdio: 'ignore' },
  );
  fs.rmSync(rawMp4, { force: true });
} else {
  console.error(
    'render-video: [warn] ffmpeg not found — the raw Remotion output is full-range ' +
      'yuvj420p and lacks a faststart moov, so it will FAIL the yuv420p budget check ' +
      'below. Install ffmpeg (brew install ffmpeg) to produce a spec-conformant clip.',
  );
}

await renderStill({
  composition: comp,
  serveUrl,
  output: posterOut,
  frame: posterFrame,
  imageFormat: 'jpeg',
  jpegQuality: 80,
  inputProps,
});

fs.writeFileSync(vttOut, buildVtt(beats, ranges), 'utf8');

// ---- verify budgets --------------------------------------------------------
const bytes = fs.statSync(mp4Out).size;
const mib = bytes / (1024 * 1024);
const posterKib = fs.statSync(posterOut).size / 1024;

let probe = null;
if (has('ffprobe')) {
  try {
    probe = ffprobe(mp4Out);
  } catch (e) {
    console.error(`render-video: [warn] ffprobe failed (${e.message}); skipping codec checks.`);
  }
}

const failures = [];
const warnings = [];

if (bytes > HARD_BYTES) {
  failures.push(`size ${mib.toFixed(2)} MiB exceeds the 20 MiB hard cap (E-ASSET-SIZE)`);
}
if (probe) {
  if (probe.duration > HARD_SECONDS) {
    failures.push(`duration ${probe.duration.toFixed(1)} s exceeds the 120 s hard cap (E-ASSET-DURATION)`);
  }
  if (probe.codec !== 'h264') {
    failures.push(`video codec is ${probe.codec}, not h264 (E-ASSET-TYPE)`);
  }
  if (probe.pixFmt !== 'yuv420p') {
    failures.push(`pixel format is ${probe.pixFmt}, not 8-bit yuv420p (E-ASSET-TYPE)`);
  }
  if (probe.width > 1280 || probe.height > 720) {
    warnings.push(`resolution ${probe.width}x${probe.height} exceeds 1280x720 SHOULD (W-ASSET-RES)`);
  }
  if (probe.fps > 30.5) {
    warnings.push(`frame rate ${probe.fps.toFixed(1)} exceeds 30 fps SHOULD`);
  }
  const d = probe.duration;
  if (d > SHOULD_MAX_S) warnings.push(`duration ${d.toFixed(1)} s over the 90 s SHOULD (W-ASSET-LONG)`);
  else if (d < SHOULD_MIN_S) warnings.push(`duration ${d.toFixed(1)} s under the 30 s SHOULD (fine for a short explainer)`);
}
if (posterKib > 150) {
  warnings.push(`poster ${posterKib.toFixed(0)} KiB over the 150 KiB SHOULD — re-encode smaller before embedding`);
}

// ---- report ----------------------------------------------------------------
console.error('');
console.error(`  mp4     ${mp4Out}  (${mib.toFixed(2)} MiB)`);
console.error(`  poster  ${posterOut}  (${posterKib.toFixed(0)} KiB, frame ${posterFrame})`);
console.error(`  vtt     ${vttOut}  (${beats.length} cues)`);
if (probe) {
  console.error(
    `  probe   ${probe.codec}/${probe.pixFmt} ${probe.width}x${probe.height} ` +
      `@ ${probe.fps.toFixed(0)}fps, ${probe.duration.toFixed(1)} s`,
  );
}
for (const w of warnings) console.error(`  [warn]  ${w}`);
if (failures.length) {
  for (const f of failures) console.error(`  [FAIL]  ${f}`);
  console.error('\nrender-video: clip is OUT OF BUDGET — do not upload it. Trim/re-encode and re-run.');
  process.exit(1);
}
console.error('\nrender-video: done — within budget. Upload the mp4 with `conceptify save-asset`.');
