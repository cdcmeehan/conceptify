// Direct follow-up composer (PRD FR-4.3, §7.4; bead conceptify-94m.5).
//
// A free-text question box at the bottom of the comments sidebar. It is
// deliberately NOT a chat box: submitting creates a Comment with a *null*
// anchor (status `open`) that queues for the next follow-up run and flows
// through the exact same sidebar/resolution machinery as anchored comments
// (94m.3/94m.4/94m.6). It reuses `api.createComment` + `appStore.addComment`,
// so the new comment appears in the list live with no bespoke plumbing.
//
// Guard (composite FK): a comment always anchors to a saved artifact version.
// While the thread is still generating and has no version, `viewerVersion` is
// null → the box is disabled with a hint. It enables the moment the first
// artifact version lands (the store updates live via `artifact-updated`).
//
// Submit on Cmd/Ctrl+Enter (consistent with the selection popover, 94m.3);
// plain Enter inserts a newline (this is a multi-line question box). Empty /
// whitespace-only submissions are rejected.

import { useRef, useState } from "preact/hooks";
import * as api from "../lib/api";
import { appStore } from "../store/appStore";

interface Props {
  threadId: string;
  /** Concrete artifact version to attach the follow-up to (the one in the
   *  viewer), or `null` when the thread has no artifact yet → composer disabled. */
  viewerVersion: number | null;
}

export function CommentComposer({ threadId, viewerVersion }: Props) {
  const [body, setBody] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const disabled = viewerVersion == null;
  const canSubmit = !disabled && !saving && body.trim().length > 0;

  function submit() {
    if (viewerVersion == null || saving) return;
    const trimmed = body.trim();
    if (trimmed.length === 0) return;

    setSaving(true);
    setError(null);
    api
      .createComment({ threadId, artifactVersion: viewerVersion, anchor: null, body: trimmed })
      .then((comment) => {
        // Same path as the popover: append optimistically + reconcile. The row
        // shows up in the list above with no anchor excerpt ("Direct question").
        appStore.addComment(comment);
        setBody("");
        setSaving(false);
        textareaRef.current?.focus();
      })
      .catch((e) => {
        setError(String(e));
        setSaving(false);
      });
  }

  return (
    <div class="shrink-0 border-t border-line bg-paper p-2.5">
      <label class="sr-only" for="follow-up-composer">
        Ask a direct follow-up
      </label>
      <textarea
        id="follow-up-composer"
        ref={textareaRef}
        value={body}
        rows={3}
        disabled={disabled || saving}
        placeholder={disabled ? "Waiting for an artifact…" : "Ask a follow-up question…"}
        onInput={(e) => setBody((e.target as HTMLTextAreaElement).value)}
        onKeyDown={(e) => {
          // Cmd/Ctrl+Enter submits (matches the selection popover); plain Enter
          // is a newline (multi-line question box).
          if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
            e.preventDefault();
            submit();
          }
        }}
        class="cfy-input resize-none"
      />
      {error != null && (
        <p class="mt-1 text-[11px] text-danger">{error}</p>
      )}
      <div class="mt-1.5 flex items-center justify-between gap-2">
        <span class="text-[11px] text-muted">
          {disabled ? "Available once the artifact is ready" : "⌘⏎ to ask"}
        </span>
        <button
          type="button"
          onClick={submit}
          disabled={!canSubmit}
          title={disabled ? "The thread has no artifact version to attach to yet" : undefined}
          class="cfy-btn cfy-btn-primary px-3 py-1"
        >
          {saving ? "Asking…" : "Ask"}
        </button>
      </div>
    </div>
  );
}
