//! Schema definitions (PRD §4).
//!
//! One `M::up` per entity, run in order by `rusqlite_migration` against the
//! `user_version` pragma. `rusqlite_migration` only executes the migrations
//! past the database's current `user_version`, so calling `to_latest` on an
//! already-migrated database is a cheap no-op (one pragma read, no SQL
//! executed) — that's what gives us idempotent startup for free.
//!
//! Timestamps are stored as ISO-8601 UTC text (`strftime('%Y-%m-%dT%H:%M:%fZ',
//! 'now')`) rather than SQLite's `CURRENT_TIMESTAMP` (which omits
//! fractional seconds and the `T`/`Z` markers) so they sort lexicographically
//! and parse directly wherever needed.
//!
//! `PRAGMA` statements are deliberately **not** included here (the
//! `rusqlite_migration` docs discourage it) — `journal_mode` and
//! `foreign_keys` are set once in `db::init`, before migrations run.

use rusqlite_migration::{Migrations, M};

/// All schema migrations, in order. Append new `M::up(...)` entries for
/// future schema changes — never edit an already-shipped entry, since
/// `rusqlite_migration` tracks progress positionally via `user_version`.
pub fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(PROJECTS),
        M::up(THREADS),
        M::up(ARTIFACTS),
        M::up(COMMENTS),
        M::up(FOLLOW_UP_RUNS),
        M::up(SETTINGS),
        M::up(THREAD_SLUG),
        M::up(COMMENT_ANCHOR_STATE),
        // The only table-rebuild migration in the chain; `foreign_key_check`
        // validates that the DROP/RENAME preserved referential integrity
        // before the migration's transaction commits (see the const's doc).
        M::up(FOLLOW_UP_RUNS_ASK_MODE).foreign_key_check(),
    ])
}

/// A workspace mapped 1:1 to a root directory (PRD §4). `root_path` is
/// unique — `ensure-project` (bead `conceptify-qxr.1`) relies on this to
/// dedupe by canonicalized path.
const PROJECTS: &str = "
CREATE TABLE projects (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    root_path   TEXT NOT NULL UNIQUE,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    archived    INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1))
);
";

