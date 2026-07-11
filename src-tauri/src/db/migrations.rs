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
        M::up(COMMENT_PARENT_ID),
        M::up(FOLLOW_UP_RUNS_OVERRIDE),
        M::up(FOLLOW_UP_RUNS_ROUTE),
        M::up(FOLLOW_UP_RUNS_QUEUE),
        M::up(FOLLOW_UP_RUNS_ACTIVITY),
        M::up(FOLLOW_UP_RUNS_NOTIFICATIONS),
        M::up(CONFLICT_CANDIDATES),
        M::up(RESPONSE_METADATA),
        M::up(LEARNING_SUGGESTIONS),
        M::up(CONCEPT_MAP),
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

/// Adds `comments.parent_id` — the self-referential link that turns a comment
/// into a threaded **reply** (epic `conceptify-6xi`, bead `conceptify-6xi.1`). A
/// reply is a `comments` row whose `parent_id` names the ROOT comment it answers.
/// Chains are **linear** (a reply's parent is always a root; reply-to-reply is
/// rejected in the domain layer), so `parent_id IS NULL` distinguishes roots from
/// replies everywhere. `ON DELETE CASCADE` means a reply dies with its root — and,
/// through the pre-existing thread cascade onto `comments`, with its thread.
///
/// **Plain `ALTER TABLE ... ADD COLUMN`, not a table rebuild.** SQLite permits
/// adding a column that carries a `REFERENCES` clause *provided the column's
/// default is NULL* — a nullable column with no `DEFAULT` clause is exactly that.
/// (The restriction exists because SQLite cannot retroactively validate a new
/// foreign key against pre-existing rows, so it requires the backfilled value be
/// NULL — which for `parent_id` means "root", the correct interpretation of every
/// comment that predates replies.) This mirrors the `COMMENT_ANCHOR_STATE`
/// plain-ALTER above; unlike the ask-mode rebuild, no existing column, type,
/// default, or constraint changes, so the create-copy-drop-rename dance is
/// unnecessary. The migration runs under `foreign_keys = ON` (inside
/// `rusqlite_migration`'s per-migration transaction) with no orphan risk: every
/// backfilled `parent_id` is NULL.
///
/// `idx_comments_parent_id` backs the reply-chain reads (`WHERE parent_id = ?` for
/// get-context exchange history) and the cascade's child lookup.
///
/// Appended (never folded into `COMMENTS`) per this file's append-only contract:
/// `rusqlite_migration` tracks progress positionally by `user_version`, so an
/// in-place edit would be silently skipped by databases already past `COMMENTS`.
const COMMENT_PARENT_ID: &str = "
ALTER TABLE comments ADD COLUMN parent_id TEXT NULL
    REFERENCES comments(id) ON DELETE CASCADE;
CREATE INDEX idx_comments_parent_id ON comments(parent_id);
";

/// Adds `follow_up_runs.override_json` — the persisted per-run adapter/model
/// override (epic `conceptify-e7m`, bead `conceptify-e7m.1`). A run started with
/// an explicit `{adapter?, model?}` override stores its serialized
/// [`crate::settings::RunOverride`] here (`NULL` when the run used pure
/// defaults). Persisting on the row — rather than re-passing from the frontend
/// — is what lets **retry** (FR-5.3) re-spawn a failed generation with the
/// SAME override the original run used, robustly across app restarts (the
/// frontend need not remember it). The row's `agent`/`model` columns already
/// record the *resolved* selection; this extra column records the *intent* so a
/// retry re-derives current defaults for an override-free run but re-applies a
/// real override verbatim.
///
/// Plain nullable `ALTER TABLE ... ADD COLUMN`, no `DEFAULT` — like
/// `COMMENT_ANCHOR_STATE`/`COMMENT_PARENT_ID` above: pre-existing rows backfill
/// to `NULL` (correctly meaning "no override"), and no table rebuild is needed
/// (no CHECK/constraint change). Appended (never folded into an earlier entry)
/// per this file's append-only contract — `rusqlite_migration` tracks progress
/// positionally by `user_version`, so an in-place edit would be silently skipped
/// by databases already migrated past `follow_up_runs`.
const FOLLOW_UP_RUNS_OVERRIDE: &str = "
ALTER TABLE follow_up_runs ADD COLUMN override_json TEXT NULL;
";

