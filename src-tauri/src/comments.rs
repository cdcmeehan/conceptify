//! Comments domain logic (PRD §7.4, FR-4.1–FR-4.5/4.7).
//!
//! A comment is a user annotation anchored to a region of an artifact version,
//! optionally carrying an agent resolution. This module owns:
//!
//! - **create** (FR-4.1/4.2/4.3): store a comment against an existing artifact
//!   version, with an anchor (text-selection or diagram-element) or a `null`
//!   anchor (direct follow-up question);
//! - **list** per thread with an optional status filter (serves the sidebar and
//!   the M5 `list-comments` CLI);
//! - **update** (FR-4.6/4.7): the status machine + `answer_html`, driving the
//!   M5 `resolve-comment` CLI, plus the `anchor_state` re-attachment flag that
//!   bead `conceptify-94m.7` sets.
//!
//! The FR-4.4 anchor **schema** itself is defined (and documented) in
//! `conceptify_types` / docs/api.md; this module validates a submitted anchor
//! against it and stores it verbatim, but never interprets its contents.
//! Re-attachment across versions is bead `conceptify-94m.7`'s job, not this
//! module's — here `anchor_state` is just a stored/served field.

use rusqlite::{Connection, OptionalExtension};

/// The comment status machine (PRD §4): `open` → `answered` → `applied`.
///
/// Transitions may only **advance** along this order (or stay put); a
/// regression (e.g. `answered` → `open`, or anything out of the terminal
/// `applied`) is rejected. `open` → `applied` directly is allowed: an
/// apply-mode run (FR-4.7) can resolve-with-update a comment that never got a
/// separate sidebar answer (the M5 `resolve-comment --applied` one-shot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentStatus {
    Open,
    Answered,
    Applied,
}

impl CommentStatus {
    /// The exact text stored in `comments.status` (matches the DB CHECK).
    pub fn as_str(&self) -> &'static str {
        match self {
            CommentStatus::Open => "open",
            CommentStatus::Answered => "answered",
            CommentStatus::Applied => "applied",
        }
    }

    /// Position in the monotonic status order; a transition is legal iff the
    /// target rank is `>=` the current rank.
    fn rank(self) -> u8 {
        match self {
            CommentStatus::Open => 0,
            CommentStatus::Answered => 1,
            CommentStatus::Applied => 2,
        }
    }

    /// Strict parse of caller-supplied input; unknown values are rejected
    /// (returns `None`) rather than defaulted.
    pub fn parse(s: &str) -> Option<CommentStatus> {
        match s {
            "open" => Some(CommentStatus::Open),
            "answered" => Some(CommentStatus::Answered),
            "applied" => Some(CommentStatus::Applied),
            _ => None,
        }
    }

    /// Lenient parse of a value read back from the DB. The CHECK constraint
    /// guarantees only the three known values are ever stored, so an unknown
    /// string is unreachable; it falls back to `Open`, keeping read paths total
    /// (mirrors `ThreadStatus::from_db_str`).
    pub fn from_db_str(s: &str) -> CommentStatus {
        CommentStatus::parse(s).unwrap_or(CommentStatus::Open)
    }

    /// Whether a transition from `self` to `next` is legal: status may only
    /// advance or stay, never regress.
    pub fn can_advance_to(self, next: CommentStatus) -> bool {
        next.rank() >= self.rank()
    }
}

/// The FR-4.4 re-attachment flag for a comment's anchor. Owned (as a stored
/// field) by this bead; the *policy* of when to flip it to `Moved` is bead
/// `conceptify-94m.7`'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorState {
    /// The anchor resolves, is a fresh/unchecked comment, or is a null-anchor
    /// direct follow-up (which can never move).
    Anchored,
    /// Re-attachment could not locate the anchor in the current artifact
    /// version — surface "reference moved", never silently drop the comment.
    Moved,
}

impl AnchorState {
    pub fn as_str(&self) -> &'static str {
        match self {
            AnchorState::Anchored => "anchored",
            AnchorState::Moved => "moved",
        }
    }

    /// Strict parse of caller-supplied input.
    pub fn parse(s: &str) -> Option<AnchorState> {
        match s {
            "anchored" => Some(AnchorState::Anchored),
            "moved" => Some(AnchorState::Moved),
            _ => None,
        }
    }

    /// Lenient parse of a DB value (CHECK-guaranteed valid), defaulting to
    /// `Anchored`.
    pub fn from_db_str(s: &str) -> AnchorState {
        AnchorState::parse(s).unwrap_or(AnchorState::Anchored)
    }
}

/// A comment row (mirrors the schema). `anchor` is the stored JSON parsed back
/// into a `Value`; `None` is a direct follow-up (FR-4.3).
#[derive(Debug, Clone)]
pub struct Comment {
    pub id: String,
    pub thread_id: String,
    pub artifact_version: i64,
    pub anchor: Option<serde_json::Value>,
    pub body: String,
    pub status: CommentStatus,
    pub answer_html: Option<String>,
    pub anchor_state: AnchorState,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

/// A comment plus its owning `project_id` (resolved via its thread). The route
/// layer needs `project_id` to scope the `comment-created` / `comment-updated`
/// Tauri events so the frontend can refetch just the affected view.
///
/// `parent_id` is carried alongside (rather than on [`Comment`]) so the response
/// layer can populate `CommentResponse.parent_id` without the shared [`Comment`]
/// struct — read verbatim by the flow layer — having to change shape.
/// `reopened_root`, when `Some`, is the ROOT comment a user reply flipped back to
/// `open` (epic conceptify-6xi): the create route emits an extra `comment-updated`
/// for it. It is always `None` for a root comment or an update.
#[derive(Debug, Clone)]
pub struct CommentContext {
    pub comment: Comment,
    pub project_id: String,
    /// The root this comment replies to, or `None` for a root comment.
    pub parent_id: Option<String>,
    /// A root re-opened as a side effect of creating this reply, if any.
    pub reopened_root: Option<Comment>,
    /// A root flipped back to `answered` as a side effect of answering the
    /// LATEST reply in its chain (epic conceptify-6xi: root status reflects the
    /// latest exchange state — once the newest message has its answer, the
    /// conversation no longer needs agent attention). The update route emits an
    /// extra `comment-updated` for it. Always `None` otherwise.
    pub answered_root: Option<Comment>,
}

/// An open ROOT comment plus its ordered reply chain — the unit the get-context
/// aggregate nests so a follow-up run inherits the full exchange history (epic
/// conceptify-6xi). `replies` is oldest-first (`created_at`, then rowid).
#[derive(Debug, Clone)]
pub struct CommentThread {
    pub root: Comment,
    pub replies: Vec<Comment>,
}

/// Errors specific to comment operations. Variants map to HTTP status codes in
/// the route handlers (see `server::comments_routes`).
#[derive(Debug, thiserror::Error)]
pub enum CommentError {
    #[error("comment body must not be empty")]
    EmptyBody,

