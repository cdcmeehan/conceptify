//! Follow-up flows on top of the run engine (PRD FR-4.6/4.7/4.8/4.9, UC4) —
//! beads `conceptify-b12.4` / `conceptify-b12.5` / `conceptify-b12.6`.
//!
//! `crate::runs` is the policy-free process engine; this module is the
//! *policy*: it assembles the prompts the headless agent actually sees,
//! prepares the child environment, starts runs, and owns the thread-status
//! side effects of the run lifecycle. Two flows:
//!
//! - **[`ask_follow_ups`]** (FR-4.6, mode `answer`): gathers ALL open comments
//!   and spawns one run whose contract is to answer each comment individually
//!   via `conceptify resolve-comment`. The artifact is never modified.
//!   Answers land in the sidebar live through the `comment-updated` events
//!   the PATCH route already emits — no flow-side bookkeeping needed.
//! - **[`apply_to_artifact`]** (FR-4.7, mode `apply`): targets specific
//!   comments (or every `answered` one) and spawns a run whose contract is to
//!   edit a working copy of the artifact, mark each target comment `applied`,
//!   and publish exactly one new version via `conceptify save-artifact`.
//!
//! # The apply ordering decision (FR-4.7 × FR-4.4)
//!
//! The comments being applied must reach status `applied` **before** the new
//! artifact version is saved: `applied` comments are frozen at their capture
//! version and excluded from the save-time re-attachment pass (see bead
//! `conceptify-94m.7`), which is exactly right — the apply typically rewrote
//! the very text they anchored to, so re-anchoring them would only produce
//! noise ("reference moved" flags on comments that were just satisfied).
//!
//! This ordering is **prompt-enforced**, not server-enforced: the apply
//! prompt instructs the agent to finish all edits, then run
//! `resolve-comment --applied` for every target, then `save-artifact` once,
//! last. Marking the comments applied server-side before spawning the run was
//! rejected — if the run then failed, the DB would claim clarifications were
//! applied that never existed. With prompt ordering, a run that dies midway
//! leaves an honest trail: comments still `open`/`answered` were truly not
//! handled; a comment marked `applied` without a following save is the only
//! residual imprecision (the agent addressed it in a working copy that never
//! published), and the run-status UI (FR-4.8) surfaces that failure loudly.
//! If the agent misbehaves and saves *first*, nothing corrupts: re-attachment
//! migrates/flags the not-yet-applied comments (harmless, just noisier) and
//! the `--applied` PATCHes still land.
//!
//! # Thread status (PRD §4 status machine)
//!
//! The run lifecycle owns `updating`: an **apply** run sets the thread to
//! `updating` at start and, when the run finishes (any terminal status),
//! restores `ready` — via a conditional `updating → ready` transition so a
//! `ready` already set by the agent's mid-run `save-artifact` is never
//! regressed. **Answer** runs never touch thread status (they are sidebar-only
//! by definition). Neither flow ever sets thread status `error`: that state
//! is reserved for *generation* runs (FR-5.3, the M6 in-app ask bead) — a
//! failed follow-up run leaves the thread as-is and is surfaced by the
//! run-status UI instead. Every flow-driven status change emits a
//! `thread-updated` Tauri event `{ project_id, thread_id, status }` so status
//! chips update live.
//!
//! # The child PATH problem (PRD §5.1)
//!
//! A Finder-launched GUI app inherits a minimal `PATH` (no `~/.local/bin`, no
//! Homebrew), but the spawned agent must be able to invoke `conceptify`. The
//! flow resolves the CLI binary once per process — `CONCEPTIFY_CLI` env
//! override (tests / escape hatch) → sibling of the running app binary (dev
//! builds put both binaries in the same `target/<profile>` dir) → login-shell
//! `which conceptify` (reusing `settings::resolve_agent_binary`'s cached
//! mechanism) — and prepends its parent directory to the child's `PATH` via
//! `StartRun::env`. Resolution failure aborts the flow *before* any run row
//! exists, with an actionable message (`just install-cli`).

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::comments::{Comment, CommentStatus};
use crate::context::{self, ContextError};
use crate::db::{self, DbHandle};
use crate::runs::{self, RunError, RunMode, RunRegistry, RunStatus, StartRun};
use crate::settings;
use crate::threads::{self, ThreadStatus};

/// Env var that pins the `conceptify` CLI binary path, bypassing discovery.
/// Used by tests (which must not depend on the machine's login shell) and as
/// a user escape hatch alongside the Settings `agentBinaryPath` analog.
const CLI_ENV_OVERRIDE: &str = "CONCEPTIFY_CLI";

/// PATH used when the app inherited none at all (launchd edge); matches the
/// macOS default for GUI processes.
const FALLBACK_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// Default number of log lines [`tail_lines`] callers surface on failure
/// (FR-4.8 "log tail inline").
pub const DEFAULT_LOG_TAIL_LINES: usize = 30;

// ---------------------------------------------------------------------------
// Public data model
// ---------------------------------------------------------------------------

/// What a successfully started flow hands back to the UI: enough to render
/// the FR-4.8 run block and compute per-comment progress (the target ids are
/// deliberately returned here — they are not persisted anywhere, so a UI that
/// re-attaches to an already-running run via [`active_run_summary`] shows an
/// indeterminate spinner instead).
#[derive(Debug, Clone)]
pub struct FlowStarted {
    pub run_id: String,
    pub thread_id: String,
    pub mode: RunMode,
    pub target_comment_ids: Vec<String>,
}

/// What a started in-app ask (bead `conceptify-959.1`, FR-5.1) hands back to the
/// composer: the new (or, on retry, the same) thread and the generation run now
/// authoring its artifact.
#[derive(Debug, Clone)]
pub struct AskStarted {
    pub run_id: String,
    pub thread_id: String,
}

/// The most recent run row for a thread (any mode/status), for the FR-5.3 error
/// state on the thread view: it needs the failed generation run's id to load
/// the log tail and offer Retry, even after an app restart when no live run is
/// tracked in memory.
#[derive(Debug, Clone)]
pub struct LatestRun {
    pub run_id: String,
    pub mode: String,
    pub status: String,
}

/// A live run's identity for the FR-4.8 UI (`get_active_run` command): the
/// registry says *which* run is live, the DB row supplies its mode.
#[derive(Debug, Clone)]
pub struct ActiveRunSummary {
    pub run_id: String,
    pub thread_id: String,
    pub mode: String,
}

/// Errors from starting a flow. Command wrappers stringify these; the strings
/// are user-facing (shown in the sidebar), so they must be actionable.
#[derive(Debug, thiserror::Error)]
pub enum FlowError {
    #[error("this thread has no saved artifact yet")]
    NoArtifact,

    #[error("this thread has no open comments to answer")]
    NoOpenComments,

    #[error("no comments to apply — answer some first, or pass explicit comment ids")]
    NoTargetComments,

    #[error("comment not found on this thread: {0}")]
    CommentNotFound(String),

    #[error("comment {0} is already applied")]
    AlreadyApplied(String),

