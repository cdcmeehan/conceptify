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
import type { ActiveRunState } from "../store/appStore";
import { appStore, useAppStore } from "../store/appStore";
import { ArtifactCommentLayer } from "./ArtifactCommentLayer";
import { CommentsSidebar } from "./CommentsSidebar";
import { GenerationError, GenerationProgress } from "./GenerationView";
import { StatusChip } from "./StatusChip";

export function ThreadView({ thread }: { thread: Thread | null }) {
  const state = useAppStore();
  const [openError, setOpenError] = useState<string | null>(null);
  // The comments sidebar (94m.6) is collapsible; the preference persists across
  // thread switches (ThreadView isn't remounted per thread — only the iframe and
  // comment layer are keyed). Default open: this is the interrogation home base.
  const [sidebarOpen, setSidebarOpen] = useState(true);

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
      <main class="flex h-full flex-1 items-center justify-center bg-well">
        <p class="text-[13px] text-muted">Select a thread to view its artifact.</p>
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
  // Live open-comment count for the sidebar toggle badge (from the store, so it
  // reflects API/CLI-driven changes without a refetch of the thread row).
  const openCommentCount = state.comments.filter((c) => c.status === "open").length;

  function onVersionChange(e: Event) {
    const value = (e.currentTarget as HTMLSelectElement).value;
    appStore.setViewerVersion(value === "latest" ? "latest" : Number(value));
  }

  function onOpenInBrowser() {
    setOpenError(null);
    api.openArtifactInBrowser(threadId).catch((e) => setOpenError(String(e)));
  }

  return (
    <main class="flex h-full min-w-0 flex-1 flex-col bg-well">
      <header class="border-b border-line bg-paper px-5 py-3">
        <div class="flex flex-wrap items-center gap-x-3 gap-y-2">
          <h1
            class="min-w-40 flex-1 truncate font-serif text-[17px] font-semibold text-ink"
            title={thread.title}
          >
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
                class="cfy-input w-auto shrink-0 px-2 py-1 text-xs font-medium"
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
                class="cfy-btn cfy-btn-secondary shrink-0"
              >
                Open in browser
              </button>
            </>
          )}
          <button
            type="button"
            onClick={() => setSidebarOpen((v) => !v)}
            aria-pressed={sidebarOpen}
            title={sidebarOpen ? "Hide comments" : "Show comments"}
            class={`cfy-btn shrink-0 ${sidebarOpen ? "cfy-btn-accent" : "cfy-btn-secondary"}`}
          >
            Comments
            {openCommentCount > 0 && (
              <span class="cfy-chip bg-info-bg px-1.5 tabular-nums text-info">
                {openCommentCount}
              </span>
            )}
          </button>
        </div>
        {(viewingOldVersion || openError != null) && (
          <div class="mt-2 flex items-center gap-3">
            {viewingOldVersion && (
              <span class="cfy-chip bg-warn-bg text-warn">
                Viewing v{resolvedVersion} of {latestVersion} — read-only
              </span>
            )}
            {openError != null && (
              <span class="truncate text-xs text-danger">{openError}</span>
            )}
          </div>
        )}
      </header>

      {/* Horizontal split: artifact viewer (left, flex) + comments sidebar
          (right, collapsible, 94m.6). The sidebar renders whenever a thread is
          selected — even with no artifact yet — so the direct-follow-up composer
          (94m.5) is visible-but-disabled during generation. */}
      <div class="flex min-h-0 flex-1">
        <div class="flex min-w-0 flex-1 flex-col">
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
                class="min-h-0 w-full flex-1 border-0 bg-raised"
              />
              {/* Text-selection + element-click commenting (94m.3/94m.4). Keyed
                  by thread so it remounts (fresh popover + bridge subscription)
                  on thread switch; version switches update via the prop. Only
                  mounted when an artifact exists → no anchored comments on a
                  generating thread. */}
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
                <p class="max-w-md px-6 text-center text-xs text-danger">
                  {state.artifactVersionsError}
                </p>
              ) : null}
            </div>
          ) : (
            <NoArtifactState thread={thread} activeRun={state.activeRun} />
          )}
        </div>

        {sidebarOpen && (
          <CommentsSidebar
            comments={state.comments}
            loading={state.commentsLoading}
            error={state.commentsError}
            threadId={threadId}
            viewerVersion={resolvedVersion}
            activeRun={state.activeRun}
            runFailure={state.runFailure}
            onClose={() => setSidebarOpen(false)}
          />
        )}
      </div>
    </main>
  );
}

/** Body shown while the thread has no saved artifact versions at all:
 *  generating/updating → the FR-5.2 live progress panel (streamed agent
 *  activity + cancel); error → the FR-5.3 failure state (log tail + one-click
 *  Retry); ready → neutral empty state. `activeRun` drives the progress feed;
 *  it is only used here for the `ask` generation run (the thread has no artifact
 *  yet, so it can't be an answer/apply run). */
function NoArtifactState({
  thread,
  activeRun,
}: {
  thread: Thread;
  activeRun: ActiveRunState | null;
}) {
  // Only surface progress for a run that actually belongs to this thread.
  const run = activeRun != null && activeRun.threadId === thread.id ? activeRun : null;

  return (
    <div class="min-h-0 flex-1 overflow-y-auto p-5">
      <div class="mx-auto max-w-2xl">
        {thread.initial_question.trim().length > 0 && (
          <section class="cfy-card mb-4 p-4">
            <h2 class="cfy-label mb-1.5">Question</h2>
            <p class="select-text whitespace-pre-wrap text-[13px] leading-relaxed text-ink">
              {thread.initial_question}
            </p>
          </section>
        )}

        {thread.status === "error" ? (
          <GenerationError threadId={thread.id} />
        ) : thread.status === "generating" || thread.status === "updating" ? (
          <GenerationProgress run={run} />
        ) : (
          <section class="rounded-card border border-dashed border-line p-8 text-center">
            <p class="font-serif text-sm font-semibold text-ink">No artifact yet</p>
            <p class="mt-1 text-xs text-muted">
              This thread has no saved artifact versions.
            </p>
          </section>
        )}
      </div>
    </div>
  );
}