    #[error("invalid anchor: {0}")]
    InvalidAnchor(String),

    #[error("thread not found: {0}")]
    ThreadNotFound(String),

    #[error("artifact version {version} not found for thread {thread_id}")]
    ArtifactVersionNotFound { thread_id: String, version: i64 },

    #[error("comment not found: {0}")]
    NotFound(String),

    #[error("illegal status transition: {from} -> {to}")]
    IllegalTransition {
        from: &'static str,
        to: &'static str,
    },

    #[error("no fields to update")]
    NoUpdateFields,

    // --- reply rules (epic conceptify-6xi) -------------------------------

    /// A reply carried an anchor. Replies attach to a root comment, not to a
    /// region of the artifact — they never carry their own anchor.
    #[error("a reply must not carry an anchor")]
    ReplyWithAnchor,

    /// The reply's `parent_id` names no comment.
    #[error("parent comment not found: {0}")]
    ParentNotFound(String),

    /// The reply's parent lives in a different thread than the one given.
    #[error("parent comment {parent_id} is not in thread {thread_id}")]
    ParentDifferentThread {
        parent_id: String,
        thread_id: String,
    },

    /// The reply's parent is itself a reply — chains are linear (reply to the
    /// root instead).
    #[error("cannot reply to a reply ({0} is itself a reply); reply to the root comment")]
    ReplyToReply(String),

