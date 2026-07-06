//! Headless agent-run engine (PRD §5.1 agent spawner, §5.5 surface 2,
//! FR-4.8/FR-4.9 backend, FR-5.3, N4) — bead `conceptify-b12.2`.
//!
//! This module owns the **process lifecycle** of a background agent run: it
//! creates the `follow_up_runs` row, resolves the invocation through the
//! settings adapter layer (`crate::settings`, bead b12.1), spawns the agent
//! with `tokio::process` (never tauri-plugin-shell — frontend-initiated exec
//! is disallowed, PRD §9 S3), streams stdout/stderr into the run log and
//! compact Tauri events, enforces the timeout, and always drives the row to a
//! terminal state. The *flows* that use it (ask-follow-ups b12.4, apply
//! b12.5, run UI b12.6, in-app ask 959.1) assemble prompts and apply
//! thread-status policy on top; the engine stays policy-free.
//!
//! # Lifecycle contract
//!
//! [`start_run`] → row `status = 'running'` + spawned child → terminal status
//! is exactly one of:
//!
//! | status      | meaning                                                    |
//! |-------------|------------------------------------------------------------|
//! | `completed` | process exited 0                                           |
//! | `failed`    | nonzero exit, spawn failure, or abnormal supervision end   |
//! | `cancelled` | [`RunRegistry::cancel`] (or the `cancel_run` command) fired |
//! | `timeout`   | the FR-5.3 timeout elapsed and the process tree was killed |
//!
//! Flow beads should treat `failed`/`timeout` uniformly as the FR-5.3 error
//! class (thread status → `error`, log viewable, retry affordance) — the
//! distinction is kept in the row/event so the UI can say *why*.
//!
//! Completion hooks for the flow beads, in preference order:
//! 1. await [`StartedRun::finished`] (a oneshot resolved *after* the row is
//!    terminal and `run-finished` was emitted) and apply side effects there;
//! 2. or listen for the `run-finished` Tauri event (what the UI beads do).
//!
//! # Events (documented in docs/api.md)
//!
//! - `run-progress` `{ run_id, thread_id, kind, detail }` — one per stdout
//!   line. The claude adapter emits `--output-format stream-json` (one JSON
//!   object per line); `kind` is that object's raw `type` field (`"output"`
//!   for non-JSON lines) and `detail` is its `subtype` when present, else the
//!   truncated raw line. Deliberately under-parsed (the bead's contract):
//!   richer rendering belongs to the UI beads. stderr lines go to the log
//!   only.
//! - `run-finished` `{ run_id, thread_id, status }` — exactly once, after the
//!   DB row reached its terminal state.
//!
//! # Process management
//!
//! The child is spawned with `process_group(0)` so it *leads its own process
//! group* (pgid == pid); cancel/timeout then `SIGKILL` the **negative pgid**,
//! reaping the whole tree — claude spawns subprocesses, and SIGKILL (unlike
//! TERM) cannot be ignored. "Cancel means cancel": no graceful-TERM phase; a
//! headless run has no state worth flushing and FR-4.8 wants the kill prompt.
//! `kill_on_drop(true)` stays on as the app-quit backstop (it only reaches
//! the direct child, which is why the group kill exists for the deliberate
//! paths).
//!
//! # Crash resilience (N4)
//!
//! - The supervision task is two-layered: the inner task does the fallible
//!   streaming/waiting; the outer task treats an inner panic or I/O error as
//!   `failed`, appends an `[run] ABNORMAL END: …` marker to the log, and
//!   still finalizes the row.
//! - [`reconcile_stale_runs`] runs in the app `setup` path (before the
//!   [`RunRegistry`] is managed): any `running` row left by a crashed
//!   previous session is marked `failed` and its log gets a trailing marker —
//!   a crashed run never wedges the FR-4.9 per-thread guard or corrupts
//!   thread state.
//! - The registry (in-memory) is the source of truth for *liveness*; the DB
//!   row is history. `start_run` reserves the thread in the registry first
//!   (closing the TOCTOU between two concurrent starts) and double-checks the
//!   DB `running` rows as a belt.
//!
//! # Log format (`runs/<run-id>.log`, §5.6)
//!
//! Line-oriented, tagged: `[out] …` / `[err] …` for the interleaved child
//! streams, `[run] …` for engine lifecycle markers (start, timeout, exit,
//! finalization, abnormal ends). Plain appends — atomicity is not required
//! for logs (the bead's contract), only that a terminal marker always lands.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::artifacts;
use crate::db::{self, DbHandle};
use crate::routing;
use crate::settings::{self, Purpose, RunOverride, SettingsError};

/// How long after a kill we keep draining the (should-be-closing) streams /
/// waiting for the exit status before declaring the run abandoned. Purely
/// defensive: a SIGKILLed process group cannot linger, but a process that
/// escaped the group (e.g. a double-fork daemon holding the pipe) must not
/// wedge the supervisor — N4 demands the row always goes terminal.
const DRAIN_GRACE: Duration = Duration::from_secs(5);
const REAP_GRACE: Duration = Duration::from_secs(10);

/// Max characters of a raw line forwarded as `run-progress.detail`.
const DETAIL_MAX_CHARS: usize = 200;

// ---------------------------------------------------------------------------
// Public data model
// ---------------------------------------------------------------------------

/// What kind of run this is — maps 1:1 onto the `follow_up_runs.mode` CHECK
/// (`'answer' | 'apply' | 'ask'`, §4) and selects the per-purpose model (§5.5).
///
/// `Ask` is the in-app "new thread" question flow (bead `conceptify-959.1`),
/// added once the ask-mode migration (bead `conceptify-iho`) widened the CHECK
/// to admit `'ask'`. The engine is mode-agnostic: the variant plus its two
/// match arms below are the whole engine-side change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Batch sidebar answers (FR-4.6) — answers land in comments, artifact
    /// untouched.
    Answer,
    /// Apply-to-artifact (FR-4.7) — the agent publishes a new version.
    Apply,
    /// In-app ask (FR-5.1) — a fresh question composed inside Conceptify,
    /// answered into a new thread's initial artifact.
    // Constructed by the in-app ask flow (bead conceptify-959.1); until that
    // lands, only this module's tests build it — same holding pattern as the
    // `active_run_for_thread` wrapper above.
    #[allow(dead_code)]
    Ask,
}

impl RunMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RunMode::Answer => "answer",
            RunMode::Apply => "apply",
            RunMode::Ask => "ask",
        }
    }

    /// Which per-purpose model (§5.5) this mode burns.
    pub fn purpose(self) -> Purpose {
        match self {
            RunMode::Answer => Purpose::FollowUp,
            RunMode::Apply => Purpose::ArtifactUpdate,
            RunMode::Ask => Purpose::InAppAsk,
        }
    }
}

/// Terminal (and initial) states of a run row. `follow_up_runs.status` is
/// free-form TEXT by design (see the migration's doc comment); this enum is
/// the authoritative value set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
            RunStatus::TimedOut => "timeout",
        }
    }
}

/// Request for [`start_run`]. The prompt arrives fully assembled — prompt
/// building from thread context is the flow beads' job (via
/// `context::thread_context`), not the engine's.
#[derive(Debug, Clone)]
pub struct StartRun {
    pub thread_id: String,
    pub mode: RunMode,
    pub prompt: String,
    /// Environment overrides applied on top of the inherited env (the engine
    /// stays policy-free: *what* to override is the flows' decision). The
    /// flow layer (`crate::flows`) uses this to hand the child a `PATH` that
    /// contains the `conceptify` CLI — a Finder-launched GUI app inherits a
    /// minimal `PATH` (PRD §5.1), and every headless run's contract is to
    /// report back through the CLI.
    pub env: Vec<(String, String)>,
    /// Optional per-run adapter/model override (epic `conceptify-e7m`). `None`
    /// (or an all-`None` override) means "use the configured defaults" —
    /// byte-identical to the pre-override behavior. When set, the engine
    /// resolves the invocation through it, records the resolved `agent`/`model`
    /// on the row, and persists the override itself in `override_json` so a
    /// retry can re-apply it (bead `conceptify-e7m.1`).
    pub run_override: Option<RunOverride>,
}

/// Handle returned by [`start_run`]. Dropping it does **not** affect the run
/// (the supervisor owns the child); `finished` is the flow beads' completion
/// hook — it resolves after the DB row is terminal and `run-finished` was
/// emitted, so side effects applied there (FR-5.3 thread `error` status,
/// post-apply refresh, …) never observe a non-terminal row.
#[derive(Debug)]
pub struct StartedRun {
    pub run_id: String,
    pub thread_id: String,
    pub finished: oneshot::Receiver<FinishedRun>,
}

/// Terminal outcome delivered through [`StartedRun::finished`].
#[derive(Debug, Clone)]
pub struct FinishedRun {
    pub run_id: String,
    pub thread_id: String,
    pub status: RunStatus,
    /// The process exit code when there was one (`None` for kills/spawn
    /// failures/abandonment).
    pub exit_code: Option<i32>,
    /// Absolute path of `runs/<run-id>.log` — FR-4.8/FR-5.3 surface the tail
    /// of this on failure.
    pub log_path: PathBuf,
}

