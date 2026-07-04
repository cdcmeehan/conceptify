// Thread view. In M1 this is a placeholder that shows the thread's metadata;
// bead conceptify-nsy.4 fills the body with the sandboxed artifact viewer
// (+ version switcher, comments sidebar, follow-up composer) in M2.

import type { Thread } from "../lib/api";
import { StatusChip } from "./StatusChip";

export function ThreadView({ thread }: { thread: Thread | null }) {
  if (thread == null) {
    return (
      <main class="flex h-full flex-1 items-center justify-center bg-neutral-100 dark:bg-neutral-900">
        <p class="text-sm text-neutral-400">Select a thread to view its artifact.</p>
      </main>
    );
  }

  return (
    <main class="flex h-full min-w-0 flex-1 flex-col bg-neutral-100 dark:bg-neutral-900">
      <header class="border-b border-neutral-200 bg-white px-5 py-3 dark:border-neutral-800 dark:bg-neutral-950">
        <div class="flex items-center gap-3">
          <h1 class="min-w-0 truncate text-lg font-semibold text-neutral-900 dark:text-neutral-50">
            {thread.title}
          </h1>
          <StatusChip status={thread.status} />
        </div>
      </header>

      <div class="min-h-0 flex-1 overflow-y-auto p-5">
        <div class="mx-auto max-w-2xl">
          {thread.initial_question.trim().length > 0 && (
            <section class="mb-4 rounded-lg border border-neutral-200 bg-white p-4 dark:border-neutral-800 dark:bg-neutral-950">
              <h2 class="mb-1 text-xs font-semibold uppercase tracking-wide text-neutral-400">
                Question
              </h2>
              <p class="whitespace-pre-wrap text-sm text-neutral-700 dark:text-neutral-300">
                {thread.initial_question}
              </p>
            </section>
          )}

          <section class="rounded-lg border border-dashed border-neutral-300 bg-white/50 p-8 text-center dark:border-neutral-700 dark:bg-neutral-950/40">
            <p class="text-sm font-medium text-neutral-500 dark:text-neutral-400">
              Artifact viewer
            </p>
            <p class="mt-1 text-xs text-neutral-400">
              The sandboxed artifact renderer arrives in M2.
            </p>
          </section>
        </div>
      </div>
    </main>
  );
}
