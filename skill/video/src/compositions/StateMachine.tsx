import React from 'react';
import { interpolate, spring, useCurrentFrame, useVideoConfig } from 'remotion';
import { theme } from '../theme';
import { Stage } from '../components/Stage';
import { activeBeat, beatRanges, type Beat } from '../timing';
import { contentBox } from '../layout';

/**
 * state-machine — labelled states on a ring with animated transitions.
 * Each beat activates one state (`to`); a token travels along the edge
 * from the previously active state to the new one while it is highlighted.
 * For temporal concepts that are really about state evolution: protocol
 * handshakes, connection lifecycles, retry/backoff loops.
 */

export type Transition = Beat & {
  /** Id of the state this beat transitions INTO (must match a node id). */
  to: string;
  /** Optional edge label shown while this transition is active. */
  edgeLabel?: string;
};

export type StateMachineProps = {
  title?: string;
  nodes: { id: string; label: string }[];
  beats: Transition[];
};

function ringLayout(n: number, cx: number, cy: number, r: number) {
  return Array.from({ length: n }, (_, i) => {
    const angle = -Math.PI / 2 + (i * 2 * Math.PI) / n;
    return { x: cx + r * Math.cos(angle), y: cy + r * Math.sin(angle) };
  });
}

export const StateMachine: React.FC<StateMachineProps> = ({ title, nodes, beats }) => {
  const frame = useCurrentFrame();
  const { fps, width, height } = useVideoConfig();
  const ranges = beatRanges(beats, fps);
  const active = activeBeat(frame, ranges);

  // All coordinates are within Stage's content box (origin at its top-left).
  const { w: boxW, h: boxH } = contentBox(width, height, true);
  const cx = boxW / 2;
  const cy = boxH / 2;
  const radius = Math.min(boxW, boxH) * 0.34;
  const pos = ringLayout(nodes.length, cx, cy, radius);
  const idIndex = new Map(nodes.map((nd, i) => [nd.id, i]));

  const activeIdx = idIndex.get(beats[active]?.to ?? '') ?? 0;
  const prevIdx =
    active > 0 ? (idIndex.get(beats[active - 1].to) ?? activeIdx) : activeIdx;

  // token progress along the active edge (prev -> active) within this beat
  const local = frame - ranges[active].start;
  const travel = interpolate(local, [2, Math.max(6, ranges[active].dur - 4)], [0, 1], {
    extrapolateLeft: 'clamp',
    extrapolateRight: 'clamp',
  });
  const from = pos[prevIdx];
  const to = pos[activeIdx];
  const moving = prevIdx !== activeIdx;
  const tokenX = from.x + (to.x - from.x) * travel;
  const tokenY = from.y + (to.y - from.y) * travel;

  const nodeW = 150;
  const nodeH = 66;

  return (
    <Stage title={title}>
      <svg
        width="100%"
        height="100%"
        viewBox={`0 0 ${boxW} ${boxH}`}
        style={{ position: 'absolute', inset: 0, overflow: 'visible' }}
      >
        {/* faint edges for every consecutive transition in beat order */}
        {beats.map((b, i) => {
          if (i === 0) return null;
          const a = idIndex.get(beats[i - 1].to) ?? 0;
          const c = idIndex.get(b.to) ?? 0;
          if (a === c) return null;
          const isActiveEdge = i === active;
          return (
            <line
              key={`e${i}`}
              x1={pos[a].x}
              y1={pos[a].y}
              x2={pos[c].x}
              y2={pos[c].y}
              stroke={isActiveEdge ? theme.accent : theme.line}
              strokeWidth={isActiveEdge ? 3 : 2}
              strokeDasharray={isActiveEdge ? undefined : '4 6'}
              opacity={isActiveEdge ? 1 : 0.6}
            />
          );
        })}
        {/* travelling token */}
        {moving ? (
          <circle cx={tokenX} cy={tokenY} r={9} fill={theme.accent} />
        ) : null}
      </svg>

      {nodes.map((nd, i) => {
        const isActive = i === activeIdx;
        const pop = spring({
          frame: isActive ? frame - ranges[active].start : 0,
          fps,
          config: { damping: 16, mass: 0.6, stiffness: 150 },
          durationInFrames: 18,
        });
        const scale = isActive ? interpolate(pop, [0, 1], [0.9, 1.06]) : 1;
        return (
          <div
            key={nd.id}
            style={{
              position: 'absolute',
              left: pos[i].x - nodeW / 2,
              top: pos[i].y - nodeH / 2,
              width: nodeW,
              height: nodeH,
              display: 'grid',
              placeItems: 'center',
              textAlign: 'center',
              padding: '0 10px',
              borderRadius: 12,
              fontFamily: theme.sans,
              fontSize: 21,
              fontWeight: 600,
              color: isActive ? theme.paper : theme.label,
              backgroundColor: isActive ? theme.accent : theme.nodeFill,
              border: `2px solid ${isActive ? theme.accent : theme.nodeStroke}`,
              boxShadow: isActive ? `0 0 0 6px ${theme.diagramAccentBg}` : 'none',
              transform: `scale(${scale})`,
              transformOrigin: 'center',
            }}
          >
            {nd.label}
          </div>
        );
      })}

      {/* active edge label / caption */}
      {beats[active]?.edgeLabel ? (
        <div
          style={{
            position: 'absolute',
            left: 0,
            bottom: 0,
            fontFamily: theme.mono,
            fontSize: 20,
            color: theme.accent,
          }}
        >
          {beats[active].edgeLabel}
        </div>
      ) : null}
    </Stage>
  );
};
