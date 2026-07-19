import React from 'react';
import { interpolate, spring, useCurrentFrame, useVideoConfig } from 'remotion';
import { theme } from '../theme';
import { Stage } from '../components/Stage';
import { activeBeat, beatRanges, type Beat } from '../timing';
import { contentBox } from '../layout';

/**
 * data-flow — a payload/token travelling through a horizontal pipeline of
 * stages, one hop per beat, with a per-hop annotation. For request
 * traces, ETL pipelines, message passing: the thing that moves matters as
 * much as the boxes it moves between.
 *
 * `stages` are the boxes (left to right). One beat == one hop: during
 * beat i the token travels from stage i to stage i+1, so there is one
 * fewer beat than stages. Each beat may override the token's `payload`
 * label (to show a transform) and supplies a `note` shown under the rail.
 */

export type Hop = Beat & {
  /** Annotation for this hop, shown under the pipeline. */
  note?: string;
  /** Optional payload label while this hop is active (shows transforms). */
  payload?: string;
};

export type DataFlowProps = {
  title?: string;
  stages: { label: string }[];
  /** Default token label; a hop's own `payload` overrides it. */
  payload?: string;
  beats: Hop[];
};

export const DataFlow: React.FC<DataFlowProps> = ({
  title,
  stages,
  payload = 'payload',
  beats,
}) => {
  const frame = useCurrentFrame();
  const { fps, width, height } = useVideoConfig();
  const ranges = beatRanges(beats, fps);
  const active = activeBeat(frame, ranges);

  // Coordinates are within Stage's content box.
  const { w: inner, h: innerH } = contentBox(width, height, true);
  const boxW = 200;
  const boxH = 96;
  const railY = innerH * 0.36; // top of the stage boxes; annotation sits below
  const n = stages.length;
  const slot = n > 1 ? (inner - boxW) / (n - 1) : 0;
  const centerX = (i: number) => boxW / 2 + i * slot;

  // token travels from stage[active] to stage[active+1] over the beat
  const local = frame - ranges[active].start;
  const t = interpolate(local, [2, Math.max(6, ranges[active].dur - 3)], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });
  const fromI = Math.min(active, n - 1);
  const toI = Math.min(active + 1, n - 1);
  const tokenX = centerX(fromI) + (centerX(toI) - centerX(fromI)) * t;
  const label = beats[active]?.payload ?? payload;

  const noteIn = spring({
    frame: local,
    fps,
    config: { damping: 200 },
    durationInFrames: 12,
  });

  return (
    <Stage title={title}>
      {/* connecting rail */}
      <div
        style={{
          position: 'absolute',
          left: centerX(0),
          top: railY + boxH / 2 - 1,
          width: centerX(n - 1) - centerX(0),
          height: 2,
          backgroundColor: theme.line,
        }}
      />

      {/* stage boxes */}
      {stages.map((s, i) => {
        const touched = i <= active || (i === active + 1 && t > 0.5);
        return (
          <div
            key={i}
            style={{
              position: 'absolute',
              left: centerX(i) - boxW / 2,
              top: railY,
              width: boxW,
              height: boxH,
              display: 'grid',
              placeItems: 'center',
              textAlign: 'center',
              padding: '0 12px',
              borderRadius: 14,
              fontFamily: theme.sans,
              fontSize: 22,
              fontWeight: 600,
              color: theme.label,
              backgroundColor: theme.nodeFill,
              border: `2px solid ${touched ? theme.accent : theme.nodeStroke}`,
              opacity: touched ? 1 : 0.7,
            }}
          >
            {s.label}
          </div>
        );
      })}

      {/* travelling payload token */}
      <div
        style={{
          position: 'absolute',
          left: tokenX,
          top: railY + boxH / 2,
          transform: 'translate(-50%, -50%)',
          padding: '8px 16px',
          borderRadius: 999,
          fontFamily: theme.mono,
          fontSize: 19,
          fontWeight: 600,
          color: theme.paper,
          backgroundColor: theme.accent,
          boxShadow: `0 4px 14px ${theme.diagramAccentBg}`,
          whiteSpace: 'nowrap',
        }}
      >
        {label}
      </div>

      {/* per-hop annotation */}
      {beats[active]?.note ? (
        <div
          style={{
            position: 'absolute',
            left: 0,
            right: 0,
            top: railY + boxH + 64,
            textAlign: 'center',
            opacity: noteIn,
          }}
        >
          <span
            style={{
              fontFamily: theme.sans,
              fontSize: 24,
              lineHeight: 1.5,
              color: theme.ink,
              background: `linear-gradient(transparent 58%, ${theme.mark} 58%)`,
              padding: '0 6px',
            }}
          >
            {beats[active].note}
          </span>
        </div>
      ) : null}
    </Stage>
  );
};
