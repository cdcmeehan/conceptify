// Project list sidebar (FR-1.3): thread counts, last activity, rename, archive
// (hide, not delete), and a "directory missing" badge + inline re-map for
// projects whose mapped root_path no longer resolves. Arrow keys move the
// selection when the list has focus.

import { useState } from "preact/hooks";
import { open as openDirectoryDialog } from "@tauri-apps/plugin-dialog";
import type { Project } from "../lib/api";
import { appStore } from "../store/appStore";
import { relativeTime } from "../lib/time";

interface Props {
  projects: Project[];
  selectedProjectId: string | null;
  showArchived: boolean;
  loading: boolean;
  error: string | null;
}

export function ProjectSidebar({ projects, selectedProjectId, showArchived, loading, error }: Props) {
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

  function closeNewProject() {
    setNewProjectOpen(false);
    setNewFolderName("");
    setNewProjectError(null);
    setNewProjectBusy(false);
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
      await appStore.createProjectFromDir(selected);
      closeNewProject();
    } catch (e) {
      setNewProjectError(String(e));
      setNewProjectBusy(false);
    }
  }

  function createFolder() {
    const name = newFolderName.trim();
    if (name.length === 0) return;
    setNewProjectBusy(true);
    setNewProjectError(null);
    appStore
      .createProjectFolder(name)
      .then(() => closeNewProject())
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
      class="flex h-full w-56 shrink-0 flex-col border-r border-neutral-200 bg-neutral-50 outline-none dark:border-neutral-800 dark:bg-neutral-900"
      tabIndex={0}
      onKeyDown={onListKeyDown}
      aria-label="Projects"
    >
      <header class="flex items-center justify-between px-3 py-2.5">
        <h2 class="text-xs font-semibold uppercase tracking-wide text-neutral-500 dark:text-neutral-400">
          Projects
        </h2>
        <label class="flex cursor-pointer items-center gap-1 text-xs text-neutral-500 dark:text-neutral-400">
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
          <div class="flex flex-col gap-2 rounded-lg border border-neutral-200 bg-white p-2.5 dark:border-neutral-800 dark:bg-neutral-950">
            <button
              type="button"
              disabled={newProjectBusy}
              onClick={() => void pickDirectory()}
              class="rounded-md border border-neutral-300 bg-white px-2 py-1.5 text-xs font-medium text-neutral-700 transition-colors hover:bg-neutral-100 disabled:opacity-50 dark:border-neutral-700 dark:bg-neutral-900 dark:text-neutral-200 dark:hover:bg-neutral-800"
            >
              Choose an existing folder…
            </button>
            <div class="flex items-center gap-2 text-[10px] uppercase tracking-wide text-neutral-400">
              <span class="h-px flex-1 bg-neutral-200 dark:bg-neutral-800" />
              or make one
              <span class="h-px flex-1 bg-neutral-200 dark:bg-neutral-800" />
            </div>
            <input
              type="text"
              value={newFolderName}
              placeholder="New topic (e.g. Distributed Systems)"
              disabled={newProjectBusy}
              autoFocus
              onInput={(e) => setNewFolderName((e.currentTarget as HTMLInputElement).value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") createFolder();
                else if (e.key === "Escape") closeNewProject();
              }}
              class="rounded-md border border-neutral-300 bg-white px-2 py-1.5 text-sm text-neutral-800 placeholder:text-neutral-400 focus:border-blue-400 focus:outline-none dark:border-neutral-700 dark:bg-neutral-950 dark:text-neutral-100"
            />
            {newProjectError != null && (
              <p class="break-words text-[11px] text-rose-600 dark:text-rose-400">
                {newProjectError}
              </p>
            )}
            <div class="flex items-center justify-end gap-1.5">
              <button
                type="button"
                onClick={closeNewProject}
                disabled={newProjectBusy}
                class="rounded-md px-2.5 py-1 text-xs font-medium text-neutral-500 transition-colors hover:bg-neutral-200 disabled:opacity-50 dark:text-neutral-400 dark:hover:bg-neutral-800"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={createFolder}
                disabled={newProjectBusy || newFolderName.trim().length === 0}
                class="rounded-md bg-blue-600 px-3 py-1 text-xs font-medium text-white transition-colors hover:bg-blue-700 disabled:cursor-not-allowed disabled:bg-neutral-200 disabled:text-neutral-400 dark:disabled:bg-neutral-800 dark:disabled:text-neutral-600"
              >
                {newProjectBusy ? "Creating…" : "Create folder"}
              </button>
            </div>
          </div>
        ) : (
          <button
            type="button"
            onClick={() => setNewProjectOpen(true)}
            class="flex w-full items-center justify-center gap-1 rounded-md border border-dashed border-neutral-300 px-2 py-1.5 text-xs font-medium text-neutral-500 transition-colors hover:border-blue-400 hover:text-blue-600 dark:border-neutral-700 dark:text-neutral-400 dark:hover:border-blue-500/50 dark:hover:text-blue-300"
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
          <p class="px-2 py-3 text-xs text-rose-600 dark:text-rose-400">{error}</p>
        ) : loading && projects.length === 0 ? (
          <p class="px-2 py-3 text-xs text-neutral-400">Loading…</p>
        ) : projects.length === 0 ? (
          <p class="px-2 py-3 text-xs text-neutral-400">
            No projects yet. Create one with the CLI or the Claude Code skill.
          </p>
        ) : (
          <ul class="flex flex-col gap-0.5">
            {projects.map((project) => {
              const selected = project.id === selectedProjectId;
              const isEditing = editingId === project.id;
              const isRemapping = remappingId === project.id;
              return (
                <li key={project.id}>
                  <div
                    role="button"
                    tabIndex={-1}
                    onClick={() => appStore.selectProject(project.id)}
                    class={`w-full rounded-md px-2 py-1.5 text-left transition-colors ${
                      selected
                        ? "bg-blue-600/10 dark:bg-blue-500/20"
                        : "hover:bg-neutral-200/60 dark:hover:bg-neutral-800/60"
                    } ${project.archived ? "opacity-60" : ""}`}
                  >
                    {isEditing ? (
                      <input
                        class="w-full rounded border border-blue-400 bg-white px-1.5 py-0.5 text-sm text-neutral-900 outline-none dark:bg-neutral-950 dark:text-neutral-100"
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
                        <span class="truncate text-sm font-medium text-neutral-800 dark:text-neutral-100">
                          {project.name}
                        </span>
                        <span class="shrink-0 text-xs tabular-nums text-neutral-400">
                          {project.thread_count}
                        </span>
                      </div>
                    )}

                    <div class="mt-0.5 flex items-center gap-2">
                      <span class="text-xs text-neutral-400">
                        {relativeTime(project.last_activity)}
                      </span>
                      {project.archived && (
                        <span class="rounded bg-neutral-200 px-1 text-[10px] font-medium uppercase tracking-wide text-neutral-500 dark:bg-neutral-800 dark:text-neutral-400">
                          Archived
                        </span>
                      )}
                      {!project.root_exists && (
                        <span
                          class="rounded bg-rose-100 px-1 text-[10px] font-medium uppercase tracking-wide text-rose-700 dark:bg-rose-500/20 dark:text-rose-300"
                          title={`Mapped directory not found: ${project.root_path}`}
                        >
                          Dir missing
                        </span>
                      )}
                    </div>

                    {/* Re-map affordance for a vanished directory. */}
                    {!project.root_exists && (
                      <div class="mt-1" onClick={(e) => e.stopPropagation()}>
                        {isRemapping ? (
                          <div class="flex flex-col gap-1">
                            <input
                              class="w-full rounded border border-neutral-300 bg-white px-1.5 py-0.5 text-xs text-neutral-900 outline-none focus:border-blue-400 dark:border-neutral-700 dark:bg-neutral-950 dark:text-neutral-100"
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
                              <span class="text-[11px] text-rose-600 dark:text-rose-400">
                                {remapError}
                              </span>
                            )}
                            <div class="flex gap-1">
                              <button
                                type="button"
                                disabled={remapBusy}
                                onClick={commitRemap}
                                class="rounded bg-blue-600 px-2 py-0.5 text-xs font-medium text-white hover:bg-blue-700 disabled:opacity-50"
                              >
                                Save
                              </button>
                              <button
                                type="button"
                                onClick={() => setRemappingId(null)}
                                class="rounded px-2 py-0.5 text-xs text-neutral-500 hover:text-neutral-800 dark:hover:text-neutral-200"
                              >
                                Cancel
                              </button>
                            </div>
                          </div>
                        ) : (
                          <button
                            type="button"
                            onClick={() => startRemap(project)}
                            class="rounded border border-rose-300 px-2 py-0.5 text-xs font-medium text-rose-700 hover:bg-rose-50 dark:border-rose-500/40 dark:text-rose-300 dark:hover:bg-rose-500/10"
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
                          class="text-xs text-neutral-500 hover:text-neutral-800 dark:hover:text-neutral-200"
                        >
                          Rename
                        </button>
                        <button
                          type="button"
                          onClick={() => void appStore.archiveProject(project.id, !project.archived)}
                          class="text-xs text-neutral-500 hover:text-neutral-800 dark:hover:text-neutral-200"
                        >
                          {project.archived ? "Unarchive" : "Archive"}
                        </button>
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
      <div class="border-t border-neutral-200 px-2 py-2 dark:border-neutral-800">
        <button
          type="button"
          onClick={() => appStore.openSettings()}
          class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-xs font-medium text-neutral-500 transition-colors hover:bg-neutral-200/60 hover:text-neutral-800 dark:text-neutral-400 dark:hover:bg-neutral-800/60 dark:hover:text-neutral-200"
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
