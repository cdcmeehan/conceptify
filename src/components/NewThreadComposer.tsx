// Immediate-submit + short multi-question review composer (conceptify-k9z.3).
// Draft/review/submission state lives in appStore keyed by project, so closing
// this panel or navigating between projects/threads never discards text.

import { useEffect } from "preact/hooks";
import type { AskQuestionDraft, AskSubmissionStatus } from "../store/appStore";
import { appStore, useAppStore } from "../store/appStore";
import { ModelOverridePicker } from "./ModelOverridePicker";

const STATUS_LABEL: Record<AskSubmissionStatus, string> = {
  submitting: "Submitting",
  queued: "Queued",
  running: "Running",
  throttled: "Waiting for provider",
  cancelling: "Cancelling",
  completed: "Complete",
  cancelled: "Cancelled",
  failed: "Needs attention",
};

const STATUS_CLASS: Record<AskSubmissionStatus, string> = {
  submitting: "bg-info-bg text-info",
  queued: "bg-well text-muted",
  running: "bg-info-bg text-info",
  throttled: "bg-warn-bg text-warn",
  cancelling: "bg-well text-muted",
  completed: "bg-ok-bg text-ok",
  cancelled: "bg-well text-muted",
  failed: "bg-danger-bg text-danger",
};

export function NewThreadComposer({
  projectId,
  onClose,
}: {
  projectId: string;
  onClose: () => void;
}) {
  const state = useAppStore();
  const workspace = state.askComposerByProject[projectId];

  useEffect(() => appStore.ensureAskWorkspace(projectId), [projectId]);
  if (workspace == null) return null;

  const { draft, staged, submissions, mode } = workspace;
  const launching = submissions.some((item) => item.status === "submitting");
  const canSubmit = draft.question.trim().length > 0;
  const validStaged = staged.filter((item) => item.question.trim().length > 0);

  function submitSingle(e: Event) {
    e.preventDefault();
    if (!canSubmit) return;
    // Store snapshots + clears this exact draft synchronously before the API
    // call, so a second question can be typed immediately and a double submit
    // of the old client id is ignored.
    void appStore.submitAskQuestions(projectId, [{ ...draft }]);
  }

  return (
    <section
      class="cfy-card overflow-hidden"
      onKeyDown={(e) => {
        if (e.key === "Escape" && !launching) {
          e.stopPropagation();
          onClose();
        }
      }}
      aria-label="New questions"
    >
      <div class="flex items-center justify-between border-b border-line px-2.5 py-2">
        <div>
          <p class="font-serif text-sm font-semibold text-ink">Question folio</p>
          <p class="text-[10px] text-muted">Drafts stay here as you move around.</p>
        </div>
        <button
          type="button"
          onClick={onClose}
          title="Close — drafts are kept"
          class="cfy-btn cfy-btn-ghost h-6 w-6 p-0 text-base"
          aria-label="Close question composer"
        >
          ×
        </button>
      </div>

      <div class="flex border-b border-line bg-well/60 p-1" role="tablist" aria-label="Question mode">
        {(["single", "multi"] as const).map((value) => (
          <button
            key={value}
            type="button"
            role="tab"
            aria-selected={mode === value}
            onClick={() => appStore.setAskComposerMode(projectId, value)}
            class={`flex-1 rounded px-2 py-1 text-[11px] font-medium transition-colors ${
              mode === value ? "bg-paper text-ink shadow-sm" : "text-muted hover:text-ink"
            }`}
          >
            {value === "single" ? "One question" : `Short list${staged.length > 0 ? ` · ${staged.length}` : ""}`}
          </button>
        ))}
      </div>

      <form onSubmit={submitSingle} class="flex flex-col gap-2 p-2.5">
        <input
          type="text"
          value={draft.title}
          onInput={(e) =>
            appStore.updateAskDraft(projectId, {
              title: (e.currentTarget as HTMLInputElement).value,
            })
          }
          placeholder="Title (optional)"
          class="cfy-input"
        />
        <textarea
          value={draft.question}
          onInput={(e) =>
            appStore.updateAskDraft(projectId, {
              question: (e.currentTarget as HTMLTextAreaElement).value,
            })
          }
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
              if (mode === "single") submitSingle(e);
              else appStore.stageAskDraft(projectId);
            }
          }}
          placeholder={mode === "single" ? "Ask a question about this project…" : "Write the next question…"}
          rows={3}
          autoFocus
          class="cfy-input resize-y"
        />
        <div class="flex items-center justify-between gap-2">
          <ModelOverridePicker
            purpose="inAppAsk"
            value={draft.modelOverride}
            onChange={(modelOverride) => appStore.updateAskDraft(projectId, { modelOverride })}
            menuAlign="left"
            ariaLabel="Model for this question"
          />
          {mode === "single" ? (
            <button type="submit" disabled={!canSubmit} class="cfy-btn cfy-btn-primary px-3">
              Ask
            </button>
          ) : (
            <button
              type="button"
              disabled={!canSubmit}
              onClick={() => appStore.stageAskDraft(projectId)}
              class="cfy-btn cfy-btn-secondary px-2.5"
            >
              Add to list
            </button>
          )}
        </div>
      </form>

      {mode === "multi" && (
        <div class="border-t border-line bg-well/35 px-2.5 py-2.5">
          <div class="mb-2 flex items-center justify-between gap-2">
            <div>
              <p class="cfy-label">Review before launch</p>
              <p class="text-[10px] text-muted">Keep it short; every item becomes its own thread.</p>
            </div>
            <button
              type="button"
              disabled={validStaged.length === 0}
              onClick={() => void appStore.submitAskQuestions(projectId, validStaged)}
              class="cfy-btn cfy-btn-primary shrink-0 px-2.5"
            >
              Launch {validStaged.length || "all"}
            </button>
          </div>
          {staged.length === 0 ? (
            <p class="rounded-ctl border border-dashed border-line px-2 py-3 text-center text-[11px] text-muted">
              Add two or three questions, then launch them together.
            </p>
          ) : (
            <ol class="flex max-h-72 flex-col gap-2 overflow-y-auto">
              {staged.map((item, index) => (
                <QuestionSlip
                  key={item.id}
                  index={index}
                  item={item}
                  onChange={(patch) => appStore.updateStagedAskDraft(projectId, item.id, patch)}
                  onRemove={() => appStore.removeStagedAskDraft(projectId, item.id)}
                />
              ))}
            </ol>
          )}
        </div>
      )}

      {submissions.length > 0 && (
        <div class="border-t border-line px-2.5 py-2.5">
          <p class="cfy-label mb-1.5">Recently sent</p>
          <ul class="flex max-h-52 flex-col gap-1.5 overflow-y-auto">
            {submissions.map((item) => {
              const cancellable = ["submitting", "queued", "running", "throttled"].includes(item.status);
              return (
                <li key={item.id} class="rounded-ctl border border-line bg-paper px-2 py-1.5">
                  <div class="flex items-start gap-2">
                    <div class="min-w-0 flex-1">
                      <p class="line-clamp-2 text-[11px] leading-snug text-ink">{item.question}</p>
                      {item.error != null && (
                        <p class="mt-1 line-clamp-2 text-[10px] text-danger">{item.error}</p>
                      )}
                    </div>
                    <span class={`cfy-chip shrink-0 ${STATUS_CLASS[item.status]}`}>
                      {STATUS_LABEL[item.status]}
                    </span>
                  </div>
                  <div class="mt-1 flex items-center justify-end gap-2 text-[10px]">
                    {item.threadId != null && (
                      <button
                        type="button"
                        onClick={() => appStore.selectThread(item.threadId!)}
                        class="text-accent-ink hover:underline"
                      >
                        Open thread
                      </button>
                    )}
                    {cancellable && item.runId != null && (
                      <button
                        type="button"
                        onClick={() => void appStore.cancelAskSubmission(projectId, item.id)}
                        class="text-muted hover:text-danger"
                      >
                        Cancel
                      </button>
                    )}
                    {item.status === "failed" && (
                      <button
                        type="button"
                        onClick={() => appStore.restoreAskSubmission(projectId, item.id)}
                        class="text-accent-ink hover:underline"
                      >
                        Edit & retry
                      </button>
                    )}
                  </div>
                </li>
              );
            })}
          </ul>
        </div>
      )}
    </section>
  );
}

