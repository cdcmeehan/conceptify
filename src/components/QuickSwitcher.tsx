import { useEffect, useMemo, useRef, useState } from "preact/hooks";
import * as api from "../lib/api";
import { appStore, useAppStore } from "../store/appStore";

type LocalItem = { key: string; kind: "project" | "thread"; label: string; detail: string; projectId: string; threadId: string | null };
type Item = { key: string; label: string; detail: string; group: string; projectId: string; threadId: string | null; hit?: api.SearchHit };

function fuzzy(value: string, query: string): boolean {
  const haystack = value.toLocaleLowerCase();
  const needle = query.toLocaleLowerCase().trim();
  if (needle === "" || haystack.includes(needle)) return true;
  let cursor = 0;
  for (const ch of haystack) if (ch === needle[cursor]) cursor += 1;
  return cursor === needle.length;
}

function marked(text: string) {
  const parts = text.split(/(<mark>|<\/mark>)/i);
  let active = false;
  return parts.map((part, index) => {
    if (part.toLowerCase() === "<mark>") { active = true; return null; }
    if (part.toLowerCase() === "</mark>") { active = false; return null; }
    return active ? <mark key={index} class="rounded-sm bg-accent-bg px-0.5 text-accent-ink">{part}</mark> : part;
  });
}

