//! Live-checkpoint IPC bridge (built for bead conceptify-e7m.5; reusable for
//! any future live checkpoint).
//!
//! A headless "real app without the WKWebView": one `#[ignore]`d, env-gated
//! test that opens the REAL app-support database, builds a mock-runtime Tauri
//! app with the FULL production `invoke_handler` list (mirrors `lib.rs`), and
//! exposes Tauri's real IPC dispatch (`tauri::test::get_ipc_response`) over a
//! loopback HTTP endpoint so the real built frontend (dist/, served with
//! `__TAURI_INTERNALS__` shimmed by `tools/live-harness.mjs`) can drive real
//! commands → real flows → real agent runs from a plain browser. Events
//! emitted on the mock app handle are buffered and served for the harness to
//! poll and re-dispatch.
//!
//! Run alongside `npm run tauri dev` (which owns port 4477 + the port/token
//! files) so agent-spawned `conceptify` CLI children round-trip through the
//! REAL production HTTP routes into the same WAL SQLite file. Two caveats,
//! both consequences of running under `cfg(test)`: run logs land in the test
//! artifacts root (temp dir), and `delete_thread`'s best-effort artifact-dir
//! removal targets that same test root rather than `~/Documents`.
//!
//! Run (from an unsandboxed shell — codex's Seatbelt can't nest):
//! ```sh
//! CONCEPTIFY_LIVE_BRIDGE=1 cargo test -p conceptify live_bridge -- --ignored --nocapture
//! ```
//!
//! The env gate exists so a blanket `cargo test -- --ignored` cannot
//! accidentally bind ports and camp on the user's DB for hours.

use std::sync::{Arc, Mutex};

use tauri::Listener;

type InvokeReply = tokio::sync::oneshot::Sender<Result<serde_json::Value, serde_json::Value>>;
type InvokeMsg = (String, serde_json::Value, InvokeReply);

#[derive(Clone)]
struct Bridge {
    tx: std::sync::mpsc::Sender<InvokeMsg>,
    events: Arc<Mutex<Vec<serde_json::Value>>>,
    db: crate::db::DbHandle,
}

async fn invoke_route(
    axum::extract::State(b): axum::extract::State<Bridge>,
    axum::Json(req): axum::Json<serde_json::Value>,
) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
    let cmd = req["cmd"].as_str().unwrap_or_default().to_owned();
    let args = req.get("args").cloned().unwrap_or(serde_json::json!({}));
    let (otx, orx) = tokio::sync::oneshot::channel();
    if b.tx.send((cmd, args, otx)).is_err() {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!("bridge dispatch thread gone")),
        );
    }
    match orx.await {
        Ok(Ok(v)) => (axum::http::StatusCode::OK, axum::Json(v)),
        Ok(Err(e)) => (axum::http::StatusCode::BAD_REQUEST, axum::Json(e)),
        Err(_) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!("bridge reply dropped")),
        ),
    }
}

