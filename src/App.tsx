// App shell (PRD §5.3): project sidebar → thread list → thread view.
//
// State lives in `appStore` (src/store/appStore.ts); this component just wires
// the current snapshot into the three panes and triggers the initial load.
// Live updates (Tauri event → refetch) live in `src/lib/events.ts` and drive the
// store's refetch seams; they're set up once here at startup.

import { useEffect } from "preact/hooks";
import { appStore, useAppStore } from "./store/appStore";
import { initEventListeners } from "./lib/events";
import { getAgentSettings } from "./lib/api";
import { setAppearance } from "./lib/theme";
import { ProjectSidebar } from "./components/ProjectSidebar";
import { ThreadList } from "./components/ThreadList";
import { ThreadView } from "./components/ThreadView";
import { SettingsView } from "./components/SettingsView";
import "./App.css";

function App() {
  const state = useAppStore();

  useEffect(() => {
    void appStore.refetchProjects();
    // Apply the stored appearance (FR-7.2). theme.ts already applied `system`
    // before first paint (main.tsx); this replaces it with the saved value.
    void getAgentSettings()
      .then((s) => setAppearance(s.appearance))
      .catch(() => {
        /* keep the system default */
      });
    return initEventListeners();
  }, []);

  const selectedProject =
    state.projects.find((p) => p.id === state.selectedProjectId) ?? null;
  const selectedThread =
    state.threads.find((t) => t.id === state.selectedThreadId) ?? null;

  return (
    <div class="relative flex h-full w-full overflow-hidden bg-neutral-100 text-neutral-900 dark:bg-neutral-900 dark:text-neutral-100">
      <ProjectSidebar
        projects={state.projects}
        selectedProjectId={state.selectedProjectId}
        showArchived={state.showArchived}
        loading={state.projectsLoading}
        error={state.projectsError}
      />
      <ThreadList
        threads={state.threads}
        selectedThreadId={state.selectedThreadId}
        projectSelected={state.selectedProjectId != null}
        projectName={selectedProject?.name ?? null}
        loading={state.threadsLoading}
        error={state.threadsError}
      />
      <ThreadView thread={selectedThread} />
      {state.settingsOpen && <SettingsView />}
    </div>
  );
}

export default App;