    /// A caller tried to move a reply to `applied`. `applied` is root-only (it
    /// tracks the artifact-apply flow); replies advance `open` → `answered`.
    #[error("cannot apply a reply ({0}); the `applied` status is root-only")]
    AppliedOnReply(String),

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
}

/// Validate a submitted anchor against the FR-4.4 `Anchor` schema
/// (`conceptify_types::Anchor`). Enforces the envelope — a JSON **object** with
/// a supported integer `v` and a known `type` whose required fields are present
/// — while tolerating unknown extra fields (so the bridge can add capture hints
/// without a server change). Does **not** rewrite the value; the caller stores
/// it verbatim.
fn validate_anchor(value: &serde_json::Value) -> Result<(), CommentError> {
    let obj = value
        .as_object()
        .ok_or_else(|| CommentError::InvalidAnchor("anchor must be a JSON object".into()))?;

    match obj.get("v").and_then(serde_json::Value::as_u64) {
        Some(1) => {}
        Some(other) => {
            return Err(CommentError::InvalidAnchor(format!(
                "unsupported anchor schema version {other} (expected 1)"
            )));
        }
        None => {
            return Err(CommentError::InvalidAnchor(
                "anchor missing required integer field \"v\"".into(),
            ));
        }
    }

    serde_json::from_value::<conceptify_types::Anchor>(value.clone())
        .map_err(|e| CommentError::InvalidAnchor(e.to_string()))?;

    Ok(())
}

/// Create a comment (PRD FR-4.1/4.2/4.3). Validates the anchor (if any), checks
/// the target thread and artifact version exist, and inserts with status
/// `open` and `anchor_state` `anchored`. Returns the stored comment plus its
/// `project_id`.
///
/// Runs entirely under the caller's single connection lock, so the existence
/// checks and the insert are one atomic unit. The composite FK on
/// `(thread_id, artifact_version)` is the integrity backstop; the explicit
/// checks here are what turn a would-be opaque FK error into a clean 404.
pub fn create_comment(
    conn: &Connection,
    thread_id: &str,
    artifact_version: i64,
    anchor: Option<&serde_json::Value>,
    body: &str,
) -> Result<CommentContext, CommentError> {
    let body = body.trim();
    if body.is_empty() {
        return Err(CommentError::EmptyBody);
    }

    if let Some(anchor) = anchor {
        validate_anchor(anchor)?;
    }

    // Resolve the owning project (also serves as the thread-existence check).
    let project_id: Option<String> = conn
        .query_row(
            "SELECT project_id FROM threads WHERE id = ?1",
            [thread_id],
            |row| row.get(0),
        )
        .optional()?;
    let project_id =
        project_id.ok_or_else(|| CommentError::ThreadNotFound(thread_id.to_owned()))?;

    // A comment always anchors to an artifact version that already exists
    // (§4 referential integrity). Check explicitly for a clean 404.
    let version_exists = conn
        .query_row(
            "SELECT 1 FROM artifacts WHERE thread_id = ?1 AND version = ?2",
            rusqlite::params![thread_id, artifact_version],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !version_exists {
        return Err(CommentError::ArtifactVersionNotFound {
            thread_id: thread_id.to_owned(),
            version: artifact_version,
        });
    }

    // Store the anchor verbatim (compact JSON) so bridge-supplied extra fields
    // survive; `anchor_state` takes its column default (`anchored`).
    let anchor_text = anchor.map(|a| a.to_string());
    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO comments (id, thread_id, artifact_version, anchor, body, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            id,
            thread_id,
            artifact_version,
            anchor_text,
            body,
            CommentStatus::Open.as_str(),
        ],
    )?;

    let comment = get_comment(conn, &id)?.ok_or_else(|| CommentError::NotFound(id.clone()))?;
    Ok(CommentContext {
        comment,
        project_id,
        parent_id: None,
        reopened_root: None,
        answered_root: None,
    })
}

/// Create a threaded **reply** to a root comment (epic conceptify-6xi).
///
/// A reply is a `comments` row whose `parent_id` names the root it answers. This
/// path is deliberately separate from [`create_comment`] (which keeps its
/// anchor-carrying signature for the in-artifact comment surface): the route and
/// command layers dispatch here when a `parent_id` is supplied.
///
/// Rules enforced (all as structured [`CommentError`]s → clean 4xx):
/// - the parent must exist ([`CommentError::ParentNotFound`]),
/// - be in the same thread ([`CommentError::ParentDifferentThread`]),
/// - and itself be a root ([`CommentError::ReplyToReply`] — chains are linear).
///
/// The reply carries **no anchor** (NULL; `anchor_state` takes its `anchored`
/// default) and **inherits the parent's `artifact_version`** (the simplest
/// truthful value — the reply is part of the same conversation, pinned to where
/// that conversation is anchored). If the root is currently `answered`/`applied`,
/// creating the reply **re-opens it** (status → `open`, `resolved_at` cleared; the
/// prior `answer_html` is kept as exchange history) in the SAME transaction, and
/// the re-opened root is returned as `reopened_root` for the route to emit a
/// `comment-updated`. Replies start `open`.
///
/// Runs under the caller's single connection lock; the reply insert and the
/// root re-open commit atomically.
pub fn create_reply(
    conn: &Connection,
    thread_id: &str,
    parent_id: &str,
    body: &str,
) -> Result<CommentContext, CommentError> {
    let body = body.trim();
    if body.is_empty() {
        return Err(CommentError::EmptyBody);
    }

    // Resolve the owning project (also the thread-existence check → clean 404).
    let project_id: Option<String> = conn
        .query_row(
            "SELECT project_id FROM threads WHERE id = ?1",
            [thread_id],
            |row| row.get(0),
        )
        .optional()?;
    let project_id =
        project_id.ok_or_else(|| CommentError::ThreadNotFound(thread_id.to_owned()))?;

    // Validate the parent: exists, same thread, and is itself a root (linear
    // chains). Fetch its version (inherited) and status (re-open decision) too.
    let parent: Option<(String, Option<String>, i64, String)> = conn
        .query_row(
            "SELECT thread_id, parent_id, artifact_version, status
             FROM comments WHERE id = ?1",
            [parent_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    let (parent_thread, parent_parent, parent_version, parent_status) =
        parent.ok_or_else(|| CommentError::ParentNotFound(parent_id.to_owned()))?;

    if parent_thread != thread_id {
        return Err(CommentError::ParentDifferentThread {
            parent_id: parent_id.to_owned(),
            thread_id: thread_id.to_owned(),
        });
    }
    if parent_parent.is_some() {
        return Err(CommentError::ReplyToReply(parent_id.to_owned()));
    }

    let reopen = matches!(
        CommentStatus::from_db_str(&parent_status),
        CommentStatus::Answered | CommentStatus::Applied
    );

    let id = uuid::Uuid::new_v4().to_string();

    // Reply row: null anchor, inherited version, status `open`, `parent_id` set.
    let insert_reply = |c: &Connection| -> Result<(), CommentError> {
        c.execute(
            "INSERT INTO comments
                 (id, thread_id, artifact_version, anchor, body, status, parent_id)
             VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6)",
            rusqlite::params![
                id,
                thread_id,
                parent_version,
                body,
                CommentStatus::Open.as_str(),
                parent_id,
            ],
        )?;
        Ok(())
    };

    let reopened_root = if reopen {
        let tx = conn.unchecked_transaction()?;
        insert_reply(&tx)?;
        // Re-open the root: back to `open`, clear `resolved_at` (so the "open ⇒
        // resolved_at is NULL" invariant holds and the next answer re-stamps it).
        // Deliberately bypasses the advance-only status machine — a re-open is a
        // legitimate regression. `answer_html` is preserved as exchange history.
        tx.execute(
            "UPDATE comments SET status = 'open', resolved_at = NULL WHERE id = ?1",
            [parent_id],
        )?;
        tx.commit()?;
        get_comment(conn, parent_id)?
    } else {
        insert_reply(conn)?;
        None
    };

    let comment = get_comment(conn, &id)?.ok_or_else(|| CommentError::NotFound(id.clone()))?;
    Ok(CommentContext {
        comment,
        project_id,
        parent_id: Some(parent_id.to_owned()),
        reopened_root,
        answered_root: None,
    })
}

/// List a thread's comments (PRD FR-4.5, FR-6.4), chronological (oldest first,
/// the reading order of the sidebar), optionally filtered to one `status`. An
/// unknown `thread_id` yields an empty list rather than a 404 (mirrors
/// `list_threads`); callers list comments for a thread they already hold.
///
/// The flat `Comment` shape (no `parent_id`) has no production caller since bead
/// conceptify-6xi.2 retired the batch flow's flat open-comment list in favour of
/// exchange threads; retained as a public convenience over
/// [`list_comments_with_parent`] and still exercised by the tests below.
#[allow(dead_code)]
pub fn list_comments(
    conn: &Connection,
    thread_id: &str,
    status: Option<CommentStatus>,
) -> Result<Vec<Comment>, CommentError> {
    Ok(list_comments_with_parent(conn, thread_id, status)?
        .into_iter()
        .map(|(comment, _parent)| comment)
        .collect())
}

/// Like [`list_comments`], but pairs each comment with its `parent_id` (the root
/// it replies to, or `None` for a root). The response layer needs `parent_id` per
/// item to populate `CommentResponse.parent_id`; the internal flow layer
/// (`crate::flows`) uses the plain [`list_comments`] over `Comment`, which is why
/// `parent_id` rides alongside rather than on the shared [`Comment`] struct.
pub fn list_comments_with_parent(
    conn: &Connection,
    thread_id: &str,
    status: Option<CommentStatus>,
) -> Result<Vec<(Comment, Option<String>)>, CommentError> {
    // Two prepared statements rather than string-concatenating a WHERE clause,
    // keeping every value bound as a parameter.
    let mut stmt = conn.prepare(
        "
        SELECT id, thread_id, artifact_version, anchor, body, status,
               answer_html, anchor_state, created_at, resolved_at, parent_id
        FROM comments
        WHERE thread_id = ?1
          AND (?2 IS NULL OR status = ?2)
        ORDER BY created_at ASC, rowid ASC
        ",
    )?;

    let status_filter = status.map(|s| s.as_str());
    // `row_to_comment` reads columns 0..=9; `parent_id` is the appended column 10.
    let rows = stmt.query_map(rusqlite::params![thread_id, status_filter], |row| {
        Ok((row_to_comment(row)?, row.get::<_, Option<String>>(10)?))
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Update a comment (PRD FR-4.6/4.7). Any subset of `status`, `answer_html`,
/// `anchor_state` may be supplied; at least one is required. A `status` change
/// must be a legal advance (`open` → `answered` → `applied`); `resolved_at` is
/// stamped the first time the comment leaves `open`. Returns the updated
/// comment plus its `project_id`.
pub fn update_comment(
    conn: &Connection,
    id: &str,
    status: Option<CommentStatus>,
    answer_html: Option<&str>,
    anchor_state: Option<AnchorState>,
) -> Result<CommentContext, CommentError> {
    if status.is_none() && answer_html.is_none() && anchor_state.is_none() {
        return Err(CommentError::NoUpdateFields);
    }

    let current = get_comment(conn, id)?.ok_or_else(|| CommentError::NotFound(id.to_owned()))?;

    // Whether this comment is a reply (its own `parent_id` is non-NULL). Fetched
    // separately so the shared `Comment` struct stays shape-stable for the flow
    // layer; the row is known to exist (we just read `current`).
    let parent_id: Option<String> =
        conn.query_row("SELECT parent_id FROM comments WHERE id = ?1", [id], |row| {
            row.get(0)
        })?;

    if let Some(next) = status {
        // `applied` is root-only — it tracks the artifact-apply flow. A reply
        // advances open → answered via this same path, but never to `applied`.
        if next == CommentStatus::Applied && parent_id.is_some() {
            return Err(CommentError::AppliedOnReply(id.to_owned()));
        }
        if !current.status.can_advance_to(next) {
            return Err(CommentError::IllegalTransition {
                from: current.status.as_str(),
                to: next.as_str(),
            });
        }
    }

    let new_status = status.unwrap_or(current.status);
    let new_answer = answer_html.or(current.answer_html.as_deref());
    let new_anchor_state = anchor_state.unwrap_or(current.anchor_state);

    // Root status reflects the latest exchange state (epic conceptify-6xi):
    // when this update answers a REPLY that is the LATEST message in its chain,
    // the conversation that re-opened the root is dealt with, so the (open)
    // root flips back to `answered` in the same transaction. Its `answer_html`
    // is untouched (exchange history); `resolved_at` re-stamps via the same
    // first-resolution CASE (the re-open cleared it). An earlier reply being
    // answered does NOT flip the root — a newer open message still owes an
    // answer. Without this, a fully-answered chain would read `open` forever:
    // stale Ask-now affordances, inflated open counts, and the next batch run
    // re-targeting the root and overwriting its original answer.
    let flip_root: Option<String> = match (&parent_id, status) {
        (Some(root_id), Some(CommentStatus::Answered)) => {
            let latest_reply: Option<String> = conn
                .query_row(
                    "SELECT id FROM comments WHERE parent_id = ?1
                     ORDER BY created_at DESC, rowid DESC LIMIT 1",
                    [root_id],
                    |row| row.get(0),
                )
                .optional()?;
            let root_status: String = conn.query_row(
                "SELECT status FROM comments WHERE id = ?1",
                [root_id],
                |row| row.get(0),
            )?;
            (latest_reply.as_deref() == Some(id) && root_status == "open")
                .then(|| root_id.clone())
        }
        _ => None,
    };

    // `resolved_at` is stamped in SQL (so the DB clock owns the timestamp) the
    // first time the comment is resolved, and left stable thereafter. The
    // target update and any root flip commit atomically.
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE comments
         SET status = ?2,
             answer_html = ?3,
             anchor_state = ?4,
             resolved_at = CASE
                 WHEN ?2 IN ('answered', 'applied') AND resolved_at IS NULL
                     THEN strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 ELSE resolved_at
             END
         WHERE id = ?1",
        rusqlite::params![
            id,
            new_status.as_str(),
            new_answer,
            new_anchor_state.as_str(),
        ],
    )?;
    if let Some(root_id) = &flip_root {
        tx.execute(
            "UPDATE comments
             SET status = 'answered',
                 resolved_at = CASE
                     WHEN resolved_at IS NULL
                         THEN strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                     ELSE resolved_at
                 END
             WHERE id = ?1",
            [root_id],
        )?;
    }
    tx.commit()?;

    let answered_root = match &flip_root {
        Some(root_id) => get_comment(conn, root_id)?,
        None => None,
    };

    let comment = get_comment(conn, id)?.ok_or_else(|| CommentError::NotFound(id.to_owned()))?;
    let project_id = find_project_for_thread(conn, &comment.thread_id)?;
    Ok(CommentContext {
        comment,
        project_id,
        parent_id,
        reopened_root: None,
        answered_root,
    })
}

/// The comments that participate in FR-4.4 re-attachment when `new_version`
/// is saved: anchored to any *earlier* version, status `open` or `answered`.
/// `applied` comments are frozen history and never participate (see
/// `crate::anchoring`); previously-`moved` comments do (they can heal).
/// Oldest first for deterministic processing/event order.
///
/// **Replies never participate** (`parent_id IS NULL`, epic conceptify-6xi): a
/// reply carries no anchor and is pinned to its parent's version by inheritance,
/// so feeding it through re-attachment would only advance its `artifact_version`
/// (as a null-anchor "direct follow-up" would) and emit a spurious
/// `comment-updated` — behavior that would be wrong. Excluding them here keeps a
/// reply's inherited version stable across saves.
pub fn reattach_candidates(
    conn: &Connection,
    thread_id: &str,
    new_version: i64,
) -> Result<Vec<Comment>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "
        SELECT id, thread_id, artifact_version, anchor, body, status,
               answer_html, anchor_state, created_at, resolved_at
        FROM comments
        WHERE thread_id = ?1
          AND artifact_version < ?2
          AND status IN ('open', 'answered')
          AND parent_id IS NULL
        ORDER BY created_at ASC, rowid ASC
        ",
    )?;
    let rows = stmt.query_map(rusqlite::params![thread_id, new_version], row_to_comment)?;
    rows.collect()
}

/// Open ROOT comments (status `open`, `parent_id IS NULL`), oldest first, each
/// with its full reply chain — the get-context exchange-history aggregation (epic
/// conceptify-6xi). A root re-opened by a user reply reappears here so a follow-up
/// run re-answers it with the whole conversation in hand. Replies are included
/// regardless of their own status (the chain is history, not a work queue).
pub fn open_roots_with_replies(
    conn: &Connection,
    thread_id: &str,
) -> Result<Vec<CommentThread>, CommentError> {
    let roots: Vec<Comment> = {
        let mut stmt = conn.prepare(
            "
            SELECT id, thread_id, artifact_version, anchor, body, status,
                   answer_html, anchor_state, created_at, resolved_at
            FROM comments
            WHERE thread_id = ?1
              AND status = 'open'
              AND parent_id IS NULL
            ORDER BY created_at ASC, rowid ASC
            ",
        )?;
        let rows = stmt.query_map([thread_id], row_to_comment)?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    let mut out = Vec::with_capacity(roots.len());
    for root in roots {
        let replies = replies_for(conn, &root.id)?;
        out.push(CommentThread { root, replies });
    }
    Ok(out)
}

/// The ordered reply chain (oldest first) under one root comment.
fn replies_for(conn: &Connection, root_id: &str) -> Result<Vec<Comment>, CommentError> {
    let mut stmt = conn.prepare(
        "
        SELECT id, thread_id, artifact_version, anchor, body, status,
               answer_html, anchor_state, created_at, resolved_at
        FROM comments
        WHERE parent_id = ?1
        ORDER BY created_at ASC, rowid ASC
        ",
    )?;
    let rows = stmt.query_map([root_id], row_to_comment)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Persist one re-attachment verdict (`crate::anchoring`): move the comment to
/// `artifact_version`, optionally rewrite its anchor JSON (`None` keeps the
/// stored anchor), and set `anchor_state`. Deliberately bypasses the status
/// machine — re-attachment never touches `status`/`answer_html`/`resolved_at`.
pub fn apply_reattachment(
    conn: &Connection,
    id: &str,
    artifact_version: i64,
    anchor: Option<&serde_json::Value>,
    anchor_state: AnchorState,
) -> Result<Comment, rusqlite::Error> {
    let anchor_text = anchor.map(|a| a.to_string());
    conn.execute(
        "UPDATE comments
         SET artifact_version = ?2,
             anchor = CASE WHEN ?3 IS NULL THEN anchor ELSE ?3 END,
             anchor_state = ?4
         WHERE id = ?1",
        rusqlite::params![id, artifact_version, anchor_text, anchor_state.as_str()],
    )?;
    conn.query_row(
        "SELECT id, thread_id, artifact_version, anchor, body, status,
                answer_html, anchor_state, created_at, resolved_at
         FROM comments WHERE id = ?1",
        [id],
        row_to_comment,
    )
}

/// Fetch a single comment by id, or `None` if absent.
fn get_comment(conn: &Connection, id: &str) -> Result<Option<Comment>, CommentError> {
    conn.query_row(
        "SELECT id, thread_id, artifact_version, anchor, body, status,
                answer_html, anchor_state, created_at, resolved_at
         FROM comments WHERE id = ?1",
        [id],
        row_to_comment,
    )
    .optional()
    .map_err(Into::into)
}

/// Resolve the project owning a thread. Used only for building event payloads
/// after an update; the thread is guaranteed to exist (a comment references it
/// via FK), so an absent row is a bug, surfaced as a plain rusqlite `Database`
/// error rather than a domain 404.
fn find_project_for_thread(conn: &Connection, thread_id: &str) -> Result<String, CommentError> {
    conn.query_row(
        "SELECT project_id FROM threads WHERE id = ?1",
        [thread_id],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

/// Map a `comments` row (in the canonical column order used by every SELECT
/// above) to a `Comment`, parsing the stored anchor text back into JSON.
fn row_to_comment(row: &rusqlite::Row) -> rusqlite::Result<Comment> {
    let anchor_text: Option<String> = row.get(3)?;
    // The stored text is JSON we validated on the way in, so parsing round-trips;
    // an unexpected parse failure degrades to `None` (logged) rather than
    // failing the whole read.
    let anchor = anchor_text.and_then(|s| match serde_json::from_str(&s) {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("[conceptify] stored comment anchor is not valid JSON: {e}");
            None
        }
    });

    Ok(Comment {
        id: row.get(0)?,
        thread_id: row.get(1)?,
        artifact_version: row.get(2)?,
        anchor,
        body: row.get(4)?,
        status: CommentStatus::from_db_str(&row.get::<_, String>(5)?),
        answer_html: row.get(6)?,
        anchor_state: AnchorState::from_db_str(&row.get::<_, String>(7)?),
        created_at: row.get(8)?,
        resolved_at: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// In-memory DB with the projects/threads/artifacts/comments schema (the
    /// comments table includes the `anchor_state` column the real migration
    /// adds) plus one project, thread, and artifact v1 to hang comments off.
    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE projects (id TEXT PRIMARY KEY, name TEXT NOT NULL);
            CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                title TEXT NOT NULL
            );
            CREATE TABLE artifacts (
                id TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL,
                version INTEGER NOT NULL,
                UNIQUE (thread_id, version)
            );
            CREATE TABLE comments (
                id TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL,
                artifact_version INTEGER NOT NULL,
                anchor TEXT,
                body TEXT NOT NULL,
                status TEXT NOT NULL
                    CHECK (status IN ('open', 'answered', 'applied')),
                answer_html TEXT,
                anchor_state TEXT NOT NULL DEFAULT 'anchored'
                    CHECK (anchor_state IN ('anchored', 'moved')),
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                resolved_at TEXT,
                parent_id TEXT REFERENCES comments(id) ON DELETE CASCADE
            );
            INSERT INTO projects (id, name) VALUES ('p1', 'Proj One');
            INSERT INTO threads (id, project_id, title) VALUES ('t1', 'p1', 'Thread One');
            INSERT INTO artifacts (id, thread_id, version) VALUES ('a1', 't1', 1);
            ",
        )
        .unwrap();
        conn
    }

    fn text_anchor() -> serde_json::Value {
        json!({
            "v": 1,
            "type": "text",
            "cfy_id": "sec-walkthrough",
            "start": 142,
            "end": 210,
            "quote": {
                "exact": "the token is refreshed here",
                "prefix": "why ",
                "suffix": " on every request"
            }
        })
    }

    fn element_anchor() -> serde_json::Value {
        json!({
            "v": 1,
            "type": "element",
            "cfy_id": "fig-auth-flow.token-service",
            "quote": { "exact": "Token Service" }
        })
    }

    #[test]
    fn create_persists_primary_and_fallback_anchor_data() {
        let conn = test_conn();
        let anchor = text_anchor();
        let ctx = create_comment(&conn, "t1", 1, Some(&anchor), "I don't get this").unwrap();

        assert_eq!(ctx.project_id, "p1");
        let c = ctx.comment;
        assert!(!c.id.is_empty());
        assert_eq!(c.status, CommentStatus::Open);
        assert_eq!(c.anchor_state, AnchorState::Anchored);
        assert!(c.resolved_at.is_none());

        // Both the primary anchor (cfy_id + offsets) and the fallback quote
        // (exact + prefix + suffix) round-trip intact.
        let stored = c.anchor.expect("anchor stored");
        assert_eq!(stored, anchor);
        assert_eq!(stored["cfy_id"], "sec-walkthrough");
        assert_eq!(stored["start"], 142);
        assert_eq!(stored["end"], 210);
        assert_eq!(stored["quote"]["exact"], "the token is refreshed here");
        assert_eq!(stored["quote"]["prefix"], "why ");
        assert_eq!(stored["quote"]["suffix"], " on every request");
    }

    #[test]
    fn create_accepts_element_anchor() {
        let conn = test_conn();
        let anchor = element_anchor();
        let c = create_comment(&conn, "t1", 1, Some(&anchor), "why this node?")
            .unwrap()
            .comment;
        assert_eq!(c.anchor.unwrap(), anchor);
    }

    #[test]
    fn create_accepts_null_anchor_direct_follow_up() {
        let conn = test_conn();
        let c = create_comment(&conn, "t1", 1, None, "a direct follow-up question")
            .unwrap()
            .comment;
        assert!(c.anchor.is_none());
        assert_eq!(c.status, CommentStatus::Open);
    }

    #[test]
    fn create_rejects_empty_body() {
        let conn = test_conn();
        let err = create_comment(&conn, "t1", 1, None, "   ").unwrap_err();
        assert!(matches!(err, CommentError::EmptyBody));
    }

    #[test]
    fn create_rejects_unknown_thread() {
        let conn = test_conn();
        let err = create_comment(&conn, "ghost", 1, None, "hi").unwrap_err();
        assert!(matches!(err, CommentError::ThreadNotFound(_)));
    }

    #[test]
    fn create_rejects_missing_artifact_version() {
        let conn = test_conn();
        let err = create_comment(&conn, "t1", 99, None, "hi").unwrap_err();
        assert!(matches!(
            err,
            CommentError::ArtifactVersionNotFound { version: 99, .. }
        ));
    }

    #[test]
    fn create_rejects_malformed_anchor() {
        let conn = test_conn();

        // Not an object.
        let err = create_comment(&conn, "t1", 1, Some(&json!("nope")), "b").unwrap_err();
        assert!(matches!(err, CommentError::InvalidAnchor(_)));

        // Unknown type.
        let bad_type = json!({ "v": 1, "type": "region", "cfy_id": "x" });
        let err = create_comment(&conn, "t1", 1, Some(&bad_type), "b").unwrap_err();
        assert!(matches!(err, CommentError::InvalidAnchor(_)));

        // Unsupported schema version.
        let bad_v = json!({ "v": 2, "type": "element", "cfy_id": "x" });
        let err = create_comment(&conn, "t1", 1, Some(&bad_v), "b").unwrap_err();
        assert!(matches!(err, CommentError::InvalidAnchor(_)));

        // Missing required field (element anchor without cfy_id).
        let missing = json!({ "v": 1, "type": "element" });
        let err = create_comment(&conn, "t1", 1, Some(&missing), "b").unwrap_err();
        assert!(matches!(err, CommentError::InvalidAnchor(_)));
    }

    #[test]
    fn create_tolerates_unknown_extra_anchor_fields() {
        let conn = test_conn();
        // Forward-compatible: bridge adds a capture hint the server doesn't know.
        let anchor = json!({
            "v": 1,
            "type": "element",
            "cfy_id": "fig-x.node",
            "captured_rect": { "x": 1, "y": 2 }
        });
        let c = create_comment(&conn, "t1", 1, Some(&anchor), "b")
            .unwrap()
            .comment;
        // Stored verbatim — the extra field survives the round trip.
        assert_eq!(c.anchor.unwrap()["captured_rect"]["x"], 1);
    }

    #[test]
    fn list_filters_by_status_and_is_chronological() {
        let conn = test_conn();
        let a = create_comment(&conn, "t1", 1, None, "first")
            .unwrap()
            .comment;
        let b = create_comment(&conn, "t1", 1, None, "second")
            .unwrap()
            .comment;
        // Force distinct, ordered created_at so the sort is deterministic.
        conn.execute(
            "UPDATE comments SET created_at = '2020-01-01T00:00:00.000Z' WHERE id = ?1",
            [&a.id],
        )
        .unwrap();
        conn.execute(
            "UPDATE comments SET created_at = '2020-01-02T00:00:00.000Z' WHERE id = ?1",
            [&b.id],
        )
        .unwrap();
        // Answer the second one.
        update_comment(
            &conn,
            &b.id,
            Some(CommentStatus::Answered),
            Some("<p>ans</p>"),
            None,
        )
        .unwrap();

        // No filter → both, oldest first.
        let all = list_comments(&conn, "t1", None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, a.id);
        assert_eq!(all[1].id, b.id);

        // Filter open → only the first.
        let open = list_comments(&conn, "t1", Some(CommentStatus::Open)).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, a.id);

        // Filter answered → only the second.
        let answered = list_comments(&conn, "t1", Some(CommentStatus::Answered)).unwrap();
        assert_eq!(answered.len(), 1);
        assert_eq!(answered[0].id, b.id);
    }

    #[test]
    fn list_unknown_thread_is_empty() {
        let conn = test_conn();
        create_comment(&conn, "t1", 1, None, "x").unwrap();
        assert!(list_comments(&conn, "ghost", None).unwrap().is_empty());
    }

    #[test]
    fn update_answer_sets_status_and_resolved_at() {
        let conn = test_conn();
        let c = create_comment(&conn, "t1", 1, Some(&text_anchor()), "q")
            .unwrap()
            .comment;
        assert!(c.resolved_at.is_none());

        let ctx = update_comment(
            &conn,
            &c.id,
            Some(CommentStatus::Answered),
            Some("<p>because …</p>"),
            None,
        )
        .unwrap();
        assert_eq!(ctx.project_id, "p1");
        let updated = ctx.comment;
        assert_eq!(updated.status, CommentStatus::Answered);
        assert_eq!(updated.answer_html.as_deref(), Some("<p>because …</p>"));
        assert!(
            updated.resolved_at.is_some(),
            "resolved_at stamped on answer"
        );
    }

    #[test]
    fn update_answer_html_only_leaves_status() {
        let conn = test_conn();
        let c = create_comment(&conn, "t1", 1, None, "q").unwrap().comment;
        let updated = update_comment(&conn, &c.id, None, Some("<p>note</p>"), None)
            .unwrap()
            .comment;
        assert_eq!(updated.status, CommentStatus::Open);
        assert_eq!(updated.answer_html.as_deref(), Some("<p>note</p>"));
        // resolved_at only stamps on leaving `open`.
        assert!(updated.resolved_at.is_none());
    }

    #[test]
    fn update_allows_open_directly_to_applied() {
        let conn = test_conn();
        let c = create_comment(&conn, "t1", 1, None, "q").unwrap().comment;
        let updated = update_comment(&conn, &c.id, Some(CommentStatus::Applied), None, None)
            .unwrap()
            .comment;
        assert_eq!(updated.status, CommentStatus::Applied);
        assert!(updated.resolved_at.is_some());
    }

    #[test]
    fn update_rejects_status_regression() {
        let conn = test_conn();
        let c = create_comment(&conn, "t1", 1, None, "q").unwrap().comment;
        update_comment(&conn, &c.id, Some(CommentStatus::Applied), None, None).unwrap();

        let err =
            update_comment(&conn, &c.id, Some(CommentStatus::Answered), None, None).unwrap_err();
        match err {
            CommentError::IllegalTransition { from, to } => {
                assert_eq!(from, "applied");
                assert_eq!(to, "answered");
            }
            other => panic!("expected IllegalTransition, got {other:?}"),
        }
    }

    #[test]
    fn update_resolved_at_stable_across_second_transition() {
        let conn = test_conn();
        let c = create_comment(&conn, "t1", 1, None, "q").unwrap().comment;
        let answered = update_comment(&conn, &c.id, Some(CommentStatus::Answered), None, None)
            .unwrap()
            .comment;
        let first_resolved = answered.resolved_at.clone().unwrap();

        let applied = update_comment(&conn, &c.id, Some(CommentStatus::Applied), None, None)
            .unwrap()
            .comment;
        assert_eq!(
            applied.resolved_at.as_deref(),
            Some(first_resolved.as_str()),
            "resolved_at is the first-resolution timestamp, unchanged by later advances"
        );
    }

    #[test]
    fn update_anchor_state_independent_of_status() {
        let conn = test_conn();
        let c = create_comment(&conn, "t1", 1, Some(&text_anchor()), "q")
            .unwrap()
            .comment;
        let updated = update_comment(&conn, &c.id, None, None, Some(AnchorState::Moved))
            .unwrap()
            .comment;
        assert_eq!(updated.anchor_state, AnchorState::Moved);
        // Status and resolution are untouched by an anchor_state flip.
        assert_eq!(updated.status, CommentStatus::Open);
        assert!(updated.resolved_at.is_none());
    }

    #[test]
    fn update_unknown_comment_is_not_found() {
        let conn = test_conn();
        let err =
            update_comment(&conn, "ghost", Some(CommentStatus::Answered), None, None).unwrap_err();
        assert!(matches!(err, CommentError::NotFound(_)));
    }

    #[test]
    fn update_requires_at_least_one_field() {
        let conn = test_conn();
        let c = create_comment(&conn, "t1", 1, None, "q").unwrap().comment;
        let err = update_comment(&conn, &c.id, None, None, None).unwrap_err();
        assert!(matches!(err, CommentError::NoUpdateFields));
    }

    #[test]
    fn status_transition_rules() {
        use CommentStatus::*;
        assert!(Open.can_advance_to(Open));
        assert!(Open.can_advance_to(Answered));
        assert!(Open.can_advance_to(Applied));
        assert!(Answered.can_advance_to(Applied));
        assert!(Answered.can_advance_to(Answered));
        assert!(!Answered.can_advance_to(Open));
        assert!(!Applied.can_advance_to(Open));
        assert!(!Applied.can_advance_to(Answered));
        assert!(Applied.can_advance_to(Applied));
    }

    // -- replies (epic conceptify-6xi) ---------------------------------------

    /// Add a second thread `t2` (+ artifact v1) to the fixture DB, for the
    /// cross-thread-parent test.
    fn add_second_thread(conn: &Connection) {
        conn.execute_batch(
            "
            INSERT INTO threads (id, project_id, title) VALUES ('t2', 'p1', 'Thread Two');
            INSERT INTO artifacts (id, thread_id, version) VALUES ('a2', 't2', 1);
            ",
        )
        .unwrap();
    }

    #[test]
    fn reply_persists_with_parent_and_null_anchor() {
        let conn = test_conn();
        let root = create_comment(&conn, "t1", 1, Some(&text_anchor()), "root q")
            .unwrap()
            .comment;

        let ctx = create_reply(&conn, "t1", &root.id, "I still don't get it").unwrap();
        assert_eq!(ctx.project_id, "p1");
        assert_eq!(ctx.parent_id.as_deref(), Some(root.id.as_str()));
        assert!(ctx.reopened_root.is_none(), "open root is not re-opened");

        let reply = ctx.comment;
        assert!(reply.anchor.is_none(), "replies carry no anchor");
        assert_eq!(reply.anchor_state, AnchorState::Anchored);
        assert_eq!(reply.status, CommentStatus::Open);
        // Inherits the parent's artifact_version.
        assert_eq!(reply.artifact_version, root.artifact_version);

        // list_comments_with_parent surfaces the link.
        let listed = list_comments_with_parent(&conn, "t1", None).unwrap();
        let (_, parent) = listed
            .iter()
            .find(|(c, _)| c.id == reply.id)
            .expect("reply listed");
        assert_eq!(parent.as_deref(), Some(root.id.as_str()));
    }

    #[test]
    fn reply_to_reply_is_rejected() {
        let conn = test_conn();
        let root = create_comment(&conn, "t1", 1, None, "root").unwrap().comment;
        let reply = create_reply(&conn, "t1", &root.id, "r1").unwrap().comment;

        let err = create_reply(&conn, "t1", &reply.id, "r2").unwrap_err();
        assert!(matches!(err, CommentError::ReplyToReply(id) if id == reply.id));
    }

    #[test]
    fn reply_rejects_unknown_parent() {
        let conn = test_conn();
        let err = create_reply(&conn, "t1", "ghost", "hi").unwrap_err();
        assert!(matches!(err, CommentError::ParentNotFound(_)));
    }

    #[test]
    fn reply_rejects_cross_thread_parent() {
        let conn = test_conn();
        add_second_thread(&conn);
        let root = create_comment(&conn, "t1", 1, None, "root in t1")
            .unwrap()
            .comment;

        // A reply in t2 that names t1's root is rejected.
        let err = create_reply(&conn, "t2", &root.id, "wrong thread").unwrap_err();
        assert!(matches!(
            err,
            CommentError::ParentDifferentThread { thread_id, .. } if thread_id == "t2"
        ));
    }

    #[test]
    fn reply_rejects_empty_body() {
        let conn = test_conn();
        let root = create_comment(&conn, "t1", 1, None, "root").unwrap().comment;
        let err = create_reply(&conn, "t1", &root.id, "   ").unwrap_err();
        assert!(matches!(err, CommentError::EmptyBody));
    }

    #[test]
    fn user_reply_reopens_answered_root_and_keeps_answer() {
        let conn = test_conn();
        let root = create_comment(&conn, "t1", 1, Some(&text_anchor()), "root q")
            .unwrap()
            .comment;
        update_comment(
            &conn,
            &root.id,
            Some(CommentStatus::Answered),
            Some("<p>the prior answer</p>"),
            None,
        )
        .unwrap();

        let ctx = create_reply(&conn, "t1", &root.id, "still confused").unwrap();
        let reopened = ctx.reopened_root.expect("answered root re-opens");
        assert_eq!(reopened.id, root.id);
        assert_eq!(reopened.status, CommentStatus::Open);
        // The prior answer is preserved as exchange history…
        assert_eq!(
            reopened.answer_html.as_deref(),
            Some("<p>the prior answer</p>")
        );
        // …but resolved_at clears so the "open ⇒ resolved_at is NULL" invariant holds.
        assert!(reopened.resolved_at.is_none());

        // The reply itself is open, and the root is back in the open list.
        assert_eq!(ctx.comment.status, CommentStatus::Open);
        let open = list_comments(&conn, "t1", Some(CommentStatus::Open)).unwrap();
        assert!(open.iter().any(|c| c.id == root.id), "root is open again");
    }

    #[test]
    fn reply_reopens_applied_root() {
        let conn = test_conn();
        let root = create_comment(&conn, "t1", 1, None, "root").unwrap().comment;
        update_comment(&conn, &root.id, Some(CommentStatus::Applied), None, None).unwrap();

        let ctx = create_reply(&conn, "t1", &root.id, "one more thing").unwrap();
        let reopened = ctx.reopened_root.expect("applied root re-opens");
        assert_eq!(reopened.status, CommentStatus::Open);
    }

    #[test]
    fn resolve_answers_a_reply_row() {
        let conn = test_conn();
        let root = create_comment(&conn, "t1", 1, None, "root").unwrap().comment;
        let reply = create_reply(&conn, "t1", &root.id, "follow-up").unwrap().comment;

        let ctx = update_comment(
            &conn,
            &reply.id,
            Some(CommentStatus::Answered),
            Some("<p>reply answer</p>"),
            None,
        )
        .unwrap();
        assert_eq!(ctx.parent_id.as_deref(), Some(root.id.as_str()));
        assert_eq!(ctx.comment.status, CommentStatus::Answered);
        assert_eq!(ctx.comment.answer_html.as_deref(), Some("<p>reply answer</p>"));
        assert!(ctx.comment.resolved_at.is_some());

        // Answering the chain's only (hence latest) reply flips the open root
        // to `answered` too — root status reflects the latest exchange state.
        let flipped = ctx.answered_root.expect("root flips answered");
        assert_eq!(flipped.id, root.id);
        assert_eq!(flipped.status, CommentStatus::Answered);
        assert!(flipped.answer_html.is_none(), "root answer untouched");
        assert!(flipped.resolved_at.is_some(), "root resolved_at stamped");
    }

    /// The full conversational loop (epic conceptify-6xi): answered root →
    /// user reply re-opens it → answering that (latest) reply flips the root
    /// back to `answered`, preserving the root's original answer and
    /// re-stamping its `resolved_at` (cleared by the re-open).
    #[test]
    fn answering_latest_reply_flips_reopened_root_back_to_answered() {
        let conn = test_conn();
        let root = create_comment(&conn, "t1", 1, None, "root q").unwrap().comment;
        update_comment(
            &conn,
            &root.id,
            Some(CommentStatus::Answered),
            Some("<p>first answer</p>"),
            None,
        )
        .unwrap();

        let reply = create_reply(&conn, "t1", &root.id, "still unclear").unwrap();
        assert_eq!(
            reply.reopened_root.as_ref().map(|r| r.status),
            Some(CommentStatus::Open),
            "reply re-opens the answered root"
        );

        let ctx = update_comment(
            &conn,
            &reply.comment.id,
            Some(CommentStatus::Answered),
            Some("<p>reply answer</p>"),
            None,
        )
        .unwrap();
        let flipped = ctx.answered_root.expect("re-opened root flips back");
        assert_eq!(flipped.status, CommentStatus::Answered);
        assert_eq!(
            flipped.answer_html.as_deref(),
            Some("<p>first answer</p>"),
            "root's original answer preserved as exchange history"
        );
        assert!(flipped.resolved_at.is_some(), "resolved_at re-stamped");
    }

    /// Answering an EARLIER reply while a newer one is still open must NOT
    /// flip the root — the newest message still owes an answer.
    #[test]
    fn answering_non_latest_reply_leaves_root_open() {
        let conn = test_conn();
        let root = create_comment(&conn, "t1", 1, None, "root q").unwrap().comment;
        let r1 = create_reply(&conn, "t1", &root.id, "reply one").unwrap().comment;
        let r2 = create_reply(&conn, "t1", &root.id, "reply two").unwrap().comment;
        // Deterministic ordering (fresh rows can share a millisecond).
        for (id, ts) in [
            (&r1.id, "2020-01-01T00:00:01.000Z"),
            (&r2.id, "2020-01-01T00:00:02.000Z"),
        ] {
            conn.execute(
                "UPDATE comments SET created_at = ?2 WHERE id = ?1",
                rusqlite::params![id, ts],
            )
            .unwrap();
        }

        let ctx = update_comment(
            &conn,
            &r1.id,
            Some(CommentStatus::Answered),
            Some("<p>a1</p>"),
            None,
        )
        .unwrap();
        assert!(ctx.answered_root.is_none(), "earlier reply does not flip");
        let root_now = get_comment(&conn, &root.id).unwrap().unwrap();
        assert_eq!(root_now.status, CommentStatus::Open, "root stays open");

        // Answering the LATEST reply then does flip it.
        let ctx = update_comment(
            &conn,
            &r2.id,
            Some(CommentStatus::Answered),
            Some("<p>a2</p>"),
            None,
        )
        .unwrap();
        assert_eq!(
            ctx.answered_root.map(|r| r.status),
            Some(CommentStatus::Answered)
        );
    }

    #[test]
    fn applied_on_reply_is_rejected() {
        let conn = test_conn();
        let root = create_comment(&conn, "t1", 1, None, "root").unwrap().comment;
        let reply = create_reply(&conn, "t1", &root.id, "follow-up").unwrap().comment;

        let err =
            update_comment(&conn, &reply.id, Some(CommentStatus::Applied), None, None).unwrap_err();
        assert!(matches!(err, CommentError::AppliedOnReply(id) if id == reply.id));

        // The root can still be applied (root-only status is fine on a root).
        assert!(
            update_comment(&conn, &root.id, Some(CommentStatus::Applied), None, None).is_ok(),
            "applied is legal on a root"
        );
    }

    #[test]
    fn reattach_candidates_excludes_replies() {
        let conn = test_conn();
        // Root anchored to v1; a reply under it (inherits v1, null anchor).
        let root = create_comment(&conn, "t1", 1, Some(&text_anchor()), "root q")
            .unwrap()
            .comment;
        let reply = create_reply(&conn, "t1", &root.id, "follow-up").unwrap().comment;

        // Re-attachment for a hypothetical v2: only the root participates.
        let candidates = reattach_candidates(&conn, "t1", 2).unwrap();
        let ids: Vec<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&root.id.as_str()), "root participates");
        assert!(
            !ids.contains(&reply.id.as_str()),
            "reply is excluded from re-attachment"
        );
    }

    #[test]
    fn open_roots_with_replies_nests_ordered_chains() {
        let conn = test_conn();
        // An open root with two replies, plus a second open root with none.
        let root = create_comment(&conn, "t1", 1, None, "root q").unwrap().comment;
        let r1 = create_reply(&conn, "t1", &root.id, "reply one")
            .unwrap()
            .comment;
        let r2 = create_reply(&conn, "t1", &root.id, "reply two")
            .unwrap()
            .comment;
        let lone = create_comment(&conn, "t1", 1, None, "lone root")
            .unwrap()
            .comment;

        // Deterministic created_at ordering (fresh rows can share a millisecond).
        for (id, ts) in [
            (&root.id, "2020-01-01T00:00:00.000Z"),
            (&r1.id, "2020-01-01T00:00:01.000Z"),
            (&r2.id, "2020-01-01T00:00:02.000Z"),
            (&lone.id, "2020-01-01T00:00:03.000Z"),
        ] {
            conn.execute(
                "UPDATE comments SET created_at = ?2 WHERE id = ?1",
                rusqlite::params![id, ts],
            )
            .unwrap();
        }

        let threads = open_roots_with_replies(&conn, "t1").unwrap();
        assert_eq!(threads.len(), 2, "two open roots");
        assert_eq!(threads[0].root.id, root.id);
        let chain: Vec<&str> = threads[0].replies.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(chain, vec![r1.id.as_str(), r2.id.as_str()], "chain ordered");
        assert_eq!(threads[1].root.id, lone.id);
        assert!(threads[1].replies.is_empty(), "lone root has no replies");
    }

    #[test]
    fn open_roots_with_replies_excludes_answered_roots() {
        let conn = test_conn();
        let answered = create_comment(&conn, "t1", 1, None, "answered root")
            .unwrap()
            .comment;
        update_comment(
            &conn,
            &answered.id,
            Some(CommentStatus::Answered),
            Some("<p>done</p>"),
            None,
        )
        .unwrap();

        // An answered root (no reply) is not an open question → excluded.
        assert!(open_roots_with_replies(&conn, "t1").unwrap().is_empty());
    }
}
