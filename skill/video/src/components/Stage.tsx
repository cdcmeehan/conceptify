import React from 'react';
import { AbsoluteFill, interpolate, useCurrentFrame } from 'remotion';
import { theme } from '../theme';
import { PAD, TITLE_H } from '../layout';

/**
 * Shared paper frame for every composition: paper background, a serif
 * title that fades in, an accent hairline under it, and a padded stage
 * region for the composition's own content. Matches the artifact's
 * cfy-diagram register (paper / hairline / serif display).
 */
export const Stage: React.FC<{ title?: string; children: React.ReactNode }> = ({
  title,
  children,
}) => {
  const frame = useCurrentFrame();
  const titleIn = interpolate(frame, [0, 14], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });
  return (
    <AbsoluteFill
      style={{
        backgroundColor: theme.paper,
        color: theme.ink,
        fontFamily: theme.sans,
        padding: PAD,
      }}
    >
      {title ? (
        <div style={{ height: TITLE_H, opacity: titleIn }}>
          <div
            style={{
              fontFamily: theme.serif,
              fontSize: 46,
              fontWeight: 700,
              letterSpacing: '-0.01em',
              lineHeight: 1.1,
            }}
          >
            {title}
          </div>
          <div
            style={{
              marginTop: 16,
              width: interpolate(titleIn, [0, 1], [0, 120]),
              height: 3,
              borderRadius: 2,
              backgroundColor: theme.accent,
            }}
          />
        </div>
      ) : null}
      <div style={{ position: 'relative', flex: 1 }}>{children}</div>
    </AbsoluteFill>
  );
};