/// One question/topic within a project; owns exactly one (versioned)
/// artifact plus its comment history (PRD §4).
const THREADS: &str = "
CREATE TABLE threads (
    id                TEXT PRIMARY KEY,
    project_id        TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    title             TEXT NOT NULL,
    initial_question  TEXT NOT NULL,
    status            TEXT NOT NULL
                          CHECK (status IN ('generating', 'ready', 'updating', 'error')),
    created_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX idx_threads_project_id ON threads(project_id);
";

/// The self-contained HTML answer. Versioned: every agent update creates a
/// new version, prior versions retained. `file_path` points at the file on
/// disk (§5.6) — the DB never stores the HTML body itself.
const ARTIFACTS: &str = "
CREATE TABLE artifacts (
    id          TEXT PRIMARY KEY,
    thread_id   TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    version     INTEGER NOT NULL,
    file_path   TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    created_by  TEXT NOT NULL CHECK (created_by IN ('initial', 'follow_up')),
    UNIQUE (thread_id, version)
);
CREATE INDEX idx_artifacts_thread_id ON artifacts(thread_id);
";

/// A user annotation anchored to a region of the artifact (PRD §7.4,
/// FR-4.4). `anchor` is nullable JSON: null models a direct follow-up
/// question (FR-4.3), which flows through the same sidebar/resolution
/// machinery as an anchored comment.
///
/// `(thread_id, artifact_version)` has a composite foreign key onto
/// `artifacts(thread_id, version)` — a comment always anchors to an artifact
/// version that already exists, so this enforces "no orphaned reference"
/// referential integrity at the DB layer (see bd notes for what this implies
/// for insert order in the comments-backend bead).
const COMMENTS: &str = "
CREATE TABLE comments (
    id                TEXT PRIMARY KEY,
    thread_id         TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    artifact_version  INTEGER NOT NULL,
    anchor            TEXT,
    body              TEXT NOT NULL,
    status            TEXT NOT NULL
                          CHECK (status IN ('open', 'answered', 'applied')),
    answer_html       TEXT,
    created_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    resolved_at       TEXT,
    FOREIGN KEY (thread_id, artifact_version)
        REFERENCES artifacts (thread_id, version)
        ON DELETE CASCADE
);
CREATE INDEX idx_comments_thread_id ON comments(thread_id);
";

/// One execution of a background agent handling a batch of comments (or a
/// direct follow-up question) (PRD §4, FR-4.6/4.7/4.8).
///
/// Note: unlike `threads.status`, `comments.status` and `artifacts.created_by`,
/// the PRD's domain-model table does **not** enumerate `follow_up_runs.status`
/// values (compare the `threads` row, which spells out
/// `generating|ready|updating|error` in parens) — so no `CHECK` constraint is
/// applied here. Left as free-form `TEXT` for the agent-spawner bead
/// (`conceptify-b12.1`) to define (candidates per FR-4.8: `running`,
/// `completed`, `failed`, `cancelled`).
const FOLLOW_UP_RUNS: &str = "
CREATE TABLE follow_up_runs (
    id           TEXT PRIMARY KEY,
    thread_id    TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    agent        TEXT NOT NULL,
    model        TEXT NOT NULL,
    mode         TEXT NOT NULL CHECK (mode IN ('answer', 'apply')),
    status       TEXT NOT NULL,
    log_path     TEXT NOT NULL,
    started_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    finished_at  TEXT
);
CREATE INDEX idx_follow_up_runs_thread_id ON follow_up_runs(thread_id);
";

/// Global app configuration (PRD §4): default agent adapter, per-purpose
/// models, theme, editor, etc. Modeled as a plain key/value store rather
/// than fixed columns — the PRD lists example settings, not a fixed schema,
/// and new settings will be added over time without needing a migration
/// each time.
const SETTINGS: &str = "
CREATE TABLE settings (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL
);
";

/// Adds `threads.slug` — the filesystem-safe artifact-folder name (§5.6),
/// unique within a project (bead `conceptify-qxr.2`).
///
/// A separate migration rather than a field folded into the original
/// `THREADS` definition: this file's contract (see the module doc) is that
/// shipped `M::up` entries are never edited, since `rusqlite_migration`
/// tracks progress positionally by `user_version`. Databases already migrated
/// to the `THREADS`-through-`SETTINGS` state would silently skip an in-place
/// edit and end up without the column, so the change ships as an appended
/// migration that both fresh and existing databases pick up.
///
/// SQLite's `ALTER TABLE ... ADD COLUMN` can't attach a `UNIQUE` constraint
/// (or a non-constant default) inline, so uniqueness is enforced by a
/// separate composite `UNIQUE INDEX` on `(project_id, slug)`. The `NOT NULL
/// DEFAULT ''` placeholder only matters for rows that predate this migration;
/// there are none, because no create-thread path existed before this bead, so
/// the unique index has no `''` collisions to trip over. Every row inserted
/// hereafter supplies a real, deduped slug.
const THREAD_SLUG: &str = "
ALTER TABLE threads ADD COLUMN slug TEXT NOT NULL DEFAULT '';
CREATE UNIQUE INDEX idx_threads_project_slug ON threads(project_id, slug);
";

/// Adds `comments.anchor_state` — the FR-4.4 re-attachment flag (bead
/// `conceptify-94m.2`), driven by the re-anchoring bead (`conceptify-94m.7`).
///
/// The comment's `anchor` JSON is the *authoring-time* selection (what the user
/// picked, against `artifact_version`); it is immutable once written. Whether
/// that anchor still resolves in the artifact's current version is a separate,
/// mutable concern, so it lives in its own first-class column rather than being
/// folded into the (verbatim-stored, opaque) anchor blob: a plain `UPDATE ...
/// SET anchor_state` avoids a read-parse-rewrite of the JSON, and the sidebar
/// (`conceptify-94m.6`) can badge "reference moved" off a queryable field.
///
/// `anchored` = the anchor resolves / is a fresh or unchecked comment / is a
/// null-anchor direct follow-up (which can never move). `moved` = re-attachment
/// could not locate the anchor in the current version → surface "reference
/// moved", never silently drop the comment.
///
/// Appended (not folded into the original `COMMENTS` definition) per this
/// file's append-only contract: `rusqlite_migration` tracks progress
/// positionally by `user_version`, so an in-place edit would be silently
/// skipped by databases already migrated past `COMMENTS`. SQLite's `ALTER TABLE
/// ... ADD COLUMN` accepts a column-local `CHECK` and a constant `NOT NULL`
/// default, so both ship inline here; the `'anchored'` default only backfills
/// rows predating this migration (there are none — no create-comment path
/// existed before this bead).
const COMMENT_ANCHOR_STATE: &str = "
ALTER TABLE comments ADD COLUMN anchor_state TEXT NOT NULL DEFAULT 'anchored'
    CHECK (anchor_state IN ('anchored', 'moved'));
";

/// Extends `follow_up_runs.mode`'s CHECK from `('answer', 'apply')` to
/// `('answer', 'apply', 'ask')` so the in-app ask flow (bead `conceptify-959.1`)
/// can record its runs as `mode = 'ask'` → `RunMode::Ask`/`Purpose::InAppAsk`,
/// giving them the same log/status/guard machinery as follow-up and apply runs
/// (decision recorded on bead `conceptify-b12.2`, this bead `conceptify-iho`).
///
/// SQLite cannot `ALTER` a column's `CHECK` constraint in place, so this
/// rebuilds the table via the standard create-copy-drop-rename procedure. The
/// new table is **byte-for-byte identical** to the original `FOLLOW_UP_RUNS`
/// definition except for the one widened `CHECK` — every column, type, default,
/// the `PRIMARY KEY`, the `REFERENCES threads(id) ON DELETE CASCADE` foreign
/// key, and the absence of a `status` CHECK (free-form `TEXT` by design) are
/// preserved. The `INSERT ... SELECT` names every column explicitly and copies
/// `started_at`/`finished_at` verbatim, so pre-existing rows survive unchanged
/// (their original `started_at` is copied, never re-defaulted). The index is
/// dropped with the old table and recreated identically.
///
/// Appended (not folded into the original `FOLLOW_UP_RUNS` definition) per this
/// file's append-only contract: `rusqlite_migration` tracks progress
/// positionally by `user_version`, so an in-place edit would be silently
/// skipped by databases already migrated past `FOLLOW_UP_RUNS`.
///
/// Foreign keys: `db::open_and_migrate` runs the chain with `PRAGMA
/// foreign_keys = ON`, and `rusqlite_migration` wraps each migration in a
/// transaction — inside which `PRAGMA foreign_keys` is a no-op — so the usual
/// "turn FKs off around a rebuild" dance is neither possible nor needed here.
/// It is unnecessary because `follow_up_runs` is a *leaf*: it only holds an
/// outbound FK to `threads`, and no table references it, so dropping and
/// renaming it cannot orphan any child rows or rewrite any other table's
/// schema. The copied rows keep referencing the same still-present `threads`,
/// so integrity holds throughout; `.foreign_key_check()` on the `M::up` entry
/// asserts exactly that before the transaction commits.
const FOLLOW_UP_RUNS_ASK_MODE: &str = "
CREATE TABLE follow_up_runs_new (
    id           TEXT PRIMARY KEY,
    thread_id    TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    agent        TEXT NOT NULL,
    model        TEXT NOT NULL,
    mode         TEXT NOT NULL CHECK (mode IN ('answer', 'apply', 'ask')),
    status       TEXT NOT NULL,
    log_path     TEXT NOT NULL,
    started_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    finished_at  TEXT
);
INSERT INTO follow_up_runs_new
    (id, thread_id, agent, model, mode, status, log_path, started_at, finished_at)
    SELECT id, thread_id, agent, model, mode, status, log_path, started_at, finished_at
    FROM follow_up_runs;
DROP TABLE follow_up_runs;
ALTER TABLE follow_up_runs_new RENAME TO follow_up_runs;
CREATE INDEX idx_follow_up_runs_thread_id ON follow_up_runs(thread_id);
";

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// `user_version` after the full chain — the count of `M::up` entries in
    /// [`migrations`]. The `follow_up_runs` ask-mode rebuild is the last one,
    /// so `LATEST - 1` is the schema state immediately before it.
    const LATEST: usize = 9;

    /// Open an in-memory DB with the same `foreign_keys = ON` posture
    /// `db::open_and_migrate` uses in production, so the rebuild migration is
    /// exercised under real FK enforcement.
    fn fresh_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.pragma_update(None, "foreign_keys", "ON")
            .expect("enable foreign keys");
        conn
    }

    fn user_version(conn: &Connection) -> usize {
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .expect("read user_version") as usize
    }

    /// Seed a project + thread so `follow_up_runs` rows have a valid FK target.
    fn seed_thread(conn: &Connection) {
        conn.execute(
            "INSERT INTO projects (id, name, root_path) VALUES ('p1', 'Proj', '/tmp/p1')",
            [],
        )
        .expect("seed project");
        conn.execute(
            "INSERT INTO threads (id, project_id, title, initial_question, status)
             VALUES ('t1', 'p1', 'Title', 'q', 'generating')",
            [],
        )
        .expect("seed thread");
    }

    /// The whole `follow_up_runs` row, in schema column order, for
    /// byte-identical before/after comparison across the rebuild.
    type Row = (
        String,         // id
        String,         // thread_id
        String,         // agent
        String,         // model
        String,         // mode
        String,         // status
        String,         // log_path
        String,         // started_at
        Option<String>, // finished_at
    );

    fn read_run(conn: &Connection, id: &str) -> Row {
        conn.query_row(
            "SELECT id, thread_id, agent, model, mode, status, log_path, started_at, finished_at
             FROM follow_up_runs WHERE id = ?1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            },
        )
        .expect("row should exist")
    }

    /// The whole chain applies cleanly on a fresh database and lands on the
    /// expected `user_version` (guards against an off-by-one if an entry is
    /// added/removed without updating [`LATEST`]).
    #[test]
    fn full_chain_migrates_cleanly() {
        let mut conn = fresh_conn();
        migrations().to_latest(&mut conn).expect("to_latest");
        assert_eq!(user_version(&conn), LATEST);
    }

    /// The load-bearing test: rows written under the pre-rebuild schema
    /// (`CHECK (mode IN ('answer','apply'))`) survive the rebuild
    /// byte-for-byte, including explicitly-set `started_at`/`finished_at` that
    /// must be *copied*, never re-defaulted.
    #[test]
    fn rebuild_preserves_existing_rows_byte_identical() {
        let mut conn = fresh_conn();
        let m = migrations();

        // Migrate to just before the ask-mode rebuild.
        m.to_version(&mut conn, LATEST - 1).expect("to pre-rebuild");
        seed_thread(&conn);

        // Two rows with distinct pre-migration modes and hand-set timestamps
        // (one with a real finished_at, one with NULL) so the copy — not a
        // default — is what we observe afterward.
        conn.execute(
            "INSERT INTO follow_up_runs
                 (id, thread_id, agent, model, mode, status, log_path, started_at, finished_at)
             VALUES
                 ('r-answer', 't1', 'claude', 'model-a', 'answer', 'completed',
                  '/logs/r-answer.log', '2020-01-02T03:04:05.678Z', '2020-01-02T03:05:06.789Z')",
            [],
        )
        .expect("insert answer row");
        conn.execute(
            "INSERT INTO follow_up_runs
                 (id, thread_id, agent, model, mode, status, log_path, started_at, finished_at)
             VALUES
                 ('r-apply', 't1', 'claude', 'model-b', 'apply', 'running',
                  '/logs/r-apply.log', '2021-06-07T08:09:10.111Z', NULL)",
            [],
        )
        .expect("insert apply row");

        // At the pre-rebuild version the old CHECK must still reject 'ask'.
        assert!(
            conn.execute(
                "INSERT INTO follow_up_runs
                     (id, thread_id, agent, model, mode, status, log_path)
                 VALUES ('r-ask-early', 't1', 'claude', 'm', 'ask', 'running', '/l.log')",
                [],
            )
            .is_err(),
            "pre-rebuild CHECK must reject mode = 'ask'"
        );

        let before_answer = read_run(&conn, "r-answer");
        let before_apply = read_run(&conn, "r-apply");

        // Apply the rebuild.
        m.to_version(&mut conn, LATEST).expect("apply ask-mode rebuild");

        // Rows survive completely unchanged.
        assert_eq!(read_run(&conn, "r-answer"), before_answer);
        assert_eq!(read_run(&conn, "r-apply"), before_apply);

        // And are the only two rows.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM follow_up_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    /// After the rebuild the widened CHECK accepts `'ask'`, still rejects
    /// anything else, and the FK to `threads` is intact.
    #[test]
    fn rebuild_accepts_ask_rejects_bogus_and_keeps_fk() {
        let mut conn = fresh_conn();
        migrations().to_latest(&mut conn).expect("to_latest");
        seed_thread(&conn);

        // 'ask' now inserts.
        conn.execute(
            "INSERT INTO follow_up_runs
                 (id, thread_id, agent, model, mode, status, log_path)
             VALUES ('r-ask', 't1', 'claude', 'm', 'ask', 'running', '/l.log')",
            [],
        )
        .expect("post-rebuild insert of mode = 'ask'");

        // A mode outside the widened set is still rejected.
        assert!(
            conn.execute(
                "INSERT INTO follow_up_runs
                     (id, thread_id, agent, model, mode, status, log_path)
                 VALUES ('r-bogus', 't1', 'claude', 'm', 'bogus', 'running', '/l.log')",
                [],
            )
            .is_err(),
            "widened CHECK must still reject an unknown mode"
        );

        // The rebuilt table's FK to threads is live (unknown thread rejected).
        assert!(
            conn.execute(
                "INSERT INTO follow_up_runs
                     (id, thread_id, agent, model, mode, status, log_path)
                 VALUES ('r-orphan', 'no-such-thread', 'claude', 'm', 'ask', 'running', '/l.log')",
                [],
            )
            .is_err(),
            "rebuilt FK to threads(id) must reject an orphan run"
        );
    }

    /// The `idx_follow_up_runs_thread_id` index is recreated by the rebuild
    /// (it was dropped along with the old table).
    #[test]
    fn rebuild_recreates_thread_id_index() {
        let mut conn = fresh_conn();
        migrations().to_latest(&mut conn).expect("to_latest");

        let idx: Option<String> = conn
            .query_row(
                "SELECT name FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_follow_up_runs_thread_id'
                   AND tbl_name = 'follow_up_runs'",
                [],
                |r| r.get(0),
            )
            .ok();
        assert_eq!(idx.as_deref(), Some("idx_follow_up_runs_thread_id"));
    }
}
