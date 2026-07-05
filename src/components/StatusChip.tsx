// Status chip for the four thread states (FR-2.2): generating / ready /
// updating / error. In-progress states (generating, updating) get a pulsing dot.

import type { ThreadStatus } from "../lib/api";

interface StatusMeta {
  label: string;
  chip: string;
  dot: string;
  pulse: boolean;
}

const STATUS_META: Record<ThreadStatus, StatusMeta> = {
  generating: {
    label: "Generating",
    chip: "bg-amber-100 text-amber-800 dark:bg-amber-500/15 dark:text-amber-300",
    dot: "bg-amber-500",
    pulse: true,
  },
  ready: {
    label: "Ready",
    chip: "bg-emerald-100 text-emerald-800 dark:bg-emerald-500/15 dark:text-emerald-300",
    dot: "bg-emerald-500",
    pulse: false,
  },
  updating: {
    label: "Updating",
    chip: "bg-sky-100 text-sky-800 dark:bg-sky-500/15 dark:text-sky-300",
    dot: "bg-sky-500",
    pulse: true,
  },
  error: {
    label: "Error",
    chip: "bg-rose-100 text-rose-800 dark:bg-rose-500/15 dark:text-rose-300",
    dot: "bg-rose-500",
    pulse: false,
  },
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
        class="inline-flex items-center gap-1.5 rounded-full bg-neutral-200 px-2 py-0.5 text-xs font-medium text-neutral-600 dark:bg-neutral-700 dark:text-neutral-300"
        title="Still generating after 30+ minutes — the run may have stalled. You can delete this thread."
      >
        <span class="h-1.5 w-1.5 rounded-full bg-neutral-400" />
        Stalled
      </span>
    );
  }

  const meta = STATUS_META[status] ?? STATUS_META.generating;
  return (
    <span
      class={`inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium ${meta.chip}`}
    >
      <span class={`h-1.5 w-1.5 rounded-full ${meta.dot} ${meta.pulse ? "animate-pulse" : ""}`} />
      {meta.label}
    </span>
  );
}
