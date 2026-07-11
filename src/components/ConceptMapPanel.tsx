import { useEffect, useMemo, useState } from "preact/hooks";
import * as api from "../lib/api";
import { appStore } from "../store/appStore";

export function ConceptMapPanel({ projectId, onClose }: { projectId: string; onClose: () => void }) {
  const [map, setMap] = useState<api.ConceptMap | null>(null);
  const [query, setQuery] = useState("");
  const [kind, setKind] = useState<"all" | api.ConceptMention["kind"]>("all");
  const [view, setView] = useState<"map" | "list">("map");
  const [error, setError] = useState<string | null>(null);
  const [mergeSource, setMergeSource] = useState<string | null>(null);
  const [editMention, setEditMention] = useState<string | null>(null);
  const [distinctName, setDistinctName] = useState("");
  const [linkFrom, setLinkFrom] = useState("");
  const [linkTo, setLinkTo] = useState("");
  const [linkLabel, setLinkLabel] = useState("");

  const refresh = () => api.getConceptMap(projectId).then(setMap).catch((cause) => setError(String(cause)));
  useEffect(() => { void refresh(); }, [projectId]);
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => { if (event.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  const visible = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    return (map?.concepts ?? []).map((concept) => {
      const conceptMatches = normalized === "" || concept.name.toLowerCase().includes(normalized);
      const mentions = concept.mentions.filter((mention) =>
        (kind === "all" || mention.kind === kind) &&
        (conceptMatches || `${mention.label} ${mention.thread_title}`.toLowerCase().includes(normalized)),
      );
      return { ...concept, mentions };
    }).filter((concept) => concept.mentions.length > 0);
  }, [map, query, kind]);
  const names = new Map((map?.concepts ?? []).map((concept) => [concept.id, concept.name]));

  async function mutate(action: () => Promise<unknown>) {
    setError(null);
    try { await action(); await refresh(); }
    catch (cause) { setError(String(cause)); }
  }

  return (
    <div class="fixed inset-0 z-[80] flex items-center justify-center bg-black/30 p-3" role="presentation" onMouseDown={(event) => { if (event.currentTarget === event.target) onClose(); }}>
      <section class="cfy-card flex max-h-[92vh] w-full max-w-5xl flex-col overflow-hidden bg-paper shadow-2xl" role="dialog" aria-modal="true" aria-label="Project concept map">
        <header class="flex flex-wrap items-center gap-2 border-b border-line p-3">
          <div class="min-w-48 flex-1">
            <p class="cfy-label">Project concept map</p>
            <p class="mt-0.5 text-[10px] text-muted">Explicit concepts linked to their sections, visuals, questions, and pinned relationships.</p>
          </div>
          <div class="flex rounded-ctl bg-well p-0.5" role="group" aria-label="Concept map view">
            {(["map", "list"] as const).map((value) => <button type="button" key={value} aria-pressed={view === value} onClick={() => setView(value)} class={`cfy-btn h-7 px-2 text-[10px] ${view === value ? "cfy-btn-accent" : "cfy-btn-ghost"}`}>{value === "map" ? "Map" : "Evidence list"}</button>)}
          </div>
          <button type="button" onClick={onClose} class="cfy-btn cfy-btn-secondary">Close</button>
        </header>
        <div class="flex flex-wrap gap-2 border-b border-line bg-well/35 p-3">
          <input type="search" value={query} onInput={(event) => setQuery(event.currentTarget.value)} placeholder="Filter concepts or evidence…" aria-label="Filter concept map" class="cfy-input min-w-56 flex-1" />
          <select value={kind} onChange={(event) => setKind(event.currentTarget.value as typeof kind)} class="cfy-input w-auto" aria-label="Filter evidence type">
            <option value="all">All evidence</option><option value="section">Sections</option><option value="visual">Visuals</option><option value="question">Questions</option>
          </select>
        </div>
        <div class="min-h-0 flex-1 overflow-y-auto p-3">
          {error != null && <p class="mb-2 rounded-ctl bg-danger-bg p-2 text-xs text-danger">{error}</p>}
          {map == null ? <p class="p-6 text-center text-sm text-muted">Building the explicit map…</p> : visible.length === 0 ? <p class="p-6 text-center text-sm text-muted">No tagged concepts match. New artifacts add explicit concept metadata incrementally.</p> : view === "list" ? (
            <table class="w-full border-collapse text-left text-[11px]">
              <thead><tr class="border-b border-line text-muted"><th class="p-2">Concept</th><th class="p-2">Evidence</th><th class="p-2">Type</th><th class="p-2">Source</th></tr></thead>
              <tbody>{visible.flatMap((concept) => concept.mentions.map((mention) => (
                <tr key={mention.id} class="border-b border-line/70">
                  <th scope="row" class="p-2 font-semibold text-ink">{concept.name}</th><td class="p-2 text-muted">{mention.label || mention.cfy_id}</td><td class="p-2"><span class="cfy-chip bg-well text-muted">{mention.kind}</span></td>
                  <td class="p-2"><button type="button" onClick={() => { appStore.openConceptEvidence(mention.thread_id, mention.cfy_id); onClose(); }} class="text-accent-ink hover:underline">{mention.thread_title} · v{mention.artifact_version}</button></td>
                </tr>
              )))}</tbody>
            </table>
          ) : (
            <div class="grid gap-3 md:grid-cols-2 xl:grid-cols-3" aria-label="Concept nodes">
              {visible.map((concept) => {
                const outgoing = (map.links ?? []).filter((link) => link.from_concept_id === concept.id || link.to_concept_id === concept.id);
                return <article key={concept.id} class="rounded-ctl border border-line bg-paper p-3 shadow-sm">
                  <div class="flex items-start justify-between gap-2"><h2 class="font-serif text-base font-semibold text-ink">{concept.name}</h2><span class="cfy-chip bg-well text-muted">{concept.mentions.length}</span></div>
                  {outgoing.length > 0 && <ul class="mt-2 space-y-1" aria-label={`Relationships for ${concept.name}`}>{outgoing.map((link) => { const other = link.from_concept_id === concept.id ? link.to_concept_id : link.from_concept_id; return <li key={link.id} class="flex items-center gap-1 text-[9px] text-muted"><span>{link.from_concept_id === concept.id ? "→" : "←"} {link.label} {names.get(other) ?? "Unknown"}</span><button type="button" onClick={() => void mutate(() => api.removeConceptLink(link.id))} aria-label={`Remove ${link.label} relationship`} class="text-danger">×</button></li>; })}</ul>}
                  <ul class="mt-2 space-y-1.5">{concept.mentions.slice(0, 6).map((mention) => <li key={mention.id} class="rounded bg-well/55 p-2">
                    <button type="button" onClick={() => { appStore.openConceptEvidence(mention.thread_id, mention.cfy_id); onClose(); }} class="w-full text-left"><span class="cfy-chip bg-paper text-muted">{mention.kind}</span><span class="mt-1 block line-clamp-2 text-[10px] text-ink">{mention.label || mention.cfy_id}</span><span class="mt-0.5 block text-[9px] text-muted">{mention.thread_title} · v{mention.artifact_version}</span></button>
                    {editMention === mention.id ? <div class="mt-1 flex gap-1"><input value={distinctName} onInput={(event) => setDistinctName(event.currentTarget.value)} aria-label="Distinct concept name" class="cfy-input h-7 text-[10px]" /><button type="button" onClick={() => void mutate(() => api.distinguishConcept(mention.id, distinctName)).then(() => { setEditMention(null); setDistinctName(""); })} class="cfy-btn cfy-btn-primary h-7 px-1.5 text-[9px]">Save</button></div> : <button type="button" onClick={() => { setEditMention(mention.id); setDistinctName(concept.name); }} class="mt-1 text-[9px] text-muted hover:text-ink">Distinguish this mention</button>}
                  </li>)}</ul>
                  <div class="mt-2 border-t border-line pt-2">{mergeSource == null ? <button type="button" onClick={() => setMergeSource(concept.id)} class="cfy-btn cfy-btn-ghost h-6 px-1.5 text-[9px]">Merge duplicate…</button> : mergeSource === concept.id ? <button type="button" onClick={() => setMergeSource(null)} class="cfy-btn cfy-btn-ghost h-6 px-1.5 text-[9px]">Cancel merge</button> : <button type="button" onClick={() => void mutate(() => api.mergeConcepts(mergeSource, concept.id)).then(() => setMergeSource(null))} class="cfy-btn cfy-btn-secondary h-6 px-1.5 text-[9px]">Merge into {concept.name}</button>}</div>
                </article>;
              })}
            </div>
          )}
          {map?.truncated && <p class="mt-3 rounded-ctl bg-warn-bg p-2 text-[10px] text-warn">This large map is bounded to 500 concepts / 2,000 evidence links. Narrow the filter or use search for the rest.</p>}
        </div>
        {map != null && map.concepts.length > 1 && <footer class="border-t border-line bg-well/35 p-3"><p class="cfy-label mb-1">Pin a relationship</p><div class="flex flex-wrap gap-1.5"><select value={linkFrom} onChange={(event) => setLinkFrom(event.currentTarget.value)} class="cfy-input w-auto"><option value="">From concept…</option>{map.concepts.map((concept) => <option value={concept.id}>{concept.name}</option>)}</select><input value={linkLabel} onInput={(event) => setLinkLabel(event.currentTarget.value)} placeholder="relationship" aria-label="Relationship label" class="cfy-input w-40" /><select value={linkTo} onChange={(event) => setLinkTo(event.currentTarget.value)} class="cfy-input w-auto"><option value="">To concept…</option>{map.concepts.map((concept) => <option value={concept.id}>{concept.name}</option>)}</select><button type="button" disabled={!linkFrom || !linkTo || !linkLabel.trim()} onClick={() => void mutate(() => api.pinConceptLink(projectId, linkFrom, linkTo, linkLabel)).then(() => { setLinkFrom(""); setLinkTo(""); setLinkLabel(""); })} class="cfy-btn cfy-btn-primary">Pin link</button></div></footer>}
      </section>
    </div>
  );
}