    #[error(
        "conceptify CLI not found (checked the CONCEPTIFY_CLI override, next to the app \
         binary, and the login-shell PATH); install it with `just install-cli`"
    )]
    CliNotFound,

    #[error("question must not be empty")]
    EmptyQuestion,

    #[error("project not found: {0}")]
    ProjectNotFound(String),

    #[error(transparent)]
    Thread(#[from] threads::ThreadError),

    #[error(transparent)]
    Context(#[from] ContextError),

    #[error(transparent)]
    Run(#[from] RunError),

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
}

// ---------------------------------------------------------------------------
// Flows
// ---------------------------------------------------------------------------

/// Everything a flow needs from one DB snapshot.
struct LoadedFlow {
    project_id: String,
    project_root: String,
    title: String,
    question: String,
    artifact_path: String,
    artifact_version: i64,
    targets: Vec<Comment>,
}

/// Start an FR-4.6 **answer** run: one headless agent for ALL open comments.
///
/// Guards: the thread must have a saved artifact and ≥ 1 open comment; the
/// engine's FR-4.9 reservation rejects a second run on the same thread
/// (surfaced as [`RunError::AlreadyRunning`]). Thread status is untouched —
/// answers are sidebar-only, and failures are the run UI's to surface
/// (FR-5.3-lite: no `error` status from follow-up runs).
pub async fn ask_follow_ups<R: Runtime>(
    app_handle: &AppHandle<R>,
    thread_id: &str,
) -> Result<FlowStarted, FlowError> {
    let db = app_handle.state::<DbHandle>().inner().clone();

    let tid = thread_id.to_owned();
    let loaded = db::with_conn_result(&db, move |conn| -> Result<LoadedFlow, FlowError> {
        let ctx = context::thread_context(conn, &tid)?;
        let latest = ctx.latest_artifact.ok_or(FlowError::NoArtifact)?;
        if ctx.open_comments.is_empty() {
            return Err(FlowError::NoOpenComments);
        }
        Ok(LoadedFlow {
            project_id: ctx.project.id,
            project_root: ctx.project.root_path,
            title: ctx.thread.title,
            question: ctx.thread.initial_question,
            artifact_path: latest.file_path,
            artifact_version: latest.version,
            targets: ctx.open_comments,
        })
    })
    .await?;

    let prompt = build_answer_prompt(&PromptContext {
        thread_id,
        title: &loaded.title,
        question: &loaded.question,
        project_root: &loaded.project_root,
        artifact_path: &loaded.artifact_path,
        artifact_version: loaded.artifact_version,
        comments: &loaded.targets,
    });
    let env = child_env().await?;

    let started = runs::start_run(
        app_handle,
        StartRun {
            thread_id: thread_id.to_owned(),
            mode: RunMode::Answer,
            prompt,
            env,
        },
    )
    .await?;
    // No completion watcher: an answer run has no thread-status side effects,
    // and its per-comment effects arrive via the PATCH route's
    // `comment-updated` events as the agent works. Dropping the `finished`
    // receiver is fine — the engine's oneshot send is best-effort.

    Ok(FlowStarted {
        run_id: started.run_id,
        thread_id: started.thread_id,
        mode: RunMode::Answer,
        target_comment_ids: loaded.targets.into_iter().map(|c| c.id).collect(),
    })
}

/// Start an FR-4.7 **apply** run for `comment_ids` (empty = every `answered`
/// comment on the thread).
///
/// Explicit ids must name comments of this thread in `open` or `answered`
/// state (`open` is allowed — the `open → applied` one-shot is a legal
/// transition and the prompt has the agent both answer-and-apply in one
/// note). On successful start the thread goes `updating` (+ `thread-updated`
/// event) and a watcher restores `ready` when the run terminates — see the
/// module docs for the full status policy and the apply *ordering* contract.
pub async fn apply_to_artifact<R: Runtime>(
    app_handle: &AppHandle<R>,
    thread_id: &str,
    comment_ids: Vec<String>,
) -> Result<FlowStarted, FlowError> {
    let db = app_handle.state::<DbHandle>().inner().clone();

    let tid = thread_id.to_owned();
    let loaded = db::with_conn_result(&db, move |conn| -> Result<LoadedFlow, FlowError> {
        let ctx = context::thread_context(conn, &tid)?;
        let latest = ctx.latest_artifact.ok_or(FlowError::NoArtifact)?;
        let all = crate::comments::list_comments(conn, &tid, None)
            .map_err(|e| FlowError::Context(ContextError::Comments(e)))?;

        let targets: Vec<Comment> = if comment_ids.is_empty() {
            all.into_iter()
                .filter(|c| c.status == CommentStatus::Answered)
                .collect()
        } else {
            let mut picked = Vec::with_capacity(comment_ids.len());
            for id in &comment_ids {
                let comment = all
                    .iter()
                    .find(|c| &c.id == id)
                    .cloned()
                    .ok_or_else(|| FlowError::CommentNotFound(id.clone()))?;
                if comment.status == CommentStatus::Applied {
                    return Err(FlowError::AlreadyApplied(id.clone()));
                }
                picked.push(comment);
            }
            picked
        };
        if targets.is_empty() {
            return Err(FlowError::NoTargetComments);
        }

        Ok(LoadedFlow {
            project_id: ctx.project.id,
            project_root: ctx.project.root_path,
            title: ctx.thread.title,
            question: ctx.thread.initial_question,
            artifact_path: latest.file_path,
            artifact_version: latest.version,
            targets,
        })
    })
    .await?;

    let prompt = build_apply_prompt(&PromptContext {
        thread_id,
        title: &loaded.title,
        question: &loaded.question,
        project_root: &loaded.project_root,
        artifact_path: &loaded.artifact_path,
        artifact_version: loaded.artifact_version,
        comments: &loaded.targets,
    });
    let env = child_env().await?;

    let started = runs::start_run(
        app_handle,
        StartRun {
            thread_id: thread_id.to_owned(),
            mode: RunMode::Apply,
            prompt,
            env,
        },
    )
    .await?;

    // Run started: the thread is now visibly `updating` (PRD §4 — owned by
    // the run lifecycle). Set *after* start_run so a rejected start (FR-4.9
    // guard, spawn failure) leaves the status untouched.
    {
        let tid = thread_id.to_owned();
        db::with_conn(&db, move |conn| {
            threads::set_thread_status(conn, &tid, ThreadStatus::Updating)
        })
        .await?;
        emit_thread_updated(app_handle, &loaded.project_id, thread_id, "updating");
    }

    // Watcher: when the run terminates — however it terminates — restore
    // `ready` iff the thread is still `updating`. On success the agent's
    // save-artifact already flipped it to `ready` (same conditional no-ops);
    // on failure/timeout/cancel this is the revert. Never `error` (module
    // docs). The conditional UPDATE makes the check-and-write atomic.
    {
        let app_handle = app_handle.clone();
        let db = db.clone();
        let project_id = loaded.project_id.clone();
        let thread_id = thread_id.to_owned();
        let finished = started.finished;
        tauri::async_runtime::spawn(async move {
            // A dropped sender (engine supervision died — N4 says it can't)
            // falls through to the same revert: never wedge `updating`.
            if let Ok(fin) = finished.await {
                if matches!(fin.status, RunStatus::Failed | RunStatus::TimedOut) {
                    eprintln!(
                        "[conceptify-flows] apply run {} on thread {} ended {} (exit {:?}); log: {}",
                        fin.run_id,
                        fin.thread_id,
                        fin.status.as_str(),
                        fin.exit_code,
                        fin.log_path.display()
                    );
                }
            }
            revert_updating(&app_handle, &db, &project_id, &thread_id).await;
        });
    }

    Ok(FlowStarted {
        run_id: started.run_id,
        thread_id: thread_id.to_owned(),
        mode: RunMode::Apply,
        target_comment_ids: loaded.targets.into_iter().map(|c| c.id).collect(),
    })
}

/// Conditionally restore `updating → ready` after an apply run terminated,
/// emitting `thread-updated` only when a row actually changed (a `ready`
/// already set by the agent's `save-artifact` no-ops silently).
async fn revert_updating<R: Runtime>(
    app_handle: &AppHandle<R>,
    db: &DbHandle,
    project_id: &str,
    thread_id: &str,
) {
    let tid = thread_id.to_owned();
    let changed = db::with_conn(db, move |conn| {
        threads::transition_thread_status(conn, &tid, ThreadStatus::Updating, ThreadStatus::Ready)
    })
    .await;
    match changed {
        Ok(true) => emit_thread_updated(app_handle, project_id, thread_id, "ready"),
        Ok(false) => {} // already `ready` (agent saved) — nothing to announce
        Err(e) => eprintln!(
            "[conceptify-flows] failed to restore thread {thread_id} from updating: {e}"
        ),
    }
}

#[derive(Serialize, Clone)]
struct ThreadUpdatedEvent<'a> {
    project_id: &'a str,
    thread_id: &'a str,
    status: &'a str,
}

/// Emit `thread-updated` — the live-status event for flow-driven thread
/// status changes (status chips in the thread list). Mirrors the payload
/// scoping convention of `thread-created`/`comment-updated`.
fn emit_thread_updated<R: Runtime>(
    app_handle: &AppHandle<R>,
    project_id: &str,
    thread_id: &str,
    status: &str,
) {
    let _ = app_handle.emit(
        "thread-updated",
        &ThreadUpdatedEvent {
            project_id,
            thread_id,
            status,
        },
    );
}

// ---------------------------------------------------------------------------
// In-app ask (PRD §7.5, UC5, FR-5.1/5.2/5.3) — beads 959.1 / 959.2
// ---------------------------------------------------------------------------

/// Start an FR-5.1 **in-app ask**: create a fresh thread in `project_id`
/// (status `generating`) and spawn a headless generation run
/// ([`RunMode::Ask`]) whose contract is to author an artifact per the
/// Conceptify skill and publish it via `conceptify save-artifact` into this
/// thread.
///
/// `title` is the composer's optional title; when blank it is derived from the
/// question ([`derive_title`]). The run's `cwd` is the project root (via the
/// adapter's `{project_root}` cwd template — see [`runs::start_run`]).
///
/// Status policy (FR-5.2/5.3): the thread stays `generating` until the agent's
/// mid-run `save-artifact` flips it to `ready` (that endpoint owns the `→ ready`
/// transition and emits `artifact-updated`, which swaps the viewer in). A
/// completion watcher then conditionally flips `generating → error` on any
/// terminal outcome that left no artifact — see [`error_thread_if_generating`].
/// N4: a start that fails *after* the thread row exists (CLI missing, cwd gone,
/// spawn failure) flips the new thread to `error` rather than stranding it in
/// `generating`.
pub async fn ask_from_app<R: Runtime>(
    app_handle: &AppHandle<R>,
    project_id: &str,
    title: Option<&str>,
    question: &str,
) -> Result<AskStarted, FlowError> {
    let question = question.trim();
    if question.is_empty() {
        return Err(FlowError::EmptyQuestion);
    }
    let title = derive_title(title, question);

    let db = app_handle.state::<DbHandle>().inner().clone();

    // One lock: verify the project (for a clean 404 + to read its root) and
    // create the thread (which also validates the project, redundantly but
    // harmlessly). `create_thread` sets status `generating`.
    let pid = project_id.to_owned();
    let title_owned = title.clone();
    let question_owned = question.to_owned();
    let (thread_id, slug, project_root) = db::with_conn_result(
        &db,
        move |conn| -> Result<(String, String, String), FlowError> {
            let root: Option<String> = conn
                .query_row("SELECT root_path FROM projects WHERE id = ?1", [&pid], |r| {
                    r.get(0)
                })
                .optional()?;
            let Some(root) = root else {
                return Err(FlowError::ProjectNotFound(pid.clone()));
            };
            let thread = threads::create_thread(conn, &pid, &title_owned, &question_owned)?;
            Ok((thread.id, thread.slug, root))
        },
    )
    .await?;

    match try_spawn_ask(
        app_handle,
        &db,
        project_id,
        &thread_id,
        &slug,
        &title,
        question,
        &project_root,
    )
    .await
    {
        Ok(started) => Ok(started),
        Err(e) => {
            // Never leave the freshly-created thread stuck `generating` (N4):
            // flip it to `error` so the thread view shows the failure + Retry.
            error_thread_if_generating(app_handle, &db, project_id, &thread_id).await;
            Err(e)
        }
    }
}

/// Retry a failed in-app ask (FR-5.3): re-spawn the SAME question into the SAME
/// thread and move it back to `generating`. Loads the thread's question/title/
/// project via [`context::thread_context`] (→ [`ContextError::ThreadNotFound`]
/// for an unknown id). The thread is set `generating` *before* the run starts
/// so the watcher's conditional `generating → error` can never race a stale
/// `error`; a rejected start reverts it to `error`.
pub async fn retry_ask<R: Runtime>(
    app_handle: &AppHandle<R>,
    thread_id: &str,
) -> Result<AskStarted, FlowError> {
    let db = app_handle.state::<DbHandle>().inner().clone();

    let tid = thread_id.to_owned();
    let (project_id, project_root, slug, title, question) = db::with_conn_result(
        &db,
        move |conn| -> Result<(String, String, String, String, String), FlowError> {
            let ctx = context::thread_context(conn, &tid)?;
            Ok((
                ctx.project.id,
                ctx.project.root_path,
                ctx.thread.slug,
                ctx.thread.title,
                ctx.thread.initial_question,
            ))
        },
    )
    .await?;

    // Show the thread `generating` again immediately (Retry is a fresh
    // generation into the same thread). Set BEFORE `start_run`: this closes the
    // window where the run could finish (and its watcher fire) before the thread
    // left `error`.
    {
        let tid = thread_id.to_owned();
        db::with_conn(&db, move |conn| {
            threads::set_thread_status(conn, &tid, ThreadStatus::Generating)
        })
        .await?;
        emit_thread_updated(app_handle, &project_id, thread_id, "generating");
    }

    match try_spawn_ask(
        app_handle,
        &db,
        &project_id,
        thread_id,
        &slug,
        &title,
        &question,
        &project_root,
    )
    .await
    {
        Ok(started) => Ok(started),
        Err(e) => {
            error_thread_if_generating(app_handle, &db, &project_id, thread_id).await;
            Err(e)
        }
    }
}

/// Assemble the ask prompt, prepare the child env, start the generation run,
/// and attach the FR-5.3 completion watcher. Shared by [`ask_from_app`] and
/// [`retry_ask`]; on any error the callers flip the thread to `error`.
#[allow(clippy::too_many_arguments)]
async fn try_spawn_ask<R: Runtime>(
    app_handle: &AppHandle<R>,
    db: &DbHandle,
    project_id: &str,
    thread_id: &str,
    slug: &str,
    title: &str,
    question: &str,
    project_root: &str,
) -> Result<AskStarted, FlowError> {
    let prompt = build_ask_prompt(&AskPromptContext {
        thread_id,
        slug,
        title,
        question,
        project_root,
    });
    let env = child_env().await?;

    let started = runs::start_run(
        app_handle,
        StartRun {
            thread_id: thread_id.to_owned(),
            mode: RunMode::Ask,
            prompt,
            env,
        },
    )
    .await?;

    // Completion watcher (FR-5.3, N4). On ANY terminal outcome, conditionally
    // flip `generating → error`. This single conditional both (a) surfaces a
    // crash / timeout / cancel and (b) catches a run that exited 0 but never
    // saved (completed-without-artifact → error), while NEVER regressing a
    // `ready` the agent's mid-run `save-artifact` already set — that transition
    // no-ops from `ready`. Same race-free pattern as the apply watcher's revert.
    {
        let app_handle = app_handle.clone();
        let db = db.clone();
        let project_id = project_id.to_owned();
        let thread_id = thread_id.to_owned();
        let finished = started.finished;
        tauri::async_runtime::spawn(async move {
            // A dropped sender (engine supervision died — N4 says it can't)
            // still falls through to the same conditional flip.
            if let Ok(fin) = finished.await {
                if matches!(
                    fin.status,
                    RunStatus::Failed | RunStatus::TimedOut | RunStatus::Cancelled
                ) {
                    eprintln!(
                        "[conceptify-flows] ask run {} on thread {} ended {} (exit {:?}); log: {}",
                        fin.run_id,
                        fin.thread_id,
                        fin.status.as_str(),
                        fin.exit_code,
                        fin.log_path.display()
                    );
                }
            }
            error_thread_if_generating(&app_handle, &db, &project_id, &thread_id).await;
        });
    }

    Ok(AskStarted {
        run_id: started.run_id,
        thread_id: thread_id.to_owned(),
    })
}

/// Conditionally flip `generating → error` after a generation run ended without
/// leaving an artifact (FR-5.3). Emits `thread-updated {status: "error"}` only
/// when a row actually changed — a `ready` set by a mid-run `save-artifact` is
/// left untouched (the conditional UPDATE no-ops from `ready`), so a run that
/// saved and then failed/cancelled never regresses the thread to `error`.
async fn error_thread_if_generating<R: Runtime>(
    app_handle: &AppHandle<R>,
    db: &DbHandle,
    project_id: &str,
    thread_id: &str,
) {
    let tid = thread_id.to_owned();
    let changed = db::with_conn(db, move |conn| {
        threads::transition_thread_status(conn, &tid, ThreadStatus::Generating, ThreadStatus::Error)
    })
    .await;
    match changed {
        Ok(true) => emit_thread_updated(app_handle, project_id, thread_id, "error"),
        Ok(false) => {} // already `ready` (agent saved) — leave it alone
        Err(e) => eprintln!("[conceptify-flows] failed to error thread {thread_id}: {e}"),
    }
}

/// Derive a thread title from the question when the composer left the title
/// field blank: the first [`MAX_TITLE_WORDS`] words, capped at
/// [`MAX_TITLE_CHARS`]. `create_thread` slugifies and per-project-dedupes it, so
/// this only needs to be a readable label, never unique. A trimmed non-empty
/// question always yields ≥ 1 word, so the derived title is never empty.
fn derive_title(title: Option<&str>, question: &str) -> String {
    const MAX_TITLE_WORDS: usize = 8;
    const MAX_TITLE_CHARS: usize = 80;

    if let Some(explicit) = title.map(str::trim).filter(|s| !s.is_empty()) {
        return explicit.to_owned();
    }

    let derived: String = question
        .split_whitespace()
        .take(MAX_TITLE_WORDS)
        .collect::<Vec<_>>()
        .join(" ");
    if derived.chars().count() > MAX_TITLE_CHARS {
        derived
            .chars()
            .take(MAX_TITLE_CHARS)
            .collect::<String>()
            .trim_end()
            .to_owned()
    } else {
        derived
    }
}

// ---------------------------------------------------------------------------
// Active-run lookup + log tail (FR-4.8 support)
// ---------------------------------------------------------------------------

/// The live run for a thread, if any: liveness from the [`RunRegistry`]
/// (source of truth), mode from the `follow_up_runs` row. Backs the
/// `get_active_run` command (UI re-attaching to a run after a thread switch).
pub fn active_run_summary(
    conn: &Connection,
    registry: &RunRegistry,
    thread_id: &str,
) -> Result<Option<ActiveRunSummary>, rusqlite::Error> {
    let Some(run_id) = registry.active_run_for_thread(thread_id) else {
        return Ok(None);
    };
    let mode: String = conn.query_row(
        "SELECT mode FROM follow_up_runs WHERE id = ?1",
        [&run_id],
        |r| r.get(0),
    )?;
    Ok(Some(ActiveRunSummary {
        run_id,
        thread_id: thread_id.to_owned(),
        mode,
    }))
}

/// The most recent run row for a thread (most recent `started_at`), or `None`
/// if the thread has never run. Backs the `get_latest_run` command: the FR-5.3
/// error state on the thread view resolves the failed generation run's id from
/// here to load its log tail — this works even after an app restart, when the
/// in-memory registry (used by [`active_run_summary`]) is empty.
pub fn latest_run_for_thread(
    conn: &Connection,
    thread_id: &str,
) -> Result<Option<LatestRun>, rusqlite::Error> {
    conn.query_row(
        "SELECT id, mode, status FROM follow_up_runs
         WHERE thread_id = ?1 ORDER BY started_at DESC, id DESC LIMIT 1",
        [thread_id],
        |r| {
            Ok(LatestRun {
                run_id: r.get(0)?,
                mode: r.get(1)?,
                status: r.get(2)?,
            })
        },
    )
    .optional()
}

/// Last `max` lines of a run log (FR-4.8 failure surfacing). Reads the whole
/// file — run logs are local and bounded by the run timeout; simplicity over
/// a reverse-seek reader.
pub fn tail_lines(path: &Path, max: usize) -> std::io::Result<Vec<String>> {
    let content = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(max);
    Ok(lines[start..].iter().map(|s| (*s).to_owned()).collect())
}

// ---------------------------------------------------------------------------
// Child environment (the §5.1 PATH problem)
// ---------------------------------------------------------------------------

/// Build the env overrides for a headless run: `PATH` with the `conceptify`
/// CLI's directory prepended. Fails (before any run row exists) when the CLI
/// cannot be found at all — a run that cannot report back is useless.
async fn child_env() -> Result<Vec<(String, String)>, FlowError> {
    let cli = tokio::task::spawn_blocking(resolve_cli_path)
        .await
        .expect("cli lookup task panicked")?;
    let dir = cli
        .parent()
        .unwrap_or_else(|| Path::new("/"))
        .to_string_lossy()
        .into_owned();
    let path_value = prepend_path(&dir, std::env::var("PATH").ok().as_deref());
    Ok(vec![("PATH".to_owned(), path_value)])
}

/// Locate the `conceptify` CLI binary. Precedence:
/// 1. `CONCEPTIFY_CLI` env override (absolute path to the binary);
/// 2. a `conceptify` file next to the running executable (dev builds — both
///    workspace binaries land in the same `target/<profile>` dir — and any
///    future bundle that ships the CLI beside the app binary);
/// 3. login-shell `which conceptify` (the `just install-cli` symlink in
///    `~/.local/bin`), via `settings::resolve_agent_binary`'s cached
///    mechanism — one slow lookup per process.
fn resolve_cli_path() -> Result<PathBuf, FlowError> {
    if let Ok(v) = std::env::var(CLI_ENV_OVERRIDE) {
        let v = v.trim();
        if !v.is_empty() {
            return Ok(PathBuf::from(v));
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("conceptify");
            if sibling.is_file() {
                return Ok(sibling);
            }
        }
    }

    settings::resolve_agent_binary("conceptify", None).map_err(|_| FlowError::CliNotFound)
}

/// Prepend `dir` to a `PATH` value unless it is already a component; a
/// missing/empty existing PATH falls back to the macOS GUI default. Pure —
/// unit-tested below.
fn prepend_path(dir: &str, existing: Option<&str>) -> String {
    let existing = existing
        .filter(|s| !s.is_empty())
        .unwrap_or(FALLBACK_PATH);
    if existing.split(':').any(|component| component == dir) {
        existing.to_owned()
    } else {
        format!("{dir}:{existing}")
    }
}

// ---------------------------------------------------------------------------
// Prompt assembly (pure)
// ---------------------------------------------------------------------------

/// Inputs to the prompt builders — everything run-specific the headless agent
/// sees, per PRD §5.5 (thread question, artifact path, open comments with
/// anchors) plus identity/invariant framing.
pub(crate) struct PromptContext<'a> {
    pub thread_id: &'a str,
    pub title: &'a str,
    pub question: &'a str,
    pub project_root: &'a str,
    pub artifact_path: &'a str,
    pub artifact_version: i64,
    pub comments: &'a [Comment],
}

