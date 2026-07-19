mod anchoring;
mod artifact_protocol;
mod artifact_diff;
mod artifacts;
mod asset_protocol;
mod assets;
mod catalog;
mod commands;
mod comments;
mod concept_map;
mod context;
mod db;
mod flows;
mod learning;
// Live-checkpoint IPC bridge (test-only, #[ignore]d + env-gated): drives the
// real command/flow/run stack headlessly for end-to-end verification against
// the real frontend in a plain browser. See src/live_bridge.rs +
// tools/live-harness.mjs. Built for bead conceptify-e7m.5.
#[cfg(test)]
mod live_bridge;
mod projects;
mod project_context;
mod routing;
mod run_queue;
mod runs;
mod search;
mod server;
mod settings;
mod skill_catalog;
mod synthesis;
mod threads;

use tauri::{Manager, RunEvent, WindowEvent};

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

/// Demo `#[tauri::command]` proving the shared `db::DbHandle` (PRD §5.1, §4)
/// managed in `run()`'s `setup` hook is reachable from the frontend side of
/// the app, not just from axum handlers (see the matching `/api/v1/debug/db-check`
/// route in `server::routes`, which runs the same kind of query through the
/// same handle).
#[tauri::command]
fn db_check(db: tauri::State<db::DbHandle>) -> Result<i64, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    conn.query_row("SELECT count(*) FROM projects", [], |row| row.get(0))
        .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();

    // PRD §5.1 Lifecycle: single-instance plugin registered first (as per
    // tauri-plugin-single-instance docs — registration order matters).
    // A duplicate launch focuses the existing instance's window instead of
    // starting a second app.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }));
    }

    builder = builder.plugin(tauri_plugin_opener::init());

    // PRD FR-1.2 / UC6: native directory picker for in-app project creation.
    // The frontend calls `@tauri-apps/plugin-dialog`'s `open({ directory: true })`
    // (native NSOpenPanel on macOS — WKWebView-safe, not a web API) and hands
    // the chosen path to the `ensure_project` command. Only the `open` command
    // is granted (`dialog:allow-open` in capabilities/default.json).
    builder = builder.plugin(tauri_plugin_dialog::init());
    builder = builder.plugin(tauri_plugin_notification::init());

    // PRD §5.4 / §9 S2: the artifact:// scheme the viewer's sandboxed
    // iframe loads from. Cross-scheme = real origin isolation from the app
    // shell; the handler applies the per-response CSP. Registered on the
    // builder so the scheme exists before any webview is created (a
    // WKWebView requirement). See `artifact_protocol` for the URL contract.
    builder = builder
        .register_asynchronous_uri_scheme_protocol("artifact", artifact_protocol::protocol_handler);

    // Epic conceptify-z9y / artifact-spec §1.4: the Range-capable video-asset
    // scheme the viewer CSP admits via `media-src cfy-asset://localhost`.
    // A second scheme (never the same as `artifact://`) keeps media
    // cross-origin from both the document and the app shell. Registered on
    // the builder for the same before-any-webview WKWebView requirement.
    builder = builder
        .register_asynchronous_uri_scheme_protocol("cfy-asset", asset_protocol::protocol_handler);

    // PRD §5.1 Lifecycle: window-state plugin for size/position persistence
    // across hide/show and relaunch.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_window_state::Builder::new().build());
    }

    builder
        .invoke_handler(tauri::generate_handler![
            greet,
            db_check,
            commands::list_projects,
            commands::list_threads,
            commands::rename_project,
            commands::set_project_archived,
            commands::remap_project,
            commands::ensure_project,
            commands::create_project_folder,
            project_context::scan_project_context,
            project_context::get_topic_context,
            project_context::set_topic_context,
            project_context::get_project_goal,
            project_context::set_project_goal,
            commands::delete_thread,
            commands::list_artifact_versions,
            commands::diff_versions,
            commands::open_artifact_in_browser,
            commands::create_comment,
            commands::list_comments,
            commands::get_agent_settings,
            commands::set_agent_settings,
            commands::reset_agent_settings,
            commands::get_agent_options,
            commands::set_openrouter_api_key,
            commands::set_local_endpoint_api_key,
            commands::get_artifact_theme,
            commands::set_artifact_theme,
            commands::ask_follow_ups,
            commands::ask_single_comment,
            commands::apply_to_artifact,
            commands::get_active_run,
            commands::list_run_activity,
            commands::dismiss_run_activity,
            commands::mark_run_activity_seen,
            commands::claim_system_run_notification,
            commands::get_conflict_review,
            commands::publish_conflict_candidate,
            commands::reject_conflict_candidate,
            commands::restore_artifact_version,
            commands::rebase_conflict,
            commands::get_run_log_tail,
            commands::ask_from_app,
            commands::retry_ask,
            commands::get_latest_run,
            commands::get_model_catalog,
            commands::refresh_model_catalog,
            skill_catalog::list_skill_capabilities,
            skill_catalog::recommend_skills,
            skill_catalog::get_response_preferences,
            skill_catalog::save_response_preference,
            skill_catalog::reset_response_preference,
            learning::list_learning_suggestions,
            learning::dismiss_learning_suggestion,
            learning::record_learning_trail,
            learning::get_learning_trail,
            concept_map::get_concept_map,
            concept_map::pin_concept_link,
            concept_map::remove_concept_link,
            concept_map::distinguish_concept,
            concept_map::merge_concepts,
            synthesis::compare_threads,
            synthesis::record_thread_synthesis,
            synthesis::get_thread_synthesis,
            search::search,
            runs::cancel_run,
        ])
        .setup(|app| {
            // Opened and migrated before anything else touches it: both the
            // axum server (spawned below) and any frontend `db_check`-style
            // command need it in managed state first.
            let db = db::init()?;

            // N4 boot reconciliation (crate::runs): any `running` run row is
            // leftover from a crashed/killed previous session — mark it
            // failed BEFORE the (empty) run registry below becomes the
            // liveness source of truth, so a crashed run can never wedge the
            // FR-4.9 one-run-per-thread guard or corrupt thread state. A
            // failure here means the DB itself is broken, so it aborts
            // startup like db::init would.
            {
                let conn = db.lock().unwrap_or_else(|p| p.into_inner());
                let reconciled = runs::reconcile_stale_runs(&conn)?;
                if reconciled > 0 {
                    eprintln!(
                        "[conceptify-runs] marked {reconciled} stale running run(s) failed at boot"
                    );
                }
            }

            app.manage(db);
            // Live-run registry for the agent-run engine (crate::runs):
            // FR-4.9 guard + cancel routing, consumed by start_run and the
            // cancel_run command.
            app.manage(runs::RunRegistry::default());

            // Recreate scheduler workers for durable queued/throttled rows.
            // Preparation and execution stay off the boot critical path; any
            // invalid payload is failed honestly by the resume routine.
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    match runs::resume_queued_runs(handle).await {
                        Ok(count) if count > 0 => {
                            eprintln!("[conceptify-runs] resumed {count} queued run(s)")
                        }
                        Ok(_) => {}
                        Err(e) => eprintln!("[conceptify-runs] queue resume failed: {e}"),
                    }
                });
            }

            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(server::start(app_handle));

            // Bead conceptify-e7m.6: warm the model catalog in the background,
            // off the boot critical path (NFR cold start ~310ms). TTL-gated and
            // failure-silent — it never blocks startup and never surfaces an
            // error dialog; a fetch failure leaves the cache/snapshot in place.
            tauri::async_runtime::spawn(catalog::refresh_on_startup());

            Ok(())
        })
        .on_window_event(|window, event| {
            // PRD §5.1 Lifecycle: hide-on-close behavior. Window close
            // (CloseRequested) hides the window instead of quitting, so the
            // HTTP API keeps serving with no window visible. The app menu
            // (wired later) provides explicit Quit.
            if let WindowEvent::CloseRequested { api, .. } = event {
                window.hide().unwrap();
                api.prevent_close();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // PRD §5.1 Lifecycle: on macOS, handle the Reopen event (dock icon
            // click when no window is visible) to re-show the hidden window.
            #[cfg(target_os = "macos")]
            if let RunEvent::Reopen { .. } = event {
                if let Some(window) = app_handle.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Proves the `db_check` command actually round-trips through Tauri's
    /// real IPC dispatch (not just that it type-checks): builds a mock app
    /// with a throwaway on-disk DB in managed state, registers `db_check`
    /// in `invoke_handler` exactly as `run()` does, and invokes it the same
    /// way the webview would. This is the automated stand-in for manually
    /// poking the webview devtools (not available headlessly in this
    /// environment — see the note on bead `conceptify-36s.2`), covering the
    /// `#[tauri::command]` half of this bead's "both a tauri command and an
    /// axum handler can query through managed state" acceptance criterion;
    /// the axum half is covered by hitting `/api/v1/debug/db-check` over
    /// HTTP (see `server::routes`).
    #[test]
    fn db_check_command_reads_through_managed_state() {
        let db_path = std::env::temp_dir().join(format!(
            "conceptify-test-db-check-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let db_handle = db::init_at(&db_path).expect("test db should init and migrate");

        let app = tauri::test::mock_builder()
            .manage(db_handle)
            .invoke_handler(tauri::generate_handler![db_check])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("failed to build mock app");

        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("failed to build mock webview");

        let response = tauri::test::get_ipc_response(
            &webview,
            tauri::webview::InvokeRequest {
                cmd: "db_check".into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: "tauri://localhost".parse().unwrap(),
                body: Default::default(),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.to_string(),
            },
        )
        .expect("db_check command should succeed over IPC");

        let project_count: i64 = response
            .deserialize()
            .expect("response should deserialize as an i64");
        assert_eq!(project_count, 0, "fresh test database should have no projects");

        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
        let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
    }

    /// Exercises the threads domain (create + list) against the *real*
    /// migration output — `db::init_at` runs the full `migrations()` chain,
    /// including the appended `THREAD_SLUG` migration that adds the `slug`
    /// column and the `(project_id, slug)` unique index. The `threads`-module
    /// unit tests use a hand-written in-memory schema; this test proves the
    /// shipped schema matches what the domain code expects (slug column
    /// present, status CHECK accepts `generating`, unique index live).
    #[test]
    fn threads_create_and_list_against_real_migrations() {
        let db_path = std::env::temp_dir().join(format!(
            "conceptify-test-threads-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let db_handle = db::init_at(&db_path).expect("test db should init and migrate");
        let conn = db_handle.lock().unwrap();

        // Seed a project the threads can hang off of.
        conn.execute(
            "INSERT INTO projects (id, name, root_path) VALUES ('p1', 'Proj', '/tmp/p1')",
            [],
        )
        .expect("insert project");

        // Create returns id + slug; two same-title threads get distinct slugs.
        let a = threads::create_thread(&conn, "p1", "Real Schema Test", "q1")
            .expect("create first thread");
        let b = threads::create_thread(&conn, "p1", "Real Schema Test", "q2")
            .expect("create second thread");
        assert_eq!(a.slug, "real-schema-test");
        assert_eq!(b.slug, "real-schema-test-2");
        assert_eq!(a.status, threads::ThreadStatus::Generating);

        // The unique index from the migration is live: a raw duplicate insert
        // on (project_id, slug) must be rejected.
        let dup = conn.execute(
            "INSERT INTO threads (id, project_id, title, slug, initial_question, status)
             VALUES ('x', 'p1', 't', 'real-schema-test', 'q', 'generating')",
            [],
        );
        assert!(dup.is_err(), "unique index should reject duplicate slug");

        let list = threads::list_threads(&conn, "p1").expect("list threads");
        assert_eq!(list.len(), 2);
        // No comments table rows → all counts 0 through the real LEFT JOIN.
        assert!(list.iter().all(|t| t.open_comment_count == 0));

        drop(conn);
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
        let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
    }

    /// Exercises the comments domain (create + list + update) against the
    /// *real* migration output — `db::init_at` runs the full `migrations()`
    /// chain, including the appended `COMMENT_ANCHOR_STATE` migration that adds
    /// the `anchor_state` column + CHECK. The `comments`-module unit tests use a
    /// hand-written in-memory schema; this test proves the shipped schema
    /// matches what the domain code expects: the `anchor_state` column is
    /// present, the composite `(thread_id, artifact_version)` FK is enforced,
    /// and both status/anchor_state CHECK constraints are live.
    #[test]
    fn comments_crud_against_real_migrations() {
        let db_path = std::env::temp_dir().join(format!(
            "conceptify-test-comments-mig-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let db_handle = db::init_at(&db_path).expect("test db should init and migrate");
        let conn = db_handle.lock().unwrap();

        // Seed project → thread → artifact v1 (the comment FK needs the row).
        conn.execute(
            "INSERT INTO projects (id, name, root_path) VALUES ('p1', 'Proj', '/tmp/cmig')",
            [],
        )
        .expect("insert project");
        let thread_id = threads::create_thread(&conn, "p1", "Real Schema", "q")
            .expect("create thread")
            .id;
        conn.execute(
            "INSERT INTO artifacts (id, thread_id, version, file_path, created_by)
             VALUES ('a1', ?1, 1, '/tmp/x.html', 'initial')",
            [&thread_id],
        )
        .expect("insert artifact");

        // A comment with a full anchor commits against the real schema and its
        // anchor_state defaults to `anchored`.
        let anchor = serde_json::json!({
            "v": 1, "type": "text", "cfy_id": "sec-x", "start": 0, "end": 3,
            "quote": { "exact": "why", "prefix": "", "suffix": " token" }
        });
        let c = comments::create_comment(&conn, &thread_id, 1, Some(&anchor), "q")
            .expect("create comment")
            .comment;
        assert_eq!(c.anchor_state, comments::AnchorState::Anchored);
        assert_eq!(c.anchor.unwrap(), anchor);

        // The composite FK rejects a comment against a nonexistent version.
        let orphan = conn.execute(
            "INSERT INTO comments (id, thread_id, artifact_version, body, status)
             VALUES ('x', ?1, 99, 'b', 'open')",
            [&thread_id],
        );
        assert!(
            orphan.is_err(),
            "composite FK should reject missing version"
        );

        // The anchor_state CHECK rejects an unknown value.
        let bad_state = conn.execute(
            "UPDATE comments SET anchor_state = 'bogus' WHERE id = ?1",
            [&c.id],
        );
        assert!(
            bad_state.is_err(),
            "anchor_state CHECK should reject 'bogus'"
        );

        // Update transitions the comment and list filters by status.
        comments::update_comment(
            &conn,
            &c.id,
            Some(comments::CommentStatus::Answered),
            Some("<p>a</p>"),
            None,
        )
        .expect("answer comment");
        let answered =
            comments::list_comments(&conn, &thread_id, Some(comments::CommentStatus::Answered))
                .expect("list answered");
        assert_eq!(answered.len(), 1);
        assert!(
            comments::list_comments(&conn, &thread_id, Some(comments::CommentStatus::Open))
                .expect("list open")
                .is_empty()
        );

        // The threads list's open-comment count reflects the (now-answered)
        // comment: 0 open.
        let threads_list = threads::list_threads(&conn, "p1").expect("list threads");
        assert_eq!(threads_list[0].open_comment_count, 0);

        drop(conn);
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
        let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
    }

    /// Exercises threaded replies (epic conceptify-6xi) against the *real*
    /// FK-enabled migration output: `db::init_at` runs the full chain including
    /// migration 10 (`comments.parent_id` + self-ref FK). Proves, on the shipped
    /// schema, that a reply persists with a null anchor and inherited version,
    /// that a user reply re-opens an answered root, that the threads-list
    /// open-count counts the re-opened root once (open replies don't inflate it),
    /// and that get-context nests the reply chain — the whole stack, not the
    /// hand-written in-memory schema the unit tests use.
    #[test]
    fn replies_against_real_migrations() {
        let db_path = std::env::temp_dir().join(format!(
            "conceptify-test-replies-mig-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let db_handle = db::init_at(&db_path).expect("test db should init and migrate");
        let conn = db_handle.lock().unwrap();

        conn.execute(
            "INSERT INTO projects (id, name, root_path) VALUES ('p1', 'Proj', '/tmp/rmig')",
            [],
        )
        .expect("insert project");
        let thread_id = threads::create_thread(&conn, "p1", "Replies", "q")
            .expect("create thread")
            .id;
        conn.execute(
            "INSERT INTO artifacts (id, thread_id, version, file_path, created_by)
             VALUES ('a1', ?1, 1, '/tmp/x.html', 'initial')",
            [&thread_id],
        )
        .expect("insert artifact");

        // A root, answered.
        let root = comments::create_comment(&conn, &thread_id, 1, None, "why?")
            .expect("create root")
            .comment;
        comments::update_comment(
            &conn,
            &root.id,
            Some(comments::CommentStatus::Answered),
            Some("<p>a</p>"),
            None,
        )
        .expect("answer root");

        // A user reply re-opens the root (composite + self-ref FKs live here).
        let reply_ctx = comments::create_reply(&conn, &thread_id, &root.id, "still confused")
            .expect("create reply");
        assert!(reply_ctx.comment.anchor.is_none());
        assert_eq!(reply_ctx.comment.artifact_version, 1, "inherited version");
        assert_eq!(
            reply_ctx.reopened_root.unwrap().status,
            comments::CommentStatus::Open
        );

        // Open-count counts the re-opened root once, not the (open) reply.
        let threads_list = threads::list_threads(&conn, "p1").expect("list threads");
        assert_eq!(threads_list[0].open_comment_count, 1);

        // get-context nests the reply chain under the open root.
        let ctx = crate::context::thread_context(&conn, &thread_id).expect("context");
        assert_eq!(ctx.open_comment_threads.len(), 1);
        assert_eq!(ctx.open_comment_threads[0].root.id, root.id);
        assert_eq!(ctx.open_comment_threads[0].replies.len(), 1);
        assert_eq!(
            ctx.open_comment_threads[0].replies[0].id,
            reply_ctx.comment.id
        );

        drop(conn);
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
        let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
    }

    /// Exercises `threads::delete_thread`'s cascade against the *real*
    /// FK-enabled migration output (bead conceptify-0kt): `db::init_at` runs the
    /// full chain with `foreign_keys = ON`, so deleting a thread must remove its
    /// artifacts, comments, AND follow_up_runs via `ON DELETE CASCADE` in one
    /// statement — and must NOT touch a sibling thread's rows. This is the
    /// load-bearing proof that no schema change is needed for thread deletion.
    #[test]
    fn delete_thread_cascades_against_real_migrations() {
        let db_path = std::env::temp_dir().join(format!(
            "conceptify-test-delete-cascade-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let db_handle = db::init_at(&db_path).expect("test db should init and migrate");
        let conn = db_handle.lock().unwrap();

        conn.execute(
            "INSERT INTO projects (id, name, root_path) VALUES ('p1', 'Proj', '/tmp/del')",
            [],
        )
        .expect("insert project");

        // The thread we will delete, fully populated: artifact v1 + a comment
        // anchored to it + a follow_up_run.
        let victim = threads::create_thread(&conn, "p1", "Doomed", "q")
            .expect("create victim thread")
            .id;
        conn.execute(
            "INSERT INTO artifacts (id, thread_id, version, file_path, created_by)
             VALUES ('a1', ?1, 1, '/tmp/a.html', 'initial')",
            [&victim],
        )
        .expect("insert artifact");
        conn.execute(
            "INSERT INTO comments (id, thread_id, artifact_version, body, status)
             VALUES ('c1', ?1, 1, 'why?', 'open')",
            [&victim],
        )
        .expect("insert comment");
        conn.execute(
            "INSERT INTO follow_up_runs (id, thread_id, agent, model, mode, status, log_path)
             VALUES ('r1', ?1, 'claude', 'm', 'ask', 'failed', '/tmp/r.log')",
            [&victim],
        )
        .expect("insert run");

        // A sibling thread with its own artifact — must survive untouched.
        let keeper = threads::create_thread(&conn, "p1", "Keeper", "q")
            .expect("create keeper thread")
            .id;
        conn.execute(
            "INSERT INTO artifacts (id, thread_id, version, file_path, created_by)
             VALUES ('a2', ?1, 1, '/tmp/b.html', 'initial')",
            [&keeper],
        )
        .expect("insert keeper artifact");

        // Delete the victim → cascades to its children only.
        assert!(threads::delete_thread(&conn, &victim).expect("delete victim"));

        let count = |sql: &str, id: &str| -> i64 {
            conn.query_row(sql, [id], |r| r.get(0)).unwrap()
        };
        assert_eq!(count("SELECT COUNT(*) FROM threads WHERE id = ?1", &victim), 0);
        assert_eq!(
            count("SELECT COUNT(*) FROM artifacts WHERE thread_id = ?1", &victim),
            0,
            "artifacts should cascade-delete"
        );
        assert_eq!(
            count("SELECT COUNT(*) FROM comments WHERE thread_id = ?1", &victim),
            0,
            "comments should cascade-delete"
        );
        assert_eq!(
            count("SELECT COUNT(*) FROM follow_up_runs WHERE thread_id = ?1", &victim),
            0,
            "follow_up_runs should cascade-delete"
        );

        // The sibling thread and its artifact are untouched.
        assert_eq!(count("SELECT COUNT(*) FROM threads WHERE id = ?1", &keeper), 1);
        assert_eq!(
            count("SELECT COUNT(*) FROM artifacts WHERE thread_id = ?1", &keeper),
            1,
            "sibling artifact must survive"
        );

        drop(conn);
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
        let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
    }
}
