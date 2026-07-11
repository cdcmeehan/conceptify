import { useEffect, useState } from "preact/hooks";
import * as api from "../lib/api";
import type { ConflictReview as ConflictReviewData } from "../lib/api";
import { appStore } from "../store/appStore";

export function ConflictReview({ runId }: { runId: string }) {
  const [review, setReview] = useState<ConflictReviewData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState(0);
  const [busy, setBusy] = useState<"rebase" | "separate" | "reject" | "undo" | null>(null);
  const [confirmSeparate, setConfirmSeparate] = useState(false);
  const [appliedVersion, setAppliedVersion] = useState<number | null>(null);

  useEffect(() => {
    let alive = true;
    api.getConflictReview(runId)
      .then((value) => { if (alive) setReview(value); })
      .catch((reason) => { if (alive) setError(String(reason)); });
    return () => { alive = false; };
  }, [runId]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && busy == null) appStore.closeConflictReview();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [busy]);

  async function rebase() {
    setBusy("rebase");
    setError(null);
    try {
      await api.rebaseConflict(runId);
      appStore.closeConflictReview();
      await appStore.refetchRunActivity();
    } catch (reason) {
      setError(String(reason));
      setBusy(null);
    }
  }

  async function publishSeparate() {
    if (review?.kind !== "revision" && !confirmSeparate) {
      setConfirmSeparate(true);
      return;
    }
    setBusy("separate");
    setError(null);
    try {
      const version = await api.publishConflictCandidate(runId);
      if (review?.kind === "revision") setAppliedVersion(version);
      else appStore.closeConflictReview();
      await appStore.refetchRunActivity();
      setBusy(null);
    } catch (reason) {
      setError(String(reason));
      setBusy(null);
    }
  }

  async function reject() {
    setBusy("reject");
    setError(null);
    try {
      await api.rejectConflictCandidate(runId);
      appStore.closeConflictReview();
      await appStore.refetchRunActivity();
    } catch (reason) {
      setError(String(reason));
      setBusy(null);
    }
  }

  async function undo() {
    if (review == null) return;
    setBusy("undo");
    setError(null);
    try {
      await api.restoreArtifactVersion(review.thread_id, review.current_version, runId);
      appStore.closeConflictReview();
    } catch (reason) {
      setError(String(reason));
      setBusy(null);
    }
  }

  const change = review?.diff.changes[selected] ?? null;
  const spillover = review?.kind === "revision"
    ? review.diff.changes.filter((item) => item.cfy_id == null || !review.target_cfy_ids.includes(item.cfy_id)).length
    : 0;
  const revision = review?.kind === "revision";

  return (
    <div class="absolute inset-0 z-50 flex items-center justify-center bg-black/55 p-5" role="dialog" aria-modal="true" aria-label={revision ? "Review targeted revision" : "Review artifact conflict"}>
      <section class="flex max-h-[90vh] w-full max-w-5xl flex-col overflow-hidden rounded-card border border-line bg-paper shadow-2xl">
        <header class="flex items-start gap-3 border-b border-line px-4 py-3">
          <div class="min-w-0 flex-1">
            <p class={`cfy-label ${revision ? "text-accent" : "text-danger"}`}>{revision ? "Revision preview" : "Conflict review"}</p>
            <h1 class="mt-0.5 truncate font-serif text-lg font-semibold text-ink">
              {review?.thread_title ?? "Loading candidate…"}
            </h1>
            {review != null && (
              <p class="mt-1 text-xs text-muted">
                {revision ? `Proposed from v${review.base_version ?? review.current_version}` : `Based on v${review.base_version ?? "none"}; v${review.current_version} is now current`} · {review.model} via {review.route ?? review.agent}
              </p>
            )}
          </div>
          <button type="button" onClick={() => appStore.closeConflictReview()} disabled={busy != null} class="cfy-btn cfy-btn-ghost h-8 w-8 p-0 text-lg" aria-label="Close conflict review">×</button>
        </header>

        {error != null && <p class="mx-4 mt-3 rounded-ctl bg-danger-bg px-3 py-2 text-xs text-danger">{error}</p>}
        {review == null ? (
          <div class="flex h-64 items-center justify-center text-sm text-muted">Loading retained candidate…</div>
        ) : (
          <div class="flex min-h-0 flex-1">
            <ol class="w-56 shrink-0 overflow-y-auto border-r border-line p-2" aria-label="Candidate changes">
              {review.diff.changes.length === 0 ? (
                <li class="px-2 py-4 text-xs text-muted">No semantic changes to recover.</li>
              ) : review.diff.changes.map((item, index) => (
                <li key={`${item.cfy_id ?? "document"}-${index}`}>
                  <button type="button" onClick={() => setSelected(index)} aria-current={selected === index ? "true" : undefined} class={`mb-1 w-full rounded-ctl px-2 py-2 text-left ${selected === index ? "bg-selected" : "hover:bg-hover"}`}>
                    <span class="block truncate text-xs font-medium text-ink">{item.cfy_id ?? "Document fallback"}</span>
                    <span class="mt-0.5 block text-[10px] uppercase tracking-wide text-muted">{item.moved ? `${item.kind} · moved` : item.kind}</span>
                  </button>
                </li>
              ))}
            </ol>
            <div class="min-w-0 flex-1 overflow-y-auto p-4">
              <div class="mb-3 rounded-ctl border border-warn/30 bg-warn-bg/45 px-3 py-2 text-xs leading-relaxed text-warn">
                {revision ? "Nothing is applied automatically. Review every affected region, then apply or reject this proposal." : "Nothing is applied automatically. Compare the retained proposal with the current artifact, then choose an explicit recovery path."}
              </div>
              {change != null && (
                <div class="grid min-h-52 grid-cols-2 overflow-hidden rounded-ctl border border-line">
                  <ComparePane label={`Current · v${review.current_version}`} text={change.old_text} tone="current" />
                  <ComparePane label="Retained candidate" text={change.new_text} tone="candidate" />
                </div>
              )}
              {review.diff.degraded && <p class="mt-2 text-[11px] text-warn">Some content lacked stable ids; this comparison includes a document-level text fallback.</p>}
              {revision && (
                <p class={`mt-2 text-[11px] ${spillover > 0 ? "text-warn" : "text-ok"}`}>
                  {spillover > 0
                    ? `${spillover} changed region${spillover === 1 ? " is" : "s are"} outside the selected target. Review that spillover before applying.`
                    : "All identified changes stay within the selected target."}
                </p>
              )}
            </div>
          </div>
        )}

        <footer class="flex flex-wrap items-center gap-2 border-t border-line px-4 py-3">
          {appliedVersion != null ? (
            <>
              <span class="text-xs font-medium text-ok">Applied as v{appliedVersion}.</span>
              <button type="button" onClick={() => void undo()} disabled={busy != null} class="cfy-btn cfy-btn-secondary px-3 py-2 text-sm">{busy === "undo" ? "Restoring…" : `Undo to v${review?.current_version}`}</button>
              <button type="button" onClick={() => appStore.closeConflictReview()} disabled={busy != null} class="cfy-btn cfy-btn-primary ml-auto px-3 py-2 text-sm">Done</button>
            </>
          ) : revision ? (
            <>
              <button type="button" onClick={() => void publishSeparate()} disabled={review == null || busy != null} class="cfy-btn cfy-btn-primary px-3 py-2 text-sm">{busy === "separate" ? "Applying…" : "Apply as new version"}</button>
              <button type="button" onClick={() => void reject()} disabled={review == null || busy != null} class="cfy-btn cfy-btn-secondary px-3 py-2 text-sm">{busy === "reject" ? "Rejecting…" : "Reject proposal"}</button>
              <button type="button" onClick={() => appStore.closeConflictReview()} disabled={busy != null} class="cfy-btn cfy-btn-ghost ml-auto px-3 py-2 text-sm">Review later</button>
            </>
          ) : (
            <>
              <button type="button" onClick={() => void rebase()} disabled={review == null || busy != null} class="cfy-btn cfy-btn-primary px-3 py-2 text-sm">{busy === "rebase" ? "Starting synthesis…" : "Synthesize onto current"}</button>
              <button type="button" onClick={() => void publishSeparate()} disabled={review == null || busy != null} class={`cfy-btn px-3 py-2 text-sm ${confirmSeparate ? "cfy-btn-danger" : "cfy-btn-secondary"}`}>{busy === "separate" ? "Publishing…" : confirmSeparate ? "Confirm separate version" : "Publish candidate separately"}</button>
              {confirmSeparate && <span class="text-[11px] text-danger">Creates a new version exactly from this candidate; current content absent from it stays absent.</span>}
              <button type="button" onClick={() => appStore.closeConflictReview()} disabled={busy != null} class="cfy-btn cfy-btn-ghost ml-auto px-3 py-2 text-sm">Keep for later</button>
            </>
          )}
        </footer>
      </section>
    </div>
  );
}

function ComparePane({ label, text, tone }: { label: string; text: string | null; tone: "current" | "candidate" }) {
  return (
    <section class={`${tone === "current" ? "border-r border-line" : ""}`}>
      <h2 class="border-b border-line bg-well px-3 py-2 text-xs font-semibold text-ink">{label}</h2>
      <div class={`whitespace-pre-wrap break-words px-3 py-3 font-mono text-xs leading-relaxed ${text == null ? "text-muted italic" : tone === "candidate" ? "text-ok" : "text-ink"}`}>
        {text ?? (tone === "current" ? "Not present in the current artifact." : "Removed by the candidate.")}
      </div>
    </section>
  );
}