/// The comments block embedded in both prompts: a pretty JSON array with the
/// stored anchor passed through as-is (its inner keys stay `snake_case` —
/// the same verbatim-anchor contract as `get-context`, docs/cli.md). Keys
/// come out alphabetized (serde_json `Value` maps sort), which is fine — and
/// deterministic, which the exact-string prompt tests rely on.
fn comments_json(comments: &[Comment]) -> String {
    let arr: Vec<serde_json::Value> = comments
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id,
                "artifactVersion": c.artifact_version,
                "status": c.status.as_str(),
                "anchor": c.anchor,
                "body": c.body,
                "answerHtml": c.answer_html,
            })
        })
        .collect();
    serde_json::to_string_pretty(&arr).expect("comment JSON always serializes")
}

/// The FR-4.6 answer-mode prompt. Contract highlights: one
/// `resolve-comment` per comment (that is what makes sidebar answers land
/// incrementally), never `--applied`, never `save-artifact`.
pub(crate) fn build_answer_prompt(ctx: &PromptContext) -> String {
    format!(
        r#"You are Conceptify's follow-up answerer, running headless inside the project this artifact explains.

A reader left comments (follow-up questions) on an explanation artifact. Answer each comment individually through the `conceptify` CLI (it is on your PATH). The artifact itself must not be modified in this mode.

## Context
- Project root (your working directory): {project_root}
- Thread: "{title}" (thread id: {thread_id})
- The question the artifact answers: {question}
- Artifact file (read-only in this mode): {artifact_path} (version {version})

## Comments to answer
Each object has: `id`; `body` (the reader's question); `anchor` (where it points in the artifact — `cfy_id` is the target element's `data-cfy-id`, `quote.exact` is the anchored text; a null anchor is a direct question about the artifact as a whole); `artifactVersion` (the version it was written against); `answerHtml` (any existing answer).

{comments}

## How to answer — exact contract
1. Read the artifact file, then whatever project sources you need to ground each answer in the real code.
2. Create a scratch directory for answer files: ANSWERS=$(mktemp -d)
3. For EACH comment above, individually:
   - Write its answer to its own file, e.g. "$ANSWERS/<comment-id>.html" — an HTML fragment or markdown, concise and specific (a short paragraph or two; small code snippets welcome; no <html>/<head>/<body> wrapper).
   - Then run: conceptify resolve-comment --id <comment-id> --answer-file "$ANSWERS/<comment-id>.html"
   This marks that comment answered and shows the answer in the app immediately — resolve each comment as soon as its answer is ready, so answers land one by one.
4. Answer every comment. Never combine several comments into one resolve-comment call, and never skip one.

## Hard rules
- Do NOT modify or save the artifact: never run `conceptify save-artifact`, and never pass `--applied` to resolve-comment. Answering and applying-to-the-artifact are deliberately separate steps; this run only answers.
- Use the conceptify CLI only as specified above.
- Your toolset is scoped: web tools are disabled, git commands that mutate the repo are denied, and your Edit/Write tools cannot touch files inside the project root — read the project freely, but write only under your scratch directory.
- If the file ~/.claude/skills/conceptify/references/follow-ups.md exists, read it before answering — it holds the house rules for follow-up answers.
"#,
        project_root = ctx.project_root,
        title = ctx.title,
        thread_id = ctx.thread_id,
        question = ctx.question,
        artifact_path = ctx.artifact_path,
        version = ctx.artifact_version,
        comments = comments_json(ctx.comments),
    )
}

