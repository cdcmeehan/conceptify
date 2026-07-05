// In-app question composer (PRD §7.5, UC5 — FR-5.1). Lives at the top of the
// thread list: an optional title + a required question. Submitting invokes the
// `ask_from_app` flow (create thread → spawn a headless `ask` generation run)
// via `appStore.askFromApp`, which then navigates to the new thread so its live
// generation progress (FR-5.2) shows in the thread view. On success the composer
// closes; on failure it shows the (user-facing) message inline and stays open.

import { useState } from "preact/hooks";
import { appStore } from "../store/appStore";

export function NewThreadComposer({ onClose }: { onClose: () => void }) {
  const [title, setTitle] = useState("");
  const [question, setQuestion] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canSubmit = question.trim().length > 0 && !submitting;

  async function onSubmit(e: Event) {
    e.preventDefault();
    if (!canSubmit) return;
    setSubmitting(true);
    setError(null);
    try {
      await appStore.askFromApp(title.trim() === "" ? null : title.trim(), question.trim());
      onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <form
      onSubmit={onSubmit}
      class="flex flex-col gap-2 rounded-lg border border-neutral-200 bg-neutral-50 p-2.5 dark:border-neutral-800 dark:bg-neutral-900"
    >
      <input
        type="text"
        value={title}
        onInput={(e) => setTitle((e.currentTarget as HTMLInputElement).value)}
        placeholder="Title (optional)"
        disabled={submitting}
        class="rounded-md border border-neutral-300 bg-white px-2 py-1.5 text-sm text-neutral-800 placeholder:text-neutral-400 focus:border-blue-400 focus:outline-none dark:border-neutral-700 dark:bg-neutral-950 dark:text-neutral-100"
      />
      <textarea
        value={question}
        onInput={(e) => setQuestion((e.currentTarget as HTMLTextAreaElement).value)}
        onKeyDown={(e) => {
          // Cmd/Ctrl+Enter submits (the textarea keeps plain Enter for newlines).
          if ((e.metaKey || e.ctrlKey) && e.key === "Enter") void onSubmit(e);
        }}
        placeholder="Ask a question about this project…"
        rows={3}
        autoFocus
        disabled={submitting}
        class="resize-y rounded-md border border-neutral-300 bg-white px-2 py-1.5 text-sm text-neutral-800 placeholder:text-neutral-400 focus:border-blue-400 focus:outline-none dark:border-neutral-700 dark:bg-neutral-950 dark:text-neutral-100"
      />
      {error != null && (
        <p class="break-words text-xs text-rose-600 dark:text-rose-400">{error}</p>
      )}
      <div class="flex items-center justify-end gap-1.5">
        <button
          type="button"
          onClick={onClose}
          disabled={submitting}
          class="rounded-md px-2.5 py-1.5 text-xs font-medium text-neutral-500 transition-colors hover:bg-neutral-200 disabled:opacity-50 dark:text-neutral-400 dark:hover:bg-neutral-800"
        >
          Cancel
        </button>
        <button
          type="submit"
          disabled={!canSubmit}
          class="rounded-md bg-blue-600 px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-blue-700 disabled:cursor-not-allowed disabled:bg-neutral-200 disabled:text-neutral-400 dark:disabled:bg-neutral-800 dark:disabled:text-neutral-600"
        >
          {submitting ? "Starting…" : "Ask"}
        </button>
      </div>
    </form>
  );
}
