//! Follow-up flows on top of the run engine (PRD FR-4.6/4.7/4.8/4.9, UC4) —
//! beads `conceptify-b12.4` / `conceptify-b12.5` / `conceptify-b12.6`.
//!
//! `crate::runs` is the policy-free process engine; this module is the
//! *policy*: it assembles the prompts the headless agent actually sees,
//! prepares the child environment, starts runs, and owns the thread-status
//! side effects of the run lifecycle. Two flows:
//!
//! - **[`ask_follow_ups`]** (FR-4.6, mode `answer`): gathers every open ROOT
//!   comment and spawns one run whose contract is to answer each exchange
//!   individually via `conceptify resolve-comment`. Each root's reply chain
//!   rides along as exchange history in the prompt; the artifact is never
//!   modified. Answers land in the sidebar live through the `comment-updated`
//!   events the PATCH route already emits — no flow-side bookkeeping needed.
//! - **[`ask_single_comment`]** (epic conceptify-6xi "Ask now", mode `answer`):
//!   the same answer-mode run for exactly ONE open root, fired without gathering
//!   the batch. Shares the prompt assembly, guard, child-env, and no-watcher
//!   policy of `ask_follow_ups`.
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

use crate::comments::{Comment, CommentStatus, CommentThread};
use crate::context::{self, ContextError};
use crate::db::{self, DbHandle};
use crate::runs::{self, RunError, RunMode, RunRegistry, RunStatus, StartRun};
use crate::settings::{self, RunOverride};
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
///
/// Also carries the run's resolved selection for the retry surface (epic
/// `conceptify-e7m`, checkpoint e7m.5): `model` + `route` are what the failed
/// run actually used (route is `None` on pre-routing rows), and `overridden`
/// says whether a per-run override was recorded — when true, Retry re-applies
/// that override verbatim; when false, Retry re-derives the *current* defaults
/// (see [`load_latest_run_override`]), so the UI must not promise the old model.
#[derive(Debug, Clone)]
pub struct LatestRun {
    pub run_id: String,
    pub mode: String,
    pub status: String,
    pub model: String,
    pub route: Option<String>,
    pub overridden: bool,
}

/// A live run's identity for the FR-4.8 UI (`get_active_run` command): the
/// registry says *which* run is live, the DB row supplies its mode.
#[derive(Debug, Clone)]
pub struct ActiveRunSummary {
    pub run_id: String,
    pub thread_id: String,
    pub mode: String,
    pub status: String,
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

    #[error("comment {0} is a reply; this action targets a root comment (reply to the root instead)")]
    TargetIsReply(String),