/// The FR-4.7 apply-mode prompt. Contract highlights, in order of importance:
/// edits happen in a working copy; `data-cfy-id`s are immutable; diagrams
/// regenerate from their embedded `cfy:src` DSL; **all `resolve-comment
/// --applied` calls precede the single final `save-artifact`** (the FR-4.4
/// freeze-before-save ordering — see the module docs).
pub(crate) fn build_apply_prompt(ctx: &PromptContext) -> String {
    format!(
        r#"You are Conceptify's artifact updater, running headless inside the project this artifact explains.

A reader asked for parts of an explanation artifact to be improved. Apply each comment below to the artifact and publish ONE new version through the `conceptify` CLI (it is on your PATH).

## Context
- Project root (your working directory): {project_root}
- Thread: "{title}" (thread id: {thread_id})
- The question the artifact answers: {question}
- Current artifact file: {artifact_path} (version {version}; your save will become version {next_version})

## Comments to apply
Each object has: `id`; `body` (what the reader wants improved); `anchor` (where it points — `cfy_id` is the target element's `data-cfy-id`, `quote.exact` is the anchored text; a null anchor concerns the artifact as a whole); `artifactVersion` (the version it was written against); `answerHtml` (the sidebar answer already given, if any — your change should deliver what it promised).

{comments}

## How to apply — exact contract; the ORDER matters
1. Copy the current artifact to a working file, e.g.: WORK=$(mktemp -d)/artifact.html
   Never edit {artifact_path} in place — the app owns that file.
2. Edit the working file until ALL the comments above are addressed:
   - Keep every existing `data-cfy-id` attribute exactly as it is — never rename, repurpose, or delete one; other comments' anchors and the app's re-attachment depend on them. New elements may introduce new `data-cfy-id`s.
   - Never hand-edit rendered diagram SVG. Each diagram carries its source in a `<!--cfy:src lang="..." for="..." ...-->` comment immediately before the rendered element: edit that DSL source, re-render it with the recorded renderer, replace the rendered element, update the cfy:src comment to match, and re-apply the `data-cfy-id`s to the new render.
   - Update `<meta name="cfy:version" content="...">` to {next_version}.
   - Keep the file fully self-contained and consistent with its existing design system.
3. When (and only when) the working file is final, mark the comments applied FIRST. For EACH comment above:
   - Write a brief note of what changed for it (HTML fragment or markdown) to its own file.
   - Then run: conceptify resolve-comment --id <comment-id> --answer-file <note-file> --applied
4. THEN publish, exactly once, as the very last CLI call:
   conceptify save-artifact --thread {thread_id} --file "$WORK"

Why this order: `--applied` freezes each comment at the artifact version it was written against, so the save's re-anchoring pass migrates only the comments you did NOT touch. Saving first would make the app try to re-anchor the very text you just rewrote. Always: all edits, then every resolve-comment --applied, then one save-artifact.

## Hard rules
- Every resolve-comment --applied call comes BEFORE the single save-artifact call, never after.
- Exactly one save-artifact per run, as the final CLI call.
- If you cannot complete the edits, do NOT run save-artifact and do NOT mark comments applied — an honest failure beats publishing a broken version.
- Your toolset is scoped: web tools are disabled, git commands that mutate the repo are denied, and your Edit/Write tools cannot touch files inside the project root — read the project freely, but write only under your working directory copy.
- If the file ~/.claude/skills/conceptify/references/follow-ups.md exists, read it first — it holds the house rules for follow-up and apply runs.
"#,
        project_root = ctx.project_root,
        title = ctx.title,
        thread_id = ctx.thread_id,
        question = ctx.question,
        artifact_path = ctx.artifact_path,
        version = ctx.artifact_version,
        next_version = ctx.artifact_version + 1,
        comments = comments_json(ctx.comments),
    )
}

/// Inputs to the in-app ask prompt (bead `conceptify-959.1`): the freshly-created
/// thread's identity, the reader's question, and the project root the agent
/// researches and runs from. Unlike the follow-up prompts there are no comments
/// or prior artifact — this is a first-generation run.
pub(crate) struct AskPromptContext<'a> {
    pub thread_id: &'a str,
    pub slug: &'a str,
    pub title: &'a str,
    pub question: &'a str,
    pub project_root: &'a str,
}

