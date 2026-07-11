//! SQLite persistence layer (PRD §4, §5.6).
//!
//! rusqlite (bundled SQLite, WAL mode) held in Tauri managed state so it's
//! reachable from both `#[tauri::command]` handlers and axum handlers (via
//! `server::ApiState`, see `crate::server`). Deliberately **not**
//! `tauri-plugin-sql` — it has no first-class Rust-side query API and our
//! server code needs direct DB access (PRD §5.1). Schema migrations run via
//! `rusqlite_migration` against the `user_version` pragma, idempotently on
//! every launch (see `migrations.rs`).
//!
//! This DB stores metadata only — projects, threads, comments, follow-up
//! runs, settings. Artifact HTML bodies live on disk as plain files (§5.6);
//! this layer only ever stores their paths.

mod migrations;

use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::server::paths;

/// Shared handle to the single SQLite connection: a `rusqlite::Connection`
/// behind a `std::sync::Mutex`, wrapped in `Arc` so it's cheap to clone into
/// both Tauri managed state and `server::ApiState` (which axum clones once
/// per request — see the gotcha recorded on bead `conceptify-36s.2`).
///
/// A single shared connection (rather than a pool) is the right amount of
/// concurrency for a personal, single-user app: WAL mode still lets readers
/// proceed without blocking on a writer, and the `Mutex` just serializes the
/// (rare, short) writes. If this ever becomes a bottleneck, swap in
/// `r2d2_sqlite`/`deadpool-sqlite` without changing this type's call sites,
/// since both `execute` (sync) and `with_conn` (async) go through this alias.
pub type DbHandle = Arc<Mutex<Connection>>;

/// `~/Library/Application Support/conceptify/conceptify.db` — the same
/// `dirs::data_dir()`-based app-support directory the axum server module
/// uses for its token/port files (`server::paths::app_support_dir`), so the
/// DB lives alongside them rather than under Tauri's bundle-id-nested
/// `app_data_dir()`.
pub fn db_path() -> std::io::Result<std::path::PathBuf> {
    Ok(paths::app_support_dir()?.join("conceptify.db"))
}

/// Open the database, enable WAL + foreign keys, and migrate to the latest
/// schema (PRD §4). Safe to call on every launch — a second run against an
/// already-migrated database is a no-op probe of a single `user_version`
/// pragma, not a re-execution of any `CREATE TABLE` statement.
///
/// Returns an error (rather than panicking) on any failure; the caller
/// (`lib.rs`'s `setup` hook) decides whether that's fatal. In practice it is:
/// unlike the axum server (which can legitimately not run if every port in
/// its range is taken), there's no reasonable degraded mode without a
/// database, so `lib.rs` propagates this out of `setup` and lets Tauri abort
/// startup. `Box<dyn std::error::Error>` (rather than `+ Send + Sync`)
/// matches `tauri::Builder::setup`'s closure signature exactly, so `?`
/// composes directly in `lib.rs` without an extra conversion.
pub fn init() -> Result<DbHandle, Box<dyn std::error::Error>> {
    let path = db_path()?;
    open_and_migrate(&path)
}

/// Test-only entry point: identical open/pragma/migrate logic to `init`,
/// but against a caller-supplied path rather than the real app-support
/// directory, so tests exercise the real schema/migration code without
/// touching (or racing with) the user's actual `conceptify.db`. Used by the
/// `db_check` command test in `lib.rs`.
#[cfg(test)]
pub fn init_at(path: &std::path::Path) -> Result<DbHandle, Box<dyn std::error::Error>> {
    open_and_migrate(path)
}

fn open_and_migrate(path: &std::path::Path) -> Result<DbHandle, Box<dyn std::error::Error>> {
    let mut conn = Connection::open(path)?;

    // WAL mode (PRD §5.1/§5.6) — set before migrations run, per
    // rusqlite_migration's own guidance that PRAGMAs don't belong inside
    // migrations. `pragma_update_and_check` surfaces the mode SQLite actually
    // switched to; a mismatch (e.g. an in-memory or read-only database,
    // neither of which applies to our on-disk file) is logged rather than
    // treated as fatal, since the app is still usable in rollback-journal
    // mode, just with less writer/reader concurrency.
    let mode: String =
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        eprintln!(
            "[conceptify-db] warning: journal_mode is '{mode}', not WAL (expected for an on-disk, read-write database)"
        );
    }

    // Foreign keys are off by default per-connection in SQLite; every
    // `REFERENCES` constraint in the schema (§4 relies on cascading deletes
    // from project → thread → artifact/comment/follow_up_run) is inert
    // without this.
    conn.pragma_update(None, "foreign_keys", "ON")?;

    migrations::migrations().to_latest(&mut conn)?;
    crate::search::rebuild_artifacts(&conn)?;

    Ok(Arc::new(Mutex::new(conn)))
}

/// Run a closure with exclusive access to the connection, off the async
/// runtime's worker thread. rusqlite is blocking I/O; holding the `Mutex`
/// while `.await`-ing inline would risk stalling other tasks scheduled on
/// the same tokio worker, so axum handlers should reach the DB through this
/// helper rather than locking `DbHandle` directly. (`#[tauri::command]`
/// handlers can lock directly — Tauri already runs command handlers off the
/// main/async-reactor thread.)
pub async fn with_conn<T, F>(db: &DbHandle, f: F) -> Result<T, rusqlite::Error>
where
    F: FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    T: Send + 'static,
{
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&conn)
    })
    .await
    .expect("db worker thread panicked")
}

/// Like `with_conn`, but generic over the error type. Use this when your
/// closure returns a custom error type (e.g. `ProjectError`) rather than
/// `rusqlite::Error`.
pub async fn with_conn_result<T, E, F>(db: &DbHandle, f: F) -> Result<T, E>
where
    F: FnOnce(&Connection) -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: Send + 'static,
{
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&conn)
    })
    .await
    .expect("db worker thread panicked")
}
