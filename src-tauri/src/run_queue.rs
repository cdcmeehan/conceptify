//! Durable run-queue state machine (`conceptify-k9z.2`).
//!
//! This module owns database transitions only. Process spawning and in-memory
//! capacity bookkeeping stay in `runs`; keeping the compare-and-set operations
//! here makes queue races testable without launching an agent binary.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunClass {
    Exploration,
    Mutation,
}

impl RunClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exploration => "exploration",
            Self::Mutation => "mutation",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "exploration" => Some(Self::Exploration),
            "mutation" => Some(Self::Mutation),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct NewQueuedRun<'a> {
    pub id: &'a str,
    pub thread_id: &'a str,
    pub agent: &'a str,
    pub model: &'a str,
    pub mode: &'a str,
    pub log_path: &'a str,
    pub override_json: Option<&'a str>,
    pub route: &'a str,
    pub run_class: RunClass,
    pub provider_pool: &'a str,
    pub prompt: &'a str,
    pub env_json: &'a str,
    pub base_artifact_version: Option<i64>,
    pub retry_of_run_id: Option<&'a str>,
    pub response_intent_json: Option<&'a str>,
    pub selected_skills_json: Option<&'a str>,
}

/// Allocate a monotonic sequence and persist the complete restart-safe payload
/// in one connection-critical section. The application's single shared
/// connection serializes callers; the unique partial index is the integrity
/// backstop if that architecture later changes to a pool.
pub fn enqueue(conn: &Connection, run: &NewQueuedRun<'_>) -> rusqlite::Result<i64> {
    let queue_seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(queue_seq), 0) + 1 FROM follow_up_runs",
        [],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO follow_up_runs
             (id, thread_id, agent, model, mode, status, log_path,
              override_json, route, run_class, provider_pool, prompt, env_json,
              base_artifact_version, queued_at, queue_seq, retry_of_run_id,
              response_intent_json, selected_skills_json)
         VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, ?7, ?8, ?9, ?10, ?11,
                 ?12, ?13, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?14, ?15,
                 ?16, ?17)",
        rusqlite::params![
            run.id,
            run.thread_id,
            run.agent,
            run.model,
            run.mode,
            run.log_path,
            run.override_json,
            run.route,
            run.run_class.as_str(),
            run.provider_pool,
            run.prompt,
            run.env_json,
            run.base_artifact_version,
            queue_seq,
            run.retry_of_run_id,
            run.response_intent_json,
            run.selected_skills_json,
        ],
    )?;
    Ok(queue_seq)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedRun {
    pub id: String,
    pub project_id: String,
    pub thread_id: String,
    pub run_class: RunClass,
    pub provider_pool: String,
}

#[derive(Debug)]
struct Candidate {
    id: String,
    project_id: String,
    thread_id: String,
    run_class: RunClass,
    provider_pool: String,
}