    #[error("comment {0} is not open")]
    CommentNotOpen(String),

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

/// Everything a flow needs from one DB snapshot. `targets` is the flow-specific
/// payload: the answer flows carry exchange threads (`Vec<CommentThread>` for the
/// batch, a single `CommentThread` for Ask now), the apply flow carries the flat
/// root comments it edits (`Vec<Comment>`).
struct LoadedFlow<T> {
    project_id: String,
    project_root: String,
    title: String,
    question: String,
    artifact_path: String,
    artifact_version: i64,
    targets: T,
}

/// Start an FR-4.6 **answer** run: one headless agent for every open exchange.
///
/// Targets are the open ROOT comments only (epic conceptify-6xi): each root's
/// reply chain rides along as its exchange history in the prompt, not as a
/// separate target. A root re-opened by a user reply is naturally included.
///
/// Guards: the thread must have a saved artifact and ≥ 1 open root. Concurrent
/// exploration is accepted into the durable provider queue. Thread status is untouched —
/// answers are sidebar-only, and failures are the run UI's to surface
/// (FR-5.3-lite: no `error` status from follow-up runs).
pub async fn ask_follow_ups<R: Runtime>(
    app_handle: &AppHandle<R>,
    thread_id: &str,
    run_override: Option<RunOverride>,
) -> Result<FlowStarted, FlowError> {
    let db = app_handle.state::<DbHandle>().inner().clone();

    let tid = thread_id.to_owned();
    let loaded =
        db::with_conn_result(&db, move |conn| -> Result<LoadedFlow<Vec<CommentThread>>, FlowError> {
            let ctx = context::thread_context(conn, &tid)?;
            let latest = ctx.latest_artifact.ok_or(FlowError::NoArtifact)?;
            // Batch targets = open ROOTS only. `open_comment_threads` already
            // filters to `status='open' AND parent_id IS NULL`; the flat
            // `open_comments` now also carries open replies, so it must NOT be
            // used for targeting (epic conceptify-6xi heads-up #1).
            if ctx.open_comment_threads.is_empty() {
                return Err(FlowError::NoOpenComments);
            }
            Ok(LoadedFlow {
                project_id: ctx.project.id,
                project_root: ctx.project.root_path,
                title: ctx.thread.title,
                question: ctx.thread.initial_question,
                artifact_path: latest.file_path,
                artifact_version: latest.version,
                targets: ctx.open_comment_threads,
            })
        },
    )
    .await?;

    let prompt = build_answer_prompt(&AnswerPromptContext {
        thread_id,
        title: &loaded.title,
        question: &loaded.question,
        project_root: &loaded.project_root,
        artifact_path: &loaded.artifact_path,
        artifact_version: loaded.artifact_version,
        exchanges: &loaded.targets,
    });
    let env = child_env().await?;

    let started = runs::start_run(
        app_handle,
        StartRun {
            thread_id: thread_id.to_owned(),
            mode: RunMode::Answer,
            prompt,
            env,
            run_override,
            retry_of_run_id: None,
            response_metadata: None,
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
        target_comment_ids: loaded.targets.into_iter().map(|t| t.root.id).collect(),
    })
}

/// Start an "Ask now" **answer** run (epic conceptify-6xi) for exactly ONE root
/// comment: the same answer-mode run as [`ask_follow_ups`], but with a single
/// exchange, fired without gathering the whole batch.
///
/// Validation (structured errors, all surfaced as user-facing strings by the
/// command wrapper): the target must exist on this thread
/// ([`FlowError::CommentNotFound`]), be a ROOT rather than a reply
/// ([`FlowError::TargetIsReply`] — reply to the root instead), and be `open`
/// ([`FlowError::CommentNotOpen`]). A root re-opened by a reply is `open`, so
/// Ask now on it re-answers with the whole conversation in hand; the prompt's
/// per-exchange resolve line points the agent at the reply's id when the latest
/// unanswered message is a reply.
///
/// Guards/side effects match the batch flow exactly: concurrent answers queue,
/// thread status is untouched, and
/// there is no completion watcher (per-comment effects arrive via
/// `comment-updated`). `target_comment_ids` is the single root id (for the
/// FR-4.8 run block); the actual resolve may land on a reply row.
pub async fn ask_single_comment<R: Runtime>(
    app_handle: &AppHandle<R>,
    thread_id: &str,
    root_comment_id: &str,
    run_override: Option<RunOverride>,
) -> Result<FlowStarted, FlowError> {
    let db = app_handle.state::<DbHandle>().inner().clone();

    let tid = thread_id.to_owned();
    let target_id = root_comment_id.to_owned();
    let loaded =
        db::with_conn_result(&db, move |conn| -> Result<LoadedFlow<CommentThread>, FlowError> {
            let ctx = context::thread_context(conn, &tid)?;
            let latest = ctx.latest_artifact.ok_or(FlowError::NoArtifact)?;

            // Validate with precise errors: `list_comments_with_parent` gives
            // both the target's status and whether it is a reply (its own
            // `parent_id`), without needing to widen the shared `Comment` shape.
            let all = crate::comments::list_comments_with_parent(conn, &tid, None)
                .map_err(|e| FlowError::Context(ContextError::Comments(e)))?;
            let (comment, parent_id) = all
                .iter()
                .find(|(c, _)| c.id == target_id)
                .ok_or_else(|| FlowError::CommentNotFound(target_id.clone()))?;
            if parent_id.is_some() {
                return Err(FlowError::TargetIsReply(target_id.clone()));
            }
            if comment.status != CommentStatus::Open {
                return Err(FlowError::CommentNotOpen(target_id.clone()));
            }

            // A validated open root is present in `open_comment_threads` (same
            // `status='open' AND parent_id IS NULL` predicate); take its
            // exchange (root + ordered replies) for the single-exchange prompt.
            let exchange = ctx
                .open_comment_threads
                .into_iter()
                .find(|t| t.root.id == target_id)
                .ok_or_else(|| FlowError::CommentNotFound(target_id.clone()))?;

            Ok(LoadedFlow {
                project_id: ctx.project.id,
                project_root: ctx.project.root_path,
                title: ctx.thread.title,
                question: ctx.thread.initial_question,
                artifact_path: latest.file_path,
                artifact_version: latest.version,
                targets: exchange,
            })
        },
    )
    .await?;

    let prompt = build_answer_prompt(&AnswerPromptContext {
        thread_id,
        title: &loaded.title,
        question: &loaded.question,
        project_root: &loaded.project_root,
        artifact_path: &loaded.artifact_path,
        artifact_version: loaded.artifact_version,
        exchanges: std::slice::from_ref(&loaded.targets),
    });
    let env = child_env().await?;

    let started = runs::start_run(
        app_handle,
        StartRun {
            thread_id: thread_id.to_owned(),
            mode: RunMode::Answer,
            prompt,
            env,
            run_override,
            retry_of_run_id: None,
            response_metadata: None,
        },
    )
    .await?;
    // No completion watcher (same rationale as `ask_follow_ups`).

    Ok(FlowStarted {
        run_id: started.run_id,
        thread_id: started.thread_id,
        mode: RunMode::Answer,
        target_comment_ids: vec![loaded.targets.root.id],
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
    run_override: Option<RunOverride>,
) -> Result<FlowStarted, FlowError> {
    let db = app_handle.state::<DbHandle>().inner().clone();

    let tid = thread_id.to_owned();
    let loaded = db::with_conn_result(
        &db,
        move |conn| -> Result<LoadedFlow<Vec<Comment>>, FlowError> {
            let ctx = context::thread_context(conn, &tid)?;
            let latest = ctx.latest_artifact.ok_or(FlowError::NoArtifact)?;
            // `list_comments_with_parent` pairs each comment with its `parent_id` so
            // apply can target ROOTS only: `resolve-comment --applied` on a reply now
            // 400s (`applied` is root-only), and an answered reply must never be
            // picked up here (epic conceptify-6xi heads-up #2).
            let all = crate::comments::list_comments_with_parent(conn, &tid, None)
                .map_err(|e| FlowError::Context(ContextError::Comments(e)))?;

            let targets: Vec<Comment> = if comment_ids.is_empty() {
                all.into_iter()
                    .filter(|(c, parent)| c.status == CommentStatus::Answered && parent.is_none())
                    .map(|(c, _)| c)
                    .collect()
            } else {
                let mut picked = Vec::with_capacity(comment_ids.len());
                for id in &comment_ids {
                    let (comment, parent_id) = all
                        .iter()
                        .find(|(c, _)| &c.id == id)
                        .ok_or_else(|| FlowError::CommentNotFound(id.clone()))?;
                    if parent_id.is_some() {
                        return Err(FlowError::TargetIsReply(id.clone()));
                    }
                    if comment.status == CommentStatus::Applied {
                        return Err(FlowError::AlreadyApplied(id.clone()));
                    }
                    picked.push(comment.clone());
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
        },
    )
    .await?;

    // Contextual "Change this" requests are mutation proposals: the agent may
    // author a complete candidate, but the save path must retain it for an
    // explicit diff review instead of publishing it immediately. The comment
    // id in the durable reason lets the review surface the intended target and
    // explain any spillover.
    let preview_comment_id = loaded.targets.iter().find_map(|comment| {
        let action = comment
            .anchor
            .as_ref()?
            .get("exploration")?
            .get("action")?
            .as_str()?;
        (action == "change").then(|| comment.id.clone())
    });

    let prompt_context = PromptContext {
        thread_id,
        title: &loaded.title,
        question: &loaded.question,
        project_root: &loaded.project_root,
        artifact_path: &loaded.artifact_path,
        artifact_version: loaded.artifact_version,
        comments: &loaded.targets,
    };
    let prompt = if preview_comment_id.is_some() {
        build_revision_preview_prompt(&prompt_context)
    } else {
        build_apply_prompt(&prompt_context)
    };
    let env = child_env().await?;

    let started = runs::start_run(
        app_handle,
        StartRun {
            thread_id: thread_id.to_owned(),
            mode: RunMode::Apply,
            prompt,
            env,
            run_override,
            retry_of_run_id: None,
            response_metadata: None,
        },
    )
    .await?;

    if let Some(comment_id) = preview_comment_id {
        let run_id = started.run_id.clone();
        db::with_conn(&db, move |conn| {
            conn.execute(
                "UPDATE follow_up_runs SET status_reason = ?2 WHERE id = ?1",
                rusqlite::params![run_id, format!("preview_required:{comment_id}")],
            )
        })
        .await?;
    }

    // Run started: the thread is now visibly `updating` (PRD §4 — owned by
    // the run lifecycle). Set after durable enqueue so validation/routing
    // failures leave the status untouched.
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

/// Resolve a retained stale mutation by generating a fresh synthesis against
/// the current artifact. This is a new queued mutation with fresh base capture
/// and retry lineage; the original conflicted row/candidate remain immutable.
pub async fn rebase_conflict<R: Runtime>(
    app_handle: &AppHandle<R>,
    source_run_id: &str,
) -> Result<FlowStarted, FlowError> {
    let db = app_handle.state::<DbHandle>().inner().clone();
    let source = source_run_id.to_owned();
    let loaded = db::with_conn_result(&db, move |conn| -> Result<_, FlowError> {
        let row = conn.query_row(
            "SELECT r.thread_id, p.id, p.root_path, t.title, t.initial_question,
                    r.candidate_path, a.file_path, r.override_json, r.agent, r.model, r.route,
                    r.status_reason
             FROM follow_up_runs r
             JOIN threads t ON t.id = r.thread_id
             JOIN projects p ON p.id = t.project_id
             JOIN artifacts a ON a.thread_id = t.id
                AND a.version = r.conflict_current_version
             WHERE r.id = ?1 AND r.status = 'conflicted'
               AND COALESCE(r.conflict_resolution, 'pending') = 'pending'",
                [&source],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, Option<String>>(7)?,
                        r.get::<_, String>(8)?,
                        r.get::<_, String>(9)?,
                        r.get::<_, Option<String>>(10)?,
                        r.get::<_, Option<String>>(11)?,
                    ))
                },
            )
            .map_err(|_| FlowError::NoArtifact)?;
        Ok(row)
    })
    .await?;

    let run_override = match loaded.7 {
        Some(json) => serde_json::from_str(&json).ok(),
        None => Some(RunOverride {
            adapter: (loaded.10.as_deref() == Some("manual")).then_some(loaded.8.clone()),
            model: Some(loaded.9.clone()),
        }),
    };
    let prompt = format!(
        "Resolve a retained stale artifact candidate by synthesizing it onto the current version.\n\n\
         Thread: {thread_id}\nProject root (read-only): {project_root}\n\
         Current artifact (authoritative base; preserve all of it): {current}\n\
         Stale candidate (proposal only): {candidate}\n\n\
         Compare both documents semantically by data-cfy-id. Start from a temporary copy of the CURRENT artifact. \
         Incorporate the candidate's useful intent only where it does not discard or regress current content. \
         If the changes cannot be reconciled safely, do not publish. Preserve data-cfy-id values. \
         When the synthesis is complete, run exactly once as your final command:\n\
         conceptify save-artifact --thread {thread_id} --file <synthesized-current-copy.html>\n\
         Do not resolve comments in this recovery run.",
        thread_id = loaded.0,
        project_root = loaded.2,
        current = loaded.6,
        candidate = loaded.5,
    );
    let env = child_env().await?;
    let started = runs::start_run(
        app_handle,
        StartRun {
            thread_id: loaded.0.clone(),
            mode: RunMode::Apply,
            prompt,
            env,
            run_override,
            retry_of_run_id: Some(source_run_id.to_owned()),
            response_metadata: None,
        },
    )
    .await?;
    if let Some(comment_id) = loaded.11.as_deref().and_then(|reason| reason.strip_prefix("stale_preview:")) {
        let run_id = started.run_id.clone();
        let reason = format!("preview_required:{comment_id}");
        db::with_conn(&db, move |conn| {
            conn.execute("UPDATE follow_up_runs SET status_reason = ?2 WHERE id = ?1", rusqlite::params![run_id, reason])
        }).await?;
    }
    {
        let source = source_run_id.to_owned();
        let new_run = started.run_id.clone();
        db::with_conn(&db, move |conn| {
            conn.execute(
                "UPDATE follow_up_runs SET conflict_resolution = ?2,
                 activity_dismissed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1 AND status = 'conflicted'",
                rusqlite::params![source, format!("rebase:{new_run}")],
            )
        })
        .await?;
    }
    let thread_id = loaded.0.clone();
    let project_id = loaded.1.clone();
    db::with_conn(&db, {
        let thread_id = thread_id.clone();
        move |conn| threads::set_thread_status(conn, &thread_id, ThreadStatus::Updating)
    })
    .await?;
    emit_thread_updated(app_handle, &project_id, &thread_id, "updating");
    let finished = started.finished;
    let app = app_handle.clone();
    let db_for_watch = db.clone();
    let pid = project_id.clone();
    let tid = thread_id.clone();
    tauri::async_runtime::spawn(async move {
        let _ = finished.await;
        revert_updating(&app, &db_for_watch, &pid, &tid).await;
    });
    Ok(FlowStarted {
        run_id: started.run_id,
        thread_id,
        mode: RunMode::Apply,
        target_comment_ids: Vec::new(),
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
        Err(e) => {
            eprintln!("[conceptify-flows] failed to restore thread {thread_id} from updating: {e}")
        }
    }
}

/// Reattach the mode-specific thread-status watcher for a durable queued run
/// reconstructed after app restart. The original in-memory watcher died with
/// the process; without this replacement, a failed resumed ask/apply could
/// strand its thread in `generating`/`updating` even though the run row became
/// terminal.
pub(crate) fn attach_recovered_run_watcher<R: Runtime>(
    app_handle: AppHandle<R>,
    thread_id: String,
    mode: RunMode,
    finished: tokio::sync::oneshot::Receiver<runs::FinishedRun>,
) {
    if mode == RunMode::Answer {
        return;
    }
    let db = app_handle.state::<DbHandle>().inner().clone();
    tauri::async_runtime::spawn(async move {
        let _ = finished.await;
        let tid = thread_id.clone();
        let project_id = db::with_conn(&db, move |conn| {
            conn.query_row(
                "SELECT project_id FROM threads WHERE id = ?1",
                [&tid],
                |r| r.get::<_, String>(0),
            )
            .optional()
        })
        .await
        .ok()
        .flatten();
        let Some(project_id) = project_id else {
            return;
        };
        match mode {
            RunMode::Answer => {}
            RunMode::Apply => {
                revert_updating(&app_handle, &db, &project_id, &thread_id).await;
            }
            RunMode::Ask => {
                error_thread_if_generating(&app_handle, &db, &project_id, &thread_id).await;
            }
        }
    });
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
    run_override: Option<RunOverride>,
    response_metadata: Option<crate::skill_catalog::RunResponseMetadata>,
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
                .query_row(
                    "SELECT root_path FROM projects WHERE id = ?1",
                    [&pid],
                    |r| r.get(0),
                )
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
        run_override,
        response_metadata,
        None,
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

/// One DB snapshot for [`retry_ask`]: the thread/project identity a re-spawn
/// needs, plus the original run's persisted override to reuse (epic
/// conceptify-e7m).
struct RetryLoad {
    project_id: String,
    project_root: String,
    slug: String,
    title: String,
    question: String,
    run_override: Option<RunOverride>,
    response_metadata: Option<crate::skill_catalog::RunResponseMetadata>,
    retry_of_run_id: String,
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
    let loaded = db::with_conn_result(&db, move |conn| -> Result<RetryLoad, FlowError> {
        let ctx = context::thread_context(conn, &tid)?;
        // Reuse the ORIGINAL run's override (epic conceptify-e7m): read it back
        // from the most recent run row (the failed generation — same ordering
        // as `latest_run_for_thread`) rather than re-passing it from the
        // frontend, so retry is robust across app restarts. A NULL
        // (override-free run) or an unparseable blob → None → current defaults;
        // a real override is re-applied verbatim.
        let (retry_of_run_id, run_override, response_metadata) =
            load_latest_retry_source(conn, &tid)?;
        Ok(RetryLoad {
            project_id: ctx.project.id,
            project_root: ctx.project.root_path,
            slug: ctx.thread.slug,
            title: ctx.thread.title,
            question: ctx.thread.initial_question,
            run_override,
            response_metadata,
            retry_of_run_id,
        })
    })
    .await?;
    let RetryLoad {
        project_id,
        project_root,
        slug,
        title,
        question,
        run_override,
        response_metadata,
        retry_of_run_id,
    } = loaded;

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
        run_override,
        response_metadata,
        Some(retry_of_run_id),
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

/// The persisted per-run override on a thread's most recent run row (epic
/// conceptify-e7m), reconstructed for retry. Reads the latest `follow_up_runs`
/// row (same ordering as [`latest_run_for_thread`]) and parses its
/// `override_json`: `NULL`/absent → `None`, a valid blob → the stored
/// [`RunOverride`]. A malformed blob degrades to `None` (retry re-derives
/// current defaults) rather than failing the retry — the override is a
/// convenience, never load-bearing for correctness.
fn load_latest_retry_source(
    conn: &Connection,
    thread_id: &str,
) -> Result<
    (
        String,
        Option<RunOverride>,
        Option<crate::skill_catalog::RunResponseMetadata>,
    ),
    rusqlite::Error,
> {
    let (run_id, raw, intent_raw, skills_raw): (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = conn.query_row(
        "SELECT id, override_json, response_intent_json, selected_skills_json
             FROM follow_up_runs
             WHERE thread_id = ?1 ORDER BY started_at DESC, id DESC LIMIT 1",
        [thread_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let run_override = raw
        .and_then(|json| serde_json::from_str::<RunOverride>(&json).ok())
        .filter(|o| !o.is_empty());
    let response_metadata = intent_raw
        .and_then(|json| serde_json::from_str(&json).ok())
        .map(|intent| crate::skill_catalog::RunResponseMetadata {
            intent,
            skills: skills_raw
                .as_deref()
                .and_then(|json| serde_json::from_str(json).ok())
                .unwrap_or_default(),
        });
    Ok((run_id, run_override, response_metadata))
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
    run_override: Option<RunOverride>,
    response_metadata: Option<crate::skill_catalog::RunResponseMetadata>,
    retry_of_run_id: Option<String>,
) -> Result<AskStarted, FlowError> {
    let prompt = build_ask_prompt(&AskPromptContext {
        thread_id,
        slug,
        title,
        question,
        project_root,
        response_metadata: response_metadata.as_ref(),
    });
    let env = child_env().await?;

    let started = runs::start_run(
        app_handle,
        StartRun {
            thread_id: thread_id.to_owned(),
            mode: RunMode::Ask,
            prompt,
            env,
            run_override,
            retry_of_run_id,
            response_metadata,
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
    _registry: &RunRegistry,
    thread_id: &str,
) -> Result<Option<ActiveRunSummary>, rusqlite::Error> {
    conn.query_row(
        "SELECT id, mode, status FROM follow_up_runs
         WHERE thread_id = ?1
           AND status IN ('queued', 'starting', 'running', 'throttled', 'cancelling')
         ORDER BY queue_seq DESC, started_at DESC, id DESC LIMIT 1",
        [thread_id],
        |r| {
            Ok(ActiveRunSummary {
                run_id: r.get(0)?,
                thread_id: thread_id.to_owned(),
                mode: r.get(1)?,
                status: r.get(2)?,
            })
        },
    )
    .optional()
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
        "SELECT id, mode, status, model, route, override_json FROM follow_up_runs
         WHERE thread_id = ?1 ORDER BY started_at DESC, id DESC LIMIT 1",
        [thread_id],
        |r| {
            // `overridden` mirrors load_latest_run_override's rule exactly
            // (parseable AND non-empty), so the flag is true iff Retry will
            // actually reuse a recorded override.
            let override_json: Option<String> = r.get(5)?;
            let overridden = override_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<RunOverride>(json).ok())
                .is_some_and(|o| !o.is_empty());
            Ok(LatestRun {
                run_id: r.get(0)?,
                mode: r.get(1)?,
                status: r.get(2)?,
                model: r.get(3)?,
                route: r.get(4)?,
                overridden,
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

/// Inputs to the **apply** prompt ([`build_apply_prompt`]) — everything
/// run-specific the headless agent sees, per PRD §5.5 (thread question, artifact
/// path, the flat root comments to apply with their anchors) plus
/// identity/invariant framing. The answer prompt uses [`AnswerPromptContext`],
/// which carries exchange threads rather than a flat comment list.
pub(crate) struct PromptContext<'a> {
    pub thread_id: &'a str,
    pub title: &'a str,
    pub question: &'a str,
    pub project_root: &'a str,
    pub artifact_path: &'a str,
    pub artifact_version: i64,
    pub comments: &'a [Comment],
}

/// Inputs to the **answer** prompt ([`build_answer_prompt`], epic
/// conceptify-6xi). Same identity/context framing as [`PromptContext`], but
/// `exchanges` carries each targeted root with its ordered reply chain so the
/// prompt renders the full exchange history. Shared by the batch
/// [`ask_follow_ups`] and single-comment [`ask_single_comment`] flows — the only
/// difference between them is how many exchanges the slice holds.
pub(crate) struct AnswerPromptContext<'a> {
    pub thread_id: &'a str,
    pub title: &'a str,
    pub question: &'a str,
    pub project_root: &'a str,
    pub artifact_path: &'a str,
    pub artifact_version: i64,
    pub exchanges: &'a [CommentThread],
}

/// The comment id the agent should resolve for one exchange: the LATEST
/// unanswered (status `open`) message in the chain — the root first, then each
/// reply in order. In every flow that builds the answer prompt the root is
/// guaranteed `open` (the batch gathers only open roots; Ask now validates the
/// target root `open`), so there is always at least one candidate, and a later
/// open reply supersedes the root. This is what makes "reply → answer the reply
/// row, fresh root → answer the root row" deterministic.
fn resolve_target(thread: &CommentThread) -> &str {
    let mut target = thread.root.id.as_str();
    for reply in &thread.replies {
        if reply.status == CommentStatus::Open {
            target = reply.id.as_str();
        }
    }
    target
}

/// Render a stored anchor for an exchange transcript: the JSON passed through
/// verbatim as one compact line (same `snake_case`, key-sorted contract as
/// [`comments_json`] / get-context — serde_json `Value` maps sort keys, which
/// the exact-string prompt tests rely on), or a fixed phrase for a null anchor.
fn compact_anchor(anchor: &Option<serde_json::Value>) -> String {
    match anchor {
        Some(a) => serde_json::to_string(a).expect("anchor JSON always serializes"),
        None => "none (a direct question about the whole artifact)".to_owned(),
    }
}

/// Render one exchange (a root comment + its ordered reply chain) as the
/// transcript block the answer prompt embeds: the root body with its anchor,
/// any answer already given, then each reply in order (its `[status]` and any
/// answer), closing with the single message to answer now (see
/// [`resolve_target`]). Every message carries its own comment id so the agent
/// resolves against the right row.
fn exchange_block(index: usize, thread: &CommentThread) -> String {
    let root = &thread.root;
    let mut lines = Vec::new();
    lines.push(format!("### Exchange {index} — root comment {}", root.id));
    lines.push(format!("- anchor: {}", compact_anchor(&root.anchor)));
    lines.push(format!(
        "- reader (root {}) [{}]: {}",
        root.id,
        root.status.as_str(),
        root.body
    ));
    if let Some(answer) = &root.answer_html {
        lines.push(format!("  - answer already given: {answer}"));
    }
    for reply in &thread.replies {
        lines.push(format!(
            "- reply ({}) [{}]: {}",
            reply.id,
            reply.status.as_str(),
            reply.body
        ));
        if let Some(answer) = &reply.answer_html {
            lines.push(format!("  - answer already given: {answer}"));
        }
    }
    lines.push(format!(
        "Answer now: resolve comment {} (the latest unanswered message in this exchange).",
        resolve_target(thread)
    ));
    lines.join("\n")
}

/// The exchanges block embedded in the answer prompt: each targeted root's
/// transcript ([`exchange_block`]), numbered from 1, separated by a blank line.
fn exchanges_block(threads: &[CommentThread]) -> String {
    threads
        .iter()
        .enumerate()
        .map(|(i, thread)| exchange_block(i + 1, thread))
        .collect::<Vec<_>>()
        .join("\n\n")
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

/// Internal sentinel the flow prompt builders emit in place of the per-adapter
/// tool-scope description; [`apply_scope_note`] swaps it for the ROUTED
/// adapter's mechanism note once routing has decided which agent runs (bead
/// `conceptify-w9e`, design choice (b)). Fenced with U+2063 (INVISIBLE
/// SEPARATOR) so it can never collide with reader-supplied prompt content
/// (comment bodies, titles, questions) interpolated into the same prompt — a
/// collision could at worst duplicate the note, never a security issue (the
/// prompt is advisory; real scoping is the adapter's CLI flags), but the fence
/// rules it out anyway. Contains no `{}`, so it passes through `format!` and the
/// invocation resolver's injection-safe `{prompt}` expansion untouched, and no
/// NUL, so it survives `execve` argv.
pub(crate) const SCOPE_MECHANISM_PLACEHOLDER: &str = "\u{2063}cfy:scope-mechanism\u{2063}";

/// The per-adapter description of HOW a flow's tool scope is enforced. The
/// BEHAVIORAL rules are identical for every adapter — no web research, no
/// mutating git, never edit the target repo; every flow writes only to its
/// scratch/temp copy — and only the mechanism *claim* varies, so the note stays
/// truthful for whichever agent actually runs (verified behavior recorded in
/// settings.rs `default_adapters()`):
///
/// - `claude` — the claude CLI's own tool rules enforce it: `WebFetch`/
///   `WebSearch` denied, `Bash(git …:*)` mutating-git denies, and `Edit`/
///   `Write` fenced out of the project root. (Byte-identical to the pre-w9e
///   prompt text.)
/// - `codex` — a kernel (Seatbelt) `workspace-write` sandbox confines WRITES to
///   the project dir + `/tmp` + `$TMPDIR` and denies everything else (incl.
///   `$HOME`), but it does NOT block web access, mutating git, or writes inside
///   the repo (all verified on codex-cli 0.142.0), so those three stay
///   instruction-enforced — "the sandbox denies X; additionally do not Y".
/// - anything else (a per-run adapter override or a user-configured G6 adapter,
///   routed `manual`) — no mechanism is known, so fall back to instruction-only
///   phrasing that makes no claim about tools being disabled or sandboxed.
fn scope_mechanism(adapter: &str) -> &'static str {
    match adapter {
        "claude" => "Your toolset is scoped: web tools are disabled, git commands that mutate the repo are denied, and your Edit/Write tools cannot touch files inside the project root",
        "codex" => "Your toolset runs in a workspace-write sandbox: the kernel lets you write inside the project directory, /tmp, and $TMPDIR and denies everything outside them (including $HOME). It does not block web access, mutating git, or writes inside the project itself, so keep these as firm rules regardless: do no web research, run no git commands that mutate the repo, and do not edit the target repo",
        _ => "Keep your work within these rules whatever your tools allow: do no web research, run no git commands that mutate the repo, and do not edit files inside the project root",
    }
}

/// Substitute the per-adapter scope note into an assembled flow prompt. The run
/// engine (`runs::start_reserved`) calls this with the ROUTED adapter, right
/// after routing decides it — so the note the agent reads always matches the
/// adapter that actually runs (one routing decision, filled into the prompt it
/// produced; no divergence — bead `conceptify-w9e` design choice (b)). A no-op
/// for any prompt without the sentinel (e.g. engine-level test prompts), so
/// non-flow callers are unaffected.
pub(crate) fn apply_scope_note(prompt: &str, adapter: &str) -> String {
    prompt.replace(SCOPE_MECHANISM_PLACEHOLDER, scope_mechanism(adapter))
}

/// Agent CLIs may intentionally sanitize environment variables before their
/// own shell/tool subprocesses. Keep the inherited env as a fast path, but
/// also make the non-secret durable run id explicit on every instructed
/// save-artifact command so the CLI can authenticate mutation provenance and
/// enforce stale/preview retention in the production server.
pub(crate) fn apply_run_identity(prompt: &str, run_id: &str) -> String {
    prompt.replace(
        "conceptify save-artifact --thread",
        &format!("CONCEPTIFY_RUN_ID={run_id} conceptify save-artifact --thread"),
    )
}

/// The FR-4.6 answer-mode prompt (epic conceptify-6xi exchange-history form).
/// Each targeted root renders as an exchange transcript (root + prior answer +
/// ordered replies); the agent addresses the LATEST unanswered message and
/// resolves against that message's id — the reply row when answering a reply,
/// the root row for a fresh root. Contract highlights: one `resolve-comment` per
/// exchange (that is what makes sidebar answers land incrementally), build on
/// prior answers rather than repeat them, never `--applied`, never
/// `save-artifact`. Shared by [`ask_follow_ups`] (many exchanges) and
/// [`ask_single_comment`] (one).
pub(crate) fn build_answer_prompt(ctx: &AnswerPromptContext) -> String {
    format!(
        r#"You are Conceptify's follow-up answerer, running headless inside the project this artifact explains.

A reader left comments (follow-up questions) on an explanation artifact, and may have replied to the answers they got. Answer each exchange below through the `conceptify` CLI (it is on your PATH), responding to the latest unanswered message in the conversation. The artifact itself must not be modified in this mode.

## Context
- Project root (your working directory): {project_root}
- Thread: "{title}" (thread id: {thread_id})
- The question the artifact answers: {question}
- Artifact file (read-only in this mode): {artifact_path} (version {version})

## Exchanges to answer
Each exchange below is one conversation under a root comment: the reader's original comment with its `anchor` (where it points in the artifact — `cfy_id` is the target element's `data-cfy-id`, `quote.exact` is the anchored text; a null anchor is a direct question about the artifact as a whole), any answer already given, then any follow-up replies in order. An anchor may include `exploration.response_intent`; when present, honor its depth, language, visuals, and shape as the answer profile rather than guessing from a canned prompt. Every message is labelled with its own comment id and `[status]`; the last line of each exchange names the single message to answer now.

{exchanges}

## How to answer — exact contract
1. Read the artifact file, then whatever project sources you need to ground each answer in the real code.
2. Create a scratch directory for answer files: ANSWERS=$(mktemp -d)
3. For EACH exchange above, individually — answer ONLY its latest unanswered message:
   - Write your answer to its own file, e.g. "$ANSWERS/<message-id>.html" — an HTML fragment or markdown, concise and specific (a short paragraph or two; small code snippets welcome; no <html>/<head>/<body> wrapper).
   - Then run: conceptify resolve-comment --id <message-id> --answer-file "$ANSWERS/<message-id>.html"
   where <message-id> is the comment id named on that exchange's "Answer now" line — the reply's id when the latest message is a reply, the root's id for a fresh root comment.
   This marks that message answered and shows the answer in the app immediately — resolve each exchange as soon as its answer is ready, so answers land one by one.
4. Answer every exchange. Build on the answers already shown in an exchange — never repeat one that was already given. Never combine several exchanges into one resolve-comment call, and never skip one.

## Hard rules
- Do NOT modify or save the artifact: never run `conceptify save-artifact`, and never pass `--applied` to resolve-comment. Answering and applying-to-the-artifact are deliberately separate steps; this run only answers.
- Use the conceptify CLI only as specified above.
- {scope_mechanism} — read the project freely, but write only under your scratch directory.
- If the file ~/.claude/skills/conceptify/references/follow-ups.md exists, read it before answering — it holds the house rules for follow-up answers.
"#,
        scope_mechanism = SCOPE_MECHANISM_PLACEHOLDER,
        project_root = ctx.project_root,
        title = ctx.title,
        thread_id = ctx.thread_id,
        question = ctx.question,
        artifact_path = ctx.artifact_path,
        version = ctx.artifact_version,
        exchanges = exchanges_block(ctx.exchanges),
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
- {scope_mechanism} — read the project freely, but write only under your working directory copy.
- If the file ~/.claude/skills/conceptify/references/follow-ups.md exists, read it first — it holds the house rules for follow-up and apply runs.
"#,
        scope_mechanism = SCOPE_MECHANISM_PLACEHOLDER,
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

/// Contextual revision proposal: author the same safe working-copy mutation as
/// apply mode, but do not resolve the comment. The run-aware save path retains
/// this output as a candidate; only the later explicit UI acceptance publishes
/// it and marks the request applied.
pub(crate) fn build_revision_preview_prompt(ctx: &PromptContext) -> String {
    format!(
        r#"You are Conceptify's targeted artifact reviser. Produce a candidate for review; do not publish a visible version and do not resolve any comment.

## Context
- Project root (read-only): {project_root}
- Thread: "{title}" (thread id: {thread_id})
- Current artifact: {artifact_path} (version {version})

## Scoped revision request
{comments}

## Exact contract
1. Copy the current artifact to a temporary working file; never edit the app-owned file in place.
2. Make the smallest change that satisfies the request at its semantic `target`/`cfy_id`. Preserve every existing `data-cfy-id`. If an unavoidable supporting change spills outside `target.cfy_ids`, keep it minimal; the review UI will identify it explicitly.
3. For a diagram, edit its adjacent `cfy:src` source and re-render it; never hand-edit only the generated SVG.
4. Keep the artifact self-contained and update `<meta name="cfy:version">` to {next_version}.
5. Run exactly once as the final command:
   conceptify save-artifact --thread {thread_id} --file <working-file>

Do not run `resolve-comment` and do not use `--applied`. The save is intercepted as a proposal and requires explicit user acceptance before publication.
{scope_mechanism} — read the project freely, but write only to the temporary working copy.
"#,
        project_root = ctx.project_root,
        title = ctx.title,
        thread_id = ctx.thread_id,
        artifact_path = ctx.artifact_path,
        version = ctx.artifact_version,
        next_version = ctx.artifact_version + 1,
        comments = comments_json(ctx.comments),
        scope_mechanism = SCOPE_MECHANISM_PLACEHOLDER,
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
    pub response_metadata: Option<&'a crate::skill_catalog::RunResponseMetadata>,
}

fn response_contract(metadata: Option<&crate::skill_catalog::RunResponseMetadata>) -> String {
    let Some(metadata) = metadata else {
        return String::new();
    };
    let intent = &metadata.intent;
    let depth = match intent.depth.as_str() {
        "quick" => "Quick — cover the essential idea and why it matters; do not expand scope for its own sake.",
        "deep" => "Deep — develop the model, edge cases, trade-offs, and useful connections.",
        _ => "Balanced — cover the core idea, trade-offs, and a useful example.",
    };
    let language = match intent.language.as_str() {
        "plain" => "Plain language — define necessary terms and avoid unexplained jargon.",
        "domain_native" => {
            "Domain-native — use the field's normal terminology without recapping basics."
        }
        _ => "Familiar — assume the basics and explain specialist terms.",
    };
    let visuals = match intent.visuals.as_str() {
        "prefer" => {
            "Prefer visuals — lead with an informative diagram, map, or comparison when possible."
        }
        "avoid" => "Text only — HARD CONSTRAINT: do not generate a diagram or image.",
        _ => "When useful — include a visual only when it materially clarifies the explanation.",
    };
    let shape = match intent.shape.as_str() {
        "walkthrough" => "Walkthrough — organize the main explanation as ordered steps.",
        "comparison" => "Comparison — put the relevant alternatives side by side.",
        "reference" => "Reference — optimize the structure for scanning and later lookup.",
        _ => "Best fit — choose the clearest organization for this question.",
    };
    let skills = if metadata.skills.is_empty() {
        "- Optional skills: none. Do not add a capability merely because one is installed."
            .to_owned()
    } else {
        metadata
            .skills
            .iter()
            .map(|skill| {
                format!(
                    "- Optional skill: {} (`{}`, capability schema v{}, {} selection). Use it for its declared outcome, but never let it override the explicit response controls above.",
                    skill.name, skill.id, skill.capability_version, skill.selection
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "\n\n## Explicit response profile (contract v{})\n- Depth: {depth}\n- Language: {language}\n- Visuals: {visuals}\n- Shape: {shape}\n{skills}\n\nThese are user-facing intent values, not suggestions. A skill may refine the answer but must not silently change them. If a requested presentation is unavailable, state the fallback clearly in the artifact.",
        intent.version
    )
}

/// The FR-5.1 in-app-ask prompt. Contract highlights: read the installed
/// Conceptify skill and follow its authoring flow, but the project/thread are
/// ALREADY created (skip `ensure-project`/`create-thread`); author into a temp
/// file (never the repo); publish exactly once via `save-artifact --thread` as
/// the final CLI call. Carries the same toolset-scope hint as the follow-up
/// prompts — the [`SCOPE_MECHANISM_PLACEHOLDER`] the run engine fills with the
/// routed adapter's mechanism note ([`apply_scope_note`]); behavior forbidden is
/// identical (no web research, no mutating git, no repo writes — see settings.rs
/// `default_adapters()` / docs/api.md permission scoping).
pub(crate) fn build_ask_prompt(ctx: &AskPromptContext) -> String {
    let response_contract = response_contract(ctx.response_metadata);
    format!(
        r#"You are Conceptify's in-app author, running headless inside the project this artifact will explain.

A reader typed a question into Conceptify and wants a self-contained HTML explanation artifact published back into the app. Author it per the Conceptify artifact spec and publish it through the `conceptify` CLI (it is on your PATH). The project and thread already exist — do NOT create them.

## Context
- Project root (your working directory): {project_root}
- Thread: "{title}" (thread id: {thread_id}, slug: {slug})
- The question to answer (verbatim): {question}{response_contract}

## How to author — exact contract
1. Read ~/.claude/skills/conceptify/SKILL.md in full, then every skill file it tells you to read (the artifact spec, the design system, and the rendering + self-review references). They are the contract for what a valid artifact is, not background.
2. Follow the skill's authoring flow, but the project and thread are ALREADY created for you: SKIP its "Check the CLI", "Ensure the project", and "Create the thread" steps entirely — never run `conceptify ensure-project` or `conceptify create-thread`. Start at "Author the artifact".
3. Size your effort to the question per the skill's sizing step: a compact question — a single concept, a definition, a bit of syntax — warrants a compact artifact (a few hundred words, diagrams only if they truly earn their place, a lightweight review) and should land in a couple of minutes; reserve the full multi-diagram treatment for subsystem and architecture questions.
4. Research the real code under the project root before writing a word — the artifact must be true of THIS codebase (real file paths, real type and function names, real control flow), never generic knowledge of how such systems usually work.
5. Author the artifact into a temp file (e.g. under $TMPDIR), NEVER inside the project root — the app copies it into its own storage on save. The question above must reappear verbatim in `<meta name="cfy:question">`, and this is a new thread so `<meta name="cfy:version">` is `1`.
6. Run the skill's pre-save review, sized to the artifact (the skill's proportional rule): always the source review, plus the visual self-review — the full four-frame loop for any hand-authored SVG or generated diagram, a single narrow dark-mode render for a text-and-code-only artifact — and fix until it is clean.
7. Publish, exactly once, as the very last CLI call:
   conceptify save-artifact --thread {thread_id} --file <path-to-your-artifact.html>

## Hard rules
- The project and thread already exist: never run `conceptify ensure-project` or `conceptify create-thread`. Publish only into thread {thread_id}.
- Exactly one save-artifact per run, as the final CLI call. If you cannot produce a valid artifact, do NOT run save-artifact — an honest failure beats publishing a broken one.
- {scope_mechanism} — read the project freely, but write only under your temp working directory.
"#,
        scope_mechanism = SCOPE_MECHANISM_PLACEHOLDER,
        project_root = ctx.project_root,
        title = ctx.title,
        thread_id = ctx.thread_id,
        slug = ctx.slug,
        question = ctx.question,
        response_contract = response_contract,
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

    fn fixture_answer_ctx<'a>(exchanges: &'a [CommentThread]) -> AnswerPromptContext<'a> {
        AnswerPromptContext {
            thread_id: "thread-1",
            title: "How does OAuth work?",
            question: "Explain the OAuth 2.0 authorization code flow.",
            project_root: "/Users/chris/code/myrepo",
            artifact_path: "/Users/chris/Documents/conceptify/artifacts/p1/threads/oauth/artifact.v1.html",
            artifact_version: 1,
            exchanges,
        }
    }

    /// A single-message exchange (root comment, no replies) — the pre-reply
    /// shape, one per open root.
    fn exchange(root: Comment) -> CommentThread {
        CommentThread {
            root,
            replies: vec![],
        }
    }

    // -- prompt assembly (exact strings for a fixture context) ---------------

    #[test]
    fn answer_prompt_exact_for_fixture() {
        let exchanges = vec![
            exchange(fixture_comment("c-anchored", true, CommentStatus::Open)),
            exchange(fixture_comment("c-direct", false, CommentStatus::Open)),
        ];
        // The claude mechanism is byte-identical to the pre-w9e prompt: proves
        // both the assembly and that the claude scope note is unchanged.
        let prompt = apply_scope_note(
            &build_answer_prompt(&fixture_answer_ctx(&exchanges)),
            "claude",
        );

        let expected = r#"You are Conceptify's follow-up answerer, running headless inside the project this artifact explains.

A reader left comments (follow-up questions) on an explanation artifact, and may have replied to the answers they got. Answer each exchange below through the `conceptify` CLI (it is on your PATH), responding to the latest unanswered message in the conversation. The artifact itself must not be modified in this mode.

## Context
- Project root (your working directory): /Users/chris/code/myrepo
- Thread: "How does OAuth work?" (thread id: thread-1)
- The question the artifact answers: Explain the OAuth 2.0 authorization code flow.
- Artifact file (read-only in this mode): /Users/chris/Documents/conceptify/artifacts/p1/threads/oauth/artifact.v1.html (version 1)

## Exchanges to answer
Each exchange below is one conversation under a root comment: the reader's original comment with its `anchor` (where it points in the artifact — `cfy_id` is the target element's `data-cfy-id`, `quote.exact` is the anchored text; a null anchor is a direct question about the artifact as a whole), any answer already given, then any follow-up replies in order. An anchor may include `exploration.response_intent`; when present, honor its depth, language, visuals, and shape as the answer profile rather than guessing from a canned prompt. Every message is labelled with its own comment id and `[status]`; the last line of each exchange names the single message to answer now.

### Exchange 1 — root comment c-anchored
- anchor: {"cfy_id":"sec-flow","end":9,"quote":{"exact":"token","prefix":"the ","suffix":" is"},"start":4,"type":"text","v":1}
- reader (root c-anchored) [open]: why c-anchored?
Answer now: resolve comment c-anchored (the latest unanswered message in this exchange).

### Exchange 2 — root comment c-direct
- anchor: none (a direct question about the whole artifact)
- reader (root c-direct) [open]: why c-direct?
Answer now: resolve comment c-direct (the latest unanswered message in this exchange).

## How to answer — exact contract
1. Read the artifact file, then whatever project sources you need to ground each answer in the real code.
2. Create a scratch directory for answer files: ANSWERS=$(mktemp -d)
3. For EACH exchange above, individually — answer ONLY its latest unanswered message:
   - Write your answer to its own file, e.g. "$ANSWERS/<message-id>.html" — an HTML fragment or markdown, concise and specific (a short paragraph or two; small code snippets welcome; no <html>/<head>/<body> wrapper).
   - Then run: conceptify resolve-comment --id <message-id> --answer-file "$ANSWERS/<message-id>.html"
   where <message-id> is the comment id named on that exchange's "Answer now" line — the reply's id when the latest message is a reply, the root's id for a fresh root comment.
   This marks that message answered and shows the answer in the app immediately — resolve each exchange as soon as its answer is ready, so answers land one by one.
4. Answer every exchange. Build on the answers already shown in an exchange — never repeat one that was already given. Never combine several exchanges into one resolve-comment call, and never skip one.

## Hard rules
- Do NOT modify or save the artifact: never run `conceptify save-artifact`, and never pass `--applied` to resolve-comment. Answering and applying-to-the-artifact are deliberately separate steps; this run only answers.
- Use the conceptify CLI only as specified above.
- Your toolset is scoped: web tools are disabled, git commands that mutate the repo are denied, and your Edit/Write tools cannot touch files inside the project root — read the project freely, but write only under your scratch directory.
- If the file ~/.claude/skills/conceptify/references/follow-ups.md exists, read it before answering — it holds the house rules for follow-up answers.
"#;
        assert_eq!(prompt, expected);
    }

    #[test]
    fn answer_prompt_carries_exploration_profile_and_destination() {
        let mut root = fixture_comment("c-explore", true, CommentStatus::Open);
        root.anchor.as_mut().unwrap()["exploration"] = serde_json::json!({
            "action": "deepen",
            "destination": "inline",
            "response_intent": {
                "version": 1,
                "depth": "deep",
                "language": "domain_native",
                "visuals": "auto",
                "shape": "walkthrough"
            }
        });
        let exchanges = vec![exchange(root)];
        let prompt = build_answer_prompt(&fixture_answer_ctx(&exchanges));
        assert!(prompt.contains("exploration.response_intent"));
        assert!(prompt.contains("\"destination\":\"inline\""));
        assert!(prompt.contains("\"depth\":\"deep\""));
        assert!(prompt.contains("\"shape\":\"walkthrough\""));
    }

    #[test]
    fn answer_prompt_exact_for_chained_exchange() {
        // A re-opened root (open, but keeps its prior answer) with one open
        // reply: the exchange transcript must show the prior answer, the reply
        // in order, and point the resolve at the REPLY's id (the latest
        // unanswered message).
        let root = Comment {
            id: "c-root".to_owned(),
            thread_id: "thread-1".to_owned(),
            artifact_version: 1,
            anchor: Some(serde_json::json!({
                "v": 1,
                "type": "text",
                "cfy_id": "sec-flow",
                "start": 4,
                "end": 9,
                "quote": { "exact": "token", "prefix": "the ", "suffix": " is" }
            })),
            body: "why c-root?".to_owned(),
            status: CommentStatus::Open,
            answer_html: Some("<p>prior answer.</p>".to_owned()),
            anchor_state: AnchorState::Anchored,
            created_at: "2026-07-04T00:00:00.000Z".to_owned(),
            resolved_at: None,
        };
        let reply = Comment {
            id: "r-1".to_owned(),
            thread_id: "thread-1".to_owned(),
            artifact_version: 1,
            anchor: None,
            body: "still confused about tokens".to_owned(),
            status: CommentStatus::Open,
            answer_html: None,
            anchor_state: AnchorState::Anchored,
            created_at: "2026-07-04T00:01:00.000Z".to_owned(),
            resolved_at: None,
        };
        let exchanges = vec![CommentThread {
            root,
            replies: vec![reply],
        }];
        let prompt = apply_scope_note(
            &build_answer_prompt(&fixture_answer_ctx(&exchanges)),
            "claude",
        );

        let expected = r#"You are Conceptify's follow-up answerer, running headless inside the project this artifact explains.

A reader left comments (follow-up questions) on an explanation artifact, and may have replied to the answers they got. Answer each exchange below through the `conceptify` CLI (it is on your PATH), responding to the latest unanswered message in the conversation. The artifact itself must not be modified in this mode.

## Context
- Project root (your working directory): /Users/chris/code/myrepo
- Thread: "How does OAuth work?" (thread id: thread-1)
- The question the artifact answers: Explain the OAuth 2.0 authorization code flow.
- Artifact file (read-only in this mode): /Users/chris/Documents/conceptify/artifacts/p1/threads/oauth/artifact.v1.html (version 1)

## Exchanges to answer
Each exchange below is one conversation under a root comment: the reader's original comment with its `anchor` (where it points in the artifact — `cfy_id` is the target element's `data-cfy-id`, `quote.exact` is the anchored text; a null anchor is a direct question about the artifact as a whole), any answer already given, then any follow-up replies in order. An anchor may include `exploration.response_intent`; when present, honor its depth, language, visuals, and shape as the answer profile rather than guessing from a canned prompt. Every message is labelled with its own comment id and `[status]`; the last line of each exchange names the single message to answer now.

### Exchange 1 — root comment c-root
- anchor: {"cfy_id":"sec-flow","end":9,"quote":{"exact":"token","prefix":"the ","suffix":" is"},"start":4,"type":"text","v":1}
- reader (root c-root) [open]: why c-root?
  - answer already given: <p>prior answer.</p>
- reply (r-1) [open]: still confused about tokens
Answer now: resolve comment r-1 (the latest unanswered message in this exchange).

## How to answer — exact contract
1. Read the artifact file, then whatever project sources you need to ground each answer in the real code.
2. Create a scratch directory for answer files: ANSWERS=$(mktemp -d)
3. For EACH exchange above, individually — answer ONLY its latest unanswered message:
   - Write your answer to its own file, e.g. "$ANSWERS/<message-id>.html" — an HTML fragment or markdown, concise and specific (a short paragraph or two; small code snippets welcome; no <html>/<head>/<body> wrapper).
   - Then run: conceptify resolve-comment --id <message-id> --answer-file "$ANSWERS/<message-id>.html"
   where <message-id> is the comment id named on that exchange's "Answer now" line — the reply's id when the latest message is a reply, the root's id for a fresh root comment.
   This marks that message answered and shows the answer in the app immediately — resolve each exchange as soon as its answer is ready, so answers land one by one.
4. Answer every exchange. Build on the answers already shown in an exchange — never repeat one that was already given. Never combine several exchanges into one resolve-comment call, and never skip one.

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
        let prompt = apply_scope_note(
            &build_apply_prompt(&fixture_prompt_ctx(&comments)),
            "claude",
        );

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
        let prompt = apply_scope_note(
            &build_ask_prompt(&AskPromptContext {
                thread_id: "thread-1",
                slug: "how-does-oauth-work",
                title: "How does OAuth work?",
                question: "Explain the OAuth 2.0 authorization code flow.",
                project_root: "/Users/chris/code/myrepo",
                response_metadata: None,
            }),
            "claude",
        );

        let expected = r#"You are Conceptify's in-app author, running headless inside the project this artifact will explain.

A reader typed a question into Conceptify and wants a self-contained HTML explanation artifact published back into the app. Author it per the Conceptify artifact spec and publish it through the `conceptify` CLI (it is on your PATH). The project and thread already exist — do NOT create them.

## Context
- Project root (your working directory): /Users/chris/code/myrepo
- Thread: "How does OAuth work?" (thread id: thread-1, slug: how-does-oauth-work)
- The question to answer (verbatim): Explain the OAuth 2.0 authorization code flow.

## How to author — exact contract
1. Read ~/.claude/skills/conceptify/SKILL.md in full, then every skill file it tells you to read (the artifact spec, the design system, and the rendering + self-review references). They are the contract for what a valid artifact is, not background.
2. Follow the skill's authoring flow, but the project and thread are ALREADY created for you: SKIP its "Check the CLI", "Ensure the project", and "Create the thread" steps entirely — never run `conceptify ensure-project` or `conceptify create-thread`. Start at "Author the artifact".
3. Size your effort to the question per the skill's sizing step: a compact question — a single concept, a definition, a bit of syntax — warrants a compact artifact (a few hundred words, diagrams only if they truly earn their place, a lightweight review) and should land in a couple of minutes; reserve the full multi-diagram treatment for subsystem and architecture questions.
4. Research the real code under the project root before writing a word — the artifact must be true of THIS codebase (real file paths, real type and function names, real control flow), never generic knowledge of how such systems usually work.
5. Author the artifact into a temp file (e.g. under $TMPDIR), NEVER inside the project root — the app copies it into its own storage on save. The question above must reappear verbatim in `<meta name="cfy:question">`, and this is a new thread so `<meta name="cfy:version">` is `1`.
6. Run the skill's pre-save review, sized to the artifact (the skill's proportional rule): always the source review, plus the visual self-review — the full four-frame loop for any hand-authored SVG or generated diagram, a single narrow dark-mode render for a text-and-code-only artifact — and fix until it is clean.
7. Publish, exactly once, as the very last CLI call:
   conceptify save-artifact --thread thread-1 --file <path-to-your-artifact.html>

## Hard rules
- The project and thread already exist: never run `conceptify ensure-project` or `conceptify create-thread`. Publish only into thread thread-1.
- Exactly one save-artifact per run, as the final CLI call. If you cannot produce a valid artifact, do NOT run save-artifact — an honest failure beats publishing a broken one.
- Your toolset is scoped: web tools are disabled, git commands that mutate the repo are denied, and your Edit/Write tools cannot touch files inside the project root — read the project freely, but write only under your temp working directory.
"#;
        assert_eq!(prompt, expected);
    }

    #[test]
    fn ask_prompt_keeps_depth_language_visual_shape_and_skill_explicit() {
        let metadata = crate::skill_catalog::RunResponseMetadata {
            intent: crate::skill_catalog::ResponseIntentInput {
                version: 1,
                depth: "deep".to_owned(),
                language: "plain".to_owned(),
                visuals: "avoid".to_owned(),
                shape: "reference".to_owned(),
            },
            skills: vec![crate::skill_catalog::SelectedSkill {
                id: "conceptify".to_owned(),
                name: "Conceptify artifact".to_owned(),
                capability_version: 1,
                selection: "manual".to_owned(),
            }],
        };
        let prompt = build_ask_prompt(&AskPromptContext {
            thread_id: "t1",
            slug: "profile",
            title: "Profile",
            question: "Explain this",
            project_root: "/tmp/project",
            response_metadata: Some(&metadata),
        });
        assert!(
            prompt.contains("Depth: Deep — develop the model, edge cases"),
            "{prompt}"
        );
        assert!(
            prompt.contains("Language: Plain language — define necessary terms"),
            "{prompt}"
        );
        assert!(
            prompt.contains("HARD CONSTRAINT: do not generate a diagram or image"),
            "{prompt}"
        );
        assert!(
            prompt.contains("Shape: Reference — optimize the structure"),
            "{prompt}"
        );
        assert!(
            prompt.contains("`conceptify`, capability schema v1, manual selection"),
            "{prompt}"
        );
        assert!(prompt.contains("must not silently change them"), "{prompt}");
    }

    #[test]
    fn adaptive_profile_matrix_keeps_depth_and_language_independent() {
        let cases = [
            ("quick", "plain", "auto", "auto", "Depth: Quick", "Language: Plain language"),
            ("deep", "plain", "auto", "walkthrough", "Depth: Deep", "Language: Plain language"),
            ("balanced", "domain_native", "avoid", "reference", "Depth: Balanced", "Language: Domain-native"),
            ("deep", "domain_native", "prefer", "comparison", "Depth: Deep", "Language: Domain-native"),
        ];
        for (depth, language, visuals, shape, depth_text, language_text) in cases {
            let metadata = crate::skill_catalog::RunResponseMetadata {
                intent: crate::skill_catalog::ResponseIntentInput {
                    version: 1,
                    depth: depth.to_owned(),
                    language: language.to_owned(),
                    visuals: visuals.to_owned(),
                    shape: shape.to_owned(),
                },
                skills: Vec::new(),
            };
            let prompt = build_ask_prompt(&AskPromptContext {
                thread_id: "t1",
                slug: "matrix",
                title: "Borrow checking",
                question: "How does Rust borrow checking work?",
                project_root: "/tmp/rust-project",
                response_metadata: Some(&metadata),
            });
            assert!(prompt.contains(depth_text), "{depth}/{language}: {prompt}");
            assert!(prompt.contains(language_text), "{depth}/{language}: {prompt}");
            if visuals == "avoid" {
                assert!(prompt.contains("HARD CONSTRAINT"), "{prompt}");
            }
            if visuals == "prefer" {
                assert!(prompt.contains("Prefer visuals"), "{prompt}");
            }
        }
    }

    #[test]
    fn answer_prompt_exact_codex() {
        // Same two-exchange fixture as `answer_prompt_exact_for_fixture`; ONLY
        // the scope line differs — the codex mechanism note verbatim. The
        // behavior it forbids (no web research, no mutating git, no repo edits)
        // is identical; only the mechanism CLAIM changes, and it matches the
        // verified codex-cli 0.142.0 posture recorded in settings.rs
        // default_adapters(). This is the per-adapter exact-string counterpart.
        let exchanges = vec![
            exchange(fixture_comment("c-anchored", true, CommentStatus::Open)),
            exchange(fixture_comment("c-direct", false, CommentStatus::Open)),
        ];
        let prompt = apply_scope_note(
            &build_answer_prompt(&fixture_answer_ctx(&exchanges)),
            "codex",
        );

        let expected = r#"You are Conceptify's follow-up answerer, running headless inside the project this artifact explains.

A reader left comments (follow-up questions) on an explanation artifact, and may have replied to the answers they got. Answer each exchange below through the `conceptify` CLI (it is on your PATH), responding to the latest unanswered message in the conversation. The artifact itself must not be modified in this mode.

## Context
- Project root (your working directory): /Users/chris/code/myrepo
- Thread: "How does OAuth work?" (thread id: thread-1)
- The question the artifact answers: Explain the OAuth 2.0 authorization code flow.
- Artifact file (read-only in this mode): /Users/chris/Documents/conceptify/artifacts/p1/threads/oauth/artifact.v1.html (version 1)

## Exchanges to answer
Each exchange below is one conversation under a root comment: the reader's original comment with its `anchor` (where it points in the artifact — `cfy_id` is the target element's `data-cfy-id`, `quote.exact` is the anchored text; a null anchor is a direct question about the artifact as a whole), any answer already given, then any follow-up replies in order. An anchor may include `exploration.response_intent`; when present, honor its depth, language, visuals, and shape as the answer profile rather than guessing from a canned prompt. Every message is labelled with its own comment id and `[status]`; the last line of each exchange names the single message to answer now.

### Exchange 1 — root comment c-anchored
- anchor: {"cfy_id":"sec-flow","end":9,"quote":{"exact":"token","prefix":"the ","suffix":" is"},"start":4,"type":"text","v":1}
- reader (root c-anchored) [open]: why c-anchored?
Answer now: resolve comment c-anchored (the latest unanswered message in this exchange).

### Exchange 2 — root comment c-direct
- anchor: none (a direct question about the whole artifact)
- reader (root c-direct) [open]: why c-direct?
Answer now: resolve comment c-direct (the latest unanswered message in this exchange).

## How to answer — exact contract
1. Read the artifact file, then whatever project sources you need to ground each answer in the real code.
2. Create a scratch directory for answer files: ANSWERS=$(mktemp -d)
3. For EACH exchange above, individually — answer ONLY its latest unanswered message:
   - Write your answer to its own file, e.g. "$ANSWERS/<message-id>.html" — an HTML fragment or markdown, concise and specific (a short paragraph or two; small code snippets welcome; no <html>/<head>/<body> wrapper).
   - Then run: conceptify resolve-comment --id <message-id> --answer-file "$ANSWERS/<message-id>.html"
   where <message-id> is the comment id named on that exchange's "Answer now" line — the reply's id when the latest message is a reply, the root's id for a fresh root comment.
   This marks that message answered and shows the answer in the app immediately — resolve each exchange as soon as its answer is ready, so answers land one by one.
4. Answer every exchange. Build on the answers already shown in an exchange — never repeat one that was already given. Never combine several exchanges into one resolve-comment call, and never skip one.

## Hard rules
- Do NOT modify or save the artifact: never run `conceptify save-artifact`, and never pass `--applied` to resolve-comment. Answering and applying-to-the-artifact are deliberately separate steps; this run only answers.
- Use the conceptify CLI only as specified above.
- Your toolset runs in a workspace-write sandbox: the kernel lets you write inside the project directory, /tmp, and $TMPDIR and denies everything outside them (including $HOME). It does not block web access, mutating git, or writes inside the project itself, so keep these as firm rules regardless: do no web research, run no git commands that mutate the repo, and do not edit the target repo — read the project freely, but write only under your scratch directory.
- If the file ~/.claude/skills/conceptify/references/follow-ups.md exists, read it before answering — it holds the house rules for follow-up answers.
"#;
        assert_eq!(prompt, expected);
    }

    #[test]
    fn scope_note_lines_exact_for_every_adapter_and_flow() {
        // Every flow prompt carries a per-write-location scope line; the ROUTED
        // adapter selects the mechanism clause verbatim while the behavioral
        // rules stay fixed. Exact-string per (adapter, flow) across all three
        // write-locations — nothing loosened, and the sentinel must never
        // survive substitution.
        const CLAUDE: &str = "Your toolset is scoped: web tools are disabled, git commands that mutate the repo are denied, and your Edit/Write tools cannot touch files inside the project root";
        const CODEX: &str = "Your toolset runs in a workspace-write sandbox: the kernel lets you write inside the project directory, /tmp, and $TMPDIR and denies everything outside them (including $HOME). It does not block web access, mutating git, or writes inside the project itself, so keep these as firm rules regardless: do no web research, run no git commands that mutate the repo, and do not edit the target repo";
        const FALLBACK: &str = "Keep your work within these rules whatever your tools allow: do no web research, run no git commands that mutate the repo, and do not edit files inside the project root";

        let comments = vec![fixture_comment("c-answered", true, CommentStatus::Answered)];
        let exchanges = vec![exchange(fixture_comment("c-1", true, CommentStatus::Open))];
        let bases = [
            (
                "your scratch directory",
                build_answer_prompt(&fixture_answer_ctx(&exchanges)),
            ),
            (
                "your working directory copy",
                build_apply_prompt(&fixture_prompt_ctx(&comments)),
            ),
            (
                "your temp working directory",
                build_ask_prompt(&AskPromptContext {
                    thread_id: "thread-1",
                    slug: "how-does-oauth-work",
                    title: "How does OAuth work?",
                    question: "Explain the OAuth 2.0 authorization code flow.",
                    project_root: "/Users/chris/code/myrepo",
                    response_metadata: None,
                }),
            ),
        ];

        for (write_loc, base) in &bases {
            assert!(
                base.contains(SCOPE_MECHANISM_PLACEHOLDER),
                "the builder emits the sentinel, not a baked mechanism"
            );
            // A custom/unknown adapter (per-run override or a G6-configured
            // harness, routed `manual`) gets the instruction-only fallback.
            for (adapter, mechanism) in [("claude", CLAUDE), ("codex", CODEX), ("my-agent", FALLBACK)] {
                let out = apply_scope_note(base, adapter);
                let expected_line = format!(
                    "- {mechanism} — read the project freely, but write only under {write_loc}."
                );
                assert!(out.contains(&expected_line), "{adapter} / {write_loc}:\n{out}");
                assert!(
                    !out.contains(SCOPE_MECHANISM_PLACEHOLDER),
                    "sentinel gone for {adapter}"
                );
            }
            // The fallback makes NO mechanism claim — neither built-in's wording.
            let fb = apply_scope_note(base, "my-agent");
            assert!(!fb.contains("workspace-write sandbox"), "no codex claim:\n{fb}");
            assert!(!fb.contains("web tools are disabled"), "no claude claim:\n{fb}");
        }
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

        fn add_revision_comment(&self, body: &str) -> String {
            let conn = self.db.lock().unwrap();
            crate::comments::create_comment(
                &conn,
                &self.thread_id,
                1,
                Some(&serde_json::json!({
                    "v": 1, "type": "element", "cfy_id": "sec-t",
                    "target": {
                        "kind": "block", "label": "Version heading", "excerpt": "Version 1 token text",
                        "cfy_ids": ["sec-t"], "multi_block": false
                    },
                    "exploration": { "action": "change", "destination": "revision" }
                })),
                body,
            ).unwrap().comment.id
        }

        /// Create a reply under `parent_id` (an answered/applied root is re-opened
        /// by this, per `create_reply`); returns the reply's id.
        fn add_reply(&self, parent_id: &str, body: &str) -> String {
            let conn = self.db.lock().unwrap();
            crate::comments::create_reply(&conn, &self.thread_id, parent_id, body)
                .unwrap()
                .comment
                .id
        }

        fn set_comment_status(&self, id: &str, status: CommentStatus) {
            let conn = self.db.lock().unwrap();
            crate::comments::update_comment(&conn, id, Some(status), Some("<p>a</p>"), None)
                .unwrap();
        }

        fn comment_status(&self, id: &str) -> String {
            let conn = self.db.lock().unwrap();
            conn.query_row("SELECT status FROM comments WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap()
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

        /// The `(agent, model, override_json)` recorded on a run row — for the
        /// e7m override-persistence / retry-reuse assertions.
        fn run_selection(&self, run_id: &str) -> (String, String, Option<String>) {
            let conn = self.db.lock().unwrap();
            conn.query_row(
                "SELECT agent, model, override_json FROM follow_up_runs WHERE id = ?1",
                [run_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
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

        let started = ask_follow_ups(&h.handle, &h.thread_id, None).await.unwrap();
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
        // The per-adapter scope note was substituted BEFORE spawn (bead
        // conceptify-w9e): the raw sentinel never reaches the agent, and this
        // fake adapter (a custom default → routed `manual`) gets the
        // instruction-only fallback note, not claude's "web tools are disabled".
        assert!(!prompt.contains(SCOPE_MECHANISM_PLACEHOLDER), "sentinel substituted: {prompt}");
        assert!(prompt.contains("do no web research"), "fallback scope note reached agent: {prompt}");

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
        let err = ask_follow_ups(&h.handle, &h.thread_id, None).await.unwrap_err();
        assert!(matches!(err, FlowError::NoArtifact), "{err:?}");

        // Artifact but no open comments → NoOpenComments.
        h.save_artifact(1);
        let err = ask_follow_ups(&h.handle, &h.thread_id, None).await.unwrap_err();
        assert!(matches!(err, FlowError::NoOpenComments), "{err:?}");

        // Concurrent exploration submissions are accepted immediately. The
        // fake/manual pool has capacity one, so the second is durably queued.
        h.add_comment("q1");
        h.install_fake_agent("#!/bin/sh\nsleep 30\n");
        let started = ask_follow_ups(&h.handle, &h.thread_id, None).await.unwrap();

        let queued = ask_follow_ups(&h.handle, &h.thread_id, None).await.unwrap();
        assert_eq!(h.run_row(&queued.run_id).0, "queued");
        let err = apply_to_artifact(&h.handle, &h.thread_id, vec![], None)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                FlowError::NoTargetComments
            ),
            "{err:?}"
        );

        // Active-run summary resolves through registry + DB row.
        {
            let conn = h.db.lock().unwrap();
            let summary = active_run_summary(&conn, &h.registry(), &h.thread_id)
                .unwrap()
                .expect("run should be active");
            assert_eq!(summary.run_id, queued.run_id);
            assert_eq!(summary.mode, "answer");
            assert_eq!(summary.status, "queued");
            assert!(active_run_summary(&conn, &h.registry(), "other-thread")
                .unwrap()
                .is_none());
        }

        runs::cancel_durable(&h.db, &h.registry(), &queued.run_id)
            .await
            .unwrap();
        assert!(wait_until(|| h.run_row(&queued.run_id).0 == "cancelled", 15_000).await);
        runs::cancel_durable(&h.db, &h.registry(), &started.run_id)
            .await
            .unwrap();
        let run_id = started.run_id.clone();
        assert!(wait_until(|| h.run_row(&run_id).0 == "cancelled", 15_000).await);

        // Released capacity admits a new ask cleanly.
        h.install_fake_agent("#!/bin/sh\nexit 0\n");
        let again = ask_follow_ups(&h.handle, &h.thread_id, None).await.unwrap();
        let run_id = again.run_id.clone();
        assert!(wait_until(|| h.run_row(&run_id).0 == "completed", 15_000).await);
    }

    // -- ask_single_comment (Ask now, epic conceptify-6xi) ---------------------

    #[tokio::test]
    async fn ask_single_comment_answers_reply_row_end_to_end() {
        let h = harness("ask-single-reply");
        h.save_artifact(1);

        // Root answered, then a user reply re-opens it (root → open, prior
        // answer kept; reply → open). Ask now on the root must direct the agent
        // at the REPLY row (the latest unanswered message).
        let root = h.add_comment("why the root?");
        h.set_comment_status(&root, CommentStatus::Answered);
        let reply = h.add_reply(&root, "still confused about tokens");
        assert_eq!(h.comment_status(&root), "open", "reply re-opened the root");
        assert_eq!(h.comment_status(&reply), "open");

        h.install_fake_agent(
            "#!/bin/sh\n\
             printf '%s' \"$1\" > \"$(dirname \"$0\")/prompt.txt\"\n\
             exit 0\n",
        );

        let started = ask_single_comment(&h.handle, &h.thread_id, &root, None)
            .await
            .unwrap();
        assert_eq!(started.mode, RunMode::Answer);
        assert_eq!(started.thread_id, h.thread_id);
        assert_eq!(
            started.target_comment_ids,
            vec![root.clone()],
            "the DTO target is the single ROOT (the resolve may land on its reply)"
        );

        let run_id = started.run_id.clone();
        assert!(wait_until(|| h.run_row(&run_id).0 == "completed", 15_000).await);
        assert_eq!(h.run_row(&run_id).1, "answer");

        // The prompt carries the exchange history and points the resolve at the
        // reply (the latest unanswered message), never the root.
        let prompt = std::fs::read_to_string(h.work_dir.join("prompt.txt")).unwrap();
        assert!(prompt.contains("### Exchange 1 — root comment"), "{prompt}");
        assert!(prompt.contains("answer already given: <p>a</p>"), "{prompt}");
        assert!(prompt.contains("still confused about tokens"), "{prompt}");
        assert!(
            prompt.contains(&format!(
                "Answer now: resolve comment {reply} (the latest unanswered"
            )),
            "{prompt}"
        );

        // The CLI stub is a no-op, so simulate the resolve the agent's
        // `resolve-comment --id <reply>` performs: the reply advances to
        // `answered`, and — because it is the chain's latest message — the
        // re-opened root flips back to `answered` in the same transaction
        // (root status reflects the latest exchange state), its original
        // answer preserved.
        h.set_comment_status(&reply, CommentStatus::Answered);
        assert_eq!(h.comment_status(&reply), "answered");
        assert_eq!(
            h.comment_status(&root),
            "answered",
            "answering the latest reply flips the re-opened root back to answered"
        );

        // Answer mode never touches thread status and emits no thread-updated.
        assert_eq!(h.thread_status(), "ready");
        assert!(h.thread_updated.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn ask_single_comment_validates_target_and_guards_concurrency() {
        let h = harness("ask-single-guards");

        // No artifact yet → NoArtifact (checked before the target is looked up,
        // so the id need not exist).
        let err = ask_single_comment(&h.handle, &h.thread_id, "any-comment", None)
            .await
            .unwrap_err();
        assert!(matches!(err, FlowError::NoArtifact), "{err:?}");

        h.save_artifact(1);

        // Unknown id → CommentNotFound.
        let err = ask_single_comment(&h.handle, &h.thread_id, "ghost", None)
            .await
            .unwrap_err();
        assert!(matches!(err, FlowError::CommentNotFound(_)), "{err:?}");

        // A reply target → TargetIsReply (Ask now targets a root; reply to it).
        let root = h.add_comment("root q");
        let reply = h.add_reply(&root, "reply q"); // root open → no re-open; reply open
        let err = ask_single_comment(&h.handle, &h.thread_id, &reply, None)
            .await
            .unwrap_err();
        assert!(matches!(err, FlowError::TargetIsReply(_)), "{err:?}");

        // A non-open (answered) root → CommentNotOpen.
        let answered_root = h.add_comment("answered root");
        h.set_comment_status(&answered_root, CommentStatus::Answered);
        let err = ask_single_comment(&h.handle, &h.thread_id, &answered_root, None)
            .await
            .unwrap_err();
        assert!(matches!(err, FlowError::CommentNotOpen(_)), "{err:?}");

        // A second Ask now and a batch are accepted into the durable queue;
        // they do not overwrite or reject the active exploration.
        h.install_fake_agent("#!/bin/sh\nsleep 30\n");
        let started = ask_single_comment(&h.handle, &h.thread_id, &root, None)
            .await
            .unwrap();
        assert_eq!(started.target_comment_ids, vec![root.clone()]);

        let second = ask_single_comment(&h.handle, &h.thread_id, &root, None)
            .await
            .unwrap();
        let batch = ask_follow_ups(&h.handle, &h.thread_id, None).await.unwrap();
        assert_eq!(h.run_row(&second.run_id).0, "queued");
        assert_eq!(h.run_row(&batch.run_id).0, "queued");

        for id in [&second.run_id, &batch.run_id, &started.run_id] {
            runs::cancel_durable(&h.db, &h.registry(), id).await.unwrap();
        }
        let run_id = started.run_id.clone();
        assert!(wait_until(|| h.run_row(&run_id).0 == "cancelled", 15_000).await);

        // No answer run ever touched thread status.
        assert_eq!(h.thread_status(), "ready");
        assert!(h.thread_updated.lock().unwrap().is_empty());
    }

    // -- apply_to_artifact (FR-4.7) ---------------------------------------------

    #[tokio::test]
    async fn apply_targets_roots_only_never_answered_replies() {
        let h = harness("apply-roots-only");
        h.save_artifact(1);

        // An answered ROOT with no reply stays answered — a valid apply target.
        let root_a = h.add_comment("root A");
        h.set_comment_status(&root_a, CommentStatus::Answered);

        // A second root gets a reply (which re-opens it) that is then answered:
        // the answered REPLY must never be an apply target — `resolve-comment
        // --applied` on a reply now 400s (epic conceptify-6xi heads-up #2).
        // Answering the chain's latest reply flips root B back to `answered`
        // (root status = latest exchange state), making the ROOT a valid
        // apply-all target again — the reply row itself never is.
        let root_b = h.add_comment("root B");
        h.set_comment_status(&root_b, CommentStatus::Answered);
        let reply_b = h.add_reply(&root_b, "reply on B"); // re-opens B → open
        assert_eq!(h.comment_status(&root_b), "open", "reply re-opened root B");
        h.set_comment_status(&reply_b, CommentStatus::Answered);
        assert_eq!(h.comment_status(&root_b), "answered", "root B flipped back");
        assert_eq!(h.comment_status(&reply_b), "answered");

        // A third root left open is never an apply-all target.
        let root_c = h.add_comment("root C (open)");

        // An explicit reply id is rejected outright (applying a reply is invalid).
        let err = apply_to_artifact(&h.handle, &h.thread_id, vec![reply_b.clone()], None)
            .await
            .unwrap_err();
        assert!(matches!(err, FlowError::TargetIsReply(_)), "{err:?}");

        // Default (empty ids) targets the answered ROOTS (including the chain
        // root flipped back by its answered reply), never the answered reply
        // and never the still-open root C.
        h.install_fake_agent("#!/bin/sh\nexit 0\n");
        let started = apply_to_artifact(&h.handle, &h.thread_id, vec![], None)
            .await
            .unwrap();
        assert_eq!(
            started.target_comment_ids,
            vec![root_a.clone(), root_b.clone()],
            "answered roots (chain root included) are applied"
        );
        assert!(!started.target_comment_ids.contains(&reply_b));
        assert!(!started.target_comment_ids.contains(&root_c));

        let run_id = started.run_id.clone();
        assert!(wait_until(|| h.run_row(&run_id).0 == "completed", 15_000).await);
        assert!(wait_until(|| h.thread_status() == "ready", 15_000).await);
    }

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

        let started = apply_to_artifact(&h.handle, &h.thread_id, vec![], None).await.unwrap();
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
        assert!(prompt.contains(&format!("CONCEPTIFY_RUN_ID={} conceptify save-artifact", run_id)), "run identity must survive agent env sanitization: {prompt}");
    }

    #[tokio::test]
    async fn contextual_change_run_is_marked_for_preview_and_never_resolves_in_prompt() {
        let h = harness("revision-preview");
        h.save_artifact(1);
        let comment_id = h.add_revision_comment("Rewrite this heading");
        h.install_fake_agent(
            "#!/bin/sh\nprintf '%s' \"$1\" > \"$(dirname \"$0\")/prompt.txt\"\nexit 0\n",
        );

        let started = apply_to_artifact(&h.handle, &h.thread_id, vec![comment_id.clone()], None)
            .await.unwrap();
        let reason: Option<String> = h.db.lock().unwrap().query_row(
            "SELECT status_reason FROM follow_up_runs WHERE id = ?1",
            [&started.run_id], |r| r.get(0),
        ).unwrap();
        assert_eq!(reason.as_deref(), Some(format!("preview_required:{comment_id}").as_str()));
        assert_eq!(h.comment_status(&comment_id), "open", "proposal must not pre-apply its request");
        assert!(wait_until(|| h.run_row(&started.run_id).0 == "completed", 15_000).await);
        let prompt = std::fs::read_to_string(h.work_dir.join("prompt.txt")).unwrap();
        assert!(prompt.contains("Produce a candidate for review"), "{prompt}");
        assert!(prompt.contains("smallest change"), "{prompt}");
        assert!(prompt.contains("do not use `--applied`"), "{prompt}");
        assert!(!prompt.contains("resolve-comment --id"), "{prompt}");
        assert!(prompt.contains(&format!("CONCEPTIFY_RUN_ID={} conceptify save-artifact", started.run_id)), "{prompt}");
    }

    #[tokio::test]
    async fn apply_failure_restores_ready_and_never_error() {
        let h = harness("apply-fail");
        h.save_artifact(1);
        let id = h.add_comment("to apply");
        h.set_comment_status(&id, CommentStatus::Answered);

        h.install_fake_agent("#!/bin/sh\nexit 3\n");
        let started = apply_to_artifact(&h.handle, &h.thread_id, vec![id], None).await.unwrap();

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
        let err = apply_to_artifact(&h.handle, &h.thread_id, vec![], None)
            .await
            .unwrap_err();
        assert!(matches!(err, FlowError::NoTargetComments), "{err:?}");

        // Unknown id → CommentNotFound.
        let err = apply_to_artifact(&h.handle, &h.thread_id, vec!["ghost".to_owned()], None)
            .await
            .unwrap_err();
        assert!(matches!(err, FlowError::CommentNotFound(_)), "{err:?}");

        // Already-applied id → AlreadyApplied.
        let err = apply_to_artifact(&h.handle, &h.thread_id, vec![applied_id], None)
            .await
            .unwrap_err();
        assert!(matches!(err, FlowError::AlreadyApplied(_)), "{err:?}");

        // An explicit OPEN id is legal (open → applied one-shot).
        h.install_fake_agent("#!/bin/sh\nexit 0\n");
        let started = apply_to_artifact(&h.handle, &h.thread_id, vec![open_id.clone()], None)
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

        let started = ask_from_app(
            &h.handle,
            &h.project_id,
            Some("OAuth"),
            "Explain OAuth.",
            None,
            None,
        )
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
                || {
                    h.thread_updated.lock().unwrap().iter().any(|e| {
                        e["thread_id"] == started.thread_id.as_str() && e["status"] == "error"
                    })
                },
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

        let started = ask_from_app(
            &h.handle,
            &h.project_id,
            None,
            "Explain the flow",
            None,
            None,
        )
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
        let started = ask_from_app(
            &h.handle,
            &h.project_id,
            None,
            "Explain crash handling",
            None,
            None,
        )
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
        let started = ask_from_app(
            &h.handle,
            &h.project_id,
            None,
            "Explain timeouts",
            None,
            None,
        )
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
        let response_metadata = crate::skill_catalog::RunResponseMetadata {
            intent: crate::skill_catalog::ResponseIntentInput {
                version: 1,
                depth: "deep".to_owned(),
                language: "plain".to_owned(),
                visuals: "avoid".to_owned(),
                shape: "reference".to_owned(),
            },
            skills: Vec::new(),
        };
        let first = ask_from_app(
            &h.handle,
            &h.project_id,
            Some("Retry me"),
            "Explain retries",
            None,
            Some(response_metadata),
        )
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
        let retry_of: Option<String> =
            h.db.lock()
                .unwrap()
                .query_row(
                    "SELECT retry_of_run_id FROM follow_up_runs WHERE id = ?1",
                    [&retry.run_id],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(retry_of.as_deref(), Some(first.run_id.as_str()));
        let profiles: Vec<(String, String)> = {
            let conn = h.db.lock().unwrap();
            let mut stmt = conn
                .prepare(
                    "SELECT response_intent_json, selected_skills_json
                     FROM follow_up_runs WHERE id IN (?1, ?2) ORDER BY id",
                )
                .unwrap();
            stmt.query_map(rusqlite::params![first.run_id, retry.run_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
        };
        assert_eq!(profiles.len(), 2);
        assert_eq!(
            profiles[0], profiles[1],
            "retry preserves immutable profile and skills"
        );

        // Thread went back to `generating`, with a thread-updated event.
        assert_eq!(h.thread_status_of(&thread_id), "generating");
        assert!(
            wait_until(
                || h.thread_updated
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|e| e["thread_id"] == thread_id.as_str() && e["status"] == "generating"),
                5_000
            )
            .await
        );

        // latest_run_for_thread now points at the retry run; an override-free
        // run reports `overridden: false` on the retry surface (e7m.5).
        let latest = {
            let conn = h.db.lock().unwrap();
            latest_run_for_thread(&conn, &thread_id).unwrap().unwrap()
        };
        assert_eq!(latest.run_id, retry.run_id);
        assert!(!latest.overridden);

        // The retry's agent saves → ready; completing doesn't regress it.
        h.save_artifact_for(&thread_id, 1);
        assert_eq!(h.thread_status_of(&thread_id), "ready");
        let run_id = retry.run_id.clone();
        assert!(wait_until(|| h.run_row(&run_id).0 == "completed", 15_000).await);
        assert!(!wait_until(|| h.thread_status_of(&thread_id) == "error", 1_500).await);
        assert_eq!(h.thread_status_of(&thread_id), "ready");
    }

    #[tokio::test]
    async fn retry_reuses_persisted_override() {
        // epic conceptify-e7m: an ask started with a model override persists it
        // on the run row; retry re-reads and re-applies the SAME override
        // (proven by the retry run's resolved model column), without the
        // frontend re-passing it.
        let h = harness("ask-retry-override");
        h.install_fake_agent("#!/bin/sh\nexit 0\n");

        let over = RunOverride {
            adapter: None,
            model: Some("custom-ask-model".to_owned()),
        };
        let first = ask_from_app(
            &h.handle,
            &h.project_id,
            Some("Override retry"),
            "Explain overrides",
            Some(over),
            None,
        )
        .await
        .unwrap();
        let thread_id = first.thread_id.clone();
        assert!(wait_until(|| h.thread_status_of(&thread_id) == "error", 15_000).await);

        // The original run recorded the resolved override model + the intent.
        let (agent, model, over_json) = h.run_selection(&first.run_id);
        assert_eq!(agent, "fake");
        assert_eq!(model, "custom-ask-model");
        assert_eq!(over_json.as_deref(), Some(r#"{"model":"custom-ask-model"}"#));

        // Retry takes NO override argument — it reuses the persisted one.
        h.install_fake_agent("#!/bin/sh\nexit 3\n");
        let retry = retry_ask(&h.handle, &thread_id).await.unwrap();
        assert_ne!(retry.run_id, first.run_id);
        assert!(wait_until(|| h.run_row(&retry.run_id).0 == "failed", 15_000).await);

        // The retry run resolved the SAME override model + re-persisted it.
        let (r_agent, r_model, r_over) = h.run_selection(&retry.run_id);
        assert_eq!(r_agent, "fake");
        assert_eq!(r_model, "custom-ask-model", "retry reused the original override model");
        assert_eq!(r_over.as_deref(), Some(r#"{"model":"custom-ask-model"}"#));

        // The retry surface (get_latest_run → LatestRun) exposes the resolved
        // selection + override flag for display (e7m.5): the fake custom
        // default_adapter is the routing bypass, so the route tag is `manual`.
        let latest = {
            let conn = h.db.lock().unwrap();
            latest_run_for_thread(&conn, &thread_id).unwrap().unwrap()
        };
        assert_eq!(latest.run_id, retry.run_id);
        assert_eq!(latest.model, "custom-ask-model");
        assert_eq!(latest.route.as_deref(), Some("manual"));
        assert!(latest.overridden, "recorded override surfaces on the retry display");
    }

    #[tokio::test]
    async fn ask_guards_empty_question_and_unknown_targets() {
        let h = harness("ask-guards2");
        h.install_fake_agent("#!/bin/sh\nexit 0\n");

        // Empty/whitespace question → EmptyQuestion (no thread created).
        let err = ask_from_app(&h.handle, &h.project_id, None, "   ", None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, FlowError::EmptyQuestion), "{err:?}");

        // Unknown project → ProjectNotFound (no thread created).
        let err = ask_from_app(&h.handle, "no-such-project", None, "q", None, None)
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