/// Adds `follow_up_runs.route` — the resolved execution route recorded by
/// provider routing (epic `conceptify-e7m`, bead `conceptify-e7m.7`):
/// `'anthropic'` (native claude CLI), `'openai'` (native codex CLI),
/// `'openrouter'` (claude CLI pointed at OpenRouter via per-run env), or
/// `'manual'` (routing bypassed by an explicit adapter choice). Token-free by
/// construction — it is a tag, never credentials — and recorded so a user can
/// always tell which path executed (the run-log header carries the same tag).
/// Free-form TEXT rather than a CHECK, matching `status`'s rationale: the
/// authoritative value set lives in `crate::routing::RouteTag`.
///
/// Plain nullable `ALTER TABLE ... ADD COLUMN`, no `DEFAULT`, like
/// `FOLLOW_UP_RUNS_OVERRIDE` above: pre-routing rows backfill to `NULL`
/// (correctly meaning "route unrecorded"). Appended per this file's
/// append-only contract — `rusqlite_migration` tracks progress positionally by
/// `user_version`, so an in-place edit would be silently skipped by databases
/// already migrated past `FOLLOW_UP_RUNS_OVERRIDE`.
const FOLLOW_UP_RUNS_ROUTE: &str = "
ALTER TABLE follow_up_runs ADD COLUMN route TEXT NULL;
";

/// Adds the durable scheduler inputs accepted in `docs/concurrency-policy.md`
/// (bead `conceptify-k9z.2`). A queued run must be executable after an app
/// restart, so its prompt and non-secret environment overrides live with the
/// historical run record instead of only in an in-memory task. Route secrets
/// are deliberately excluded: routing resolves them again from the write-only
/// settings row when execution starts.
///
/// Every column is nullable for backward compatibility. Rows written before
/// the scheduler migration are terminal history (startup reconciliation has
/// already failed any stale `running` row) and are never admitted, so inventing
/// queue metadata for them would be misleading. New scheduler submissions
/// populate the full set in one INSERT and domain code treats a queued row with
/// missing metadata as invalid/failed rather than guessing.
///
/// `queue_seq` is allocated while holding the app's single DB connection and is
/// the deterministic tie-break after `queued_at`. The partial indexes back the
/// admission scan and same-thread mutation guard without bloating lookups over
/// terminal history.
const FOLLOW_UP_RUNS_QUEUE: &str = "
ALTER TABLE follow_up_runs ADD COLUMN run_class TEXT NULL;
ALTER TABLE follow_up_runs ADD COLUMN provider_pool TEXT NULL;
ALTER TABLE follow_up_runs ADD COLUMN prompt TEXT NULL;
ALTER TABLE follow_up_runs ADD COLUMN env_json TEXT NULL;
ALTER TABLE follow_up_runs ADD COLUMN base_artifact_version INTEGER NULL;
ALTER TABLE follow_up_runs ADD COLUMN queued_at TEXT NULL;
ALTER TABLE follow_up_runs ADD COLUMN queue_seq INTEGER NULL;
ALTER TABLE follow_up_runs ADD COLUMN execution_started_at TEXT NULL;
ALTER TABLE follow_up_runs ADD COLUMN not_before TEXT NULL;
ALTER TABLE follow_up_runs ADD COLUMN retry_of_run_id TEXT NULL
    REFERENCES follow_up_runs(id) ON DELETE SET NULL;
ALTER TABLE follow_up_runs ADD COLUMN status_reason TEXT NULL;

CREATE UNIQUE INDEX idx_follow_up_runs_queue_seq
    ON follow_up_runs(queue_seq) WHERE queue_seq IS NOT NULL;
CREATE INDEX idx_follow_up_runs_admission
    ON follow_up_runs(provider_pool, status, queued_at, queue_seq)
    WHERE status IN ('queued', 'throttled');
CREATE INDEX idx_follow_up_runs_mutation_target
    ON follow_up_runs(thread_id, status)
    WHERE run_class = 'mutation'
      AND status IN ('starting', 'running', 'cancelling');
";

/// Adds a durable dismissal marker for the global activity tray
/// (`conceptify-k9z.4`). Active work ignores this field; terminal completed or
/// attention items remain hidden after the user explicitly clears them. NULL
/// preserves every pre-migration row as undisposed history.
const FOLLOW_UP_RUNS_ACTIVITY: &str = "
ALTER TABLE follow_up_runs ADD COLUMN activity_dismissed_at TEXT NULL;
CREATE INDEX idx_follow_up_runs_activity
    ON follow_up_runs(status, activity_dismissed_at, finished_at);
