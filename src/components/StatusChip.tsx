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

export function StatusChip({ status }: { status: ThreadStatus }) {
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