/// The FR-5.1 in-app-ask prompt. Contract highlights: read the installed
/// Conceptify skill and follow its authoring flow, but the project/thread are
/// ALREADY created (skip `ensure-project`/`create-thread`); author into a temp
/// file (never the repo); publish exactly once via `save-artifact --thread` as
/// the final CLI call. Carries the same toolset-scope hint as the follow-up
/// prompts (repo read-only, temp dirs writable, web/mutating-git denied — see
/// settings.rs `default_adapters()` / docs/api.md permission scoping).
pub(crate) fn build_ask_prompt(ctx: &AskPromptContext) -> String {
    format!(
        r#"You are Conceptify's in-app author, running headless inside the project this artifact will explain.

A reader typed a question into Conceptify and wants a self-contained HTML explanation artifact published back into the app. Author it per the Conceptify artifact spec and publish it through the `conceptify` CLI (it is on your PATH). The project and thread already exist — do NOT create them.

## Context
- Project root (your working directory): {project_root}
- Thread: "{title}" (thread id: {thread_id}, slug: {slug})
- The question to answer (verbatim): {question}

## How to author — exact contract
1. Read ~/.claude/skills/conceptify/SKILL.md in full, then every skill file it tells you to read (the artifact spec, the design system, and the rendering + self-review references). They are the contract for what a valid artifact is, not background.
2. Follow the skill's authoring flow, but the project and thread are ALREADY created for you: SKIP its "Check the CLI", "Ensure the project", and "Create the thread" steps entirely — never run `conceptify ensure-project` or `conceptify create-thread`. Start at "Author the artifact".
3. Research the real code under the project root before writing a word — the artifact must be true of THIS codebase (real file paths, real type and function names, real control flow), never generic knowledge of how such systems usually work.
4. Author the artifact into a temp file (e.g. under $TMPDIR), NEVER inside the project root — the app copies it into its own storage on save. The question above must reappear verbatim in `<meta name="cfy:question">`, and this is a new thread so `<meta name="cfy:version">` is `1`.
5. Run the skill's pre-save review in full — the source review AND the visual self-review (references/self-review.md) — and fix until every frame is clean.
6. Publish, exactly once, as the very last CLI call:
   conceptify save-artifact --thread {thread_id} --file <path-to-your-artifact.html>

## Hard rules
- The project and thread already exist: never run `conceptify ensure-project` or `conceptify create-thread`. Publish only into thread {thread_id}.
- Exactly one save-artifact per run, as the final CLI call. If you cannot produce a valid artifact, do NOT run save-artifact — an honest failure beats publishing a broken one.
- Your toolset is scoped: web tools are disabled, git commands that mutate the repo are denied, and your Edit/Write tools cannot touch files inside the project root — read the project freely, but write only under your temp working directory.
"#,
        project_root = ctx.project_root,
        title = ctx.title,
        thread_id = ctx.thread_id,
        slug = ctx.slug,
        question = ctx.question,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::Duration;

    use tauri::test::{mock_builder, mock_context, noop_assets, MockRuntime};
    use tauri::Listener;

    use crate::comments::AnchorState;
    use crate::settings::{Adapter, AgentSettings};

    // -- fixtures ------------------------------------------------------------

    fn fixture_comment(id: &str, anchored: bool, status: CommentStatus) -> Comment {
        Comment {
            id: id.to_owned(),
            thread_id: "thread-1".to_owned(),
            artifact_version: 1,
            anchor: anchored.then(|| {
                serde_json::json!({
                    "v": 1,
                    "type": "text",
                    "cfy_id": "sec-flow",
                    "start": 4,
                    "end": 9,
                    "quote": { "exact": "token", "prefix": "the ", "suffix": " is" }
                })
            }),
            body: format!("why {id}?"),
            status,
            answer_html: matches!(status, CommentStatus::Answered)
                .then(|| "<p>because.</p>".to_owned()),
            anchor_state: AnchorState::Anchored,
            created_at: "2026-07-04T00:00:00.000Z".to_owned(),
            resolved_at: None,
        }
    }

    fn fixture_prompt_ctx<'a>(comments: &'a [Comment]) -> PromptContext<'a> {
        PromptContext {
            thread_id: "thread-1",
            title: "How does OAuth work?",
            question: "Explain the OAuth 2.0 authorization code flow.",
            project_root: "/Users/chris/code/myrepo",
            artifact_path: "/Users/chris/Documents/conceptify/artifacts/p1/threads/oauth/artifact.v1.html",
            artifact_version: 1,
            comments,
        }
    }

    // -- prompt assembly (exact strings for a fixture context) ---------------

    #[test]
    fn answer_prompt_exact_for_fixture() {
        let comments = vec![
            fixture_comment("c-anchored", true, CommentStatus::Open),
            fixture_comment("c-direct", false, CommentStatus::Open),
        ];
        let prompt = build_answer_prompt(&fixture_prompt_ctx(&comments));

        let expected = r#"You are Conceptify's follow-up answerer, running headless inside the project this artifact explains.

A reader left comments (follow-up questions) on an explanation artifact. Answer each comment individually through the `conceptify` CLI (it is on your PATH). The artifact itself must not be modified in this mode.

## Context
- Project root (your working directory): /Users/chris/code/myrepo
- Thread: "How does OAuth work?" (thread id: thread-1)
- The question the artifact answers: Explain the OAuth 2.0 authorization code flow.
- Artifact file (read-only in this mode): /Users/chris/Documents/conceptify/artifacts/p1/threads/oauth/artifact.v1.html (version 1)

## Comments to answer
Each object has: `id`; `body` (the reader's question); `anchor` (where it points in the artifact — `cfy_id` is the target element's `data-cfy-id`, `quote.exact` is the anchored text; a null anchor is a direct question about the artifact as a whole); `artifactVersion` (the version it was written against); `answerHtml` (any existing answer).

[
  {
    "anchor": {
      "cfy_id": "sec-flow",
      "end": 9,
      "quote": {
        "exact": "token",
        "prefix": "the ",
        "suffix": " is"
      },
      "start": 4,
      "type": "text",
      "v": 1
    },
    "answerHtml": null,
    "artifactVersion": 1,
    "body": "why c-anchored?",
    "id": "c-anchored",
    "status": "open"
  },
  {
    "anchor": null,
    "answerHtml": null,
    "artifactVersion": 1,
    "body": "why c-direct?",
    "id": "c-direct",
    "status": "open"
  }
]

## How to answer — exact contract
1. Read the artifact file, then whatever project sources you need to ground each answer in the real code.
2. Create a scratch directory for answer files: ANSWERS=$(mktemp -d)
3. For EACH comment above, individually:
   - Write its answer to its own file, e.g. "$ANSWERS/<comment-id>.html" — an HTML fragment or markdown, concise and specific (a short paragraph or two; small code snippets welcome; no <html>/<head>/<body> wrapper).
   - Then run: conceptify resolve-comment --id <comment-id> --answer-file "$ANSWERS/<comment-id>.html"
   This marks that comment answered and shows the answer in the app immediately — resolve each comment as soon as its answer is ready, so answers land one by one.
4. Answer every comment. Never combine several comments into one resolve-comment call, and never skip one.

## Hard rules
- Do NOT modify or save the artifact: never run `conceptify save-artifact`, and never pass `--applied` to resolve-comment. Answering and applying-to-the-artifact are deliberately separate steps; this run only answers.
- Use the conceptify CLI only as specified above.
- Your toolset is scoped: web tools are disabled, git commands that mutate the repo are denied, and your Edit/Write tools cannot touch files inside the project root — read the project freely, but write only under your scratch directory.
- If the file ~/.claude/skills/conceptify/references/follow-ups.md exists, read it before answering — it holds the house rules for follow-up answers.
"#;
        assert_eq!(prompt, expected);
    }

    #[test]
    fn apply_prompt_exact_for_fixture() {
        let comments = vec![fixture_comment("c-answered", true, CommentStatus::Answered)];
        let prompt = build_apply_prompt(&fixture_prompt_ctx(&comments));

        let expected = r#"You are Conceptify's artifact updater, running headless inside the project this artifact explains.

A reader asked for parts of an explanation artifact to be improved. Apply each comment below to the artifact and publish ONE new version through the `conceptify` CLI (it is on your PATH).

## Context
- Project root (your working directory): /Users/chris/code/myrepo
- Thread: "How does OAuth work?" (thread id: thread-1)
- The question the artifact answers: Explain the OAuth 2.0 authorization code flow.
- Current artifact file: /Users/chris/Documents/conceptify/artifacts/p1/threads/oauth/artifact.v1.html (version 1; your save will become version 2)

## Comments to apply
Each object has: `id`; `body` (what the reader wants improved); `anchor` (where it points — `cfy_id` is the target element's `data-cfy-id`, `quote.exact` is the anchored text; a null anchor concerns the artifact as a whole); `artifactVersion` (the version it was written against); `answerHtml` (the sidebar answer already given, if any — your change should deliver what it promised).

[
  {
    "anchor": {
      "cfy_id": "sec-flow",
      "end": 9,
      "quote": {
        "exact": "token",
        "prefix": "the ",
        "suffix": " is"
      },
      "start": 4,
      "type": "text",
      "v": 1
    },
    "answerHtml": "<p>because.</p>",
    "artifactVersion": 1,
    "body": "why c-answered?",
    "id": "c-answered",
    "status": "answered"
  }
]

## How to apply — exact contract; the ORDER matters
1. Copy the current artifact to a working file, e.g.: WORK=$(mktemp -d)/artifact.html
   Never edit /Users/chris/Documents/conceptify/artifacts/p1/threads/oauth/artifact.v1.html in place — the app owns that file.
2. Edit the working file until ALL the comments above are addressed:
   - Keep every existing `data-cfy-id` attribute exactly as it is — never rename, repurpose, or delete one; other comments' anchors and the app's re-attachment depend on them. New elements may introduce new `data-cfy-id`s.
   - Never hand-edit rendered diagram SVG. Each diagram carries its source in a `<!--cfy:src lang="..." for="..." ...-->` comment immediately before the rendered element: edit that DSL source, re-render it with the recorded renderer, replace the rendered element, update the cfy:src comment to match, and re-apply the `data-cfy-id`s to the new render.
   - Update `<meta name="cfy:version" content="...">` to 2.
   - Keep the file fully self-contained and consistent with its existing design system.
3. When (and only when) the working file is final, mark the comments applied FIRST. For EACH comment above:
   - Write a brief note of what changed for it (HTML fragment or markdown) to its own file.
   - Then run: conceptify resolve-comment --id <comment-id> --answer-file <note-file> --applied
4. THEN publish, exactly once, as the very last CLI call:
   conceptify save-artifact --thread thread-1 --file "$WORK"

Why this order: `--applied` freezes each comment at the artifact version it was written against, so the save's re-anchoring pass migrates only the comments you did NOT touch. Saving first would make the app try to re-anchor the very text you just rewrote. Always: all edits, then every resolve-comment --applied, then one save-artifact.

## Hard rules
- Every resolve-comment --applied call comes BEFORE the single save-artifact call, never after.
- Exactly one save-artifact per run, as the final CLI call.
- If you cannot complete the edits, do NOT run save-artifact and do NOT mark comments applied — an honest failure beats publishing a broken version.
- Your toolset is scoped: web tools are disabled, git commands that mutate the repo are denied, and your Edit/Write tools cannot touch files inside the project root — read the project freely, but write only under your working directory copy.
- If the file ~/.claude/skills/conceptify/references/follow-ups.md exists, read it first — it holds the house rules for follow-up and apply runs.
"#;
        assert_eq!(prompt, expected);
    }

    #[test]
    fn ask_prompt_exact_for_fixture() {
        let prompt = build_ask_prompt(&AskPromptContext {
            thread_id: "thread-1",
            slug: "how-does-oauth-work",
            title: "How does OAuth work?",
            question: "Explain the OAuth 2.0 authorization code flow.",
            project_root: "/Users/chris/code/myrepo",
        });

        let expected = r#"You are Conceptify's in-app author, running headless inside the project this artifact will explain.

A reader typed a question into Conceptify and wants a self-contained HTML explanation artifact published back into the app. Author it per the Conceptify artifact spec and publish it through the `conceptify` CLI (it is on your PATH). The project and thread already exist — do NOT create them.

## Context
- Project root (your working directory): /Users/chris/code/myrepo
- Thread: "How does OAuth work?" (thread id: thread-1, slug: how-does-oauth-work)
- The question to answer (verbatim): Explain the OAuth 2.0 authorization code flow.

## How to author — exact contract
1. Read ~/.claude/skills/conceptify/SKILL.md in full, then every skill file it tells you to read (the artifact spec, the design system, and the rendering + self-review references). They are the contract for what a valid artifact is, not background.
2. Follow the skill's authoring flow, but the project and thread are ALREADY created for you: SKIP its "Check the CLI", "Ensure the project", and "Create the thread" steps entirely — never run `conceptify ensure-project` or `conceptify create-thread`. Start at "Author the artifact".
3. Research the real code under the project root before writing a word — the artifact must be true of THIS codebase (real file paths, real type and function names, real control flow), never generic knowledge of how such systems usually work.
4. Author the artifact into a temp file (e.g. under $TMPDIR), NEVER inside the project root — the app copies it into its own storage on save. The question above must reappear verbatim in `<meta name="cfy:question">`, and this is a new thread so `<meta name="cfy:version">` is `1`.
5. Run the skill's pre-save review in full — the source review AND the visual self-review (references/self-review.md) — and fix until every frame is clean.
6. Publish, exactly once, as the very last CLI call:
   conceptify save-artifact --thread thread-1 --file <path-to-your-artifact.html>

## Hard rules
- The project and thread already exist: never run `conceptify ensure-project` or `conceptify create-thread`. Publish only into thread thread-1.
- Exactly one save-artifact per run, as the final CLI call. If you cannot produce a valid artifact, do NOT run save-artifact — an honest failure beats publishing a broken one.
- Your toolset is scoped: web tools are disabled, git commands that mutate the repo are denied, and your Edit/Write tools cannot touch files inside the project root — read the project freely, but write only under your temp working directory.
"#;
        assert_eq!(prompt, expected);
    }

    #[test]
    fn derive_title_uses_explicit_or_truncates_question() {
        // Explicit non-blank title wins, trimmed.
        assert_eq!(derive_title(Some("  My Title "), "some question"), "My Title");
        // Blank/whitespace-only title falls through to the derived one (first
        // 8 words of the question).
        assert_eq!(
            derive_title(Some("   "), "How does the anchor re-attachment pass work exactly here"),
            "How does the anchor re-attachment pass work exactly"
        );
        assert_eq!(
            derive_title(None, "Explain the boot sequence"),
            "Explain the boot sequence"
        );
        // A derived title is never empty for a non-empty question.
        assert!(!derive_title(None, "word").is_empty());
    }

    // -- PATH preparation ------------------------------------------------------

    #[test]
    fn prepend_path_prepends_dedupes_and_falls_back() {
        assert_eq!(
            prepend_path("/x/bin", Some("/usr/bin:/bin")),
            "/x/bin:/usr/bin:/bin"
        );
        // Already a component (anywhere) → unchanged.
        assert_eq!(
            prepend_path("/usr/bin", Some("/usr/bin:/bin")),
            "/usr/bin:/bin"
        );
        assert_eq!(
            prepend_path("/x/bin", Some("/usr/bin:/x/bin:/bin")),
            "/usr/bin:/x/bin:/bin"
        );
        // Missing/empty existing PATH → macOS GUI default.
        assert_eq!(prepend_path("/x/bin", None), format!("/x/bin:{FALLBACK_PATH}"));
        assert_eq!(
            prepend_path("/x/bin", Some("")),
            format!("/x/bin:{FALLBACK_PATH}")
        );
        // Substring of a component is NOT a match.
        assert_eq!(
            prepend_path("/x", Some("/x/bin:/bin")),
            "/x:/x/bin:/bin"
        );
    }

    #[test]
    fn tail_lines_returns_last_n() {
        let path = std::env::temp_dir().join(format!(
            "conceptify-test-tail-{}-{}.log",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, "a\nb\nc\nd\ne\n").unwrap();
        assert_eq!(tail_lines(&path, 3).unwrap(), vec!["c", "d", "e"]);
        assert_eq!(tail_lines(&path, 99).unwrap(), vec!["a", "b", "c", "d", "e"]);
        assert!(tail_lines(Path::new("/nonexistent-conceptify.log"), 3).is_err());
        let _ = std::fs::remove_file(&path);
    }

    // -- flow harness ----------------------------------------------------------

    /// The one shared per-process scratch artifacts root (bead
    /// `conceptify-028`). Delegates to `artifacts::test_artifacts_root`, the
    /// single source of truth `artifacts::artifacts_root` also resolves to in
    /// test builds; isolation comes from unique per-test project ids under it.
    fn shared_artifacts_root() -> PathBuf {
        crate::artifacts::test_artifacts_root()
    }

    /// Install a process-wide `CONCEPTIFY_CLI` stub so `resolve_cli_path`
    /// never consults the machine's login shell in tests. Deterministic path
    /// formula → the benign set-race between parallel tests converges.
    fn shared_cli_stub() -> PathBuf {
        if let Ok(v) = std::env::var(CLI_ENV_OVERRIDE) {
            return PathBuf::from(v);
        }
        let dir = std::env::temp_dir().join(format!(
            "conceptify-test-cli-stub-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("conceptify");
        std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        let mut perm = std::fs::metadata(&bin).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&bin, perm).unwrap();
        std::env::set_var(CLI_ENV_OVERRIDE, bin.as_os_str());
        bin
    }

    struct Harness {
        handle: AppHandle<MockRuntime>,
        db: DbHandle,
        db_path: PathBuf,
        work_dir: PathBuf,
        project_id: String,
        thread_id: String,
        thread_updated: Arc<StdMutex<Vec<serde_json::Value>>>,
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

    fn artifact_html(version: i64) -> String {
        format!(
            r#"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<title>T</title>
<meta name="cfy:question" content="q">
<meta name="cfy:version" content="{version}">
<meta name="cfy:generated-by" content="claude-code/test">
</head><body><h1 data-cfy-id="sec-t">Version {version} token text</h1></body></html>"#
        )
    }

    fn harness(tag: &str) -> Harness {
        shared_cli_stub();
        let unique = format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let db_path = std::env::temp_dir().join(format!("conceptify-test-flows-{unique}.db"));
        let work_dir = std::env::temp_dir().join(format!("conceptify-test-flows-wd-{unique}"));
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
            crate::threads::create_thread(&conn, &project_id, "Flow Test", "the question")
                .unwrap()
                .id
        };

        let app = mock_builder()
            .manage(db.clone())
            .manage(RunRegistry::default())
            .build(mock_context(noop_assets()))
            .expect("mock app");
        let handle = app.handle().clone();

        let thread_updated: Arc<StdMutex<Vec<serde_json::Value>>> = Arc::default();
        {
            let sink = thread_updated.clone();
            handle.listen_any("thread-updated", move |event| {
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
            thread_updated,
            _app: app,
        }
    }

    impl Harness {
        fn save_artifact(&self, version: i64) {
            let conn = self.db.lock().unwrap();
            crate::artifacts::save_artifact(
                &conn,
                &shared_artifacts_root(),
                &self.thread_id,
                artifact_html(version).as_bytes(),
            )
            .unwrap_or_else(|e| panic!("save v{version}: {e:?}"));
        }

        fn add_comment(&self, body: &str) -> String {
            let conn = self.db.lock().unwrap();
            crate::comments::create_comment(&conn, &self.thread_id, 1, None, body)
                .unwrap()
                .comment
                .id
        }

        fn set_comment_status(&self, id: &str, status: CommentStatus) {
            let conn = self.db.lock().unwrap();
            crate::comments::update_comment(&conn, id, Some(status), Some("<p>a</p>"), None)
                .unwrap();
        }

        fn thread_status(&self) -> String {
            let conn = self.db.lock().unwrap();
            conn.query_row(
                "SELECT status FROM threads WHERE id = ?1",
                [&self.thread_id],
                |r| r.get(0),
            )
            .unwrap()
        }

        fn run_row(&self, run_id: &str) -> (String, String) {
            let conn = self.db.lock().unwrap();
            conn.query_row(
                "SELECT status, mode FROM follow_up_runs WHERE id = ?1",
                [run_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        }

        /// Fake agent whose argv[1] is the assembled prompt; tests use the
        /// script body to capture the prompt/env or control the exit.
        fn install_fake_agent(&self, script_body: &str) {
            self.install_fake_agent_timeout(script_body, 60);
        }

        /// Same, with an explicit run timeout (seconds) so the FR-5.3 timeout
        /// path can be exercised without a 15-minute wait.
        fn install_fake_agent_timeout(&self, script_body: &str, timeout_secs: u64) {
            let script = self.work_dir.join("fake-agent.sh");
            std::fs::write(&script, script_body).unwrap();
            let mut perm = std::fs::metadata(&script).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&script, perm).unwrap();

            let mut s = AgentSettings::default();
            s.adapters.insert(
                "fake".to_owned(),
                Adapter {
                    command: script.to_string_lossy().into_owned(),
                    args: vec!["{prompt}".to_owned()],
                    cwd: "{project_root}".to_owned(),
                },
            );
            s.default_adapter = "fake".to_owned();
            s.timeout_secs = timeout_secs;
            let conn = self.db.lock().unwrap();
            crate::settings::update_settings(&conn, &s).unwrap();
        }

        /// Status of an arbitrary thread (the ask flow creates fresh threads,
        /// so tests can't rely on `self.thread_id`).
        fn thread_status_of(&self, thread_id: &str) -> String {
            let conn = self.db.lock().unwrap();
            conn.query_row(
                "SELECT status FROM threads WHERE id = ?1",
                [thread_id],
                |r| r.get(0),
            )
            .unwrap()
        }

        /// Save an artifact for an arbitrary thread (simulates the ask agent's
        /// mid-run `save-artifact`, which flips the thread to `ready`).
        fn save_artifact_for(&self, thread_id: &str, version: i64) {
            let conn = self.db.lock().unwrap();
            crate::artifacts::save_artifact(
                &conn,
                &shared_artifacts_root(),
                thread_id,
                artifact_html(version).as_bytes(),
            )
            .unwrap_or_else(|e| panic!("save v{version} for {thread_id}: {e:?}"));
        }

        /// How many run rows exist for a thread (retry must add a fresh one).
        fn run_count(&self, thread_id: &str) -> i64 {
            let conn = self.db.lock().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM follow_up_runs WHERE thread_id = ?1",
                [thread_id],
                |r| r.get(0),
            )
            .unwrap()
        }

        fn registry(&self) -> RunRegistry {
            self.handle.state::<RunRegistry>().inner().clone()
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

    // -- ask_follow_ups (FR-4.6/4.9) -------------------------------------------

    #[tokio::test]
    async fn ask_follow_ups_spawns_answer_run_with_prompt_and_cli_on_path() {
        let h = harness("ask-ok");
        h.save_artifact(1);
        let c1 = h.add_comment("what is a token?");
        let c2 = h.add_comment("why refresh?");

        // Capture the prompt (argv[1]) and the child PATH.
        h.install_fake_agent(
            "#!/bin/sh\n\
             printf '%s' \"$1\" > \"$(dirname \"$0\")/prompt.txt\"\n\
             printf '%s' \"$PATH\" > \"$(dirname \"$0\")/path.txt\"\n\
             command -v conceptify > \"$(dirname \"$0\")/which.txt\"\n\
             exit 0\n",
        );

        let started = ask_follow_ups(&h.handle, &h.thread_id).await.unwrap();
        assert_eq!(started.mode, RunMode::Answer);
        assert_eq!(started.thread_id, h.thread_id);
        assert_eq!(
            started.target_comment_ids,
            vec![c1.clone(), c2.clone()],
            "targets are the open comments, oldest first"
        );

        let run_id = started.run_id.clone();
        assert!(
            wait_until(|| h.run_row(&run_id).0 == "completed", 15_000).await,
            "run should complete; row = {:?}",
            h.run_row(&run_id)
        );
        assert_eq!(h.run_row(&run_id).1, "answer");

        // The agent saw the assembled prompt: both comment ids, the artifact
        // path, the CLI contract, and no apply-mode instructions.
        let prompt = std::fs::read_to_string(h.work_dir.join("prompt.txt")).unwrap();
        assert!(prompt.contains(&c1) && prompt.contains(&c2), "{prompt}");
        assert!(prompt.contains("artifact.v1.html"), "{prompt}");
        assert!(prompt.contains("resolve-comment"), "{prompt}");
        assert!(prompt.contains("references/follow-ups.md"), "{prompt}");
        assert!(!prompt.contains("save-artifact --thread"), "{prompt}");

        // The child PATH starts with the CLI stub's directory, and `conceptify`
        // actually resolves in the child's environment (the §5.1 fix).
        let cli_dir = shared_cli_stub().parent().unwrap().to_string_lossy().into_owned();
        let path = std::fs::read_to_string(h.work_dir.join("path.txt")).unwrap();
        assert!(
            path.split(':').next() == Some(cli_dir.as_str()) || path.split(':').any(|c| c == cli_dir),
            "child PATH should contain the CLI dir: {path}"
        );
        let which = std::fs::read_to_string(h.work_dir.join("which.txt")).unwrap();
        assert_eq!(which.trim(), shared_cli_stub().to_string_lossy());

        // Answer mode never touches thread status and emits no thread-updated.
        assert_eq!(h.thread_status(), "ready");
        assert!(h.thread_updated.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn ask_follow_ups_guards_no_artifact_no_open_comments_and_concurrency() {
        let h = harness("ask-guards");

        // No artifact yet → NoArtifact (and no run row).
        let err = ask_follow_ups(&h.handle, &h.thread_id).await.unwrap_err();
        assert!(matches!(err, FlowError::NoArtifact), "{err:?}");

        // Artifact but no open comments → NoOpenComments.
        h.save_artifact(1);
        let err = ask_follow_ups(&h.handle, &h.thread_id).await.unwrap_err();
        assert!(matches!(err, FlowError::NoOpenComments), "{err:?}");

        // FR-4.9: while a run is active, both flows are rejected with the
        // engine's structured AlreadyRunning.
        h.add_comment("q1");
        h.install_fake_agent("#!/bin/sh\nsleep 30\n");
        let started = ask_follow_ups(&h.handle, &h.thread_id).await.unwrap();

        let err = ask_follow_ups(&h.handle, &h.thread_id).await.unwrap_err();
        assert!(
            matches!(err, FlowError::Run(RunError::AlreadyRunning { .. })),
            "{err:?}"
        );
        let err = apply_to_artifact(&h.handle, &h.thread_id, vec![])
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                FlowError::Run(RunError::AlreadyRunning { .. }) | FlowError::NoTargetComments
            ),
            "{err:?}"
        );

        // Active-run summary resolves through registry + DB row.
        {
            let conn = h.db.lock().unwrap();
            let summary = active_run_summary(&conn, &h.registry(), &h.thread_id)
                .unwrap()
                .expect("run should be active");
            assert_eq!(summary.run_id, started.run_id);
            assert_eq!(summary.mode, "answer");
            assert!(active_run_summary(&conn, &h.registry(), "other-thread")
                .unwrap()
                .is_none());
        }

        h.registry().cancel(&started.run_id).unwrap();
        let run_id = started.run_id.clone();
        assert!(wait_until(|| h.run_row(&run_id).0 == "cancelled", 15_000).await);

        // Guard released: a new ask starts cleanly (and is cancelled to clean up).
        h.install_fake_agent("#!/bin/sh\nexit 0\n");
        let again = ask_follow_ups(&h.handle, &h.thread_id).await.unwrap();
        let run_id = again.run_id.clone();
        assert!(wait_until(|| h.run_row(&run_id).0 == "completed", 15_000).await);
    }

    // -- apply_to_artifact (FR-4.7) ---------------------------------------------

    #[tokio::test]
    async fn apply_defaults_to_answered_sets_updating_then_ready() {
        let h = harness("apply-ok");
        h.save_artifact(1);
        let open_id = h.add_comment("open one");
        let answered_id = h.add_comment("answered one");
        h.set_comment_status(&answered_id, CommentStatus::Answered);

        // Sleep long enough to observe `updating` deterministically.
        h.install_fake_agent(
            "#!/bin/sh\n\
             printf '%s' \"$1\" > \"$(dirname \"$0\")/prompt.txt\"\n\
             sleep 1\n\
             exit 0\n",
        );

        let started = apply_to_artifact(&h.handle, &h.thread_id, vec![]).await.unwrap();
        assert_eq!(started.mode, RunMode::Apply);
        assert_eq!(
            started.target_comment_ids,
            vec![answered_id.clone()],
            "empty ids = all answered comments (not open, not applied)"
        );

        // Thread went `updating` before the call returned, with the event
        // (event delivery may lag the emit by a beat — poll for it).
        assert_eq!(h.thread_status(), "updating");
        assert!(wait_until(|| h.thread_updated.lock().unwrap().len() >= 1, 5_000).await);
        {
            let events = h.thread_updated.lock().unwrap().clone();
            assert_eq!(events[0]["status"], "updating");
            assert_eq!(events[0]["thread_id"], h.thread_id.as_str());
            assert_eq!(events[0]["project_id"], h.project_id.as_str());
        }

        // Run terminates (agent did NOT save) → watcher restores `ready`.
        let run_id = started.run_id.clone();
        assert!(wait_until(|| h.run_row(&run_id).0 == "completed", 15_000).await);
        assert!(
            wait_until(|| h.thread_status() == "ready", 15_000).await,
            "watcher should restore ready; status = {}",
            h.thread_status()
        );
        assert!(wait_until(|| h.thread_updated.lock().unwrap().len() >= 2, 5_000).await);
        {
            let events = h.thread_updated.lock().unwrap().clone();
            assert_eq!(events.len(), 2, "{events:?}");
            assert_eq!(events[1]["status"], "ready");
        }
        assert_eq!(h.run_row(&run_id).1, "apply");

        // The apply prompt targeted only the answered comment and carries the
        // ordering contract.
        let prompt = std::fs::read_to_string(h.work_dir.join("prompt.txt")).unwrap();
        assert!(prompt.contains(&answered_id), "{prompt}");
        assert!(!prompt.contains(&open_id), "{prompt}");
        assert!(prompt.contains("--applied"), "{prompt}");
        assert!(
            prompt.contains("mark the comments applied FIRST"),
            "{prompt}"
        );
        assert!(prompt.contains("save-artifact --thread"), "{prompt}");
    }

    #[tokio::test]
    async fn apply_failure_restores_ready_and_never_error() {
        let h = harness("apply-fail");
        h.save_artifact(1);
        let id = h.add_comment("to apply");
        h.set_comment_status(&id, CommentStatus::Answered);

        h.install_fake_agent("#!/bin/sh\nexit 3\n");
        let started = apply_to_artifact(&h.handle, &h.thread_id, vec![id]).await.unwrap();

        let run_id = started.run_id.clone();
        assert!(wait_until(|| h.run_row(&run_id).0 == "failed", 15_000).await);
        // FR-5.3-lite: the thread is restored to `ready`, never `error` —
        // failure is the run UI's to surface.
        assert!(wait_until(|| h.thread_status() == "ready", 15_000).await);
        assert!(wait_until(|| h.thread_updated.lock().unwrap().len() >= 2, 5_000).await);
        let events = h.thread_updated.lock().unwrap().clone();
        assert_eq!(events.len(), 2, "{events:?}"); // updating, then ready
        assert_eq!(events[1]["status"], "ready");
    }

    #[tokio::test]
    async fn apply_validates_targets() {
        let h = harness("apply-targets");
        h.save_artifact(1);
        let open_id = h.add_comment("still open");
        let applied_id = h.add_comment("done already");
        h.set_comment_status(&applied_id, CommentStatus::Applied);

        // Empty ids with nothing answered → NoTargetComments.
        let err = apply_to_artifact(&h.handle, &h.thread_id, vec![])
            .await
            .unwrap_err();
        assert!(matches!(err, FlowError::NoTargetComments), "{err:?}");

        // Unknown id → CommentNotFound.
        let err = apply_to_artifact(&h.handle, &h.thread_id, vec!["ghost".to_owned()])
            .await
            .unwrap_err();
        assert!(matches!(err, FlowError::CommentNotFound(_)), "{err:?}");

        // Already-applied id → AlreadyApplied.
        let err = apply_to_artifact(&h.handle, &h.thread_id, vec![applied_id])
            .await
            .unwrap_err();
        assert!(matches!(err, FlowError::AlreadyApplied(_)), "{err:?}");

        // An explicit OPEN id is legal (open → applied one-shot).
        h.install_fake_agent("#!/bin/sh\nexit 0\n");
        let started = apply_to_artifact(&h.handle, &h.thread_id, vec![open_id.clone()])
            .await
            .unwrap();
        assert_eq!(started.target_comment_ids, vec![open_id]);
        let run_id = started.run_id.clone();
        assert!(wait_until(|| h.run_row(&run_id).0 == "completed", 15_000).await);
        assert!(wait_until(|| h.thread_status() == "ready", 15_000).await);

        // No status ever touched by the rejected *starts* (guards fire before
        // any run/status work) — only the successful start's pair exists.
        assert!(wait_until(|| h.thread_updated.lock().unwrap().len() >= 2, 5_000).await);
        let events = h.thread_updated.lock().unwrap().clone();
        assert!(events.iter().all(|e| e["thread_id"] == h.thread_id.as_str()));
        assert_eq!(events.len(), 2, "{events:?}");
    }

    // -- ask_from_app / retry_ask (FR-5.1/5.2/5.3) ------------------------------

    #[tokio::test]
    async fn ask_from_app_completed_without_artifact_errors_thread() {
        let h = harness("ask-app-noartifact");
        // Capture the assembled prompt; exit 0 WITHOUT saving an artifact.
        h.install_fake_agent(
            "#!/bin/sh\n\
             printf '%s' \"$1\" > \"$(dirname \"$0\")/prompt.txt\"\n\
             exit 0\n",
        );

        let started = ask_from_app(&h.handle, &h.project_id, Some("OAuth"), "Explain OAuth.")
            .await
            .unwrap();
        assert_ne!(started.thread_id, h.thread_id, "a fresh thread is created");

        let run_id = started.run_id.clone();
        assert!(wait_until(|| h.run_row(&run_id).0 == "completed", 15_000).await);
        assert_eq!(h.run_row(&run_id).1, "ask");

        // Exit 0 but nothing saved → FR-5.3 completed-without-artifact = error.
        assert!(
            wait_until(|| h.thread_status_of(&started.thread_id) == "error", 15_000).await,
            "status = {}",
            h.thread_status_of(&started.thread_id)
        );
        // A thread-updated {status:"error"} landed for the new thread.
        assert!(
            wait_until(
                || h.thread_updated.lock().unwrap().iter().any(|e| e["thread_id"]
                    == started.thread_id.as_str()
                    && e["status"] == "error"),
                5_000
            )
            .await
        );

        // The agent saw the ask prompt: the new thread id in the save contract,
        // the skill reference, and the verbatim question.
        let prompt = std::fs::read_to_string(h.work_dir.join("prompt.txt")).unwrap();
        assert!(
            prompt.contains(&format!("save-artifact --thread {}", started.thread_id)),
            "{prompt}"
        );
        assert!(prompt.contains("~/.claude/skills/conceptify/SKILL.md"), "{prompt}");
        assert!(prompt.contains("Explain OAuth."), "{prompt}");

        // latest_run_for_thread resolves the just-finished run (FR-5.3 log state).
        let latest = {
            let conn = h.db.lock().unwrap();
            latest_run_for_thread(&conn, &started.thread_id).unwrap().unwrap()
        };
        assert_eq!(latest.run_id, run_id);
        assert_eq!(latest.mode, "ask");
        assert_eq!(latest.status, "completed");
    }

    #[tokio::test]
    async fn ask_ready_survives_when_agent_saves() {
        let h = harness("ask-ready");
        // Sleep so a save can land mid-run before the process exits 0.
        h.install_fake_agent("#!/bin/sh\nsleep 1\nexit 0\n");

        let started = ask_from_app(&h.handle, &h.project_id, None, "Explain the flow")
            .await
            .unwrap();
        assert_eq!(h.thread_status_of(&started.thread_id), "generating");

        // Simulate the agent's mid-run save-artifact → thread flips to `ready`.
        h.save_artifact_for(&started.thread_id, 1);
        assert_eq!(h.thread_status_of(&started.thread_id), "ready");

        // Run completes; the watcher's conditional generating→error must NOT
        // regress the `ready` the save set.
        let run_id = started.run_id.clone();
        assert!(wait_until(|| h.run_row(&run_id).0 == "completed", 15_000).await);
        assert!(
            !wait_until(|| h.thread_status_of(&started.thread_id) == "error", 1_500).await,
            "watcher must not regress ready → error"
        );
        assert_eq!(h.thread_status_of(&started.thread_id), "ready");
        assert!(h
            .thread_updated
            .lock()
            .unwrap()
            .iter()
            .all(|e| !(e["thread_id"] == started.thread_id.as_str() && e["status"] == "error")));
    }

    #[tokio::test]
    async fn ask_crash_errors_thread() {
        let h = harness("ask-crash");
        h.install_fake_agent("#!/bin/sh\nexit 3\n");
        let started = ask_from_app(&h.handle, &h.project_id, None, "Explain crash handling")
            .await
            .unwrap();
        let run_id = started.run_id.clone();
        assert!(wait_until(|| h.run_row(&run_id).0 == "failed", 15_000).await);
        assert!(wait_until(|| h.thread_status_of(&started.thread_id) == "error", 15_000).await);
    }

    #[tokio::test]
    async fn ask_timeout_errors_thread() {
        let h = harness("ask-timeout");
        h.install_fake_agent_timeout("#!/bin/sh\nsleep 30\n", 1);
        let started = ask_from_app(&h.handle, &h.project_id, None, "Explain timeouts")
            .await
            .unwrap();
        let run_id = started.run_id.clone();
        assert!(wait_until(|| h.run_row(&run_id).0 == "timeout", 15_000).await);
        assert!(wait_until(|| h.thread_status_of(&started.thread_id) == "error", 15_000).await);
    }

    #[tokio::test]
    async fn retry_ask_respawns_into_same_thread_and_reaches_ready() {
        let h = harness("ask-retry");
        // First ask: exit 0 without saving → error.
        h.install_fake_agent("#!/bin/sh\nexit 0\n");
        let first = ask_from_app(&h.handle, &h.project_id, Some("Retry me"), "Explain retries")
            .await
            .unwrap();
        let thread_id = first.thread_id.clone();
        assert!(wait_until(|| h.thread_status_of(&thread_id) == "error", 15_000).await);
        assert_eq!(h.run_count(&thread_id), 1);

        // Retry: a sleeping agent so we can observe `generating` + land a save.
        h.install_fake_agent("#!/bin/sh\nsleep 1\nexit 0\n");
        let retry = retry_ask(&h.handle, &thread_id).await.unwrap();
        assert_eq!(retry.thread_id, thread_id, "retry re-uses the same thread");
        assert_ne!(retry.run_id, first.run_id, "retry spawns a NEW run row");
        assert_eq!(h.run_count(&thread_id), 2);

        // Thread went back to `generating`, with a thread-updated event.
        assert_eq!(h.thread_status_of(&thread_id), "generating");
        assert!(
            wait_until(
                || h.thread_updated.lock().unwrap().iter().any(|e| e["thread_id"]
                    == thread_id.as_str()
                    && e["status"] == "generating"),
                5_000
            )
            .await
        );

        // latest_run_for_thread now points at the retry run.
        let latest = {
            let conn = h.db.lock().unwrap();
            latest_run_for_thread(&conn, &thread_id).unwrap().unwrap()
        };
        assert_eq!(latest.run_id, retry.run_id);

        // The retry's agent saves → ready; completing doesn't regress it.
        h.save_artifact_for(&thread_id, 1);
        assert_eq!(h.thread_status_of(&thread_id), "ready");
        let run_id = retry.run_id.clone();
        assert!(wait_until(|| h.run_row(&run_id).0 == "completed", 15_000).await);
        assert!(!wait_until(|| h.thread_status_of(&thread_id) == "error", 1_500).await);
        assert_eq!(h.thread_status_of(&thread_id), "ready");
    }

    #[tokio::test]
    async fn ask_guards_empty_question_and_unknown_targets() {
        let h = harness("ask-guards2");
        h.install_fake_agent("#!/bin/sh\nexit 0\n");

        // Empty/whitespace question → EmptyQuestion (no thread created).
        let err = ask_from_app(&h.handle, &h.project_id, None, "   ")
            .await
            .unwrap_err();
        assert!(matches!(err, FlowError::EmptyQuestion), "{err:?}");

        // Unknown project → ProjectNotFound (no thread created).
        let err = ask_from_app(&h.handle, "no-such-project", None, "q")
            .await
            .unwrap_err();
        assert!(matches!(err, FlowError::ProjectNotFound(_)), "{err:?}");

        // Retry on an unknown thread → ThreadNotFound (via ContextError).
        let err = retry_ask(&h.handle, "no-such-thread").await.unwrap_err();
        assert!(
            matches!(err, FlowError::Context(ContextError::ThreadNotFound(_))),
            "{err:?}"
        );
    }
}