export function QuickSwitcher() {
  const state = useAppStore();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [remote, setRemote] = useState<api.SearchResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setOpen((value) => !value);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  useEffect(() => {
    if (open) requestAnimationFrame(() => inputRef.current?.focus());
    else { setQuery(""); setRemote(null); setActive(0); }
  }, [open]);

  useEffect(() => {
    const value = query.trim();
    if (value === "") { setRemote(null); setLoading(false); return; }
    setLoading(true);
    let current = true;
    const timer = window.setTimeout(() => {
      void api.search(value).then((result) => { if (current) setRemote(result); }).catch(() => { if (current) setRemote({ projects: [], threads: [], artifacts: [], comments: [] }); }).finally(() => { if (current) setLoading(false); });
    }, 160);
    return () => { current = false; window.clearTimeout(timer); };
  }, [query]);

  const local = useMemo<LocalItem[]>(() => {
    const projects = state.projects.filter((project) => fuzzy(project.name, query)).map((project) => ({ key: `local-project-${project.id}`, kind: "project" as const, label: project.name, detail: "Project", projectId: project.id, threadId: null }));
    const threads = state.threads.filter((thread) => fuzzy(`${thread.title} ${thread.initial_question}`, query)).map((thread) => ({ key: `local-thread-${thread.id}`, kind: "thread" as const, label: thread.title, detail: query.trim() === "" ? "Recent thread" : "Thread", projectId: thread.project_id, threadId: thread.id }));
    return query.trim() === "" ? [...threads, ...projects] : [...projects, ...threads];
  }, [state.projects, state.threads, query]);

  const items = useMemo<Item[]>(() => {
    const result: Item[] = local.slice(0, query.trim() === "" ? 7 : 8).map((item) => ({ ...item, group: item.kind === "project" ? "Jump to" : "Threads" }));
    if (remote == null) return result;
    const seen = new Set(result.map((item) => `${item.projectId}:${item.threadId ?? ""}`));
    for (const [group, hits] of [["Projects", remote.projects], ["Threads", remote.threads], ["Artifact sections", remote.artifacts], ["Comments & answers", remote.comments]] as const) {
      for (const hit of hits) {
        const identity = `${hit.projectId}:${hit.threadId ?? ""}`;
        if ((hit.kind === "project" || hit.kind === "thread") && seen.has(identity)) continue;
        result.push({ key: `remote-${hit.kind}-${hit.entityId}`, label: hit.title || (hit.kind === "comment" ? "Comment" : "Artifact section"), detail: hit.snippet, group, projectId: hit.projectId, threadId: hit.threadId, hit });
      }
    }
    return result;
  }, [local, remote, query]);

  useEffect(() => setActive((value) => Math.min(value, Math.max(0, items.length - 1))), [items.length]);

  function close(restore = true) {
    setOpen(false);
    if (restore) requestAnimationFrame(() => triggerRef.current?.focus());
  }

  async function choose(item: Item) {
    close(false);
    if (item.hit != null) await appStore.navigateToSearchHit(item.hit);
    else await appStore.navigateTo(item.projectId, item.threadId);
  }

  return <>
    <button ref={triggerRef} type="button" onClick={() => setOpen(true)} class="fixed bottom-3 left-3 z-30 flex items-center gap-2 rounded-ctl border border-line bg-paper/95 px-2.5 py-1.5 text-[11px] font-medium text-muted shadow-sm backdrop-blur hover:border-accent/40 hover:text-ink" aria-keyshortcuts="Meta+K Control+K">
      <span aria-hidden="true">⌕</span> Search <kbd class="rounded border border-line px-1 font-mono text-[9px]">⌘K</kbd>
    </button>
    {open && <div class="fixed inset-0 z-[70] flex items-start justify-center bg-ink/25 px-4 pt-[12vh] backdrop-blur-[2px]" onMouseDown={(event) => { if (event.currentTarget === event.target) close(); }}>
      <section role="dialog" aria-modal="true" aria-label="Search and switch" class="flex max-h-[72vh] w-full max-w-2xl flex-col overflow-hidden rounded-panel border border-line-strong bg-paper shadow-2xl" onKeyDown={(event) => {
        if (event.key === "Escape") { event.preventDefault(); close(); }
        else if (event.key === "ArrowDown") { event.preventDefault(); setActive((value) => Math.min(items.length - 1, value + 1)); }
        else if (event.key === "ArrowUp") { event.preventDefault(); setActive((value) => Math.max(0, value - 1)); }
        else if (event.key === "Enter" && items[active]) { event.preventDefault(); void choose(items[active]); }
        else if (event.key === "Tab") { event.preventDefault(); inputRef.current?.focus(); }
      }}>
        <div class="flex items-center gap-3 border-b border-line px-4 py-3">
          <span class="text-accent" aria-hidden="true">⌕</span>
          <input ref={inputRef} value={query} onInput={(event) => { setQuery(event.currentTarget.value); setActive(0); }} class="min-w-0 flex-1 bg-transparent text-base text-ink outline-none placeholder:text-muted" type="search" placeholder="Search projects, threads, artifacts, comments…" aria-label="Search" aria-controls="quick-switcher-results" aria-activedescendant={items[active]?.key} />
          {loading && <span class="text-[10px] text-muted">Searching…</span>}
          <kbd class="rounded border border-line px-1.5 py-0.5 text-[10px] text-muted">esc</kbd>
        </div>
        <div id="quick-switcher-results" role="listbox" class="min-h-24 overflow-y-auto p-2">
          {items.length === 0 && <div class="px-4 py-10 text-center"><p class="font-serif text-base font-semibold">No matches</p><p class="mt-1 text-xs text-muted">Try a concept, code identifier, or exact phrase.</p></div>}
          {items.map((item, index) => <div key={item.key}>
            {(index === 0 || items[index - 1].group !== item.group) && <p class="cfy-label px-2 pb-1 pt-2">{item.group}</p>}
            <button id={item.key} role="option" aria-selected={index === active} type="button" onMouseEnter={() => setActive(index)} onClick={() => void choose(item)} class={`w-full rounded-ctl px-3 py-2 text-left ${index === active ? "bg-accent-bg" : "hover:bg-hover"}`}>
              <div class="flex items-center justify-between gap-3"><span class="truncate text-[13px] font-medium text-ink">{marked(item.label)}</span>{item.hit?.artifactVersion != null && <span class="cfy-chip shrink-0">v{item.hit.artifactVersion}</span>}</div>
              {item.detail && <p class="mt-0.5 line-clamp-2 text-[11px] leading-relaxed text-muted">{marked(item.detail)}</p>}
            </button>
          </div>)}
        </div>
        <footer class="flex gap-4 border-t border-line px-4 py-2 text-[10px] text-muted"><span>↑↓ move</span><span>↵ open</span><span>esc close</span></footer>
      </section>
    </div>}
  </>;
}
