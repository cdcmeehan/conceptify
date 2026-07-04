// Thread view: the artifact reading surface (PRD §5.4, FR-2.3/2.4/2.5).
//
// The artifact renders in a sandboxed iframe loaded from the cross-scheme
// `artifact://` protocol (src-tauri/src/artifact_protocol.rs owns the URL
// contract and per-response CSP). The sandbox attribute is the containment
// boundary S2: `sandbox="allow-scripts"` with **no `allow-same-origin`**, so
// artifact JS runs in an opaque origin — it cannot touch the app DOM, Tauri
// IPC, storage, or the localhost API. Do not add sandbox tokens here without
// going through PRD §9.
//
// Live refresh (N2): `src/lib/events.ts` feeds `artifact-updated` into
// `appStore.handleArtifactUpdated`, which records the new version; while the
// switcher is on "Latest" the computed iframe src flips to the new concrete
// version and the iframe reloads in place. Concrete version URLs are
// immutable/cacheable, so history browsing is instant (FR-2.4).

import { useCallback, useRef, useState } from "preact/hooks";
import * as api from "../lib/api";
import type { Thread } from "../lib/api";
import { artifactBridge } from "../lib/bridge";
import { appStore, useAppStore } from "../store/appStore";
import { ArtifactCommentLayer } from "./ArtifactCommentLayer";
import { StatusChip } from "./StatusChip";

export function ThreadView({ thread }: { thread: Thread | null }) {
  const state = useAppStore();
  const [openError, setOpenError] = useState<string | null>(null);

  // Register the viewer iframe with the bridge (src/lib/bridge.ts owns the
  // postMessage handshake; comment UI riding on it is 94m.3/94m.6). Stable
  // callback so it only fires on mount/unmount — version switches reload the
  // same element and the bridge re-handshakes via its `ready` message. We also
  // stash the element so the comment layer can convert iframe-viewport rects to
  // shell coordinates (it reads this ref at popover-show time).
  const iframeElRef = useRef<HTMLIFrameElement | null>(null);
  const iframeRef = useCallback((el: HTMLIFrameElement | null) => {
    iframeElRef.current = el;
    if (el != null) artifactBridge.attach(el);
    else artifactBridge.detach();
  }, []);

  if (thread == null) {
    return (
      <main class="flex h-full flex-1 items-center justify-center bg-neutral-100 dark:bg-neutral-900">
        <p class="text-sm text-neutral-400">Select a thread to view its artifact.</p>
      </main>
    );
  }

  const threadId = thread.id;
  const versions = state.artifactVersions;
  const latestVersion = versions.length > 0 ? versions[versions.length - 1].version : null;
  // The concrete version the iframe shows. "latest" tracks the newest known
  // version (loading it by number keeps the immutable-cache fast path); a
  // pinned number is a read-only look at history.
  const resolvedVersion =
    state.viewerVersion === "latest" ? latestVersion : state.viewerVersion;
  const viewingOldVersion =
    resolvedVersion != null && latestVersion != null && resolvedVersion < latestVersion;

  const hasArtifact = resolvedVersion != null;
  const waitingOnVersions =
    !hasArtifact && (state.artifactVersionsLoading || state.artifactVersionsError != null);

  function onVersionChange(e: Event) {
    const value = (e.currentTarget as HTMLSelectElement).value;
    appStore.setViewerVersion(value === "latest" ? "latest" : Number(value));
  }

  function onOpenInBrowser() {
    setOpenError(null);
    api.openArtifactInBrowser(threadId).catch((e) => setOpenError(String(e)));
  }

  return (
    <main class="flex h-full min-w-0 flex-1 flex-col bg-neutral-100 dark:bg-neutral-900">
      <header class="border-b border-neutral-200 bg-white px-5 py-3 dark:border-neutral-800 dark:bg-neutral-950">
        <div class="flex items-center gap-3">
          <h1 class="min-w-0 flex-1 truncate text-lg font-semibold text-neutral-900 dark:text-neutral-50">
            {thread.title}
          </h1>
          <StatusChip status={thread.status} />
          {versions.length > 0 && (
            <>
              <label class="sr-only" for="artifact-version">
                Artifact version
              </label>
              <select
                id="artifact-version"
                value={state.viewerVersion === "latest" ? "latest" : String(state.viewerVersion)}
                onChange={onVersionChange}
                class="shrink-0 rounded-md border border-neutral-300 bg-white px-2 py-1 text-xs font-medium text-neutral-700 dark:border-neutral-700 dark:bg-neutral-900 dark:text-neutral-200"
              >
                <option value="latest">
                  Latest{latestVersion != null ? ` (v${latestVersion})` : ""}
                </option>
                {[...versions].reverse().map((v) => (
                  <option key={v.version} value={String(v.version)}>
                    v{v.version}
                  </option>
                ))}
              </select>
              <button
                type="button"
                onClick={onOpenInBrowser}
                title="Open the artifact file with your default browser"
                class="shrink-0 rounded-md border border-neutral-300 bg-white px-2.5 py-1 text-xs font-medium text-neutral-700 transition-colors hover:bg-neutral-100 dark:border-neutral-700 dark:bg-neutral-900 dark:text-neutral-200 dark:hover:bg-neutral-800"
              >
                Open in browser
              </button>
            </>
          )}
        </div>
        {(viewingOldVersion || openError != null) && (
          <div class="mt-2 flex items-center gap-3">
            {viewingOldVersion && (
              <span class="rounded-full bg-amber-100 px-2 py-0.5 text-xs font-medium text-amber-800 dark:bg-amber-500/15 dark:text-amber-300">
                Viewing v{resolvedVersion} of {latestVersion} — read-only
              </span>
            )}
            {openError != null && (
              <span class="truncate text-xs text-rose-600 dark:text-rose-400">{openError}</span>
            )}
          </div>
        )}
      </header>

      {hasArtifact ? (
        // S2 containment boundary: allow-scripts ONLY — never add
        // allow-same-origin (opaque origin is the whole point).
        <>
          <iframe
            key={thread.id}
            ref={iframeRef}
            src={`artifact://localhost/${thread.id}/${resolvedVersion}`}
            sandbox="allow-scripts"
            title="Artifact"
            class="min-h-0 w-full flex-1 border-0 bg-white dark:bg-neutral-950"
          />
          {/* Text-selection + element-click commenting (94m.3/94m.4). Keyed by
              thread so it remounts (fresh popover + bridge subscription) on
              thread switch; version switches update via the prop. Only mounted
              when an artifact exists → no commenting on a generating thread. */}
          {resolvedVersion != null && (
            <ArtifactCommentLayer
              key={thread.id}
              threadId={thread.id}
              artifactVersion={resolvedVersion}
              iframeRef={iframeElRef}
            />
          )}
        </>
      ) : waitingOnVersions ? (
        // Version list still in flight (or failed): render nothing heavy —
        // the list resolves in a beat; on error the state below takes over
        // on the next successful fetch.
        <div class="flex min-h-0 flex-1 items-center justify-center">
          {state.artifactVersionsError != null ? (
            <p class="max-w-md px-6 text-center text-xs text-rose-600 dark:text-rose-400">
              {state.artifactVersionsError}
            </p>
          ) : null}
        </div>
      ) : (
        <NoArtifactState thread={thread} />
      )}
    </main>
  );
}

