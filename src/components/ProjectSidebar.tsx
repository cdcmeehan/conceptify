// Project list sidebar (FR-1.3): thread counts, last activity, rename, archive
// (hide, not delete), and a "directory missing" badge + inline re-map for
// projects whose mapped root_path no longer resolves. Arrow keys move the
// selection when the list has focus.

import { useState } from "preact/hooks";
import { open as openDirectoryDialog } from "@tauri-apps/plugin-dialog";
import type { Project, RunActivity } from "../lib/api";
import { appStore } from "../store/appStore";
import { relativeTime } from "../lib/time";

interface Props {
  projects: Project[];
  selectedProjectId: string | null;
  showArchived: boolean;
  loading: boolean;
  error: string | null;
  runActivity: RunActivity[];
}

export function ProjectSidebar({ projects, selectedProjectId, showArchived, loading, error, runActivity }: Props) {
  // Only one project may be inline-editing / re-mapping at a time.
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editName, setEditName] = useState("");
  const [remappingId, setRemappingId] = useState<string | null>(null);
  const [remapPath, setRemapPath] = useState("");
  const [remapError, setRemapError] = useState<string | null>(null);
  const [remapBusy, setRemapBusy] = useState(false);

  // FR-1.2 / UC6 "New project": pick an existing folder (native dir picker) or
  // create a fresh topic folder for a non-codebase subject.
  const [newProjectOpen, setNewProjectOpen] = useState(false);
  const [newFolderName, setNewFolderName] = useState("");
  const [newProjectError, setNewProjectError] = useState<string | null>(null);
  const [newProjectBusy, setNewProjectBusy] = useState(false);
  const [newTopicNotes, setNewTopicNotes] = useState("");
  const [newTopicLinks, setNewTopicLinks] = useState("");
  const [topicContextOpen, setTopicContextOpen] = useState(false);
  const [newTopicFiles, setNewTopicFiles] = useState<string[]>([]);
  const [firstQuestion, setFirstQuestion] = useState("");
  const [contextOpenId, setContextOpenId] = useState<string | null>(null);

  function closeNewProject() {
    setNewProjectOpen(false);
    setNewFolderName("");
    setNewProjectError(null);
    setNewProjectBusy(false);
    setNewTopicNotes("");
    setNewTopicLinks("");
    setTopicContextOpen(false);
    setNewTopicFiles([]);
    setFirstQuestion("");
  }

  async function pickDirectory() {
    setNewProjectError(null);
    try {
      // Native NSOpenPanel via the dialog plugin (WKWebView-safe). `null` =
      // cancelled; a single directory returns its absolute path.
      const selected = await openDirectoryDialog({
        directory: true,
        multiple: false,
        title: "Choose a project folder",
      });
      if (typeof selected !== "string") return; // cancelled
      setNewProjectBusy(true);
      const projectId = await appStore.createProjectFromDir(selected);
      if (firstQuestion.trim() !== "") await appStore.launchFirstQuestion(projectId, firstQuestion);
      closeNewProject();
    } catch (e) {
      setNewProjectError(String(e));
      setNewProjectBusy(false);
    }
  }

  async function pickTopicFiles() {
    const selected = await openDirectoryDialog({ directory: false, multiple: true, title: "Choose source files" });
    if (Array.isArray(selected)) setNewTopicFiles(selected);
    else if (typeof selected === "string") setNewTopicFiles([selected]);
  }

  function createFolder() {
    const name = newFolderName.trim();
    if (name.length === 0) return;
    setNewProjectBusy(true);
    setNewProjectError(null);
    appStore
      .createProjectFolder(name, {
        notes: newTopicNotes.trim(),
        links: newTopicLinks.split("\n").map((value) => value.trim()).filter(Boolean),
        files: newTopicFiles,
      })
      .then(async (projectId) => {
        if (firstQuestion.trim() !== "") await appStore.launchFirstQuestion(projectId, firstQuestion);
        closeNewProject();
      })
      .catch((e) => {
        setNewProjectError(String(e));
        setNewProjectBusy(false);
      });
  }

  function startRename(project: Project) {
    setEditingId(project.id);
    setEditName(project.name);
  }

  function commitRename() {
    const id = editingId;
    if (id == null) return;
    const name = editName.trim();
    setEditingId(null);
    if (name.length > 0) void appStore.renameProject(id, name);
  }

  function startRemap(project: Project) {
    setRemappingId(project.id);
    setRemapPath("");
    setRemapError(null);
  }

  function commitRemap() {
    const id = remappingId;
    const path = remapPath.trim();
    if (id == null || path.length === 0) return;
    setRemapBusy(true);
    setRemapError(null);
    appStore
      .remapProject(id, path)
      .then(() => {
        setRemappingId(null);
        setRemapPath("");
      })
      .catch((e) => setRemapError(String(e)))
      .finally(() => setRemapBusy(false));
  }

  function onListKeyDown(e: KeyboardEvent) {
    // Don't hijack arrows while typing in an inline input.
    if ((e.target as HTMLElement).tagName === "INPUT") return;
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    if (projects.length === 0) return;
    e.preventDefault();

    const index = projects.findIndex((p) => p.id === selectedProjectId);
    const delta = e.key === "ArrowDown" ? 1 : -1;
    const next = index < 0 ? (delta === 1 ? 0 : projects.length - 1) : index + delta;
    const clamped = Math.max(0, Math.min(projects.length - 1, next));
    appStore.selectProject(projects[clamped].id);
  }

  return (
    <nav
      class="flex h-full w-48 shrink-0 flex-col border-r border-line bg-well outline-none lg:w-56"
      tabIndex={0}
      onKeyDown={onListKeyDown}
      aria-label="Projects"
    >
      <header class="flex items-center justify-between px-3 py-2.5">
        <h2 class="cfy-label">Projects</h2>
        <label class="flex items-center gap-1.5 text-[11px] text-muted">
          <input
            type="checkbox"
            checked={showArchived}
            onChange={(e) => appStore.setShowArchived((e.target as HTMLInputElement).checked)}
          />
          Archived
        </label>
      </header>

      {/* FR-1.2 / UC6: create a project — pick an existing folder or make one. */}
      <div class="px-2 pb-2">
        {newProjectOpen ? (
          <div
            class="cfy-card flex flex-col gap-2 p-2.5"
            onKeyDown={(e) => {
              // Escape backs out of the panel (unless a request is in flight).
              if (e.key === "Escape" && !newProjectBusy) {
                e.stopPropagation();
                closeNewProject();
              }
            }}
          >
            <div>
              <label class="cfy-label" for="quick-start-question">First question (optional)</label>
              <textarea
                id="quick-start-question"
                value={firstQuestion}
                onInput={(e) => setFirstQuestion((e.currentTarget as HTMLTextAreaElement).value)}
                rows={2}
                class="cfy-input mt-1 resize-y text-[10px]"
                placeholder="What would you like to understand first?"
              />
              <div class="mt-1 flex flex-wrap gap-1">
                {["Give me a useful overview", "Show me the architecture", "What are the key concepts?", "Create a learning path"].map((starter) => (
                  <button key={starter} type="button" onClick={() => setFirstQuestion(starter)} class="rounded-full border border-line px-1.5 py-0.5 text-[9px] text-muted hover:border-accent/40 hover:text-ink">{starter}</button>
                ))}
              </div>
            </div>
            <button
              type="button"
              disabled={newProjectBusy}
              onClick={() => void pickDirectory()}
              class="cfy-btn cfy-btn-secondary"
            >
              {firstQuestion.trim() === "" ? "Choose an existing folder…" : "Choose folder & ask…"}
            </button>
            <div class="flex items-center gap-2 text-[10px] uppercase tracking-wide text-muted">
              <span class="h-px flex-1 bg-line" />
              or learn a topic
              <span class="h-px flex-1 bg-line" />
            </div>
            <input
              type="text"
              value={newFolderName}
              placeholder="Topic (e.g. Distributed Systems)"
              disabled={newProjectBusy}
              autoFocus
              onInput={(e) => setNewFolderName((e.currentTarget as HTMLInputElement).value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") createFolder();
              }}
              class="cfy-input"
            />
            <button
              type="button"
              onClick={() => setTopicContextOpen((value) => !value)}
              class="text-left text-[10px] font-medium text-accent-ink hover:underline"
              aria-expanded={topicContextOpen}
            >
              {topicContextOpen ? "Hide optional context" : "Add optional notes or links"}
            </button>
            {topicContextOpen && (
              <div class="flex flex-col gap-1.5">
                <textarea
                  value={newTopicNotes}
                  onInput={(e) => setNewTopicNotes((e.currentTarget as HTMLTextAreaElement).value)}
                  rows={2}
                  class="cfy-input resize-y text-[10px]"
                  placeholder="What do you already know or want to focus on?"
                  aria-label="Topic notes"
                />
                <div class="flex items-center justify-between gap-2">
                  <button type="button" onClick={() => void pickTopicFiles()} class="cfy-btn cfy-btn-secondary h-7 px-2 text-[9px]">Add source files…</button>
                  {newTopicFiles.length > 0 && <button type="button" onClick={() => setNewTopicFiles([])} class="text-[9px] text-muted hover:text-danger">Remove all</button>}
                </div>
                {newTopicFiles.length > 0 && <p class="text-[9px] text-muted">{newTopicFiles.length} file{newTopicFiles.length === 1 ? "" : "s"} will be copied into the topic project.</p>}
                <textarea
                  value={newTopicLinks}
                  onInput={(e) => setNewTopicLinks((e.currentTarget as HTMLTextAreaElement).value)}
                  rows={2}
                  class="cfy-input resize-y text-[10px]"
                  placeholder="Reference links, one per line (optional)"
                  aria-label="Topic reference links"
                />
                <p class="text-[9px] leading-relaxed text-muted">Stored inside this private topic project. Links are references only; nothing is fetched automatically.</p>
              </div>
            )}
            {newProjectError != null && (
              <p class="break-words text-[11px] text-danger">{newProjectError}</p>
            )}
            <div class="flex items-center justify-end gap-1.5">
              <button
                type="button"
                onClick={closeNewProject}
                disabled={newProjectBusy}
                class="cfy-btn cfy-btn-ghost"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={createFolder}
                disabled={newProjectBusy || newFolderName.trim().length === 0}
                class="cfy-btn cfy-btn-primary"
              >
                {newProjectBusy ? "Starting…" : firstQuestion.trim() === "" ? "Start topic" : "Start & ask"}
              </button>
            </div>
            <p class="text-[9px] leading-relaxed text-muted">
              {firstQuestion.trim() === "" ? "Creates the project and opens its home." : "Creates the project, saves this question once, and opens its generating thread."}
            </p>
          </div>
        ) : (
          <button
            type="button"
            onClick={() => setNewProjectOpen(true)}
            class="flex w-full items-center justify-center gap-1 rounded-ctl border border-dashed border-line px-2 py-1.5 text-xs font-medium text-muted transition-colors hover:border-accent/50 hover:text-accent-ink"
          >
            <svg viewBox="0 0 20 20" fill="none" class="h-3.5 w-3.5" aria-hidden="true">
              <path
                d="M10 4.5v11M4.5 10h11"
                stroke="currentColor"
                stroke-width="1.75"
                stroke-linecap="round"
              />
            </svg>
            New project
          </button>
        )}
      </div>

      <div class="min-h-0 flex-1 overflow-y-auto px-2 pb-2">
        {error != null ? (
          <p class="px-2 py-3 text-xs text-danger">{error}</p>
        ) : loading && projects.length === 0 ? (
          <div class="flex flex-col gap-2.5 px-2 py-3" aria-hidden="true">
            <div class="cfy-skeleton w-4/5" />
            <div class="cfy-skeleton w-3/5" />
            <div class="cfy-skeleton w-2/3" />
          </div>
        ) : projects.length === 0 ? (
          // First-run empty state (bead conceptify-vxc): one quiet sentence +
          // the action that gets things moving.
          <div class="px-3 py-10 text-center">
            <p class="font-serif text-sm font-semibold text-ink">No projects yet</p>
            <p class="mt-1 text-xs leading-relaxed text-muted">
              Map a folder — or create one — and start asking questions about it.
            </p>
            {!newProjectOpen && (
              <button
                type="button"
                onClick={() => setNewProjectOpen(true)}
                class="cfy-btn cfy-btn-primary mt-3"
              >
                Create a project
              </button>
            )}
          </div>
        ) : (
          <ul class="flex flex-col gap-0.5">
            {projects.map((project) => {
              const selected = project.id === selectedProjectId;
              const isEditing = editingId === project.id;
              const isRemapping = remappingId === project.id;
              const activeRuns = runActivity.filter(
                (item) =>
                  item.project_id === project.id &&
                  ["queued", "starting", "running", "throttled", "cancelling"].includes(item.status),
              ).length;
              return (
                <li key={project.id}>
                  <div
                    role="button"
                    tabIndex={-1}
                    onClick={() => appStore.selectProject(project.id)}
                    class={`w-full rounded-ctl px-2 py-1.5 text-left transition-colors ${
                      selected ? "bg-accent-bg" : "hover:bg-hover"
                    } ${project.archived ? "opacity-60" : ""}`}
                  >
                    {isEditing ? (
                      <input
                        class="cfy-input px-1.5 py-0.5"
                        value={editName}
                        autoFocus
                        onClick={(e) => e.stopPropagation()}
                        onInput={(e) => setEditName((e.target as HTMLInputElement).value)}
                        onBlur={commitRename}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") commitRename();
                          else if (e.key === "Escape") setEditingId(null);
                        }}
                      />
                    ) : (
                      <div class="flex items-baseline justify-between gap-2">
                        <span
                          class="truncate text-[13px] font-medium text-ink"
                          title={project.name}
                        >
                          {project.name}
                        </span>
                        <span class="flex shrink-0 items-center gap-1 text-[11px] tabular-nums text-muted">
                          {activeRuns > 0 && (
                            <span
                              class="h-1.5 w-1.5 animate-pulse rounded-full bg-info"
                              title={`${activeRuns} active run${activeRuns === 1 ? "" : "s"}`}
                            />
                          )}
                          {project.thread_count}
                        </span>
                      </div>
                    )}

                    <div class="mt-0.5 flex items-center gap-2">
                      <span class="text-[11px] text-muted">
                        {relativeTime(project.last_activity)}
                      </span>
                      {project.archived && (
                        <span class="cfy-chip bg-hover text-[10px] uppercase tracking-wide text-muted">
                          Archived
                        </span>
                      )}
                      {!project.root_exists && (
                        <span
                          class="cfy-chip bg-danger-bg text-[10px] uppercase tracking-wide text-danger"
                          title={`Mapped directory not found: ${project.root_path}`}
                        >
                          Dir missing
                        </span>
                      )}
                    </div>

                    {/* Re-map affordance for a vanished directory. */}
                    {!project.root_exists && (
                      <div class="mt-1.5" onClick={(e) => e.stopPropagation()}>
                        {isRemapping ? (
                          <div class="flex flex-col gap-1">
                            <input
                              class="cfy-input px-1.5 py-0.5 text-xs"
                              placeholder="/new/absolute/path"
                              value={remapPath}
                              autoFocus
                              onInput={(e) => setRemapPath((e.target as HTMLInputElement).value)}
                              onKeyDown={(e) => {
                                if (e.key === "Enter") commitRemap();
                                else if (e.key === "Escape") setRemappingId(null);
                              }}
                            />
                            {remapError != null && (
                              <span class="text-[11px] text-danger">{remapError}</span>
                            )}
                            <div class="flex gap-1.5">
                              <button
                                type="button"
                                disabled={remapBusy}
                                onClick={commitRemap}
                                class="cfy-btn cfy-btn-primary px-2 py-0.5 text-[11px]"
                              >
                                Save
                              </button>
                              <button
                                type="button"
                                onClick={() => setRemappingId(null)}
                                class="cfy-btn cfy-btn-ghost px-2 py-0.5 text-[11px]"
                              >
                                Cancel
                              </button>
                            </div>
                          </div>
                        ) : (
                          <button
                            type="button"
                            onClick={() => startRemap(project)}
                            class="cfy-btn cfy-btn-danger px-2 py-0.5 text-[11px]"
                          >
                            Re-map…
                          </button>
                        )}
                      </div>
                    )}

                    {/* Rename / archive actions on the selected project. */}
                    {selected && !isEditing && (
                      <div class="mt-1 flex gap-2" onClick={(e) => e.stopPropagation()}>
                        <button
                          type="button"
                          onClick={() => startRename(project)}
                          class="rounded text-[11px] text-muted transition-colors hover:text-ink"
                        >
                          Rename
                        </button>
                        <button
                          type="button"
                          onClick={() => void appStore.archiveProject(project.id, !project.archived)}
                          class="rounded text-[11px] text-muted transition-colors hover:text-ink"
                        >
                          {project.archived ? "Unarchive" : "Archive"}
                        </button>
                      </div>
                    )}
                    {selected && project.context != null && (
                      <div class="mt-1.5" onClick={(e) => e.stopPropagation()}>
                        <button
                          type="button"
                          onClick={() => setContextOpenId(contextOpenId === project.id ? null : project.id)}
                          class="w-full rounded-ctl border border-line bg-well/45 px-2 py-1 text-left text-[10px] text-muted hover:border-accent/35"
                          aria-expanded={contextOpenId === project.id}
                        >
                          <span class="font-medium text-ink">
                            {project.context.status === "scanning" ? "Checking context…" : project.context.status === "error" ? "Context needs attention" : project.context.status === "limited" ? "Context overview · limited" : "Context ready"}
                          </span>
                          {project.context.languages.length > 0 && (
                            <span class="ml-1">· {project.context.languages.slice(0, 3).map((item) => item.name).join(", ")}</span>
                          )}
                        </button>
                        {contextOpenId === project.id && (
                          <div class="mt-1 rounded-ctl border border-line bg-paper p-2 text-[9px] leading-relaxed text-muted">
                            <p class="font-medium text-ink">{project.context.repository} · {project.context.included_files.toLocaleString()} files inspected</p>
                            <p class="mt-1">Excluded: {project.context.excluded_paths.length > 0 ? project.context.excluded_paths.join(", ") : "none detected"}</p>
                            <p class="mt-1">This is a lightweight local overview, not a full index. Agents read relevant files when you ask.</p>
                            {project.context.warning != null && <p class="mt-1 text-warn">{project.context.warning}</p>}
                          </div>
                        )}
                      </div>
                    )}
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </div>

      {/* Settings entry (FR-7.x) in the sidebar footer. */}
      <div class="border-t border-line px-2 py-2">
        <button
          type="button"
          onClick={() => appStore.openSettings()}
          class="cfy-btn cfy-btn-ghost w-full justify-start gap-2 px-2 py-1.5"
        >
          <svg viewBox="0 0 20 20" fill="none" class="h-4 w-4" aria-hidden="true">
            <path
              d="M10 12.5a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5Z"
              stroke="currentColor"
              stroke-width="1.4"
            />
            <path
              d="M10 2.5v1.6M10 15.9v1.6M4.7 4.7l1.1 1.1M14.2 14.2l1.1 1.1M2.5 10h1.6M15.9 10h1.6M4.7 15.3l1.1-1.1M14.2 5.8l1.1-1.1"
              stroke="currentColor"
              stroke-width="1.4"
              stroke-linecap="round"
            />
          </svg>
          Settings
        </button>
      </div>
    </nav>
  );
}
