// Status chip for the four thread states (FR-2.2): generating / ready /
// updating / error. In-progress states (generating, updating) get a pulsing
// dot. Colors come from the shell's status token families (bead
// conceptify-vxc): warn = working, ok = done, info = revising, danger = error.
// The dot rides `currentColor`, so each family needs only one class pair.

import type { ThreadStatus } from "../lib/api";

interface StatusMeta {
  label: string;
  chip: string;
  pulse: boolean;
}

const STATUS_META: Record<ThreadStatus, StatusMeta> = {
  generating: { label: "Generating", chip: "bg-warn-bg text-warn", pulse: true },
  ready: { label: "Ready", chip: "bg-ok-bg text-ok", pulse: false },
  updating: { label: "Updating", chip: "bg-info-bg text-info", pulse: true },
  error: { label: "Error", chip: "bg-danger-bg text-danger", pulse: false },
};

export function StatusChip({
  status,
  stalled = false,
}: {
  status: ThreadStatus;
  /** A `generating` thread with no artifact that has sat idle past the stall
   *  threshold (bead conceptify-0kt, option b-lite). Visual only: a muted
   *  "Stalled" chip with no pulse, hinting the run likely died and the thread
   *  can be deleted. */
  stalled?: boolean;
}) {
  if (stalled) {
    return (
      <span
        class="cfy-chip bg-hover text-muted"
        title="Still generating after 30+ minutes — the run may have stalled. You can delete this thread."
      >
        <span class="h-1.5 w-1.5 shrink-0 rounded-full bg-current opacity-70" />
        Stalled
      </span>
    );
  }

  const meta = STATUS_META[status] ?? STATUS_META.generating;
  return (
    <span class={`cfy-chip ${meta.chip}`}>
      <span
        class={`h-1.5 w-1.5 shrink-0 rounded-full bg-current ${meta.pulse ? "animate-pulse" : ""}`}
      />
      {meta.label}
    </span>
  );
}
