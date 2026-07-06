// In-app question composer (PRD §7.5, UC5 — FR-5.1). Lives at the top of the
// thread list: an optional title + a required question. Submitting invokes the
// `ask_from_app` flow (create thread → spawn a headless `ask` generation run)
// via `appStore.askFromApp`, which then navigates to the new thread so its live
// generation progress (FR-5.2) shows in the thread view. On success the composer
// closes; on failure it shows the (user-facing) message inline and stays open.

import { useState } from "preact/hooks";
import { appStore } from "../store/appStore";
import { ModelOverridePicker, runOverrideOf } from "./ModelOverridePicker";

export function NewThreadComposer({ onClose }: { onClose: () => void }) {
  const [title, setTitle] = useState("");
  const [question, setQuestion] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Per-invocation model override (bead e7m.4): null = the settings default for
  // the in-app-ask purpose. Sent as run_override only when it differs.
  const [modelOverride, setModelOverride] = useState<string | null>(null);

  const canSubmit = question.trim().length > 0 && !submitting;

  async function onSubmit(e: Event) {
    e.preventDefault();
    if (!canSubmit) return;
    setSubmitting(true);
    setError(null);
    try {
      await appStore.askFromApp(
        title.trim() === "" ? null : title.trim(),
        question.trim(),
        runOverrideOf(modelOverride),
      );
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
      onKeyDown={(e) => {
        // Escape backs out of the composer (unless a submit is in flight).
        if (e.key === "Escape" && !submitting) {
          e.stopPropagation();
          onClose();
        }
      }}
      class="cfy-card flex flex-col gap-2 p-2.5"
    >
      <input
        type="text"
        value={title}
        onInput={(e) => setTitle((e.currentTarget as HTMLInputElement).value)}
        placeholder="Title (optional)"
        disabled={submitting}
        class="cfy-input"
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
        class="cfy-input resize-y"
      />
      {error != null && (
        <p class="break-words text-xs text-danger">{error}</p>
      )}
      <div class="flex items-center justify-between gap-2">
        <ModelOverridePicker
          purpose="inAppAsk"
          value={modelOverride}
          onChange={setModelOverride}
          disabled={submitting}
          menuAlign="left"
          ariaLabel="Model for this question"
        />
        <div class="flex shrink-0 items-center gap-1.5">
          <button
            type="button"
            onClick={onClose}
            disabled={submitting}
            class="cfy-btn cfy-btn-ghost"
          >
            Cancel
          </button>
          <button type="submit" disabled={!canSubmit} class="cfy-btn cfy-btn-primary px-3">
            {submitting ? "Starting…" : "Ask"}
          </button>
        </div>
      </div>
    </form>
  );
}