/** Body shown while the thread has no saved artifact versions at all:
 *  generating/updating → progress placeholder (streamed progress is M6);
 *  error → failure state (log viewing is M5); ready → neutral empty state. */
function NoArtifactState({ thread }: { thread: Thread }) {
  return (
    <div class="min-h-0 flex-1 overflow-y-auto p-5">
      <div class="mx-auto max-w-2xl">
        {thread.initial_question.trim().length > 0 && (
          <section class="mb-4 rounded-lg border border-neutral-200 bg-white p-4 dark:border-neutral-800 dark:bg-neutral-950">
            <h2 class="mb-1 text-xs font-semibold uppercase tracking-wide text-neutral-400">
              Question
            </h2>
            <p class="whitespace-pre-wrap text-sm text-neutral-700 dark:text-neutral-300">
              {thread.initial_question}
            </p>
          </section>
        )}

        {thread.status === "error" ? (
          <section class="rounded-lg border border-rose-200 bg-rose-50 p-8 text-center dark:border-rose-500/30 dark:bg-rose-500/10">
            <p class="text-sm font-medium text-rose-700 dark:text-rose-300">
              Generation failed
            </p>
            <p class="mt-1 text-xs text-rose-600/80 dark:text-rose-400/80">
              No artifact was produced for this thread. Run logs land here in a
              later milestone.
            </p>
          </section>
        ) : thread.status === "generating" || thread.status === "updating" ? (
          <section class="rounded-lg border border-dashed border-neutral-300 bg-white/50 p-8 text-center dark:border-neutral-700 dark:bg-neutral-950/40">
            <p class="animate-pulse text-sm font-medium text-neutral-500 dark:text-neutral-400">
              Generating artifact…
            </p>
            <p class="mt-1 text-xs text-neutral-400">
              The artifact appears here the moment it is saved.
            </p>
          </section>
        ) : (
          <section class="rounded-lg border border-dashed border-neutral-300 bg-white/50 p-8 text-center dark:border-neutral-700 dark:bg-neutral-950/40">
            <p class="text-sm font-medium text-neutral-500 dark:text-neutral-400">
              No artifact yet
            </p>
            <p class="mt-1 text-xs text-neutral-400">
              This thread has no saved artifact versions.
            </p>
          </section>
        )}
      </div>
    </div>
  );
}