function QuestionSlip({
  item,
  index,
  onChange,
  onRemove,
}: {
  item: AskQuestionDraft;
  index: number;
  onChange: (patch: Partial<AskQuestionDraft>) => void;
  onRemove: () => void;
}) {
  return (
    <li class="relative rounded-ctl border border-line bg-paper p-2 pl-7 shadow-sm">
      <span class="absolute left-2 top-2 font-serif text-xs font-semibold text-accent">
        {index + 1}.
      </span>
      <input
        type="text"
        value={item.title}
        onInput={(e) => onChange({ title: (e.currentTarget as HTMLInputElement).value })}
        placeholder="Optional title"
        class="mb-1 w-full bg-transparent text-[11px] font-medium text-ink outline-none placeholder:text-muted/70"
      />
      <textarea
        value={item.question}
        onInput={(e) => onChange({ question: (e.currentTarget as HTMLTextAreaElement).value })}
        rows={2}
        class="w-full resize-y bg-transparent text-[11px] leading-relaxed text-ink outline-none"
        aria-label={`Question ${index + 1}`}
      />
      <div class="mt-1 flex items-center justify-between gap-2">
        <ModelOverridePicker
          purpose="inAppAsk"
          value={item.modelOverride}
          onChange={(modelOverride) => onChange({ modelOverride })}
          menuAlign="left"
          ariaLabel={`Model for question ${index + 1}`}
        />
        <button type="button" onClick={onRemove} class="text-[10px] text-muted hover:text-danger">
          Remove
        </button>
      </div>
    </li>
  );
}