";

/// Durable delivery/read markers for activity notifications
/// (`conceptify-k9z.5`). They make the in-app unread badge survive restarts and
/// provide an atomic at-most-once claim for optional native notifications.
const FOLLOW_UP_RUNS_NOTIFICATIONS: &str = "
ALTER TABLE follow_up_runs ADD COLUMN activity_seen_at TEXT NULL;
ALTER TABLE follow_up_runs ADD COLUMN system_notified_at TEXT NULL;
CREATE INDEX idx_follow_up_runs_unseen_activity
    ON follow_up_runs(activity_seen_at, finished_at);
";

/// Retains stale mutation output plus provenance for explicit conflict review
/// and records provenance on any later explicit publication (`k9z.6`).
const CONFLICT_CANDIDATES: &str = "
ALTER TABLE follow_up_runs ADD COLUMN candidate_path TEXT NULL;
ALTER TABLE follow_up_runs ADD COLUMN conflict_current_version INTEGER NULL;
ALTER TABLE follow_up_runs ADD COLUMN conflict_resolution TEXT NULL;
ALTER TABLE artifacts ADD COLUMN source_run_id TEXT NULL
    REFERENCES follow_up_runs(id) ON DELETE SET NULL;
ALTER TABLE artifacts ADD COLUMN source_base_version INTEGER NULL;
ALTER TABLE artifacts ADD COLUMN resolution TEXT NULL;
";

/// Immutable response intent and versioned skill selection provenance. Runs
/// retain what the agent received; artifacts snapshot the same metadata so a
/// viewed historical version never depends on mutable run or preference state.
const RESPONSE_METADATA: &str = "
ALTER TABLE follow_up_runs ADD COLUMN response_intent_json TEXT NULL;
ALTER TABLE follow_up_runs ADD COLUMN selected_skills_json TEXT NULL;
ALTER TABLE artifacts ADD COLUMN response_intent_json TEXT NULL;
ALTER TABLE artifacts ADD COLUMN selected_skills_json TEXT NULL;
";

/// Durable, editable next-question branches extracted from artifact markup.
/// A launched row is also the source→destination trail; retaining dismissed
/// and superseded rows makes decisions and backtracking stable across updates.
const LEARNING_SUGGESTIONS: &str = "
CREATE TABLE learning_suggestions (
    id                      TEXT PRIMARY KEY,
    project_id              TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    source_thread_id        TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    source_artifact_version INTEGER NOT NULL,
    source_cfy_id           TEXT NOT NULL,
    branch                  TEXT NOT NULL CHECK (branch IN ('example', 'counterexample', 'mechanism', 'tradeoff', 'prerequisite')),
    question                TEXT NOT NULL,
    reason                  TEXT NOT NULL,
    status                  TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'dismissed', 'launched', 'superseded')),
    launched_thread_id      TEXT NULL REFERENCES threads(id) ON DELETE SET NULL,
    edited_question         TEXT NULL,
    created_at              TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (source_thread_id, source_artifact_version, source_cfy_id)
);
CREATE INDEX idx_learning_suggestions_project_status
    ON learning_suggestions(project_id, status, created_at DESC);
CREATE UNIQUE INDEX idx_learning_suggestions_launched_thread
    ON learning_suggestions(launched_thread_id) WHERE launched_thread_id IS NOT NULL;
";

