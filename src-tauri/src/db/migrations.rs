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
