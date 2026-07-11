import { useEffect, useMemo, useState } from "preact/hooks";
import type { Thread } from "../lib/api";
import * as api from "../lib/api";
import { appStore } from "../store/appStore";

export function ThreadComparePanel({ projectId, threads, onClose }: { projectId: string; threads: Thread[]; onClose: () => void }) {
  const ready = threads.filter((thread) => thread.status === "ready");
  const [chosen, setChosen] = useState<string[]>(ready.slice(0, 2).map((thread) => thread.id));
  const [comparison, setComparison] = useState<api.ThreadComparison | null>(null);
  const [selected, setSelected] = useState<Record<string, string[]>>({});
  const [instruction, setInstruction] = useState("Reconcile the strongest parts into one coherent explanation. Call out genuine disagreements instead of smoothing them over.");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => { if (event.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  async function compare() {
    setBusy(true); setError(null);
    try {
      const result = await api.compareThreads(projectId, chosen);
      setComparison(result);
      setSelected(Object.fromEntries(result.threads.map((thread) => {
        const preferred = thread.sections.filter((section) => section.role !== "explanation").map((section) => section.cfy_id);
        return [thread.thread_id, (preferred.length > 0 ? preferred : thread.sections.slice(0, 2).map((section) => section.cfy_id))];
      })));
    } catch (cause) { setError(String(cause)); }
    finally { setBusy(false); }
  }

  const sourcePayload = useMemo(() => comparison?.threads.map((thread) => ({ thread_id: thread.thread_id, cfy_ids: selected[thread.thread_id] ?? [] })) ?? [], [comparison, selected]);
  const canSynthesize = sourcePayload.length >= 2 && sourcePayload.every((source) => source.cfy_ids.length > 0);
  const differingProfile = new Set<"depth" | "language" | "visuals" | "shape">();
  if (comparison != null) {
    for (const dimension of ["depth", "language", "visuals", "shape"] as const) {
      if (new Set(comparison.threads.map((thread) => thread.profile?.[dimension] ?? "unknown")).size > 1) differingProfile.add(dimension);
    }
  }

  async function synthesize() {
    if (comparison == null || !canSynthesize || busy) return;
    setBusy(true); setError(null);
    try {
      const excerpts = comparison.threads.map((thread) => {
        const ids = new Set(selected[thread.thread_id] ?? []);
        const sections = thread.sections.filter((section) => ids.has(section.cfy_id)).slice(0, 8);
        return `Source thread: ${thread.title} (${thread.thread_id}, artifact v${thread.artifact_version})\nOriginal question: ${thread.question}\nProfile: ${thread.profile == null ? "unknown" : `${thread.profile.depth}/${thread.profile.language}/${thread.profile.visuals}/${thread.profile.shape}`}\n${sections.map((section) => `- [${section.cfy_id}] ${section.label} (${section.role}): ${section.excerpt}`).join("\n")}`;
      }).join("\n\n");
      const question = `Create a new synthesis from the selected semantic sections below. ${instruction.trim()}\n\n${excerpts}`;
      const preferences = await api.getResponsePreferences(projectId).catch(() => null);
      const started = await api.askFromApp(projectId, `Synthesis: ${comparison.threads.map((thread) => thread.title).join(" + ")}`.slice(0, 120), question, null, preferences?.intent, "auto", []);
      await api.recordThreadSynthesis(projectId, started.thread_id, sourcePayload, instruction);
      await appStore.refetchProjects();
      await appStore.refetchThreads(projectId);
      appStore.selectThread(started.thread_id);
      onClose();
    } catch (cause) { setError(String(cause)); }
    finally { setBusy(false); }
  }

  function toggleThread(id: string) {
    setComparison(null);
    setChosen((current) => current.includes(id) ? current.filter((value) => value !== id) : current.length < 4 ? [...current, id] : current);
  }
  function toggleSection(threadId: string, cfyId: string) {
    setSelected((current) => {
      const values = new Set(current[threadId] ?? []);
      if (values.has(cfyId)) values.delete(cfyId); else values.add(cfyId);
      return { ...current, [threadId]: [...values] };
    });
  }

  return <div class="fixed inset-0 z-[80] flex items-center justify-center bg-black/30 p-3" role="presentation" onMouseDown={(event) => { if (event.currentTarget === event.target) onClose(); }}>
    <section role="dialog" aria-modal="true" aria-label="Compare explanations" class="cfy-card flex max-h-[94vh] w-full max-w-6xl flex-col overflow-hidden bg-paper shadow-2xl">
      <header class="flex items-center gap-3 border-b border-line p-3"><div class="flex-1"><p class="cfy-label">Compare explanations</p><p class="mt-0.5 text-[10px] text-muted">Semantic sections, source questions, and stored profiles—not raw HTML diffs.</p></div><button type="button" onClick={onClose} class="cfy-btn cfy-btn-secondary">Close</button></header>
      <div class="border-b border-line bg-well/35 p-3"><p class="cfy-label mb-1.5">Choose 2–4 ready threads</p><div class="flex max-h-28 flex-wrap gap-1.5 overflow-y-auto">{ready.map((thread) => <label key={thread.id} class={`cursor-pointer rounded-ctl border px-2 py-1.5 text-[10px] ${chosen.includes(thread.id) ? "border-accent/50 bg-accent-bg text-ink" : "border-line bg-paper text-muted"}`}><input type="checkbox" checked={chosen.includes(thread.id)} onChange={() => toggleThread(thread.id)} class="mr-1.5 accent-current" />{thread.title}</label>)}</div><div class="mt-2 flex justify-end"><button type="button" onClick={() => void compare()} disabled={chosen.length < 2 || busy} class="cfy-btn cfy-btn-primary">{busy && comparison == null ? "Comparing…" : "Compare selected"}</button></div></div>
      <div class="min-h-0 flex-1 overflow-auto p-3">
        {error != null && <p class="mb-2 rounded-ctl bg-danger-bg p-2 text-xs text-danger">{error}</p>}
        {comparison == null ? <p class="p-8 text-center text-sm text-muted">Choose parallel results to inspect their assumptions, profiles, selected explanations, and conclusions.</p> : <>
          {comparison.warnings.map((warning) => <p class="mb-2 rounded-ctl bg-warn-bg p-2 text-[10px] text-warn">{warning}</p>)}
          <div class="grid min-w-max gap-3" style={{ gridTemplateColumns: `repeat(${comparison.threads.length}, minmax(260px, 1fr))` }}>
            {comparison.threads.map((thread) => <article key={thread.thread_id} class="w-[min(76vw,360px)] rounded-ctl border border-line bg-paper p-3 md:w-auto">
              <h2 class="font-serif text-base font-semibold text-ink">{thread.title}</h2><p class="mt-1 text-[10px] leading-snug text-muted">{thread.question}</p>
              <div class="mt-2 flex flex-wrap gap-1" aria-label="Stored response profile">{thread.profile == null ? <span class="cfy-chip bg-warn-bg text-warn">Profile unavailable</span> : <>{(["depth", "language", "visuals", "shape"] as const).map((dimension) => <span key={dimension} class={`cfy-chip ${differingProfile.has(dimension) ? "bg-warn-bg text-warn" : "bg-well text-muted"}`} title={differingProfile.has(dimension) ? `${dimension} differs across sources` : dimension}>{thread.profile![dimension]}</span>)}</>}</div>
              {thread.concepts.length > 0 && <p class="mt-2 text-[9px] text-muted">Concepts: {thread.concepts.join(" · ")}</p>}
              <fieldset class="mt-3"><legend class="cfy-label mb-1">Sections for synthesis</legend><div class="space-y-1.5">{thread.sections.map((section) => <label key={section.cfy_id} class={`block cursor-pointer rounded-ctl border p-2 ${selected[thread.thread_id]?.includes(section.cfy_id) ? "border-accent/40 bg-accent-bg/35" : "border-line"}`}><span class="flex items-start gap-2"><input type="checkbox" checked={selected[thread.thread_id]?.includes(section.cfy_id)} onChange={() => toggleSection(thread.thread_id, section.cfy_id)} class="mt-0.5 accent-current" /><span><span class="flex items-center gap-1"><strong class="text-[10px] text-ink">{section.label}</strong><span class="cfy-chip bg-well text-muted">{section.role}</span></span><span class="mt-1 line-clamp-4 block text-[9px] leading-snug text-muted">{section.excerpt || "Heading only; no bounded section excerpt was available."}</span></span></span></label>)}</div></fieldset>
            </article>)}
          </div>
        </>}
      </div>
      {comparison != null && <footer class="border-t border-line bg-well/35 p-3"><label class="cfy-label" for="synthesis-instruction">Synthesis instruction</label><textarea id="synthesis-instruction" value={instruction} onInput={(event) => setInstruction(event.currentTarget.value)} rows={2} class="cfy-input mt-1 resize-y" /><div class="mt-2 flex items-center justify-between gap-3"><p class="text-[9px] text-muted">Creates a separately tracked thread. Every original remains unchanged.</p><button type="button" onClick={() => void synthesize()} disabled={!canSynthesize || busy} class="cfy-btn cfy-btn-primary">{busy ? "Starting synthesis…" : "Create synthesis"}</button></div></footer>}
    </section>
  </div>;
}