/// Explicit semantic concept mentions plus user-pinned relationships. The map
/// is a derived index: each artifact save replaces only that thread's mentions.
const CONCEPT_MAP: &str = "
CREATE TABLE concepts (
    id             TEXT PRIMARY KEY,
    project_id     TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    canonical_name TEXT NOT NULL,
    display_name   TEXT NOT NULL,
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (project_id, canonical_name)
);
CREATE TABLE concept_mentions (
    id               TEXT PRIMARY KEY,
    concept_id       TEXT NOT NULL REFERENCES concepts(id) ON DELETE CASCADE,
    thread_id        TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    artifact_version INTEGER NOT NULL,
    cfy_id           TEXT NOT NULL,
    kind             TEXT NOT NULL CHECK (kind IN ('section', 'visual', 'question')),
    label            TEXT NOT NULL,
    UNIQUE (concept_id, thread_id, artifact_version, cfy_id)
);
CREATE INDEX idx_concept_mentions_thread ON concept_mentions(thread_id);
CREATE TABLE concept_links (
    id              TEXT PRIMARY KEY,
    project_id      TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    from_concept_id TEXT NOT NULL REFERENCES concepts(id) ON DELETE CASCADE,
    to_concept_id   TEXT NOT NULL REFERENCES concepts(id) ON DELETE CASCADE,
    label           TEXT NOT NULL,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK (from_concept_id <> to_concept_id),
    UNIQUE (from_concept_id, to_concept_id, label)
);
CREATE INDEX idx_concept_links_project ON concept_links(project_id);
";

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// `user_version` after the full chain — the count of `M::up` entries in
    /// [`migrations`].
    const LATEST: usize = 19;

    /// Position of the durable scheduler metadata migration.
    const RUN_QUEUE: usize = 13;

    /// Position of the resolved execution-route tag migration.
    const ROUTE: usize = 12;

    const RUN_ACTIVITY: usize = 14;
    const RUN_NOTIFICATIONS: usize = 15;
    const CONFLICTS: usize = 16;
    const RESPONSE_PROFILE: usize = 17;
    const LEARNING_PATHS: usize = 18;
    const CONCEPTS: usize = 19;

    /// Position of the `follow_up_runs.override_json` ALTER (the 11th
    /// migration), pinned explicitly — like `ASK_MODE` below — so appending
    /// later migrations never shifts its before/after boundary out from under
    /// its test.
    const OVERRIDE_JSON: usize = 11;

    /// Position of the `follow_up_runs` ask-mode rebuild (the 9th migration), so
    /// `ASK_MODE - 1` is the schema state immediately before it. Pinned
    /// explicitly rather than derived from `LATEST` so appending later migrations
    /// (e.g. `COMMENT_PARENT_ID` at position 10) never shifts the rebuild's
    /// before/after boundary out from under the byte-identity test.
    const ASK_MODE: usize = 9;

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
        m.to_version(&mut conn, ASK_MODE - 1).expect("to pre-rebuild");
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
        m.to_version(&mut conn, ASK_MODE).expect("apply ask-mode rebuild");

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

    /// The load-bearing test for migration 10 (`COMMENT_PARENT_ID`): a comment
    /// written under the pre-`parent_id` schema survives the plain ALTER as a
    /// **root** (`parent_id IS NULL`), the self-referential FK + `ON DELETE
    /// CASCADE` are live afterward (an orphan reply is rejected; deleting a root
    /// cascade-deletes its reply), and the `idx_comments_parent_id` index exists.
    #[test]
    fn add_parent_id_preserves_comments_and_enables_reply_cascade() {
        let mut conn = fresh_conn();
        let m = migrations();

        // Migrate to just before parent_id (the ask-mode state), then seed a
        // project + thread + artifact v1 + one root comment under the OLD schema.
        m.to_version(&mut conn, ASK_MODE).expect("to pre-parent_id");
        seed_thread(&conn);
        conn.execute(
            "INSERT INTO artifacts (id, thread_id, version, file_path, created_by)
             VALUES ('a1', 't1', 1, '/x.html', 'initial')",
            [],
        )
        .expect("seed artifact v1");
        conn.execute(
            "INSERT INTO comments (id, thread_id, artifact_version, body, status)
             VALUES ('root', 't1', 1, 'q', 'answered')",
            [],
        )
        .expect("seed pre-migration comment");

        // Apply the parent_id migration.
        m.to_version(&mut conn, LATEST)
            .expect("apply parent_id migration");

        // The pre-existing comment survives and is a root (parent_id NULL).
        let parent: Option<String> = conn
            .query_row(
                "SELECT parent_id FROM comments WHERE id = 'root'",
                [],
                |r| r.get(0),
            )
            .expect("row should survive");
        assert!(parent.is_none(), "existing comment becomes a root");

        // A reply referencing the root inserts; the self-ref FK rejects an orphan.
        conn.execute(
            "INSERT INTO comments (id, thread_id, artifact_version, body, status, parent_id)
             VALUES ('reply', 't1', 1, 'follow-up', 'open', 'root')",
            [],
        )
        .expect("reply with a valid parent inserts");
        assert!(
            conn.execute(
                "INSERT INTO comments (id, thread_id, artifact_version, body, status, parent_id)
                 VALUES ('orphan', 't1', 1, 'x', 'open', 'ghost')",
                [],
            )
            .is_err(),
            "self-referential FK must reject an unknown parent"
        );

        // Deleting the root cascades to the reply.
        conn.execute("DELETE FROM comments WHERE id = 'root'", [])
            .expect("delete root");
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM comments WHERE id IN ('root', 'reply')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0, "reply cascade-deletes with its root");

        // The index the cascade / chain reads rely on is present.
        let idx: Option<String> = conn
            .query_row(
                "SELECT name FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_comments_parent_id'",
                [],
                |r| r.get(0),
            )
            .ok();
        assert_eq!(idx.as_deref(), Some("idx_comments_parent_id"));
    }

    /// Migration 11 (`FOLLOW_UP_RUNS_OVERRIDE`): a `follow_up_runs` row written
    /// under the pre-`override_json` schema survives the plain ALTER with
    /// `override_json` backfilled to `NULL` ("no override"), and a row inserted
    /// afterward can store and read back a serialized override blob.
    #[test]
    fn add_override_json_preserves_runs_and_stores_blob() {
        let mut conn = fresh_conn();
        let m = migrations();

        // Migrate to just before override_json, then seed a run row under the
        // OLD schema (no override_json column exists yet).
        m.to_version(&mut conn, OVERRIDE_JSON - 1)
            .expect("to pre-override_json");
        seed_thread(&conn);
        conn.execute(
            "INSERT INTO follow_up_runs
                 (id, thread_id, agent, model, mode, status, log_path)
             VALUES ('r-old', 't1', 'claude', 'claude-sonnet-5', 'ask', 'failed', '/l.log')",
            [],
        )
        .expect("seed pre-migration run");

        // Apply the override_json migration.
        m.to_version(&mut conn, OVERRIDE_JSON)
            .expect("apply override_json migration");

        // The pre-existing row survives with override_json NULL.
        let over: Option<String> = conn
            .query_row(
                "SELECT override_json FROM follow_up_runs WHERE id = 'r-old'",
                [],
                |r| r.get(0),
            )
            .expect("row should survive");
        assert!(over.is_none(), "pre-migration run backfills to NULL override");

        // A new row can store and read back a serialized override blob.
        conn.execute(
            "INSERT INTO follow_up_runs
                 (id, thread_id, agent, model, mode, status, log_path, override_json)
             VALUES ('r-new', 't1', 'codex', 'gpt-5', 'ask', 'running', '/l2.log', ?1)",
            [r#"{"adapter":"codex","model":"gpt-5"}"#],
        )
        .expect("insert run with override_json");
        let over: Option<String> = conn
            .query_row(
                "SELECT override_json FROM follow_up_runs WHERE id = 'r-new'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(over.as_deref(), Some(r#"{"adapter":"codex","model":"gpt-5"}"#));
    }

    /// Migration 12 (`FOLLOW_UP_RUNS_ROUTE`): a run row written under the
    /// pre-`route` schema survives the plain ALTER with `route` backfilled to
    /// `NULL` ("route unrecorded"), and a row inserted afterward stores a tag.
    #[test]
    fn add_route_preserves_runs_and_stores_tag() {
        let mut conn = fresh_conn();
        let m = migrations();

        m.to_version(&mut conn, ROUTE - 1).expect("to pre-route");
        seed_thread(&conn);
        conn.execute(
            "INSERT INTO follow_up_runs
                 (id, thread_id, agent, model, mode, status, log_path)
             VALUES ('r-pre', 't1', 'claude', 'claude-sonnet-5', 'ask', 'completed', '/l.log')",
            [],
        )
        .expect("seed pre-migration run");

        m.to_version(&mut conn, ROUTE).expect("apply route migration");

        let route: Option<String> = conn
            .query_row(
                "SELECT route FROM follow_up_runs WHERE id = 'r-pre'",
                [],
                |r| r.get(0),
            )
            .expect("row should survive");
        assert!(route.is_none(), "pre-routing run backfills to NULL route");

        conn.execute(
            "INSERT INTO follow_up_runs
                 (id, thread_id, agent, model, mode, status, log_path, route)
             VALUES ('r-routed', 't1', 'claude', 'google/gemini-3-pro', 'ask', 'running',
                     '/l2.log', 'openrouter')",
            [],
        )
        .expect("insert run with route");
        let route: Option<String> = conn
            .query_row(
                "SELECT route FROM follow_up_runs WHERE id = 'r-routed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(route.as_deref(), Some("openrouter"));
    }

    /// Migration 13 adds enough durable input to execute queued work after a
    /// restart, while retaining pre-scheduler rows as nullable history.
    #[test]
    fn add_run_queue_metadata_preserves_history_and_enforces_order_keys() {
        let mut conn = fresh_conn();
        let m = migrations();

        m.to_version(&mut conn, RUN_QUEUE - 1)
            .expect("to pre-queue schema");
        seed_thread(&conn);
        conn.execute(
            "INSERT INTO follow_up_runs
                 (id, thread_id, agent, model, mode, status, log_path, route)
             VALUES ('r-old', 't1', 'claude', 'claude-sonnet-5', 'answer',
                     'completed', '/old.log', 'anthropic')",
            [],
        )
        .expect("seed historical run");

        m.to_version(&mut conn, RUN_QUEUE)
            .expect("apply queue metadata migration");

        let old: (Option<String>, Option<i64>, Option<String>) = conn
            .query_row(
                "SELECT run_class, queue_seq, prompt
                 FROM follow_up_runs WHERE id = 'r-old'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("historical row survives");
        assert_eq!(old, (None, None, None));

        conn.execute(
            "INSERT INTO follow_up_runs
                 (id, thread_id, agent, model, mode, status, log_path, route,
                  run_class, provider_pool, prompt, env_json,
                  base_artifact_version, queued_at, queue_seq)
             VALUES ('r-queued', 't1', 'claude', 'claude-sonnet-5', 'answer',
                     'queued', '/queued.log', 'anthropic', 'exploration',
                     'anthropic', 'answer this', '[[\"PATH\",\"/bin\"]]', NULL,
                     '2026-07-11T08:00:00.000Z', 1)",
            [],
        )
        .expect("insert durable queued run");

        let queued: (String, String, String, i64) = conn
            .query_row(
                "SELECT run_class, provider_pool, prompt, queue_seq
                 FROM follow_up_runs WHERE id = 'r-queued'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            queued,
            (
                "exploration".to_owned(),
                "anthropic".to_owned(),
                "answer this".to_owned(),
                1,
            )
        );

        let duplicate = conn.execute(
            "INSERT INTO follow_up_runs
                 (id, thread_id, agent, model, mode, status, log_path, queue_seq)
             VALUES ('r-duplicate', 't1', 'claude', 'claude-sonnet-5', 'answer',
                     'queued', '/duplicate.log', 1)",
            [],
        );
        assert!(duplicate.is_err(), "queue sequence must be unique when present");

        let index_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index'
                   AND name IN ('idx_follow_up_runs_queue_seq',
                                'idx_follow_up_runs_admission',
                                'idx_follow_up_runs_mutation_target')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 3);
    }

    #[test]
    fn add_run_activity_marker_preserves_rows_and_can_dismiss_terminal_history() {
        let mut conn = fresh_conn();
        let m = migrations();
        m.to_version(&mut conn, RUN_ACTIVITY - 1)
            .expect("to pre-activity schema");
        seed_thread(&conn);
        conn.execute(
            "INSERT INTO follow_up_runs
                 (id, thread_id, agent, model, mode, status, log_path)
             VALUES ('r1', 't1', 'claude', 'm', 'answer', 'completed', '/r.log')",
            [],
        )
        .unwrap();
        m.to_version(&mut conn, RUN_ACTIVITY).unwrap();
        let dismissed: Option<String> = conn
            .query_row(
                "SELECT activity_dismissed_at FROM follow_up_runs WHERE id = 'r1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(dismissed.is_none());
        conn.execute(
            "UPDATE follow_up_runs SET activity_dismissed_at = '2026-07-11T12:00:00.000Z'
             WHERE id = 'r1'",
            [],
        )
        .unwrap();
    }

    #[test]
    fn add_notification_markers_preserves_unseen_and_unnotified_history() {
        let mut conn = fresh_conn();
        let m = migrations();
        m.to_version(&mut conn, RUN_NOTIFICATIONS - 1)
            .expect("to pre-notification schema");
        seed_thread(&conn);
        conn.execute(
            "INSERT INTO follow_up_runs
                 (id, thread_id, agent, model, mode, status, log_path)
             VALUES ('r1', 't1', 'claude', 'm', 'answer', 'failed', '/r.log')",
            [],
        )
        .unwrap();
        m.to_version(&mut conn, RUN_NOTIFICATIONS).unwrap();
        let markers: (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT activity_seen_at, system_notified_at
                 FROM follow_up_runs WHERE id = 'r1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(markers, (None, None));
    }

    #[test]
    fn add_conflict_candidate_and_artifact_provenance_columns() {
        let mut conn = fresh_conn();
        let migrations = migrations();
        migrations.to_version(&mut conn, CONFLICTS - 1).unwrap();
        seed_thread(&conn);
        migrations.to_version(&mut conn, CONFLICTS).unwrap();
        conn.execute(
            "INSERT INTO follow_up_runs
                 (id, thread_id, agent, model, mode, status, log_path,
                  candidate_path, conflict_current_version, conflict_resolution)
             VALUES ('r1', 't1', 'claude', 'm', 'apply', 'conflicted', '/r.log',
                     '/candidate.html', 2, 'pending')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO artifacts
                 (id, thread_id, version, file_path, created_by,
                  source_run_id, source_base_version, resolution)
             VALUES ('a1', 't1', 1, '/a.html', 'follow_up', 'r1', 1, 'separate')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn add_response_metadata_preserves_and_snapshots_profile() {
        let mut conn = fresh_conn();
        let migrations = migrations();
        migrations
            .to_version(&mut conn, RESPONSE_PROFILE - 1)
            .unwrap();
        seed_thread(&conn);
        migrations.to_version(&mut conn, RESPONSE_PROFILE).unwrap();
        let intent = r#"{"version":1,"depth":"deep","language":"plain","visuals":"avoid","shape":"reference"}"#;
        let skills = r#"[{"id":"conceptify","name":"Conceptify artifact","capability_version":1,"selection":"manual"}]"#;
        conn.execute(
            "INSERT INTO follow_up_runs
                 (id, thread_id, agent, model, mode, status, log_path,
                  response_intent_json, selected_skills_json)
             VALUES ('r1', 't1', 'claude', 'm', 'ask', 'completed', '/r.log', ?1, ?2)",
            rusqlite::params![intent, skills],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO artifacts
                 (id, thread_id, version, file_path, created_by,
                  response_intent_json, selected_skills_json)
             VALUES ('a1', 't1', 1, '/a.html', 'initial', ?1, ?2)",
            rusqlite::params![intent, skills],
        )
        .unwrap();
        let stored: (String, String) = conn
            .query_row(
                "SELECT response_intent_json, selected_skills_json FROM artifacts WHERE id = 'a1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored, (intent.to_owned(), skills.to_owned()));
    }

    #[test]
    fn add_learning_suggestions_tracks_launch_trails() {
        let mut conn = fresh_conn();
        let migrations = migrations();
        migrations.to_version(&mut conn, LEARNING_PATHS - 1).unwrap();
        seed_thread(&conn);
        migrations.to_version(&mut conn, LEARNING_PATHS).unwrap();
        conn.execute(
            "INSERT INTO learning_suggestions
                 (id, project_id, source_thread_id, source_artifact_version,
                  source_cfy_id, branch, question, reason)
             VALUES ('s1', 'p1', 't1', 1, 'next-example', 'example',
                     'Show an example?', 'Applies the mechanism.')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn add_concept_map_supports_mentions_and_pinned_links() {
        let mut conn = fresh_conn();
        let migrations = migrations();
        migrations.to_version(&mut conn, CONCEPTS - 1).unwrap();
        seed_thread(&conn);
        migrations.to_version(&mut conn, CONCEPTS).unwrap();
        conn.execute(
            "INSERT INTO concepts (id, project_id, canonical_name, display_name)
             VALUES ('c1', 'p1', 'ownership', 'Ownership'),
                    ('c2', 'p1', 'borrowing', 'Borrowing')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO concept_links (id, project_id, from_concept_id, to_concept_id, label)
             VALUES ('l1', 'p1', 'c1', 'c2', 'enables')",
            [],
        )
        .unwrap();
    }
}