/// Errors from starting or cancelling a run.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("thread not found: {0}")]
    ThreadNotFound(String),

    /// The FR-4.9 concurrency guard: one active run per thread.
    #[error("thread {thread_id} already has an active run ({run_id})")]
    AlreadyRunning { thread_id: String, run_id: String },

    /// Cancel target is not in the live registry (already finished, or never
    /// existed).
    #[error("run {0} is not active")]
    NotActive(String),

    #[error("run working directory does not exist: {0} (re-map the project?)")]
    CwdMissing(String),

    #[error(transparent)]
    Settings(#[from] SettingsError),

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Registry (managed state) — liveness source of truth, FR-4.9 guard
// ---------------------------------------------------------------------------

/// One live run's control block.
struct ActiveRun {
    thread_id: String,
    /// Set right after spawn; `None` only in the tiny reserve→spawn window.
    pid: Option<u32>,
    /// Cancel latch: set by [`RunRegistry::cancel`], read by the supervisor
    /// to pick `cancelled` over `failed` when the killed child exits.
    cancel_requested: Arc<AtomicBool>,
}

/// In-memory map `run_id → ActiveRun`, held in Tauri managed state
/// (`app.manage(RunRegistry::default())` in `lib.rs`). Source of truth for
/// *liveness* — the DB rows are history. Cheap to clone (Arc inside).
#[derive(Clone, Default)]
pub struct RunRegistry {
    inner: Arc<Mutex<HashMap<String, ActiveRun>>>,
}

impl RunRegistry {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, ActiveRun>> {
        // A poisoned lock only means a panic elsewhere while holding it; the
        // map itself is always structurally valid.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Atomically claim the per-thread slot (FR-4.9). Checking and inserting
    /// under one lock closes the race between two concurrent `start_run`s on
    /// the same thread. Returns the run's cancel latch.
    fn reserve(&self, run_id: &str, thread_id: &str) -> Result<Arc<AtomicBool>, RunError> {
        let mut map = self.lock();
        if let Some((existing, _)) = map.iter().find(|(_, a)| a.thread_id == thread_id) {
            return Err(RunError::AlreadyRunning {
                thread_id: thread_id.to_owned(),
                run_id: existing.clone(),
            });
        }
        let flag = Arc::new(AtomicBool::new(false));
        map.insert(
            run_id.to_owned(),
            ActiveRun {
                thread_id: thread_id.to_owned(),
                pid: None,
                cancel_requested: flag.clone(),
            },
        );
        Ok(flag)
    }

    fn set_pid(&self, run_id: &str, pid: Option<u32>) {
        if let Some(active) = self.lock().get_mut(run_id) {
            active.pid = pid;
        }
    }

    fn remove(&self, run_id: &str) {
        self.lock().remove(run_id);
    }

    /// The live run for `thread_id`, if any (FR-4.9 guard / FR-4.8 UI).
    pub fn active_run_for_thread(&self, thread_id: &str) -> Option<String> {
        self.lock()
            .iter()
            .find(|(_, a)| a.thread_id == thread_id)
            .map(|(id, _)| id.clone())
    }

    /// Cancel a live run: latch the cancel flag and SIGKILL its process
    /// group. Idempotent while the run is still finalizing; `NotActive` once
    /// it left the registry. The DB transition to `cancelled` (and the
    /// `run-finished` event) is done by the supervisor when the killed child
    /// exits — never here — so there is exactly one finalization path.
    pub fn cancel(&self, run_id: &str) -> Result<(), RunError> {
        let map = self.lock();
        let Some(active) = map.get(run_id) else {
            return Err(RunError::NotActive(run_id.to_owned()));
        };
        active.cancel_requested.store(true, Ordering::SeqCst);
        if let Some(pid) = active.pid {
            kill_group(pid);
        }
        // pid == None: cancel raced the spawn; start_run re-checks the latch
        // right after registering the pid and kills then.
        Ok(())
    }
}

/// Convenience wrapper over the managed registry for flow beads holding an
/// `AppHandle`.
// The current flows (b12.4–b12.6) reach the registry through managed state /
// `flows::active_run_summary` instead; this wrapper stays for the in-app ask
// flow (bead 959.1) and is exercised by this module's tests.
#[allow(dead_code)]
pub fn active_run_for_thread<R: Runtime>(
    app_handle: &AppHandle<R>,
    thread_id: &str,
) -> Option<String> {
    app_handle
        .state::<RunRegistry>()
        .active_run_for_thread(thread_id)
}

/// Cancel entry point for the frontend (bead b12.6's cancel button —
/// FR-4.8). Thin wrapper over [`RunRegistry::cancel`]; Rust-side callers use
/// the registry directly.
#[tauri::command(rename_all = "snake_case")]
pub fn cancel_run(
    registry: tauri::State<'_, RunRegistry>,
    run_id: String,
) -> Result<(), String> {
    registry.cancel(&run_id).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Boot reconciliation (N4)
// ---------------------------------------------------------------------------

/// Mark every `running` run row `failed` — called once at startup (lib.rs
/// `setup`, before the registry is managed), when no run can actually be
/// live: a `running` row can only be leftover from a crashed/killed previous
/// session. Appends a trailing `ABNORMAL END` marker to each run's log
/// (best-effort) so the transcript records why it never finished. Returns
/// how many rows were reconciled.
pub fn reconcile_stale_runs(conn: &Connection) -> Result<usize, rusqlite::Error> {
    let stale: Vec<(String, String)> = conn
        .prepare("SELECT id, log_path FROM follow_up_runs WHERE status = 'running'")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    if stale.is_empty() {
        return Ok(0);
    }

    conn.execute(
        "UPDATE follow_up_runs
         SET status = 'failed',
             finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE status = 'running'",
        [],
    )?;

    for (id, log_path) in &stale {
        append_log(
            Path::new(log_path),
            &format!(
                "[run] ABNORMAL END: run {id} was still 'running' at app startup \
                 (previous session crashed or was killed); marked failed"
            ),
        );
    }
    Ok(stale.len())
}

// ---------------------------------------------------------------------------
// start_run
// ---------------------------------------------------------------------------

/// Everything loaded from the DB in one lock before spawning.
struct Loaded {
    project_id: String,
    root_path: String,
    slug: String,
    settings: settings::AgentSettings,
    /// The stored OpenRouter key (bead e7m.7), consumed only by an
    /// openrouter-routed run. **Secret**: reaches the child exclusively via
    /// `Command::env`; never logged, never persisted on the row, never part of
    /// an error/event (test-proven below).
    openrouter_key: Option<String>,
}

/// Start a headless agent run for a thread (PRD §5.1, §5.5 surface 2).
///
/// Sequence: registry reservation (FR-4.9 guard, atomic) → thread/project +
/// settings/OpenRouter-key load + DB `running` double-check (one connection
/// lock) → provider routing (`crate::routing`, bead e7m.7: model → adapter +
/// per-run env + route tag; missing-key/unroutable-model fail fast here) →
/// invocation resolution (pure) + binary lookup (cached login-shell `which`)
/// → `follow_up_runs` row inserted (`running`) → child spawned
/// (`process_group(0)`, `kill_on_drop`, cwd = adapter template's cwd —
/// project root by default) → supervisor task takes over. Any failure past
/// the row insert marks the row `failed` before returning the error; any
/// failure at all releases the reservation.
///
/// Requires `DbHandle` and [`RunRegistry`] in managed state.
pub async fn start_run<R: Runtime>(
    app_handle: &AppHandle<R>,
    req: StartRun,
) -> Result<StartedRun, RunError> {
    let db = app_handle.state::<DbHandle>().inner().clone();
    let registry = app_handle.state::<RunRegistry>().inner().clone();

    let run_id = uuid::Uuid::new_v4().to_string();
    let cancel_flag = registry.reserve(&run_id, &req.thread_id)?;

    match start_reserved(app_handle, &db, &registry, &run_id, &cancel_flag, req).await {
        Ok(started) => Ok(started),
        Err(e) => {
            // Every failure path releases the FR-4.9 slot; row cleanup (if it
            // was inserted) already happened inside start_reserved.
            registry.remove(&run_id);
            Err(e)
        }
    }
}

/// The fallible body of [`start_run`], run while holding a registry
/// reservation the caller releases on error.
async fn start_reserved<R: Runtime>(
    app_handle: &AppHandle<R>,
    db: &DbHandle,
    registry: &RunRegistry,
    run_id: &str,
    cancel_flag: &Arc<AtomicBool>,
    req: StartRun,
) -> Result<StartedRun, RunError> {
    // -- Load thread/project + settings, and belt-check the DB for a running
    //    row (the registry reservation is the real guard; after boot
    //    reconciliation a 'running' row can only belong to a live run of this
    //    process, which the reservation already caught).
    let thread_id = req.thread_id.clone();
    let loaded = db::with_conn_result(db, move |conn| -> Result<Loaded, RunError> {
        let row = conn
            .query_row(
                "SELECT p.id, p.root_path, t.slug
                 FROM threads t JOIN projects p ON p.id = t.project_id
                 WHERE t.id = ?1",
                [&thread_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((project_id, root_path, slug)) = row else {
            return Err(RunError::ThreadNotFound(thread_id));
        };

        if let Some(existing) = conn
            .query_row(
                "SELECT id FROM follow_up_runs
                 WHERE thread_id = ?1 AND status = 'running' LIMIT 1",
                [&thread_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return Err(RunError::AlreadyRunning {
                thread_id,
                run_id: existing,
            });
        }

        let settings = settings::get_settings(conn)?;
        let openrouter_key = settings::get_openrouter_api_key(conn)?;
        Ok(Loaded {
            project_id,
            root_path,
            slug,
            settings,
            openrouter_key,
        })
    })
    .await?;

    // -- Route, then resolve (bead conceptify-e7m.7). Routing derives the
    //    (adapter, model, env, tag) from the chosen model's provider — or
    //    passes the user's explicit adapter choice through untouched (manual
    //    bypass) — and fails fast BEFORE any row exists on a missing
    //    OpenRouter key or an unroutable model, exactly like the
    //    unknown-adapter/bad-model validation below it. The catalog lookup is
    //    disk-only (cache/snapshot), never the network. Resolution then stays
    //    the pure, injection-safe template expansion (see settings.rs): the
    //    routed selection is fed through the same override mechanism, so an
    //    override-free anthropic-routed run is byte-identical to the
    //    pre-routing invocation by construction.
    let purpose = req.mode.purpose();
    let over = req.run_override.as_ref();
    let route = routing::route_run(
        &loaded.settings,
        purpose,
        over,
        crate::catalog::provider_of,
        loaded.openrouter_key.as_deref(),
    )?;
    let routed_selection = RunOverride {
        adapter: Some(route.adapter.clone()),
        model: Some(route.model.clone()),
    };
    let invocation = loaded.settings.resolve_with_override(
        purpose,
        Path::new(&loaded.root_path),
        &req.prompt,
        Some(&routed_selection),
    )?;
    let program = {
        let command = invocation.program.clone();
        let override_path = loaded.settings.agent_binary_path.clone();
        tokio::task::spawn_blocking(move || {
            settings::resolve_agent_binary(&command, override_path.as_deref())
        })
        .await
        .expect("agent binary lookup task panicked")?
    };
    if !Path::new(&invocation.cwd).is_dir() {
        // Fail with a pointed error before any row exists: a missing project
        // root is an FR-1.3 re-map situation, not a run failure.
        return Err(RunError::CwdMissing(invocation.cwd));
    }

    // The RESOLVED adapter key + model actually used (honoring override +
    // routing), so the row honestly records what ran rather than the bare
    // defaults. The route tag is recorded alongside (token-free — route
    // visibility, bead e7m.7).
    let (agent, model) = (route.adapter.clone(), route.model.clone());
    // The override INTENT persisted on the row (NULL for an override-free run),
    // so retry re-applies a real override but re-derives current defaults for a
    // run that had none. `is_empty()` collapses an all-None override to NULL.
    let override_json: Option<String> = match over {
        Some(o) if !o.is_empty() => {
            Some(serde_json::to_string(o).expect("RunOverride always serializes"))
        }
        _ => None,
    };
    let timeout = Duration::from_secs(loaded.settings.timeout_secs.max(1));

    // -- Log file under the thread's artifact dir (§5.6).
    let artifacts_root = artifacts::artifacts_root()?;
    let log_path =
        artifacts::run_log_path(&artifacts_root, &loaded.project_id, &loaded.slug, run_id);
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // -- Row first (status running), then spawn: an attempted-but-unspawnable
    //    run is honest history (marked failed below), and the reverse order
    //    could leave a live process with no row.
    {
        let (run_id, thread_id, agent, model, mode, log_path_str, override_json, route_tag) = (
            run_id.to_owned(),
            req.thread_id.clone(),
            agent.clone(),
            model.clone(),
            req.mode.as_str(),
            log_path.to_string_lossy().into_owned(),
            override_json,
            route.tag.as_str(),
        );
        db::with_conn(db, move |conn| {
            conn.execute(
                "INSERT INTO follow_up_runs
                     (id, thread_id, agent, model, mode, status, log_path, override_json, route)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6, ?7, ?8)",
                rusqlite::params![
                    run_id,
                    thread_id,
                    agent,
                    model,
                    mode,
                    log_path_str,
                    override_json,
                    route_tag
                ],
            )
        })
        .await?;
    }

    // Route visibility in the log header (bead e7m.7): tag + base-url note,
    // NEVER env values — the OpenRouter token must not appear in any logged or
    // persisted representation of the invocation (test-proven below).
    let route_note = match route.tag {
        routing::RouteTag::Openrouter => {
            format!(" route=openrouter base_url={}", routing::OPENROUTER_BASE_URL)
        }
        tag => format!(" route={}", tag.as_str()),
    };
    append_log(
        &log_path,
        &format!(
            "[run] started {run_id} at {} mode={} agent={agent} model={model}{route_note} program={} cwd={} timeout={}s prompt_chars={}",
            now_iso(),
            req.mode.as_str(),
            program.display(),
            invocation.cwd,
            timeout.as_secs(),
            req.prompt.chars().count(),
        ),
    );

    // -- Spawn. Direct exec of the resolved argv — no shell anywhere near the
    //    prompt (PRD §9 S3; see settings.rs substitution safety).
    let mut cmd = tokio::process::Command::new(&program);
    cmd.args(&invocation.args)
        .current_dir(&invocation.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (key, value) in &req.env {
        cmd.env(key, value);
    }
    // Route env last (last-writer-wins over the flow env): the OpenRouter
    // route's ANTHROPIC_* triple, empty for every other route. Values may be
    // secrets — they go ONLY into the child's env, never into logs/rows/events.
    for (key, value) in &route.env {
        cmd.env(key, value);
    }
    #[cfg(unix)]
    cmd.process_group(0); // child leads its own group → pgid == pid

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            append_log(
                &log_path,
                &format!(
                    "[run] ABNORMAL END: failed to spawn '{}': {e}",
                    program.display()
                ),
            );
            mark_run_failed(db, run_id).await;
            return Err(RunError::Io(e));
        }
    };

    let pid = child.id();
    registry.set_pid(run_id, pid);
    // Close the reserve→spawn cancel race: if cancel() latched the flag while
    // pid was still None, deliver the kill now.
    if cancel_flag.load(Ordering::SeqCst) {
        if let Some(pid) = pid {
            kill_group(pid);
        }
    }

    let (done_tx, done_rx) = oneshot::channel();
    let ctx = RunCtx {
        app_handle: app_handle.clone(),
        db: db.clone(),
        registry: registry.clone(),
        run_id: run_id.to_owned(),
        thread_id: req.thread_id.clone(),
        log_path: log_path.clone(),
        cancel_flag: cancel_flag.clone(),
    };
    spawn_supervisor(ctx, child, timeout, done_tx);

    Ok(StartedRun {
        run_id: run_id.to_owned(),
        thread_id: req.thread_id,
        finished: done_rx,
    })
}

/// Best-effort `running → failed` for rows whose process never (properly)
/// started. Guarded on `status = 'running'` so it can never regress an
/// already-terminal row.
async fn mark_run_failed(db: &DbHandle, run_id: &str) {
    let run_id = run_id.to_owned();
    let res = db::with_conn(db, move |conn| {
        conn.execute(
            "UPDATE follow_up_runs
             SET status = 'failed',
                 finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1 AND status = 'running'",
            [&run_id],
        )
    })
    .await;
    if let Err(e) = res {
        eprintln!("[conceptify-runs] failed to mark run failed: {e}");
    }
}

// ---------------------------------------------------------------------------
// Supervision
// ---------------------------------------------------------------------------

/// Everything the supervisor/finalizer needs. Cloned into the inner task.
struct RunCtx<R: Runtime> {
    app_handle: AppHandle<R>,
    db: DbHandle,
    registry: RunRegistry,
    run_id: String,
    thread_id: String,
    log_path: PathBuf,
    cancel_flag: Arc<AtomicBool>,
}

// Manual impl: `#[derive(Clone)]` would demand `R: Clone`, but `AppHandle<R>`
// is unconditionally cloneable (same pattern as `server::ApiState`).
impl<R: Runtime> Clone for RunCtx<R> {
    fn clone(&self) -> Self {
        RunCtx {
            app_handle: self.app_handle.clone(),
            db: self.db.clone(),
            registry: self.registry.clone(),
            run_id: self.run_id.clone(),
            thread_id: self.thread_id.clone(),
            log_path: self.log_path.clone(),
            cancel_flag: self.cancel_flag.clone(),
        }
    }
}

#[derive(Serialize, Clone)]
struct RunProgressEvent<'a> {
    run_id: &'a str,
    thread_id: &'a str,
    kind: &'a str,
    detail: &'a str,
}

#[derive(Serialize, Clone)]
struct RunFinishedEvent<'a> {
    run_id: &'a str,
    thread_id: &'a str,
    status: &'a str,
}

/// What the inner supervision loop observed.
struct SupOutcome {
    timed_out: bool,
    exit_code: Option<i32>,
    exit_success: bool,
}

/// Two-layer supervision (N4): the inner task streams/waits and can fail or
/// panic; the outer task maps *any* inner outcome — including a panic — to a
/// terminal status and always finalizes (DB row, registry slot, log marker,
/// `run-finished`, oneshot). Nothing after spawn can leave the row `running`
/// while the app lives.
fn spawn_supervisor<R: Runtime>(
    ctx: RunCtx<R>,
    child: tokio::process::Child,
    timeout: Duration,
    done_tx: oneshot::Sender<FinishedRun>,
) {
    tauri::async_runtime::spawn(async move {
        let pid = child.id();
        let inner_ctx = ctx.clone();
        let inner =
            tauri::async_runtime::spawn(async move { supervise(inner_ctx, child, timeout).await });

        let (status, exit_code) = match inner.await {
            Ok(Ok(out)) => {
                // Cancel wins over everything: the kill it delivered is what
                // made the child exit (and even in the exit-vs-cancel photo
                // finish, the user asked for cancelled and should read
                // cancelled).
                let status = if ctx.cancel_flag.load(Ordering::SeqCst) {
                    RunStatus::Cancelled
                } else if out.timed_out {
                    RunStatus::TimedOut
                } else if out.exit_success {
                    RunStatus::Completed
                } else {
                    RunStatus::Failed
                };
                (status, out.exit_code)
            }
            Ok(Err(e)) => {
                append_log(
                    &ctx.log_path,
                    &format!("[run] ABNORMAL END: supervision I/O error: {e}"),
                );
                if let Some(pid) = pid {
                    kill_group(pid);
                }
                (RunStatus::Failed, None)
            }
            // JoinError: the inner task panicked. The child was moved into
            // it, so the panic dropped it → kill_on_drop already delivered a
            // SIGKILL to the direct child; the group kill sweeps any
            // grandchildren.
            Err(e) => {
                append_log(
                    &ctx.log_path,
                    &format!("[run] ABNORMAL END: supervision task panicked: {e}"),
                );
                if let Some(pid) = pid {
                    kill_group(pid);
                }
                (RunStatus::Failed, None)
            }
        };

        finalize(ctx, status, exit_code, done_tx).await;
    });
}

/// Stream both pipes into the log (and stdout lines into `run-progress`
/// events), enforce the timeout, and reap the exit status.
async fn supervise<R: Runtime>(
    ctx: RunCtx<R>,
    mut child: tokio::process::Child,
    timeout: Duration,
) -> std::io::Result<SupOutcome> {
    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let (tx, mut rx) = mpsc::unbounded_channel::<(bool, String)>();
    if let Some(stdout) = stdout {
        spawn_line_reader(stdout, tx.clone(), false);
    }
    if let Some(stderr) = stderr {
        spawn_line_reader(stderr, tx, true);
    }
    // (tx clones now live only in the readers: rx closes when both streams
    // reach EOF — i.e. when the whole process group is dead or done.)

    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&ctx.log_path)?;

    let sleep = tokio::time::sleep(timeout);
    tokio::pin!(sleep);
    let mut timed_out = false;

    loop {
        tokio::select! {
            maybe_line = rx.recv() => match maybe_line {
                Some((is_err, line)) => {
                    let tag = if is_err { "[err]" } else { "[out]" };
                    let _ = writeln!(log, "{tag} {line}");
                    if !is_err {
                        let (kind, detail) = classify_line(&line);
                        let _ = ctx.app_handle.emit(
                            "run-progress",
                            &RunProgressEvent {
                                run_id: &ctx.run_id,
                                thread_id: &ctx.thread_id,
                                kind: &kind,
                                detail: &detail,
                            },
                        );
                    }
                }
                None => break, // both streams EOF
            },
            _ = &mut sleep => {
                if !timed_out {
                    timed_out = true;
                    let _ = writeln!(
                        log,
                        "[run] timeout after {}s — killing process group",
                        timeout.as_secs()
                    );
                    if let Some(pid) = pid {
                        kill_group(pid);
                    }
                    // Give the (SIGKILLed) streams a bounded window to close.
                    sleep.as_mut().reset(tokio::time::Instant::now() + DRAIN_GRACE);
                } else {
                    // Something outside the group still holds the pipe; do
                    // not let it wedge the supervisor.
                    let _ = writeln!(log, "[run] stream drain forced shutdown");
                    break;
                }
            }
        }
    }

    match tokio::time::timeout(REAP_GRACE, child.wait()).await {
        Ok(Ok(exit)) => {
            let _ = writeln!(log, "[run] process exited: {exit}");
            Ok(SupOutcome {
                timed_out,
                exit_code: exit.code(),
                exit_success: exit.success(),
            })
        }
        Ok(Err(e)) => Err(e),
        Err(_elapsed) => {
            // Unreapable child (shouldn't happen post-SIGKILL). Dropping it
            // re-delivers a kill via kill_on_drop; report a failure-class
            // outcome rather than hanging (N4).
            let _ = writeln!(
                log,
                "[run] process did not exit within {}s of kill; abandoning (kill_on_drop)",
                REAP_GRACE.as_secs()
            );
            Ok(SupOutcome {
                timed_out: true,
                exit_code: None,
                exit_success: false,
            })
        }
    }
}

/// Forward every line of one child stream into the funnel channel.
fn spawn_line_reader<S>(stream: S, tx: mpsc::UnboundedSender<(bool, String)>, is_err: bool)
where
    S: AsyncRead + Unpin + Send + 'static,
{
    tauri::async_runtime::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send((is_err, line)).is_err() {
                break; // supervisor gone
            }
        }
    });
}

