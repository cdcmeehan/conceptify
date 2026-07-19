// Beat timing helpers shared by every composition and the render script.
//
// A "beat" is one storyboard sentence (references/video.md): it carries a
// narration line, a duration in seconds, and composition-specific visual
// fields. Beats are the single source of truth for (a) the animation
// segments, (b) the clip's total duration, and (c) the WebVTT cues — so
// the transcript, the captions, and the motion always stay in lockstep.
//
// Deterministic: pure functions of the props, no wall-clock or randomness.

export type Beat = {
  /** One narration sentence. Verbatim into the artifact transcript + VTT. */
  narration: string;
  /** How long this beat holds, in seconds (>= 1 frame after rounding). */
  seconds: number;
};

export type BeatRange = { start: number; dur: number; end: number };

/** Frame span [start, end) for each beat, at the given fps. */
export function beatRanges(beats: { seconds: number }[], fps: number): BeatRange[] {
  let acc = 0;
  return beats.map((b) => {
    const start = acc;
    const dur = Math.max(1, Math.round(b.seconds * fps));
    acc += dur;
    return { start, dur, end: acc };
  });
}

/** Total frame count for a beat list. */
export function totalFrames(beats: { seconds: number }[], fps: number): number {
  return beatRanges(beats, fps).reduce((n, r) => Math.max(n, r.end), 0);
}

/** Index of the beat active at `frame` (clamped to the last beat). */
export function activeBeat(frame: number, ranges: BeatRange[]): number {
  for (let i = 0; i < ranges.length; i++) {
    if (frame < ranges[i].end) return i;
  }
  return ranges.length - 1;
}