async fn events_route(
    axum::extract::State(b): axum::extract::State<Bridge>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> axum::Json<serde_json::Value> {
    let since: usize = q.get("since").and_then(|s| s.parse().ok()).unwrap_or(0);
    let buf = b.events.lock().unwrap();
    let events: Vec<serde_json::Value> = buf.iter().skip(since).cloned().collect();
    axum::Json(serde_json::json!({ "next": buf.len(), "events": events }))
}

/// Raw artifact HTML by thread + version ("latest" supported) so the harness
/// can serve the artifact iframe the same way `artifact_protocol.rs` does.
async fn artifact_route(
    axum::extract::State(b): axum::extract::State<Bridge>,
    axum::extract::Path((thread_id, version)): axum::extract::Path<(String, String)>,
) -> Result<axum::response::Html<String>, axum::http::StatusCode> {
    let file_path: String = {
        let conn = b.db.lock().unwrap();
        let res = if version == "latest" {
            conn.query_row(
                "SELECT file_path FROM artifacts WHERE thread_id = ?1
                 ORDER BY version DESC LIMIT 1",
                [&thread_id],
                |r| r.get(0),
            )
        } else {
            conn.query_row(
                "SELECT file_path FROM artifacts WHERE thread_id = ?1 AND version = ?2",
                rusqlite::params![thread_id, version.parse::<i64>().unwrap_or(0)],
                |r| r.get(0),
            )
        };
        res.map_err(|_| axum::http::StatusCode::NOT_FOUND)?
    };
    let html =
        std::fs::read_to_string(&file_path).map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    Ok(axum::response::Html(html))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live checkpoint bridge: serves the REAL app DB on 127.0.0.1:4560 until killed (needs CONCEPTIFY_LIVE_BRIDGE=1)"]
async fn live_bridge() {
    if std::env::var_os("CONCEPTIFY_LIVE_BRIDGE").is_none() {
        eprintln!("[live-bridge] CONCEPTIFY_LIVE_BRIDGE not set; skipping (see module docs)");
        return;
    }

    // REAL app database — the same file the dev app's HTTP server uses.
    // Migrations are a no-op at HEAD parity; WAL allows the two processes.
    let db = crate::db::init().expect("real app db should open");
    {
        // Two writer processes (dev app + this bridge) share the file: give
        // this side a busy timeout so brief write overlaps retry, not error.
        let conn = db.lock().unwrap();
        conn.busy_timeout(std::time::Duration::from_millis(5000)).unwrap();
    }

    let app = tauri::test::mock_builder()
        .manage(db.clone())
        .manage(crate::runs::RunRegistry::default())
        .invoke_handler(tauri::generate_handler![
            crate::greet,
            crate::db_check,
            crate::commands::list_projects,
            crate::commands::list_threads,
            crate::commands::rename_project,
            crate::commands::set_project_archived,
            crate::commands::remap_project,
            crate::commands::ensure_project,
            crate::commands::create_project_folder,
            crate::commands::delete_thread,
            crate::commands::list_artifact_versions,
            crate::commands::diff_versions,
            crate::commands::open_artifact_in_browser,
            crate::commands::create_comment,
            crate::commands::list_comments,
            crate::commands::get_agent_settings,
            crate::commands::set_agent_settings,
            crate::commands::reset_agent_settings,
            crate::commands::get_agent_options,
            crate::commands::set_openrouter_api_key,
            crate::commands::ask_follow_ups,
            crate::commands::ask_single_comment,
            crate::commands::apply_to_artifact,
            crate::commands::get_active_run,
            crate::commands::list_run_activity,
            crate::commands::dismiss_run_activity,
            crate::commands::mark_run_activity_seen,
            crate::commands::claim_system_run_notification,
            crate::commands::get_conflict_review,
            crate::commands::publish_conflict_candidate,
            crate::commands::rebase_conflict,
            crate::commands::get_run_log_tail,
            crate::commands::ask_from_app,
            crate::commands::retry_ask,
            crate::commands::get_latest_run,
            crate::commands::get_model_catalog,
            crate::commands::refresh_model_catalog,
            crate::skill_catalog::list_skill_capabilities,
            crate::skill_catalog::recommend_skills,
            crate::skill_catalog::get_response_preferences,
            crate::skill_catalog::save_response_preference,
            crate::skill_catalog::reset_response_preference,
            crate::runs::cancel_run,
        ])
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("mock app");
    let handle = app.handle().clone();

    // Buffer every event the frontend subscribes to (src/lib/events.ts) plus
    // catalog-refreshed; the harness polls /events and re-dispatches.
    let events: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
    for name in [
        "projects-changed",
        "thread-created",
        "artifact-updated",
        "comment-created",
        "comment-updated",
        "thread-updated",
        "run-progress",
        "run-state-changed",
        "run-finished",
        "navigate",
        "catalog-refreshed",
        "settings-changed",
    ] {
        let sink = events.clone();
        let n = name.to_owned();
        handle.listen_any(name, move |ev| {
            let payload: serde_json::Value = serde_json::from_str(ev.payload())
                .unwrap_or_else(|_| serde_json::Value::String(ev.payload().to_owned()));
            sink.lock()
                .unwrap()
                .push(serde_json::json!({ "event": n, "payload": payload }));
        });
    }

    // Dedicated dispatch thread owning the mock webview: get_ipc_response
    // blocks per call (commands themselves are quick — runs are spawned).
    let (tx, rx) = std::sync::mpsc::channel::<InvokeMsg>();
    std::thread::spawn(move || {
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("mock webview");
        for (cmd, args, reply) in rx {
            let res = tauri::test::get_ipc_response(
                &webview,
                tauri::webview::InvokeRequest {
                    cmd,
                    callback: tauri::ipc::CallbackFn(0),
                    error: tauri::ipc::CallbackFn(1),
                    url: "tauri://localhost".parse().unwrap(),
                    body: tauri::ipc::InvokeBody::Json(args),
                    headers: Default::default(),
                    invoke_key: tauri::test::INVOKE_KEY.to_string(),
                },
            );
            let mapped = match res {
                Ok(body) => body
                    .deserialize::<serde_json::Value>()
                    .map_err(|e| serde_json::Value::String(e.to_string())),
                Err(e) => Err(e),
            };
            let _ = reply.send(mapped);
        }
    });

    let bridge = Bridge { tx, events, db };
    let router = axum::Router::new()
        .route("/health", axum::routing::get(|| async { "ok" }))
        .route("/invoke", axum::routing::post(invoke_route))
        .route("/events", axum::routing::get(events_route))
        .route("/artifact/{thread_id}/{version}", axum::routing::get(artifact_route))
        .with_state(bridge);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:4560")
        .await
        .expect("bind 127.0.0.1:4560");
    eprintln!("[live-bridge] serving on 127.0.0.1:4560");

    // Serve until killed, with a hard lifetime cap so a forgotten bridge
    // can't outlive the checkpoint session.
    tokio::select! {
        r = axum::serve(listener, router) => { r.expect("bridge server"); }
        _ = tokio::time::sleep(std::time::Duration::from_secs(4 * 3600)) => {
            eprintln!("[live-bridge] lifetime cap reached; exiting");
        }
    }
}
