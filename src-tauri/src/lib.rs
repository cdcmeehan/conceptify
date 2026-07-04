mod db;
mod projects;
mod server;
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

    // PRD §5.1 Lifecycle: window-state plugin for size/position persistence
    // across hide/show and relaunch.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_window_state::Builder::new().build());
    }

    builder
        .invoke_handler(tauri::generate_handler![greet, db_check])
        .setup(|app| {
            // Opened and migrated before anything else touches it: both the
            // axum server (spawned below) and any frontend `db_check`-style
            // command need it in managed state first.
            let db = db::init()?;
            app.manage(db);

            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(server::start(app_handle));
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
}
