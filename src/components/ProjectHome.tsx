import { useEffect, useState } from "preact/hooks";
import type { Project, RunActivity, Thread } from "../lib/api";
import { getProjectGoal, getTopicContext, setProjectGoal, setTopicContext } from "../lib/api";
import { appStore } from "../store/appStore";

export function ProjectHome({ project, threads, activity }: { project: Project; threads: Thread[]; activity: RunActivity[] }) {
  const [goal, setGoal] = useState("");
  const [question, setQuestion] = useState("");
  const [topicNotes, setTopicNotes] = useState("");
  const [topicContext, setTopicContextState] = useState({ notes: "", links: [] as string[], files: [] as string[] });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    void getProjectGoal(project.id).then(setGoal).catch(() => setGoal(""));
    void getTopicContext(project.id).then((context) => { setTopicContextState(context); setTopicNotes(context.notes); }).catch(() => undefined);
  }, [project.id]);
  const active = activity.filter((item) => item.project_id === project.id && ["queued", "starting", "running", "throttled", "cancelling"].includes(item.status));
  const suggestions = threads.length === 0
    ? ["Give me a useful overview", "Show me the architecture", "What are the key concepts?", "Create a learning path"]
    : ["What should I understand next?", "Connect the recent explanations", "Show me an important trade-off"];
  async function ask() {
    if (question.trim() === "" || busy) return;
    setBusy(true); setError(null);
    try { await appStore.launchFirstQuestion(project.id, question); setQuestion(""); }
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }
  return (
    <main class="min-w-0 flex-1 overflow-y-auto bg-well p-6" aria-label={`${project.name} project home`}>
      <div class="mx-auto max-w-3xl">
        <p class="cfy-label">Project home</p>
        <h1 class="mt-1 font-serif text-2xl font-semibold text-ink">{project.name}</h1>
        <section class="cfy-card mt-4 p-4">
          <label class="cfy-label" for="project-goal">What are you trying to understand?</label>
          <textarea id="project-goal" value={goal} onInput={(e) => setGoal((e.currentTarget as HTMLTextAreaElement).value)} onBlur={() => void setProjectGoal(project.id, goal)} rows={2} class="cfy-input mt-2 resize-y" placeholder="Add a short goal or learning brief…" />
          <p class="mt-1 text-[10px] text-muted">Saved for this project. Existing threads keep their original questions and profiles.</p>
        </section>
        <div class="mt-4 grid gap-4 md:grid-cols-2">
          <section class="cfy-card p-4">
            <p class="cfy-label">Available context</p>
            {project.context == null ? <p class="mt-2 text-xs text-muted">Context overview will appear after a folder check.</p> : <>
              <p class="mt-2 text-sm font-medium text-ink">{project.context.repository} · {project.context.included_files.toLocaleString()} files</p>
              <p class="mt-1 text-xs text-muted">{project.context.languages.map((item) => item.name).join(", ") || "Topic context"}</p>
              <p class="mt-2 text-[10px] text-muted">Excluded: {project.context.excluded_paths.join(", ") || "none detected"}</p>
              {project.context.warning != null && <p class="mt-2 text-[10px] text-warn">{project.context.warning}</p>}
            </>}
            <label class="cfy-label mt-3 block" for="project-context-notes">Context notes</label>
            <textarea id="project-context-notes" value={topicNotes} onInput={(e) => setTopicNotes((e.currentTarget as HTMLTextAreaElement).value)} onBlur={() => { const next = { ...topicContext, notes: topicNotes }; setTopicContextState(next); void setTopicContext(project.id, next); }} rows={2} class="cfy-input mt-1 resize-y text-[10px]" placeholder="Add or update local context notes…" />
          </section>
          <section class="cfy-card p-4">
            <p class="cfy-label">Activity</p>
            {active.length === 0 ? <p class="mt-2 text-xs text-muted">No work running. You can ask the next question below.</p> : <ul class="mt-2 space-y-2">{active.slice(0, 3).map((item) => <li key={item.run_id}><button type="button" onClick={() => void appStore.jumpToRunActivity(item)} class="text-left text-xs text-accent-ink hover:underline">{item.thread_title} · {item.status}</button></li>)}</ul>}
          </section>
        </div>
        {threads.length > 0 && <section class="mt-4"><div class="flex items-center justify-between"><p class="cfy-label">Recent results</p><span class="text-[10px] text-muted">Full history stays in the thread list</span></div><div class="mt-2 grid gap-2 sm:grid-cols-3">{threads.slice(0, 3).map((thread) => <button type="button" key={thread.id} onClick={() => appStore.selectThread(thread.id)} class="cfy-card p-3 text-left hover:border-accent/40"><span class="line-clamp-2 text-xs font-medium text-ink">{thread.title}</span><span class="mt-2 block text-[10px] text-muted">{thread.status}</span></button>)}</div></section>}
        <section class="cfy-card mt-4 p-4">
          <p class="cfy-label">Ask the next question</p>
          <textarea value={question} onInput={(e) => setQuestion((e.currentTarget as HTMLTextAreaElement).value)} rows={3} class="cfy-input mt-2 resize-y" placeholder="What would you like to understand next?" />
          <div class="mt-2 flex flex-wrap gap-1.5">{suggestions.map((value) => <button type="button" key={value} onClick={() => setQuestion(value)} class="rounded-full border border-line px-2 py-1 text-[10px] text-muted hover:border-accent/40 hover:text-ink">{value}</button>)}</div>
          {error != null && <p class="mt-2 text-xs text-danger">{error}</p>}
          <div class="mt-3 flex justify-end"><button type="button" onClick={() => void ask()} disabled={busy || question.trim() === ""} class="cfy-btn cfy-btn-primary">{busy ? "Asking…" : "Ask"}</button></div>
        </section>
      </div>
    </main>
  );
}
