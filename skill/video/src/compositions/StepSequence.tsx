import React from 'react';
import { interpolate, spring, useCurrentFrame, useVideoConfig } from 'remotion';
import { theme } from '../theme';
import { Stage } from '../components/Stage';
import { activeBeat, beatRanges, type Beat } from '../timing';

/**
 * step-sequence — numbered stages revealed and highlighted in order, for
 * pipelines, request lifecycles, and ordered processes. Visual language
 * echoes the artifact's `cfy-steps` component: serif numeral badges on a
 * connecting rail, step name in a serif weight, detail in muted sans.
 *
 * One beat == one step. Steps before the active beat read as "done"
 * (filled accent badge), the active step pops in and is fully inked, and
 * later steps sit muted until their turn.
 */

export type Step = Beat & {
  /** Step name — the first words, like the <strong>Name.</strong> convention. */
  label: string;
  /** Optional supporting sentence shown under the label. */
  detail?: string;
};

export type StepSequenceProps = {
  title?: string;
  beats: Step[];
};

const ROW = 92; // fixed row pitch so the rail geometry is predictable

export const StepSequence: React.FC<StepSequenceProps> = ({ title, beats }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const ranges = beatRanges(beats, fps);
  const active = activeBeat(frame, ranges);

  const badge = 56;
  const railX = badge / 2;
  const firstCenter = badge / 2;
  const activeCenter = active * ROW + firstCenter;
  const railBottom = (beats.length - 1) * ROW + firstCenter;

  return (
    <Stage title={title}>
      <div style={{ position: 'relative', paddingTop: 8 }}>
        {/* full rail */}
        <div
          style={{
            position: 'absolute',
            left: railX - 1,
            top: firstCenter,
            height: Math.max(0, railBottom - firstCenter),
            width: 2,
            backgroundColor: theme.line,
          }}
        />
        {/* progress rail up to the active badge */}
        <div
          style={{
            position: 'absolute',
            left: railX - 1,
            top: firstCenter,
            height: Math.max(0, activeCenter - firstCenter),
            width: 2,
            backgroundColor: theme.accent,
          }}
        />
        {beats.map((step, i) => {
          const done = i < active;
          const isActive = i === active;
          const pop = spring({
            frame: frame - ranges[i].start,
            fps,
            config: { damping: 18, mass: 0.6, stiffness: 140 },
            durationInFrames: 20,
          });
          const rowOpacity = isActive
            ? interpolate(pop, [0, 1], [0.5, 1])
            : done
              ? 0.85
              : 0.34;
          const scale = isActive ? interpolate(pop, [0, 1], [0.92, 1]) : 1;
          const filled = done || isActive;
          return (
            <div
              key={i}
              style={{
                position: 'relative',
                height: ROW,
                display: 'flex',
                alignItems: 'flex-start',
                gap: 24,
                opacity: rowOpacity,
                transform: `scale(${scale})`,
                transformOrigin: 'left center',
              }}
            >
              <div
                style={{
                  width: badge,
                  height: badge,
                  flex: '0 0 auto',
                  borderRadius: '50%',
                  display: 'grid',
                  placeItems: 'center',
                  fontFamily: theme.serif,
                  fontSize: 26,
                  fontWeight: 700,
                  color: filled ? theme.paper : theme.accent,
                  backgroundColor: filled ? theme.accent : theme.surface,
                  border: `2px solid ${filled ? theme.accent : theme.line}`,
                  boxShadow: isActive ? `0 0 0 6px ${theme.diagramAccentBg}` : 'none',
                }}
              >
                {done ? '✓' : i + 1}
              </div>
              <div style={{ paddingTop: 4 }}>
                <div
                  style={{
                    fontFamily: theme.serif,
                    fontSize: 30,
                    fontWeight: 700,
                    color: theme.ink,
                  }}
                >
                  {step.label}
                </div>
                {step.detail ? (
                  <div
                    style={{
                      marginTop: 6,
                      fontSize: 22,
                      lineHeight: 1.4,
                      color: theme.muted,
                      maxWidth: 820,
                    }}
                  >
                    {step.detail}
                  </div>
                ) : null}
              </div>
            </div>
          );
        })}
      </div>
    </Stage>
  );
};