/// Single finalization path for every run: persist the terminal status
/// (before releasing the registry slot, so the FR-4.9 guard and the DB never
/// disagree in the observable order), free the slot, append the trailing log
/// marker, emit `run-finished`, resolve the flow hook.
async fn finalize<R: Runtime>(
    ctx: RunCtx<R>,
    status: RunStatus,
    exit_code: Option<i32>,
    done_tx: oneshot::Sender<FinishedRun>,
) {
    let run_id = ctx.run_id.clone();
    let persisted = db::with_conn(&ctx.db, move |conn| {
        conn.execute(
            "UPDATE follow_up_runs
             SET status = ?1,
                 finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?2",
            rusqlite::params![status.as_str(), run_id],
        )
    })
    .await;
    if let Err(e) = persisted {
        // The row stays 'running' until boot reconciliation; the log marker
        // below still records the truth. Nothing else useful can be done.
        eprintln!(
            "[conceptify-runs] failed to persist terminal status for {}: {e}",
            ctx.run_id
        );
        append_log(
            &ctx.log_path,
            &format!(
                "[run] WARNING: failed to persist terminal status '{}': {e}",
                status.as_str()
            ),
        );
    }

    ctx.registry.remove(&ctx.run_id);
    append_log(
        &ctx.log_path,
        &format!("[run] finalized: {} at {}", status.as_str(), now_iso()),
    );
    let _ = ctx.app_handle.emit(
        "run-finished",
        &RunFinishedEvent {
            run_id: &ctx.run_id,
            thread_id: &ctx.thread_id,
            status: status.as_str(),
        },
    );
    let _ = done_tx.send(FinishedRun {
        run_id: ctx.run_id,
        thread_id: ctx.thread_id,
        status,
        exit_code,
        log_path: ctx.log_path,
    });
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// SIGKILL the whole process group led by `pid` (spawned with
/// `process_group(0)`, so pgid == pid). SIGKILL because it cannot be ignored
/// — the fake-agent tests include a TERM-trapping grandchild for exactly this
/// reason — and a cancelled/timed-out headless run has nothing to flush.
#[cfg(unix)]
fn kill_group(pid: u32) {
    // SAFETY: plain syscall; a stale/reused pgid in the worst case delivers a
    // kill to a group we no longer own, which the OS permission check gates.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_group(_pid: u32) {
    // Non-unix: rely on kill_on_drop (direct child only). The app ships on
    // macOS; this stub only keeps the crate compiling elsewhere.
}

/// Minimal stream-json classification (deliberately shallow — the bead's
/// contract): `kind` = the line's JSON `type` (or `"output"` for non-JSON),
/// `detail` = its `subtype` when present, else the truncated raw line.
///
/// One structured special case (bead `conceptify-pri`): a `rate_limit_event`
/// carries no `subtype`, so the generic path would forward the truncated raw
/// JSON line — which surfaced as scary, half-cut noise in the progress feed
/// even though almost every such event is a purely informational
/// `status: "allowed"` heartbeat. Its actionability lives in the nested
/// `rate_limit_info` object (`status` / `isUsingOverage` / `resetsAt`), so we
/// forward *that* sub-object as compact JSON. The decision to show or hide it
/// (and how to phrase genuine limiting) stays in the frontend — the single
/// place run-progress display policy lives — which can parse this cleanly
/// instead of a truncated line. Falls back to the generic path if the field
/// is absent (unexpected shape).
fn classify_line(line: &str) -> (String, String) {
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(value) => {
            let kind = value
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("output")
                .to_owned();
            if kind == "rate_limit_event" {
                if let Some(info) = value.get("rate_limit_info") {
                    if let Ok(compact) = serde_json::to_string(info) {
                        return (kind, truncate_chars(&compact, DETAIL_MAX_CHARS));
                    }
                }
            }
            let detail = value
                .get("subtype")
                .and_then(|s| s.as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| truncate_chars(line, DETAIL_MAX_CHARS));
            (kind, detail)
        }
        Err(_) => ("output".to_owned(), truncate_chars(line, DETAIL_MAX_CHARS)),
    }
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_owned()
    } else {
        let cut: String = s.chars().take(max_chars).collect();
        format!("{cut}…")
    }
}