/// Admit at most one eligible row for `provider_pool`.
///
/// `active_in_pool` and `active_mutation_threads` are snapshots held by the
/// caller's scheduler lock. The DB compare-and-set is still authoritative: if
/// cancellation or another admission won, this returns `None` and the caller
/// simply schedules another pass.
pub fn admit_next(
    conn: &Connection,
    provider_pool: &str,
    capacity: usize,
    active_in_pool: usize,
    active_mutation_threads: &HashSet<String>,
    last_project: Option<&str>,
    claimant_id: Option<&str>,
) -> rusqlite::Result<Option<AdmittedRun>> {
    if active_in_pool >= capacity {
        return Ok(None);
    }

    // An elapsed throttle becomes ordinary queued work without changing its
    // original queue timestamp/sequence.
    conn.execute(
        "UPDATE follow_up_runs
         SET status = 'queued', not_before = NULL, status_reason = NULL
         WHERE provider_pool = ?1 AND status = 'throttled'
           AND (not_before IS NULL OR not_before <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        [provider_pool],
    )?;

    let mut stmt = conn.prepare(
        "SELECT r.id, t.project_id, r.thread_id, r.run_class, r.provider_pool
         FROM follow_up_runs r
         JOIN threads t ON t.id = r.thread_id
         WHERE r.provider_pool = ?1 AND r.status = 'queued'
         ORDER BY r.queued_at ASC, r.queue_seq ASC, r.id ASC",
    )?;
    let rows = stmt.query_map([provider_pool], |r| {
        let class_text: String = r.get(3)?;
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            class_text,
            r.get::<_, String>(4)?,
        ))
    })?;

    let mut runnable = Vec::new();
    for row in rows {
        let (id, project_id, thread_id, class_text, pool) = row?;
        let Some(run_class) = RunClass::parse(&class_text) else {
            // Invalid durable metadata must never execute by guesswork.
            conn.execute(
                "UPDATE follow_up_runs
                 SET status = 'failed', status_reason = 'invalid_run_class',
                     finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE id = ?1 AND status = 'queued'",
                [&id],
            )?;
            continue;
        };
        if run_class == RunClass::Mutation && active_mutation_threads.contains(&thread_id) {
            continue;
        }
        runnable.push(Candidate {
            id,
            project_id,
            thread_id,
            run_class,
            provider_pool: pool,
        });
    }

    let Some(index) = (match last_project {
        Some(last) => runnable.iter().position(|r| r.project_id != last),
        None => None,
    })
    .or_else(|| (!runnable.is_empty()).then_some(0))
    else {
        return Ok(None);
    };
    let selected = runnable.swap_remove(index);
    if claimant_id.is_some_and(|claimant| claimant != selected.id) {
        return Ok(None);
    }

    let changed = conn.execute(
        "UPDATE follow_up_runs
         SET status = 'starting',
             execution_started_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE id = ?1 AND status = 'queued'",
        [&selected.id],
    )?;
    if changed != 1 {
        return Ok(None);
    }

    Ok(Some(AdmittedRun {
        id: selected.id,
        project_id: selected.project_id,
        thread_id: selected.thread_id,
        run_class: selected.run_class,
        provider_pool: selected.provider_pool,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelTransition {
    CancelledBeforeSpawn,
    Cancelling,
    AlreadyTerminal,
    NotFound,
}

/// Persist cancellation before signalling a process. This closes the
/// queued→starting→spawn race: an executor must compare-and-set `starting` to
/// `running` immediately before spawn and abort if cancellation changed it.
pub fn request_cancel(conn: &Connection, run_id: &str) -> rusqlite::Result<CancelTransition> {
    let status: Option<String> = conn
        .query_row(
            "SELECT status FROM follow_up_runs WHERE id = ?1",
            [run_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(status) = status else {
        return Ok(CancelTransition::NotFound);
    };

    match status.as_str() {
        "queued" | "throttled" => {
            conn.execute(
                "UPDATE follow_up_runs
                 SET status = 'cancelled', status_reason = 'user_cancelled',
                     finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE id = ?1 AND status IN ('queued', 'throttled')",
                [run_id],
            )?;
            Ok(CancelTransition::CancelledBeforeSpawn)
        }
        "starting" | "running" | "cancelling" => {
            conn.execute(
                "UPDATE follow_up_runs
                 SET status = 'cancelling', status_reason = 'user_cancelled'
                 WHERE id = ?1 AND status IN ('starting', 'running')",
                [run_id],
            )?;
            Ok(CancelTransition::Cancelling)
        }
        _ => Ok(CancelTransition::AlreadyTerminal),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reconciled {
    pub interrupted: usize,
    pub throttle_elapsed: usize,
}

/// Reconcile durable non-terminal states before any new admission on startup.
pub fn reconcile_after_restart(conn: &Connection) -> rusqlite::Result<Reconciled> {
    let interrupted = conn.execute(
        "UPDATE follow_up_runs
         SET status = 'failed', status_reason = 'app_interrupted',
             finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE status IN ('starting', 'running', 'cancelling')",
        [],
    )?;
    let throttle_elapsed = conn.execute(
        "UPDATE follow_up_runs
         SET status = 'queued', not_before = NULL, status_reason = NULL
         WHERE status = 'throttled'
           AND (not_before IS NULL OR not_before <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        [],
    )?;
    Ok(Reconciled {
        interrupted,
        throttle_elapsed,
    })
}

/// Transition a running attempt to throttled and retain its original ordering.
pub fn throttle(
    conn: &Connection,
    run_id: &str,
    not_before: &str,
    reason: &str,
) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "UPDATE follow_up_runs
         SET status = 'throttled', not_before = ?2, status_reason = ?3,
             execution_started_at = NULL
         WHERE id = ?1 AND status IN ('starting', 'running')",
        rusqlite::params![run_id, not_before, reason],
    )? == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, projects, threads};

    struct Harness {
        db: db::DbHandle,
        path: std::path::PathBuf,
        root: std::path::PathBuf,
        projects: Vec<String>,
        threads: Vec<String>,
    }

    impl Harness {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "conceptify-run-queue-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&root).unwrap();
            let path = root.join("queue.db");
            let db = db::init_at(&path).unwrap();
            let mut projects_out = Vec::new();
            let mut threads_out = Vec::new();
            {
                let conn = db.lock().unwrap();
                for n in 1..=3 {
                    let project_root = root.join(format!("project-{n}"));
                    std::fs::create_dir_all(&project_root).unwrap();
                    let project = projects::ensure_project(
                        &conn,
                        project_root.to_str().unwrap(),
                        Some(&format!("P{n}")),
                    )
                    .unwrap();
                    let thread = threads::create_thread(
                        &conn,
                        &project.project.id,
                        &format!("T{n}"),
                        "question",
                    )
                    .unwrap();
                    projects_out.push(project.project.id);
                    threads_out.push(thread.id);
                }
            }
            Self {
                db,
                path,
                root,
                projects: projects_out,
                threads: threads_out,
            }
        }

        fn enqueue(&self, id: &str, thread: usize, class: RunClass, pool: &str) -> i64 {
            let conn = self.db.lock().unwrap();
            enqueue(
                &conn,
                &NewQueuedRun {
                    id,
                    thread_id: &self.threads[thread],
                    agent: "agent",
                    model: "model",
                    mode: if class == RunClass::Mutation { "apply" } else { "answer" },
                    log_path: "/tmp/run.log",
                    override_json: None,
                    route: pool,
                    run_class: class,
                    provider_pool: pool,
                    prompt: "prompt",
                    env_json: "[]",
                    base_artifact_version: None,
                    retry_of_run_id: None,
                    response_intent_json: None,
                    selected_skills_json: None,
                },
            )
            .unwrap()
        }

        fn status(&self, id: &str) -> String {
            self.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT status FROM follow_up_runs WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap()
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let db = std::mem::replace(
                &mut self.db,
                std::sync::Arc::new(std::sync::Mutex::new(Connection::open_in_memory().unwrap())),
            );
            drop(db);
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn enqueue_allocates_stable_monotonic_sequence_and_payload() {
        let h = Harness::new();
        assert_eq!(h.enqueue("r1", 0, RunClass::Exploration, "anthropic"), 1);
        assert_eq!(h.enqueue("r2", 1, RunClass::Mutation, "anthropic"), 2);
        let conn = h.db.lock().unwrap();
        let row: (String, String, String, i64) = conn
            .query_row(
                "SELECT status, run_class, prompt, queue_seq
                 FROM follow_up_runs WHERE id = 'r2'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(row, ("queued".into(), "mutation".into(), "prompt".into(), 2));
    }

    #[test]
    fn admission_respects_capacity_mutation_guard_scan_around_and_fairness() {
        let h = Harness::new();
        h.enqueue("p1-first", 0, RunClass::Mutation, "anthropic");
        h.enqueue("p1-blocked", 0, RunClass::Mutation, "anthropic");
        h.enqueue("p2", 1, RunClass::Exploration, "anthropic");

        let conn = h.db.lock().unwrap();
        let none = admit_next(&conn, "anthropic", 1, 1, &HashSet::new(), None, None).unwrap();
        assert!(none.is_none(), "full provider pool admits nothing");

        let first = admit_next(&conn, "anthropic", 2, 0, &HashSet::new(), None, None)
            .unwrap()
            .unwrap();
        assert_eq!(first.id, "p1-first");

        let active_mutations = HashSet::from([h.threads[0].clone()]);
        let second = admit_next(
            &conn,
            "anthropic",
            2,
            1,
            &active_mutations,
            Some(&h.projects[0]),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(second.id, "p2", "blocked head is scanned around fairly");
        drop(conn);
        assert_eq!(h.status("p1-blocked"), "queued");
    }

    #[test]
    fn cancellation_is_idempotent_and_wins_before_spawn() {
        let h = Harness::new();
        h.enqueue("queued", 0, RunClass::Exploration, "anthropic");
        assert_eq!(
            request_cancel(&h.db.lock().unwrap(), "queued").unwrap(),
            CancelTransition::CancelledBeforeSpawn
        );
        assert_eq!(h.status("queued"), "cancelled");
        assert_eq!(
            request_cancel(&h.db.lock().unwrap(), "queued").unwrap(),
            CancelTransition::AlreadyTerminal
        );
        assert_eq!(
            request_cancel(&h.db.lock().unwrap(), "missing").unwrap(),
            CancelTransition::NotFound
        );

        h.enqueue("starting", 0, RunClass::Exploration, "anthropic");
        let conn = h.db.lock().unwrap();
        let admitted = admit_next(
            &conn,
            "anthropic",
            1,
            0,
            &HashSet::new(),
            None,
            Some("starting"),
        )
        .unwrap();
        assert!(admitted.is_some());
        assert_eq!(
            request_cancel(&conn, "starting").unwrap(),
            CancelTransition::Cancelling
        );
        let spawn_won = conn
            .execute(
                "UPDATE follow_up_runs SET status = 'running'
                 WHERE id = 'starting' AND status = 'starting'",
                [],
            )
            .unwrap();
        assert_eq!(spawn_won, 0, "durable cancellation wins the spawn CAS race");
    }

    #[test]
    fn restart_preserves_queue_releases_elapsed_throttle_and_fails_live_states() {
        let h = Harness::new();
        for (id, state) in [
            ("queued", "queued"),
            ("starting", "starting"),
            ("running", "running"),
            ("cancelling", "cancelling"),
            ("elapsed", "throttled"),
            ("future", "throttled"),
        ] {
            h.enqueue(id, 0, RunClass::Exploration, "anthropic");
            h.db
                .lock()
                .unwrap()
                .execute(
                    "UPDATE follow_up_runs SET status = ?2 WHERE id = ?1",
                    rusqlite::params![id, state],
                )
                .unwrap();
        }
        let conn = h.db.lock().unwrap();
        conn.execute(
            "UPDATE follow_up_runs SET not_before = '2000-01-01T00:00:00.000Z'
             WHERE id = 'elapsed'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE follow_up_runs SET not_before = '2999-01-01T00:00:00.000Z'
             WHERE id = 'future'",
            [],
        )
        .unwrap();

        let result = reconcile_after_restart(&conn).unwrap();
        assert_eq!(result.interrupted, 3);
        assert_eq!(result.throttle_elapsed, 1);
        drop(conn);
        assert_eq!(h.status("queued"), "queued");
        assert_eq!(h.status("elapsed"), "queued");
        assert_eq!(h.status("future"), "throttled");
        for id in ["starting", "running", "cancelling"] {
            assert_eq!(h.status(id), "failed");
        }
    }

    #[test]
    fn throttle_releases_row_until_not_before() {
        let h = Harness::new();
        h.enqueue("run", 0, RunClass::Exploration, "anthropic");
        h.db.lock()
            .unwrap()
            .execute(
                "UPDATE follow_up_runs SET status = 'running' WHERE id = 'run'",
                [],
            )
            .unwrap();
        assert!(throttle(
            &h.db.lock().unwrap(),
            "run",
            "2999-01-01T00:00:00.000Z",
            "provider_rate_limit"
        )
        .unwrap());
        assert_eq!(h.status("run"), "throttled");
        let admitted = admit_next(
            &h.db.lock().unwrap(),
            "anthropic",
            1,
            0,
            &HashSet::new(),
            None,
            None,
        )
        .unwrap();
        assert!(admitted.is_none());
    }
}
