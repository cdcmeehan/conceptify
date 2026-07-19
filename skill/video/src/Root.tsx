import React from 'react';
import { Composition } from 'remotion';
import { RENDER } from './theme';
import { totalFrames } from './timing';
import { StepSequence } from './compositions/StepSequence';
import { StateMachine } from './compositions/StateMachine';
import { DataFlow } from './compositions/DataFlow';

// Every composition derives its duration from the `beats` prop, so a clip
// is exactly as long as its narration. Width/height/fps are fixed to the
// budget-safe render target (theme.RENDER); do not vary them per clip.
const metadataFromBeats = ({ props }: { props: { beats: { seconds: number }[] } }) => ({
  durationInFrames: Math.max(1, totalFrames(props.beats, RENDER.fps)),
  fps: RENDER.fps,
  width: RENDER.width,
  height: RENDER.height,
});

// Minimal defaultProps so Remotion Studio can open each template; real
// renders always pass inputProps from a storyboard JSON.
export const RemotionRoot: React.FC = () => {
  return (
    <>
      <Composition
        id="step-sequence"
        component={StepSequence}
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        calculateMetadata={metadataFromBeats as any}
        defaultProps={{
          title: 'Step sequence',
          beats: [
            { label: 'First', detail: 'A step appears.', narration: 'A step appears.', seconds: 3 },
            { label: 'Second', detail: 'The next lights up.', narration: 'The next lights up.', seconds: 3 },
          ],
        }}
      />
      <Composition
        id="state-machine"
        component={StateMachine}
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        calculateMetadata={metadataFromBeats as any}
        defaultProps={{
          title: 'State machine',
          nodes: [
            { id: 'a', label: 'Idle' },
            { id: 'b', label: 'Open' },
            { id: 'c', label: 'Closed' },
          ],
          beats: [
            { to: 'a', narration: 'It starts idle.', seconds: 3 },
            { to: 'b', narration: 'Then it opens.', seconds: 3 },
            { to: 'c', narration: 'Then it closes.', seconds: 3 },
          ],
        }}
      />
      <Composition
        id="data-flow"
        component={DataFlow}
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        calculateMetadata={metadataFromBeats as any}
        defaultProps={{
          title: 'Data flow',
          payload: 'GET /x',
          stages: [{ label: 'Client' }, { label: 'Server' }, { label: 'Store' }],
          beats: [
            { note: 'Request leaves the client.', narration: 'The request leaves the client.', seconds: 3 },
            { note: 'Server reads the store.', narration: 'The server reads the store.', seconds: 3 },
          ],
        }}
      />
    </>
  );
};