/// Append one line to a run log, creating the file if needed. Best-effort by
/// design: a log write must never take down a run (the DB row is the source
/// of truth for status; the log is the debugging transcript).
fn append_log(path: &Path, line: &str) {
    let res = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| writeln!(f, "{line}"));
    if let Err(e) = res {
        eprintln!(
            "[conceptify-runs] failed to append to {}: {e}",
            path.display()
        );
    }
}

fn now_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex as StdMutex;

    use tauri::test::{mock_builder, mock_context, noop_assets, MockRuntime};
    use tauri::Listener;

    use crate::settings::{Adapter, AgentSettings};

    /// The one shared per-process scratch artifacts root (bead
    /// `conceptify-028`). Delegates to `artifacts::test_artifacts_root`, the
    /// single source of truth that `artifacts::artifacts_root` also resolves to
    /// in test builds — so the run engine's own `artifacts_root()` call and this
    /// harness's `Drop` cleanup can never disagree (the leak that dumped
    /// `proj-*` dirs into the real ~/Documents). Isolation comes from unique
    /// per-test project ids under this root.
    fn shared_artifacts_root() -> std::path::PathBuf {
        crate::artifacts::test_artifacts_root()
    }

    struct Harness {
        handle: AppHandle<MockRuntime>,
        db: DbHandle,
        db_path: PathBuf,
        work_dir: PathBuf, // project root (cwd) + scripts + pidfiles
        project_id: String,
        thread_id: String,
        progress: Arc<StdMutex<Vec<serde_json::Value>>>,
        finished_events: Arc<StdMutex<Vec<serde_json::Value>>>,
        _app: tauri::App<MockRuntime>,
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.work_dir);
            let _ = std::fs::remove_dir_all(shared_artifacts_root().join(&self.project_id));
            let _ = std::fs::remove_file(&self.db_path);
            let _ = std::fs::remove_file(self.db_path.with_extension("db-wal"));
            let _ = std::fs::remove_file(self.db_path.with_extension("db-shm"));
        }
    }

    fn harness(tag: &str) -> Harness {
        let unique = format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let db_path = std::env::temp_dir().join(format!("conceptify-test-runs-{unique}.db"));
        let work_dir = std::env::temp_dir().join(format!("conceptify-test-runs-wd-{unique}"));
        std::fs::create_dir_all(&work_dir).unwrap();
        let project_id = format!("proj-{unique}");

        let db = crate::db::init_at(&db_path).expect("test db should init");
        let thread_id = {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO projects (id, name, root_path) VALUES (?1, 'Proj', ?2)",
                rusqlite::params![project_id, work_dir.to_string_lossy()],
            )
            .unwrap();
            crate::threads::create_thread(&conn, &project_id, "Run Test", "q")
                .unwrap()
                .id
        };

        let app = mock_builder()
            .manage(db.clone())
            .manage(RunRegistry::default())
            .build(mock_context(noop_assets()))
            .expect("mock app");
        let handle = app.handle().clone();

        let progress: Arc<StdMutex<Vec<serde_json::Value>>> = Arc::default();
        let finished_events: Arc<StdMutex<Vec<serde_json::Value>>> = Arc::default();
        {
            let sink = progress.clone();
            handle.listen_any("run-progress", move |event| {
                sink.lock()
                    .unwrap()
                    .push(serde_json::from_str(event.payload()).unwrap());
            });
            let sink = finished_events.clone();
            handle.listen_any("run-finished", move |event| {
                sink.lock()
                    .unwrap()
                    .push(serde_json::from_str(event.payload()).unwrap());
            });
        }

        Harness {
            handle,
            db,
            db_path,
            work_dir,
            project_id,
            thread_id,
            progress,
            finished_events,
            _app: app,
        }
    }

    impl Harness {
        /// Write an executable fake-agent script and point the settings at it
        /// (a `fake` adapter whose only arg is `{prompt}` — tests smuggle
        /// per-run data, like a pidfile path, through the prompt).
        fn install_fake_agent(&self, script_body: &str, timeout_secs: u64) -> PathBuf {
            let script = self.work_dir.join("fake-agent.sh");
            std::fs::write(&script, script_body).unwrap();
            let mut perm = std::fs::metadata(&script).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&script, perm).unwrap();
            self.install_adapter_command(&script.to_string_lossy(), timeout_secs);
            script
        }

        fn install_adapter_command(&self, command: &str, timeout_secs: u64) {
            let mut s = AgentSettings::default();
            s.adapters.insert(
                "fake".to_owned(),
                Adapter {
                    command: command.to_owned(),
                    args: vec!["{prompt}".to_owned()],
                    cwd: "{project_root}".to_owned(),
                },
            );
            s.default_adapter = "fake".to_owned();
            s.timeout_secs = timeout_secs;
            let conn = self.db.lock().unwrap();
            crate::settings::update_settings(&conn, &s).unwrap();
        }

        fn run_row(&self, run_id: &str) -> (String, String, String, Option<String>) {
            let conn = self.db.lock().unwrap();
            conn.query_row(
                "SELECT status, mode, agent, finished_at FROM follow_up_runs WHERE id = ?1",
                [run_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
        }

        fn registry(&self) -> RunRegistry {
            self.handle.state::<RunRegistry>().inner().clone()
        }

        async fn start(&self, mode: RunMode, prompt: &str) -> Result<StartedRun, RunError> {
            self.start_over(mode, prompt, None).await
        }

        async fn start_over(
            &self,
            mode: RunMode,
            prompt: &str,
            run_override: Option<RunOverride>,
        ) -> Result<StartedRun, RunError> {
            start_run(
                &self.handle,
                StartRun {
                    thread_id: self.thread_id.clone(),
                    mode,
                    prompt: prompt.to_owned(),
                    env: Vec::new(),
                    run_override,
                },
            )
            .await
        }

        /// The `(agent, model, override_json)` recorded on a run row — for the
        /// e7m override-persistence assertions.
        fn run_selection(&self, run_id: &str) -> (String, String, Option<String>) {
            let conn = self.db.lock().unwrap();
            conn.query_row(
                "SELECT agent, model, override_json FROM follow_up_runs WHERE id = ?1",
                [run_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
        }

        /// The `route` tag recorded on a run row (bead e7m.7).
        fn run_route(&self, run_id: &str) -> Option<String> {
            let conn = self.db.lock().unwrap();
            conn.query_row(
                "SELECT route FROM follow_up_runs WHERE id = ?1",
                [run_id],
                |r| r.get(0),
            )
            .unwrap()
        }

        /// Every TEXT column of a run row concatenated — the haystack for the
        /// "no secret ever persisted" assertions (bead e7m.7).
        fn run_row_text(&self, run_id: &str) -> String {
            let conn = self.db.lock().unwrap();
            conn.query_row(
                "SELECT id || thread_id || agent || model || mode || status || log_path
                        || COALESCE(override_json,'') || COALESCE(route,'')
                 FROM follow_up_runs WHERE id = ?1",
                [run_id],
                |r| r.get(0),
            )
            .unwrap()
        }

        /// Point a BUILT-IN adapter's command at a fake capture script while
        /// keeping `default_adapter = "claude"` — a ROUTABLE config, so
        /// provider routing engages (unlike `install_fake_agent`, whose custom
        /// default adapter deliberately hits the manual bypass). The script
        /// records its argv and the ANTHROPIC_* env to files in the work dir
        /// (never to stdout/stderr — those land in the run log, and the secret
        /// tests assert the log stays token-free).
        fn install_routed_capture(&self, adapter_key: &str) -> PathBuf {
            let script = self.work_dir.join(format!("fake-{adapter_key}.sh"));
            std::fs::write(
                &script,
                "#!/bin/sh\n\
                 d=\"$(dirname \"$0\")\"\n\
                 printf '%s\\n' \"$@\" > \"$d/argv.txt\"\n\
                 printf 'base=%s\\ntoken=%s\\nkey=<%s>\\nkey_present=%s\\n' \\\n\
                   \"$ANTHROPIC_BASE_URL\" \"$ANTHROPIC_AUTH_TOKEN\" \\\n\
                   \"$ANTHROPIC_API_KEY\" \"${ANTHROPIC_API_KEY+set}\" > \"$d/env.txt\"\n\
                 exit 0\n",
            )
            .unwrap();
            let mut perm = std::fs::metadata(&script).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&script, perm).unwrap();

            let mut s = AgentSettings::default();
            let adapter = s
                .adapters
                .get_mut(adapter_key)
                .expect("built-in adapter key");
            adapter.command = script.to_string_lossy().into_owned();
            adapter.args = vec![
                "--model".to_owned(),
                "{model}".to_owned(),
                "{prompt}".to_owned(),
            ];
            s.timeout_secs = 60;
            let conn = self.db.lock().unwrap();
            crate::settings::update_settings(&conn, &s).unwrap();
            script
        }
    }

    async fn wait_until(mut f: impl FnMut() -> bool, timeout_ms: u64) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
        while std::time::Instant::now() < deadline {
            if f() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        f()
    }

    async fn finished(started: StartedRun) -> FinishedRun {
        tokio::time::timeout(Duration::from_secs(20), started.finished)
            .await
            .expect("run did not finalize within 20s")
            .expect("finished channel dropped without a FinishedRun")
    }

    fn pid_alive(pid: i32) -> bool {
        unsafe { libc::kill(pid, 0) == 0 }
    }

    // -- pure helpers --------------------------------------------------------

    #[test]
    fn classify_line_parses_stream_json_and_falls_back() {
        let (kind, detail) = classify_line(r#"{"type":"system","subtype":"init"}"#);
        assert_eq!(kind, "system");
        assert_eq!(detail, "init");

        let (kind, detail) = classify_line(r#"{"type":"assistant","message":{}}"#);
        assert_eq!(kind, "assistant");
        assert_eq!(detail, r#"{"type":"assistant","message":{}}"#);

        // rate_limit_event: no `subtype`, so `detail` carries the nested
        // `rate_limit_info` as compact JSON (not the truncated raw line) so the
        // frontend can decide whether to surface it (bead conceptify-pri).
        let (kind, detail) = classify_line(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1783222800,"isUsingOverage":false},"uuid":"u","session_id":"s"}"#,
        );
        assert_eq!(kind, "rate_limit_event");
        // serde_json re-serializes `Value` maps with alphabetized keys; the
        // frontend `JSON.parse`s this, so the order is immaterial there.
        assert_eq!(
            detail,
            r#"{"isUsingOverage":false,"resetsAt":1783222800,"status":"allowed"}"#
        );

        // Malformed rate_limit_event (no `rate_limit_info`) falls back to the
        // generic truncated-raw-line path rather than dropping the type.
        let (kind, detail) = classify_line(r#"{"type":"rate_limit_event","oops":true}"#);
        assert_eq!(kind, "rate_limit_event");
        assert_eq!(detail, r#"{"type":"rate_limit_event","oops":true}"#);

        let (kind, detail) = classify_line("plain text noise");
        assert_eq!(kind, "output");
        assert_eq!(detail, "plain text noise");

        // Long lines are truncated (char-safe).
        let long = "x".repeat(500);
        let (_, detail) = classify_line(&long);
        assert!(detail.chars().count() <= DETAIL_MAX_CHARS + 1); // +1 for the ellipsis
        assert!(detail.ends_with('…'));
    }

    #[test]
    fn run_status_strings_are_stable() {
        assert_eq!(RunStatus::Running.as_str(), "running");
        assert_eq!(RunStatus::Completed.as_str(), "completed");
        assert_eq!(RunStatus::Failed.as_str(), "failed");
        assert_eq!(RunStatus::Cancelled.as_str(), "cancelled");
        assert_eq!(RunStatus::TimedOut.as_str(), "timeout");
        assert_eq!(RunMode::Answer.as_str(), "answer");
        assert_eq!(RunMode::Apply.as_str(), "apply");
        assert_eq!(RunMode::Ask.as_str(), "ask");
    }

    #[test]
    fn run_mode_purposes_map_to_settings() {
        assert_eq!(RunMode::Answer.purpose(), Purpose::FollowUp);
        assert_eq!(RunMode::Apply.purpose(), Purpose::ArtifactUpdate);
        // `Ask` -> in-app-ask model bucket (§5.5); its `as_str` must match the
        // migrated `follow_up_runs.mode` CHECK value.
        assert_eq!(RunMode::Ask.purpose(), Purpose::InAppAsk);
        assert_eq!(RunMode::Ask.as_str(), "ask");
    }

    // -- lifecycle -----------------------------------------------------------

    #[tokio::test]
    async fn successful_run_streams_logs_and_completes() {
        let h = harness("ok");
        h.install_fake_agent(
            "#!/bin/sh\n\
             echo '{\"type\":\"system\",\"subtype\":\"init\"}'\n\
             echo '{\"type\":\"result\",\"subtype\":\"success\"}'\n\
             echo 'warn: something odd' >&2\n\
             exit 0\n",
            60,
        );

        let started = h.start(RunMode::Answer, "explain please").await.unwrap();
        let run_id = started.run_id.clone();

        // Row exists as running with the right mode/agent while in flight (it
        // may already be terminal if the script raced us — accept both).
        let (status_now, mode, agent, _) = h.run_row(&run_id);
        assert!(status_now == "running" || status_now == "completed");
        assert_eq!(mode, "answer");
        assert_eq!(agent, "fake");

        let fin = finished(started).await;
        assert_eq!(fin.status, RunStatus::Completed);
        assert_eq!(fin.exit_code, Some(0));
        assert_eq!(fin.thread_id, h.thread_id);

        // Terminal row.
        let (status, _, _, finished_at) = h.run_row(&run_id);
        assert_eq!(status, "completed");
        assert!(finished_at.is_some());

        // Log: header, both streams tagged and interleaved, exit + final
        // marker (full transcript per FR-4.8).
        let log = std::fs::read_to_string(&fin.log_path).unwrap();
        assert!(log.contains(&format!("[run] started {run_id}")), "{log}");
        assert!(log.contains("[out] {\"type\":\"system\""), "{log}");
        assert!(log.contains("[out] {\"type\":\"result\""), "{log}");
        assert!(log.contains("[err] warn: something odd"), "{log}");
        assert!(log.contains("[run] process exited: exit status: 0"), "{log}");
        assert!(log.contains("[run] finalized: completed"), "{log}");

        // Log lives in the thread's artifact dir under runs/ (§5.6).
        assert!(fin
            .log_path
            .to_string_lossy()
            .contains(&format!("{}/threads/", h.project_id)));
        assert!(fin.log_path.to_string_lossy().contains("/runs/"));

        // Events: run-progress only for stdout lines (2), with parsed kinds;
        // run-finished exactly once with terminal status.
        let progress = h.progress.lock().unwrap().clone();
        assert_eq!(progress.len(), 2, "{progress:?}");
        assert_eq!(progress[0]["kind"], "system");
        assert_eq!(progress[0]["detail"], "init");
        assert_eq!(progress[0]["run_id"], run_id.as_str());
        assert_eq!(progress[0]["thread_id"], h.thread_id.as_str());
        assert_eq!(progress[1]["kind"], "result");

        let fin_events = h.finished_events.lock().unwrap().clone();
        assert_eq!(fin_events.len(), 1, "{fin_events:?}");
        assert_eq!(fin_events[0]["status"], "completed");
        assert_eq!(fin_events[0]["run_id"], run_id.as_str());

        // Registry slot freed.
        assert_eq!(h.registry().active_run_for_thread(&h.thread_id), None);
        assert_eq!(active_run_for_thread(&h.handle, &h.thread_id), None);
    }

    #[tokio::test]
    async fn ask_mode_run_records_ask_row_and_completes() {
        // End-to-end proof that the ask-mode migration (bead conceptify-iho)
        // took: the engine's `INSERT ... mode = 'ask'` lands against the real
        // migrated schema (harness uses `db::init_at` → full chain), and the
        // run drives to a terminal `completed` state like any other mode.
        let h = harness("ask");
        h.install_fake_agent(
            "#!/bin/sh\n\
             echo '{\"type\":\"result\",\"subtype\":\"success\"}'\n\
             exit 0\n",
            60,
        );

        let started = h.start(RunMode::Ask, "start a new thread").await.unwrap();
        let run_id = started.run_id.clone();

        let fin = finished(started).await;
        assert_eq!(fin.status, RunStatus::Completed);

        let (status, mode, agent, finished_at) = h.run_row(&run_id);
        assert_eq!(status, "completed");
        assert_eq!(mode, "ask");
        assert_eq!(agent, "fake");
        assert!(finished_at.is_some());

        let fin_events = h.finished_events.lock().unwrap().clone();
        assert_eq!(fin_events[0]["status"], "completed");
    }

    // -- per-run override (epic conceptify-e7m) ------------------------------

    #[tokio::test]
    async fn override_reaches_invocation_and_persists_on_row() {
        // End-to-end at the engine seam: a model override reaches the spawned
        // child's argv verbatim (via {model}), the row records the RESOLVED
        // agent/model, and the override intent is persisted in override_json.
        let h = harness("override");

        // A fake adapter whose args carry {model}; the script records its argv.
        let script = h.work_dir.join("fake-agent.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/argv.txt\"\nexit 0\n",
        )
        .unwrap();
        let mut perm = std::fs::metadata(&script).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&script, perm).unwrap();
        {
            let mut s = AgentSettings::default();
            s.adapters.insert(
                "fake".to_owned(),
                Adapter {
                    command: script.to_string_lossy().into_owned(),
                    args: vec![
                        "--model".to_owned(),
                        "{model}".to_owned(),
                        "{prompt}".to_owned(),
                    ],
                    cwd: "{project_root}".to_owned(),
                },
            );
            s.default_adapter = "fake".to_owned();
            s.timeout_secs = 60;
            let conn = h.db.lock().unwrap();
            crate::settings::update_settings(&conn, &s).unwrap();
        }

        let over = RunOverride {
            adapter: None,
            model: Some("override-model-z".to_owned()),
        };
        let started = h
            .start_over(RunMode::Ask, "the prompt", Some(over))
            .await
            .unwrap();
        let run_id = started.run_id.clone();
        let fin = finished(started).await;
        assert_eq!(fin.status, RunStatus::Completed);

        // The child saw the override model as its own argv element (verbatim).
        let argv = std::fs::read_to_string(h.work_dir.join("argv.txt")).unwrap();
        assert_eq!(
            argv.lines().collect::<Vec<_>>(),
            vec!["--model", "override-model-z", "the prompt"]
        );

        // The row records the resolved selection + the persisted override.
        let (agent, model, over_json) = h.run_selection(&run_id);
        assert_eq!(agent, "fake");
        assert_eq!(model, "override-model-z");
        assert_eq!(over_json.as_deref(), Some(r#"{"model":"override-model-z"}"#));
    }

    #[tokio::test]
    async fn no_override_persists_null_and_default_selection() {
        // The override-free path: the row stores the resolved DEFAULT selection
        // and a NULL override_json — so a retry re-derives current defaults.
        let h = harness("nooverride");
        h.install_fake_agent("#!/bin/sh\nexit 0\n", 60);
        let started = h.start(RunMode::Answer, "p").await.unwrap();
        let run_id = started.run_id.clone();
        let fin = finished(started).await;
        assert_eq!(fin.status, RunStatus::Completed);

        let (agent, model, over_json) = h.run_selection(&run_id);
        assert_eq!(agent, "fake");
        assert_eq!(model, "claude-haiku-4-5"); // Answer -> FollowUp default
        assert!(over_json.is_none(), "override-free run stores NULL override_json");
        // A custom default_adapter bypasses provider routing (bead e7m.7):
        // the row records the bypass, and the invocation is byte-identical to
        // pre-routing behavior (this whole test ran unchanged through it).
        assert_eq!(h.run_route(&run_id).as_deref(), Some("manual"));
    }

    #[tokio::test]
    async fn unknown_adapter_override_errors_before_row() {
        // An invalid override is rejected before any run row is created (like
        // CwdMissing), and frees the FR-4.9 registry slot.
        let h = harness("badoverride");
        h.install_fake_agent("#!/bin/sh\nexit 0\n", 60);
        let over = RunOverride {
            adapter: Some("no-such-adapter".to_owned()),
            model: None,
        };
        let err = h
            .start_over(RunMode::Ask, "p", Some(over))
            .await
            .unwrap_err();
        assert!(
            matches!(err, RunError::Settings(SettingsError::UnknownAdapter(_))),
            "{err:?}"
        );
        // No row was inserted, and the per-thread guard is released.
        let count: i64 = {
            let conn = h.db.lock().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM follow_up_runs WHERE thread_id = ?1",
                [&h.thread_id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(count, 0, "invalid override creates no run row");
        assert_eq!(h.registry().active_run_for_thread(&h.thread_id), None);
    }

    // -- provider routing (bead conceptify-e7m.7) -----------------------------

    #[tokio::test]
    async fn openrouter_route_env_reaches_child_and_secret_never_logged_or_persisted() {
        // The invocation-contract proof for the OpenRouter route, at the real
        // engine seam (real subprocess): a slash-form model on a routable
        // config (default_adapter=claude, its command re-pointed at a capture
        // script) must (a) hand the child EXACTLY the verified ANTHROPIC_* env
        // triple, (b) pass the OpenRouter slug through --model verbatim (no
        // remap), (c) record route=openrouter on the row + log header, and
        // (d) keep the token out of the entire log file and every TEXT column
        // of the run row.
        let h = harness("orroute");
        h.install_routed_capture("claude");
        let token = "sk-or-v1-DEADBEEF-secret";
        {
            let conn = h.db.lock().unwrap();
            crate::settings::set_openrouter_api_key(&conn, Some(token)).unwrap();
        }

        let over = RunOverride {
            adapter: None,
            model: Some("google/gemini-3-pro".to_owned()),
        };
        let started = h
            .start_over(RunMode::Answer, "the routed prompt", Some(over))
            .await
            .unwrap();
        let run_id = started.run_id.clone();
        let fin = finished(started).await;
        assert_eq!(fin.status, RunStatus::Completed);

        // (a)+(b): the child observed the exact env contract + verbatim slug.
        let argv = std::fs::read_to_string(h.work_dir.join("argv.txt")).unwrap();
        assert_eq!(
            argv.lines().collect::<Vec<_>>(),
            vec!["--model", "google/gemini-3-pro", "the routed prompt"]
        );
        let env = std::fs::read_to_string(h.work_dir.join("env.txt")).unwrap();
        assert_eq!(
            env,
            format!(
                "base=https://openrouter.ai/api\ntoken={token}\nkey=<>\nkey_present=set\n"
            ),
            "ANTHROPIC_BASE_URL → OpenRouter, AUTH_TOKEN = stored key, \
             API_KEY set-but-empty"
        );

        // (c): route visibility on row + log header (token-free tag).
        assert_eq!(h.run_route(&run_id).as_deref(), Some("openrouter"));
        let (agent, model, over_json) = h.run_selection(&run_id);
        assert_eq!(agent, "claude");
        assert_eq!(model, "google/gemini-3-pro");
        assert_eq!(
            over_json.as_deref(),
            Some(r#"{"model":"google/gemini-3-pro"}"#),
            "override intent persists the MODEL choice, not the routed adapter"
        );
        let log = std::fs::read_to_string(&fin.log_path).unwrap();
        assert!(
            log.contains("route=openrouter base_url=https://openrouter.ai/api"),
            "{log}"
        );

        // (d): the secret is nowhere in the log or the persisted row.
        assert!(!log.contains(token), "token leaked into run log:\n{log}");
        assert!(!log.contains("DEADBEEF"), "token fragment in run log:\n{log}");
        let row_text = h.run_row_text(&run_id);
        assert!(!row_text.contains("DEADBEEF"), "token in run row: {row_text}");
    }

    #[tokio::test]
    async fn openrouter_route_without_key_errors_before_row() {
        // FR-4.9 discipline for the missing-key path (same contract as the
        // unknown-adapter override): actionable error BEFORE spawning — no run
        // row, registry slot freed.
        let h = harness("orkeyless");
        h.install_routed_capture("claude"); // routable config, NO key stored

        let over = RunOverride {
            adapter: None,
            model: Some("google/gemini-3-pro".to_owned()),
        };
        let err = h
            .start_over(RunMode::Answer, "p", Some(over))
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                RunError::Settings(SettingsError::OpenRouterKeyMissing(ref m))
                    if m == "google/gemini-3-pro"
            ),
            "{err:?}"
        );
        assert!(err.to_string().contains("Settings"), "actionable: {err}");

        let count: i64 = {
            let conn = h.db.lock().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM follow_up_runs WHERE thread_id = ?1",
                [&h.thread_id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(count, 0, "missing key creates no run row");
        assert_eq!(h.registry().active_run_for_thread(&h.thread_id), None);
        // The capture script never ran.
        assert!(!h.work_dir.join("argv.txt").exists());
    }

    #[tokio::test]
    async fn openai_model_routes_to_codex_adapter_without_env() {
        // provider openai → the codex adapter, even though default_adapter is
        // claude — and NO ANTHROPIC_* env is injected on a native route.
        let h = harness("openairoute");
        h.install_routed_capture("codex");

        let over = RunOverride {
            adapter: None,
            model: Some("gpt-5.4-mini".to_owned()),
        };
        let started = h
            .start_over(RunMode::Ask, "codex prompt", Some(over))
            .await
            .unwrap();
        let run_id = started.run_id.clone();
        let fin = finished(started).await;
        assert_eq!(fin.status, RunStatus::Completed);

        let argv = std::fs::read_to_string(h.work_dir.join("argv.txt")).unwrap();
        assert_eq!(
            argv.lines().collect::<Vec<_>>(),
            vec!["--model", "gpt-5.4-mini", "codex prompt"]
        );
        // Native route: no base-url/auth env reaches the child.
        let env = std::fs::read_to_string(h.work_dir.join("env.txt")).unwrap();
        assert_eq!(env, "base=\ntoken=\nkey=<>\nkey_present=\n");

        assert_eq!(h.run_route(&run_id).as_deref(), Some("openai"));
        let (agent, model, _) = h.run_selection(&run_id);
        assert_eq!(agent, "codex");
        assert_eq!(model, "gpt-5.4-mini");
        let log = std::fs::read_to_string(&fin.log_path).unwrap();
        assert!(log.contains("route=openai"), "{log}");
    }

    #[tokio::test]
    async fn anthropic_default_routes_native_and_unroutable_fails_fast() {
        // No override at all: the per-purpose anthropic default routes native
        // (route=anthropic, no env) — the engine-level byte-identity check.
        // Then an unroutable custom id on the same routable config fails fast
        // pre-row.
        let h = harness("anthroute");
        h.install_routed_capture("claude");

        let started = h.start(RunMode::Answer, "plain prompt").await.unwrap();
        let run_id = started.run_id.clone();
        let fin = finished(started).await;
        assert_eq!(fin.status, RunStatus::Completed);
        let argv = std::fs::read_to_string(h.work_dir.join("argv.txt")).unwrap();
        assert_eq!(
            argv.lines().collect::<Vec<_>>(),
            vec!["--model", "claude-haiku-4-5", "plain prompt"]
        );
        let env = std::fs::read_to_string(h.work_dir.join("env.txt")).unwrap();
        assert_eq!(env, "base=\ntoken=\nkey=<>\nkey_present=\n");
        assert_eq!(h.run_route(&run_id).as_deref(), Some("anthropic"));
        let log = std::fs::read_to_string(&fin.log_path).unwrap();
        assert!(log.contains("route=anthropic"), "{log}");

        // Unroutable custom id → structured error, no second row.
        let over = RunOverride {
            adapter: None,
            model: Some("totally-custom-llm".to_owned()),
        };
        let err = h
            .start_over(RunMode::Answer, "p", Some(over))
            .await
            .unwrap_err();
        assert!(
            matches!(err, RunError::Settings(SettingsError::UnroutableModel(..))),
            "{err:?}"
        );
        let count: i64 = {
            let conn = h.db.lock().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM follow_up_runs WHERE thread_id = ?1",
                [&h.thread_id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(count, 1, "only the successful anthropic run has a row");
        assert_eq!(h.registry().active_run_for_thread(&h.thread_id), None);
    }

    #[tokio::test]
    async fn nonzero_exit_marks_failed_with_exit_code() {
        let h = harness("fail");
        h.install_fake_agent(
            "#!/bin/sh\n\
             echo '{\"type\":\"system\",\"subtype\":\"init\"}'\n\
             echo 'boom' >&2\n\
             exit 3\n",
            60,
        );

        let started = h.start(RunMode::Apply, "p").await.unwrap();
        let run_id = started.run_id.clone();
        let fin = finished(started).await;

        assert_eq!(fin.status, RunStatus::Failed);
        assert_eq!(fin.exit_code, Some(3));
        let (status, mode, _, finished_at) = h.run_row(&run_id);
        assert_eq!(status, "failed");
        assert_eq!(mode, "apply");
        assert!(finished_at.is_some());

        let log = std::fs::read_to_string(&fin.log_path).unwrap();
        assert!(log.contains("[err] boom"), "{log}");
        assert!(log.contains("[run] finalized: failed"), "{log}");

        let fin_events = h.finished_events.lock().unwrap().clone();
        assert_eq!(fin_events[0]["status"], "failed");
    }

    #[tokio::test]
    async fn timeout_kills_process_group_and_marks_timeout() {
        let h = harness("timeout");
        h.install_fake_agent(
            "#!/bin/sh\n\
             echo '{\"type\":\"system\",\"subtype\":\"init\"}'\n\
             sleep 30\n",
            1, // FR-5.3 timeout, configurable — 1s for the test
        );

        let t0 = std::time::Instant::now();
        let started = h.start(RunMode::Answer, "p").await.unwrap();
        let run_id = started.run_id.clone();
        let fin = finished(started).await;

        assert_eq!(fin.status, RunStatus::TimedOut);
        // Well under the script's 30s sleep: the group kill did its job.
        assert!(t0.elapsed() < Duration::from_secs(15));

        let (status, _, _, finished_at) = h.run_row(&run_id);
        assert_eq!(status, "timeout");
        assert!(finished_at.is_some());

        let log = std::fs::read_to_string(&fin.log_path).unwrap();
        assert!(
            log.contains("[run] timeout after 1s — killing process group"),
            "{log}"
        );
        assert!(log.contains("[run] finalized: timeout"), "{log}");

        let fin_events = h.finished_events.lock().unwrap().clone();
        assert_eq!(fin_events[0]["status"], "timeout");
        assert_eq!(h.registry().active_run_for_thread(&h.thread_id), None);
    }

    #[tokio::test]
    async fn cancel_kills_whole_process_tree_promptly() {
        let h = harness("cancel");
        // The agent spawns a TERM-trapping grandchild (claude spawns
        // subprocesses) and reports its pid through the pidfile (= prompt
        // arg). Group-SIGKILL must take BOTH down.
        h.install_fake_agent(
            "#!/bin/sh\n\
             sh -c 'trap \"\" TERM; while :; do sleep 1; done' &\n\
             echo $! > \"$1\"\n\
             echo '{\"type\":\"system\",\"subtype\":\"init\"}'\n\
             while :; do sleep 1; done\n",
            600,
        );
        let pidfile = h.work_dir.join("grandchild.pid");

        let started = h
            .start(RunMode::Answer, &pidfile.to_string_lossy())
            .await
            .unwrap();
        let run_id = started.run_id.clone();

        // Wait until the grandchild is up and registered.
        assert!(
            wait_until(
                || std::fs::read_to_string(&pidfile)
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false),
                5000
            )
            .await,
            "grandchild pidfile never appeared"
        );
        let grandchild: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(pid_alive(grandchild), "grandchild should be running");
        assert_eq!(
            h.registry().active_run_for_thread(&h.thread_id),
            Some(run_id.clone())
        );

        h.registry().cancel(&run_id).unwrap();

        let fin = finished(started).await;
        assert_eq!(fin.status, RunStatus::Cancelled);
        let (status, _, _, finished_at) = h.run_row(&run_id);
        assert_eq!(status, "cancelled");
        assert!(finished_at.is_some());

        // The TERM-ignoring grandchild died too (SIGKILL to the group). Give
        // init a moment to reap the orphan.
        assert!(
            wait_until(|| !pid_alive(grandchild), 5000).await,
            "grandchild survived the process-group kill"
        );

        let fin_events = h.finished_events.lock().unwrap().clone();
        assert_eq!(fin_events[0]["status"], "cancelled");

        // Cancelling again is NotActive (slot already freed).
        assert!(matches!(
            h.registry().cancel(&run_id),
            Err(RunError::NotActive(_))
        ));
    }

    #[tokio::test]
    async fn one_active_run_per_thread_guard() {
        let h = harness("guard");
        h.install_fake_agent(
            "#!/bin/sh\n\
             echo '{\"type\":\"system\",\"subtype\":\"init\"}'\n\
             sleep 30\n",
            600,
        );

        let first = h.start(RunMode::Answer, "p").await.unwrap();
        let first_id = first.run_id.clone();

        // FR-4.9: second start on the same thread is a structured error
        // naming the live run.
        let err = h.start(RunMode::Answer, "p2").await.unwrap_err();
        match err {
            RunError::AlreadyRunning { thread_id, run_id } => {
                assert_eq!(thread_id, h.thread_id);
                assert_eq!(run_id, first_id);
            }
            other => panic!("expected AlreadyRunning, got {other:?}"),
        }
        // The rejected attempt inserted no row.
        let count: i64 = {
            let conn = h.db.lock().unwrap();
            conn.query_row(
                "SELECT count(*) FROM follow_up_runs WHERE thread_id = ?1",
                [&h.thread_id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(count, 1);

        // Guard releases after the run finishes.
        h.registry().cancel(&first_id).unwrap();
        finished(first).await;
        let second = h.start(RunMode::Answer, "p3").await.unwrap();
        h.registry().cancel(&second.run_id).unwrap();
        finished(second).await;
    }

    #[tokio::test]
    async fn spawn_failure_marks_row_failed_and_frees_guard() {
        let h = harness("nospawn");
        // Absolute path → resolve_agent_binary returns it as-is; spawn fails.
        h.install_adapter_command("/nonexistent-conceptify/agent-zzz", 60);

        let err = h.start(RunMode::Answer, "p").await.unwrap_err();
        assert!(matches!(err, RunError::Io(_)), "{err:?}");

        // The attempted run is honest history: row exists, terminal 'failed'.
        let (run_id, status, finished_at, log_path): (String, String, Option<String>, String) = {
            let conn = h.db.lock().unwrap();
            conn.query_row(
                "SELECT id, status, finished_at, log_path FROM follow_up_runs
                 WHERE thread_id = ?1",
                [&h.thread_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
        };
        assert_eq!(status, "failed");
        assert!(finished_at.is_some());
        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(log.contains("[run] ABNORMAL END: failed to spawn"), "{log}");

        // Guard released — a (still-broken) retry gets a fresh attempt, not
        // AlreadyRunning.
        let err2 = h.start(RunMode::Answer, "p").await.unwrap_err();
        assert!(matches!(err2, RunError::Io(_)), "{err2:?}");
        let _ = run_id;
        assert_eq!(h.registry().active_run_for_thread(&h.thread_id), None);
    }

    #[tokio::test]
    async fn missing_cwd_is_a_clean_error_before_any_row() {
        let h = harness("nocwd");
        h.install_fake_agent("#!/bin/sh\nexit 0\n", 60);
        // Break the project root (FR-1.3 re-map situation).
        {
            let conn = h.db.lock().unwrap();
            conn.execute(
                "UPDATE projects SET root_path = '/nonexistent-conceptify-root' WHERE id = ?1",
                [&h.project_id],
            )
            .unwrap();
        }

        let err = h.start(RunMode::Answer, "p").await.unwrap_err();
        assert!(matches!(err, RunError::CwdMissing(_)), "{err:?}");

        let count: i64 = {
            let conn = h.db.lock().unwrap();
            conn.query_row(
                "SELECT count(*) FROM follow_up_runs WHERE thread_id = ?1",
                [&h.thread_id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(count, 0);
        assert_eq!(h.registry().active_run_for_thread(&h.thread_id), None);
    }

    #[tokio::test]
    async fn unknown_thread_errors_and_frees_guard() {
        let h = harness("nothread");
        let err = start_run(
            &h.handle,
            StartRun {
                thread_id: "no-such-thread".to_owned(),
                mode: RunMode::Answer,
                prompt: "p".to_owned(),
                env: Vec::new(),
                run_override: None,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, RunError::ThreadNotFound(_)), "{err:?}");
        assert_eq!(h.registry().active_run_for_thread("no-such-thread"), None);
    }

    // -- boot reconciliation (N4) --------------------------------------------

    #[test]
    fn boot_reconciliation_fails_stale_running_rows() {
        let h = harness("boot");
        let log_path = h.work_dir.join("stale-run.log");
        std::fs::write(&log_path, "[out] partial transcript\n").unwrap();
        {
            let conn = h.db.lock().unwrap();
            conn.execute(
                "INSERT INTO follow_up_runs (id, thread_id, agent, model, mode, status, log_path)
                 VALUES ('stale-1', ?1, 'claude', 'm', 'answer', 'running', ?2)",
                rusqlite::params![h.thread_id, log_path.to_string_lossy()],
            )
            .unwrap();
            // A terminal row must be left alone.
            conn.execute(
                "INSERT INTO follow_up_runs
                     (id, thread_id, agent, model, mode, status, log_path, finished_at)
                 VALUES ('done-1', ?1, 'claude', 'm', 'answer', 'completed', ?2,
                         strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                rusqlite::params![h.thread_id, log_path.to_string_lossy()],
            )
            .unwrap();
        }

        let n = {
            let conn = h.db.lock().unwrap();
            reconcile_stale_runs(&conn).unwrap()
        };
        assert_eq!(n, 1);

        let (status, _, _, finished_at) = h.run_row("stale-1");
        assert_eq!(status, "failed");
        assert!(finished_at.is_some());
        let (status_done, _, _, _) = h.run_row("done-1");
        assert_eq!(status_done, "completed");

        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(log.contains("[out] partial transcript"), "{log}");
        assert!(
            log.contains("[run] ABNORMAL END: run stale-1 was still 'running' at app startup"),
            "{log}"
        );

        // Idempotent: a second pass finds nothing.
        let conn = h.db.lock().unwrap();
        assert_eq!(reconcile_stale_runs(&conn).unwrap(), 0);
    }
}
