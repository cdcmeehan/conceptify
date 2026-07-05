# Conceptify local HTTP API

Reference for the local HTTP API embedded in the Conceptify app (PRD §5.1,
§7.8). This is the one source of truth for endpoint shapes shared by the
`conceptify` CLI, the Claude Code skill, and the app's own frontend — extend
this doc alongside any route you add.

## Transport & discovery

- The server binds `127.0.0.1:4477` by default. It never listens on any
  non-loopback interface.
- If `4477` is taken, it probes the occupant's `GET /health`. If the
  occupant identifies itself as Conceptify (`"service":"conceptify"` in the
  JSON body), this process defers to it and does not start its own server
  (this should only happen if the single-instance guard is bypassed).
  Otherwise it walks `4478..=4487` looking for a free port.
- The actually-bound port is written to
  `~/Library/Application Support/conceptify/port` (plain text, just the
  port number) so the CLI and other local tools can discover it without
  guessing.

## Auth (PRD §9 S1)

- A random 32-byte token is generated on first run and persisted to
  `~/Library/Application Support/conceptify/token`, mode `0600`. It is
  reused across restarts.
- Every route **except `GET /health`** (at either path below) requires:

  ```
  Authorization: Bearer <token>
  ```

  Missing header, wrong scheme, or wrong token → `401 Unauthorized`.
- This is containment against other local processes / browser-page
  localhost probing, not adversarial hardening — the threat model is a
  single-user machine (PRD §9).

## Versioning

All endpoints beyond health live under `/api/v1/`. This path is versioned
from day one; breaking changes get a new `/api/v2/` rather than mutating
`/api/v1/` in place.

## Events

Mutating endpoints emit Tauri events (via the shared `AppHandle`) so the
webview updates live instead of polling. The frontend subscribes with
`@tauri-apps/api/event`'s `listen()`. Event names so far:

| Event | Emitted by | Payload |
|---|---|---|
| `api-ping` | `GET /api/v1/ping` (demo/health-check route) | `{ message: string, unix_ms: number }` |
| `projects-changed` | `POST /api/v1/projects/ensure`, `PATCH /api/v1/projects/:id`, `PUT /api/v1/projects/:id/archive` | `null` (no payload) |
| `thread-created` | `POST /api/v1/threads` | `{ project_id: string, thread_id: string }` |
| `artifact-updated` | `POST /api/v1/threads/:thread_id/artifact` | `{ project_id: string, thread_id: string, version: number }` |
| `comment-created` | `POST /api/v1/comments` | `{ project_id: string, thread_id: string, comment_id: string }` |
| `comment-updated` | `PATCH /api/v1/comments/:id`; `POST /api/v1/threads/:thread_id/artifact` (one per re-attached comment) | `{ project_id: string, thread_id: string, comment_id: string, status: string }` |
| `navigate` | `POST /api/v1/open` | `{ project_id: string, thread_id: string \| null }` |
| `run-progress` | agent-run engine (`runs` module, not an HTTP route) | `{ run_id: string, thread_id: string, kind: string, detail: string }` |
| `run-finished` | agent-run engine (`runs` module, not an HTTP route) | `{ run_id: string, thread_id: string, status: string }` |
| `thread-updated` | follow-up flow layer (`flows` module, not an HTTP route): thread status changes owned by the run lifecycle (apply-mode `updating` ↔ `ready`, PRD §4) | `{ project_id: string, thread_id: string, status: string }` |

`artifact-updated` is the viewer's live-refresh trigger (PRD N2: save →
visible refresh < 500ms): the frontend reloads the artifact iframe for
`thread_id` at `version` when it arrives.

`comment-updated` is the live-sidebar trigger for every mutation to a comment —
status transitions (the M5 `resolve-comment` flow), landing `answer_html`, and
the `anchor_state` "reference moved" flip. Besides `PATCH /comments/:id`, the
save-artifact endpoint emits it once per comment its re-attachment pass
changed (advanced to the new version, anchor repaired, and/or flagged
`moved`). It generalizes what PRD §5.1 sketched
as `comment-resolved`: one event covers all comment mutations (consistent with
`artifact-updated`), and the frontend scopes its refetch by the `project_id` /
`thread_id` in the payload.

### Run events (headless agent runs)

`run-progress` / `run-finished` come from the headless agent-run engine
(`src-tauri/src/runs.rs`, PRD §5.1 agent spawner / FR-4.8), not from an HTTP
endpoint — they fire while a background `follow_up_runs` row is in flight.

- `run-progress` — one per agent **stdout** line (the `claude` adapter emits
  `--output-format stream-json`, one JSON object per line). `kind` is the
  line's raw `type` field (`"output"` for non-JSON lines); `detail` is the
  line's `subtype` when present, else the raw line truncated to 200 chars.
  Deliberately shallow parsing — richer rendering is a frontend concern.
  stderr lines are not forwarded (they land in the run log only).
- `run-finished` — exactly once per run, emitted **after** the DB row reached
  its terminal state. `status` is one of `completed` (exit 0), `failed`
  (nonzero exit / spawn failure / abnormal supervision end), `cancelled`
  (user cancel), `timeout` (the FR-5.3 timeout killed the process group).
  Treat `failed` and `timeout` uniformly as the FR-5.3 error class.

The full transcript of every run is retained at
`~/Documents/conceptify/artifacts/<project-id>/threads/<slug>/runs/<run-id>.log`
(§5.6), with `[out]`/`[err]` stream tags and `[run]` lifecycle markers.
Cancellation is exposed to the frontend as the `cancel_run` Tauri command
(`invoke('cancel_run', { run_id })`), which SIGKILLs the run's whole process
group. One run per thread at a time (FR-4.9): starting a second returns a
structured error naming the live run.

### Flow commands (Tauri, not HTTP — FR-4.6/4.7/4.8)

The follow-up flows live in `src-tauri/src/flows.rs` on top of the run engine
and are invoked by the app shell as Tauri commands (snake_case args):

- `ask_follow_ups { thread_id }` → `{ run_id, thread_id, mode: "answer",
  target_comment_ids }` — FR-4.6: ONE headless run answers every **open**
  comment individually via `conceptify resolve-comment`; the artifact is
  never modified in this mode. Errors (user-facing strings): no artifact, no
  open comments, run already active (FR-4.9).
- `apply_to_artifact { thread_id, comment_ids }` → same shape with
  `mode: "apply"` — FR-4.7: `comment_ids` empty targets every **answered**
  comment; explicit ids may be `open` or `answered` (never `applied`). The
  run edits a working copy, marks each target `applied`, then publishes ONE
  new version via `conceptify save-artifact`.
- `get_active_run { thread_id }` → the live run summary
  (`{ run_id, thread_id, mode, status: "running" }`) or `null` — the UI
  re-attaches to an in-flight run on thread switch. Target ids are not
  persisted, so a re-attached run renders indeterminate progress.
- `get_run_log_tail { run_id, max_lines? }` →
  `{ run_id, log_path, lines }` (default last 30 lines) — FR-4.8 failure
  surfacing; `log_path` is always returned even when the file is unreadable.

**Apply ordering (FR-4.7 × FR-4.4):** the apply prompt instructs the agent to
finish all edits, then run `resolve-comment --applied` for **every** target
comment, then `save-artifact` exactly once, last. `applied` comments are
frozen at their capture version and excluded from save-time re-attachment, so
this order keeps re-anchoring away from the very text the apply rewrote. The
ordering is prompt-enforced (marking comments applied server-side before the
run could succeed would lie on failure); an agent that saves first anyway
degrades gracefully — re-attachment just processes the not-yet-applied
targets (noisier, never corrupting).

**Thread status:** apply runs set the thread to `updating` on start and
conditionally restore `ready` when the run terminates (a `ready` already set
by the agent's mid-run `save-artifact` is never regressed); each change emits
`thread-updated`. Answer runs never touch thread status, and **no follow-up
run ever sets thread status `error`** — that state belongs to generation runs
(FR-5.3); follow-up failures are surfaced by the run-status UI instead.

**Child environment:** the flow layer resolves the `conceptify` CLI
(`CONCEPTIFY_CLI` env override → binary sibling of the app executable →
login-shell `which`, cached) and prepends its directory to the spawned
agent's `PATH` — Finder-launched GUI apps inherit a minimal `PATH` (PRD
§5.1), and the prompts' contract is that plain `conceptify` works.

**Permission scoping (PRD §12 OQ3, decided by b12.8):** the default `claude`
adapter template (`src-tauri/src/settings.rs`, `default_adapters()`) runs
headless as `--permission-mode acceptEdits` **plus** `--allowedTools Bash
Edit Write` — required, because in `-p` (print) mode `acceptEdits` alone
auto-approves only a safelist of read-only Bash commands, and everything the
flows depend on (`conceptify …`, `mktemp`, `d2`/`dot`/`node` renders,
temp-dir writes outside the cwd) would be denied headlessly. Subtracted from
that via `--disallowedTools` (denies always beat allows):

- `WebFetch` / `WebSearch` — flows are grounded in local code + the artifact;
  web tools are unneeded accident/injection surface.
- Mutating git (`commit`, `push`, `add`, `rebase`, `merge`, `reset`,
  `checkout`, `switch`, `restore`, `stash`, `clean`) — no flow touches the
  repo's history or working tree; git *reads* (`log`/`diff`/`blame`/`grep`)
  stay available for grounding.
- `Edit(/{project_root}/**)` + `Write(/{project_root}/**)` — the target repo
  is **read-only in every mode**: answer runs write only sidebar answer
  files, apply/ask runs edit a temp working copy, and artifact-dir writes go
  through the CLI/server, never the agent's file tools.

`--strict-mcp-config` keeps user-configured MCP servers out of headless runs.
This is §9 right-sized containment (hygiene against accidents, not
adversarial sandboxing): rejected alternatives — `bypassPermissions` (no
containment), a fine-grained Bash whitelist (prefix rules break on pipes/env
assignments and every headless denial is a wasted flail), `--tools`
whitelisting (brittle enumeration of built-ins), per-purpose arg overrides
(a new adapter dimension for a prompt-forbidden, versioning-recoverable
accident), and `sandbox-exec`/network firewalls (adversarial-grade, out of
scope). Future adapters (Codex, …) should follow the same principle: repo
read-only, temp dirs writable, arbitrary local commands allowed, web and
repo-mutating commands denied. All behavior verified against claude CLI
2.1.201 with headless probes; both prompts tell the agent its toolset is
scoped so it does not flail against denials.

Read-only routes (`GET /api/v1/debug/db-check`, `GET /api/v1/projects`,
`GET /api/v1/threads`, `GET /api/v1/threads/:id/context`, `GET /api/v1/comments`)
emit no events.

## Endpoints

### `GET /health`

Unauthenticated. Also mirrored at `GET /api/v1/health`. Used by:

- the CLI's launch-and-wait contract (§5.2): probe → `open -a Conceptify` on
  failure → poll until healthy;
- this server's own occupant-probe when a port is already taken.

Response `200 OK`:

```json
{
  "service": "conceptify",
  "status": "ok",
  "version": "0.1.0"
}
```

### `GET /api/v1/ping`

Authenticated. Demo/smoke-test route: confirms the bearer token works and
that an axum handler can emit a Tauri event received by the webview.

Response `200 OK`:

```json
{ "pong": true }
```

Side effect: emits an `api-ping` event (see Events above).

Errors: `401 Unauthorized` if the bearer token is missing or wrong.

### `GET /api/v1/debug/db-check`

Authenticated. Demo/smoke-test route (PRD §5.1, §4): confirms the SQLite
connection held in Tauri managed state (`db::DbHandle`) is also reachable
from an axum handler, alongside the equivalent `db_check` `#[tauri::command]`
used from the frontend. Runs `SELECT count(*) FROM projects` through
`db::with_conn` (off the async runtime's worker thread — see `src-tauri/src/db/mod.rs`).

Response `200 OK`:

```json
{ "ok": true, "project_count": 0 }
```

Response `500 Internal Server Error` (query failed):

```json
{ "ok": false, "error": "..." }
```

Errors: `401 Unauthorized` if the bearer token is missing or wrong.

---

## Projects

### `POST /api/v1/projects/ensure`

Authenticated. Ensure-project by directory (PRD FR-1.1): given a root path,
canonicalize it and return the existing project or create one, defaulting name
to the directory name (deduped with numeric suffix if taken). Symlinks and
trailing slashes resolve to one identity via canonicalization.

Request body:

```json
{
  "root_path": "/Users/chris/code/myrepo",
  "name": "Optional Name Override"
}
```

The `name` field is optional. If omitted, defaults to the directory name.

Response `200 OK`:

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "name": "myrepo",
  "root_path": "/Users/chris/code/myrepo",
  "created_at": "2026-07-03T12:34:56.789Z",
  "archived": false,
  "created": true
}
```

`created` is `true` if this call created a new project, `false` if it already
existed.

Response `400 Bad Request` (path does not exist):

```json
{ "error": "path not found: /nonexistent/path" }
```

Side effect: emits `projects-changed` event if a new project was created.

Errors: `401 Unauthorized` if bearer token missing/wrong; `400` if path
doesn't exist or can't be canonicalized; `500` on database error.

---

### `GET /api/v1/projects`

Authenticated. List all projects with thread counts and last activity. Excludes
archived by default; pass `?archived=true` to include them.

Query parameters:
- `archived` (optional, boolean, default `false`): include archived projects.

Response `200 OK`:

```json
{
  "projects": [
    {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "name": "myrepo",
      "root_path": "/Users/chris/code/myrepo",
      "created_at": "2026-07-03T12:34:56.789Z",
      "archived": false,
      "thread_count": 5,
      "last_activity": "2026-07-03T15:20:10.456Z"
    }
  ]
}
```

`last_activity` is `max(threads.updated_at)` or `project.created_at` if the
project has no threads yet.

Errors: `401 Unauthorized` if bearer token missing/wrong; `500` on database
error.

---

### `PATCH /api/v1/projects/:id`

Authenticated. Rename a project.

Request body:

```json
{ "name": "New Project Name" }
```

Response `200 OK`:

```json
{ "ok": true }
```

Response `404 Not Found` (unknown project id):

```json
{ "error": "project not found: <id>" }
```

Side effect: emits `projects-changed` event.

Errors: `401 Unauthorized` if bearer token missing/wrong; `404` if project
not found; `500` on database error.

---

### `PUT /api/v1/projects/:id/archive`

Authenticated. Archive or unarchive a project. Archived projects are hidden
from the default list (they remain in the database; deletion is not
implemented).

Request body:

```json
{ "archived": true }
```

Set `archived: false` to unarchive.

Response `200 OK`:

```json
{ "ok": true }
```

Response `404 Not Found` (unknown project id):

```json
{ "error": "project not found: <id>" }
```

Side effect: emits `projects-changed` event.

Errors: `401 Unauthorized` if bearer token missing/wrong; `404` if project
not found; `500` on database error.

---

## Threads

### `POST /api/v1/threads`

Authenticated. Create a thread (PRD FR-2.1). A filesystem-safe `slug` for the
artifact folder (§5.6) is derived server-side from `title` and deduped to be
unique within the project (`slug`, `slug-2`, `slug-3`, ...); the caller never
supplies it. New threads start in status `generating` (OQ4: create early;
`error`/retry transitions land in later milestones).

Request body:

```json
{
  "project_id": "550e8400-e29b-41d4-a716-446655440000",
  "title": "How does OAuth work?",
  "initial_question": "Explain the OAuth 2.0 authorization code flow."
}
```

Response `200 OK`:

```json
{
  "id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "project_id": "550e8400-e29b-41d4-a716-446655440000",
  "title": "How does OAuth work?",
  "slug": "how-does-oauth-work",
  "initial_question": "Explain the OAuth 2.0 authorization code flow.",
  "status": "generating",
  "created_at": "2026-07-04T12:34:56.789Z",
  "updated_at": "2026-07-04T12:34:56.789Z"
}
```

Response `400 Bad Request` (empty/whitespace-only title):

```json
{ "error": "title must not be empty" }
```

Response `404 Not Found` (unknown `project_id`):

```json
{ "error": "project not found: <id>" }
```

Side effect: emits `thread-created` (see Events above).

Errors: `401 Unauthorized` if bearer token missing/wrong; `400` on empty
title; `404` if the project doesn't exist; `500` on database error.

---

### `GET /api/v1/threads`

Authenticated. List a project's threads (PRD FR-2.2), sorted by last activity
(`updated_at`, most recent first), each with its status and open-comment count.

Query parameters:
- `project_id` (required): the project whose threads to list.

Response `200 OK`:

```json
{
  "threads": [
    {
      "id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
      "project_id": "550e8400-e29b-41d4-a716-446655440000",
      "title": "How does OAuth work?",
      "slug": "how-does-oauth-work",
      "initial_question": "Explain the OAuth 2.0 authorization code flow.",
      "status": "generating",
      "created_at": "2026-07-04T12:34:56.789Z",
      "updated_at": "2026-07-04T12:34:56.789Z",
      "open_comment_count": 0
    }
  ]
}
```

`open_comment_count` counts comments still in the `open` state (a real
`LEFT JOIN` on `comments`; 0 until the comments backend starts inserting rows).
An unknown `project_id` returns an empty `threads` array rather than a 404.

Errors: `401 Unauthorized` if bearer token missing/wrong; `500` on database
error.

---

### `GET /api/v1/threads/:id/context`

Authenticated. **get-context** (PRD §5.2, §5.5): the one-round-trip aggregate a
headless follow-up run needs to answer a thread's open comments without touching
the DB directly — the thread, its owning project, the latest artifact on disk,
and the open comments (with anchors). The `conceptify get-context` CLI wraps
this; internal server-side prompt assembly (the headless spawner) reuses the
same aggregation.

Path parameter:
- `id` (required): the thread to assemble context for.

Response `200 OK`:

```json
{
  "thread": {
    "id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
    "title": "How does OAuth work?",
    "initial_question": "Explain the OAuth 2.0 authorization code flow.",
    "status": "ready",
    "slug": "how-does-oauth-work"
  },
  "project": {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "name": "myrepo",
    "root_path": "/Users/chris/code/myrepo"
  },
  "latest_artifact": {
    "version": 2,
    "file_path": "/Users/chris/Documents/conceptify/artifacts/550e8400-…/threads/how-does-oauth-work/artifact.v2.html"
  },
  "open_comments": [
    {
      "id": "b3f1…",
      "thread_id": "7c9e6679-…",
      "artifact_version": 1,
      "anchor": { "v": 1, "type": "text", "cfy_id": "sec-walkthrough", "start": 142, "end": 210, "quote": { "exact": "the token is refreshed here" } },
      "body": "why is the token refreshed here?",
      "status": "open",
      "answer_html": null,
      "anchor_state": "anchored",
      "created_at": "2026-07-04T12:34:56.789Z",
      "resolved_at": null
    }
  ]
}
```

- `latest_artifact` is the highest artifact version on disk (`file_path` is the
  absolute path of that immutable `artifact.vN.html`), or **`null`** when the
  thread has no artifact yet (still `generating`).
- `open_comments` lists only comments still in the `open` state, oldest first —
  the questions the run must answer. Each is the full comment shape (identical to
  `GET /api/v1/comments`), so its `anchor` is carried **verbatim** (the stored
  snake_case contract — see [The anchor model](#the-anchor-model-fr-44)).

Response `404 Not Found` (unknown thread id):

```json
{ "error": "thread not found: <id>" }
```

Errors: `401 Unauthorized` if bearer token missing/wrong; `404` if the thread
doesn't exist; `500` on database error.

---

## Artifacts

### `POST /api/v1/threads/:thread_id/artifact`

Authenticated. **save-artifact** (PRD FR-3.6, §5.6): validate the submitted
HTML file and store it as the thread's next artifact version (v1, v2, …; prior
versions retained). This endpoint owns the thread's `→ ready` status
transition.

**Request body: the raw artifact HTML bytes** — not JSON, not multipart. Send
`Content-Type: text/html` (not enforced). Rationale: a JSON wrapper cannot
carry invalid UTF-8 (the validator's `E-UTF8` rule would be unreachable) and
needlessly escapes multi-megabyte payloads; the CLI just streams the file it
was given. Bodies over 60 MiB are rejected at the transport layer with a bare
`413`; the spec's 50 MiB cap (`E-SIZE-MAX`) applies below that with a
structured error.

```
POST /api/v1/threads/7c9e6679-7425-40de-944b-e07fc1f90ae7/artifact
Authorization: Bearer <token>
Content-Type: text/html

<!doctype html> ...
```

**Validation** runs the rule set from
[docs/artifact-spec.md](artifact-spec.md) §8 — that doc is the contract; rule
IDs (`E-*` hard failures, `W-*` warnings) are stable identifiers and are not
restated here. Hard failures reject the save (nothing is stored, no version
consumed); warnings are returned in the success response for the CLI to print
to stderr. `W-VERSION-MISMATCH` is checked against the server-assigned
version, which is authoritative over the file's `cfy:version` meta.

**Versioning & storage** (§5.6): the file is stored as
`~/Documents/conceptify/artifacts/<project-id>/threads/<thread-slug>/artifact.vN.html`,
and `artifact.html` in the same directory is atomically replaced with a copy
of the new version (temp + rename, never a symlink — PRD N4; a `runs/`
directory is also created, reserved for headless-agent transcripts). All
writes are temp + rename; a crash mid-save never leaves a partial file visible
or a DB row pointing at a missing file.

**`created_by` is inferred, never caller-supplied**: version 1 → `initial`,
version ≥ 2 → `follow_up`.

Response `200 OK` (stored — possibly with warnings):

```json
{
  "thread_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "project_id": "550e8400-e29b-41d4-a716-446655440000",
  "version": 2,
  "created_by": "follow_up",
  "file_path": "/Users/chris/Documents/conceptify/artifacts/550e8400-…/threads/how-does-oauth-work/artifact.v2.html",
  "warnings": [
    { "code": "W-ANCHOR-DIAGRAM", "message": "svg \"fig-map\" has thin anchor coverage: 8 shape elements but only 1 data-cfy-id bearers (need ≥ 3)" }
  ]
}
```

Response `422 Unprocessable Entity` (validation hard failure — nothing
stored). `error`/`code` carry the first violated rule (the shape promised by
artifact-spec.md §8); `errors` lists every hard failure found:

```json
{
  "error": "script src \"https://evil.example.com/x.js\" is not on the Tier-2 CDN allowlist",
  "code": "E-EXTERNAL-CODE",
  "errors": [
    { "code": "E-EXTERNAL-CODE", "message": "script src \"https://evil.example.com/x.js\" is not on the Tier-2 CDN allowlist" }
  ]
}
```

Response `404 Not Found` (unknown `thread_id`):

```json
{ "error": "thread not found: <id>" }
```

Side effects: thread status → `ready` (and `updated_at` bumped), emits
`artifact-updated` (see Events above). For follow-up saves (version ≥ 2) the
FR-4.4 **re-attachment pass** runs in the same transaction (see
[Re-attachment across versions](#re-attachment-across-versions-fr-44)): earlier-version
comments are migrated onto the new version or flagged `moved`, and one
`comment-updated` event is emitted per comment whose row changed.

Errors: `401 Unauthorized` if bearer token missing/wrong; `404` unknown
thread; `413` body over the 60 MiB transport cap; `422` validation hard
failure; `500` on database or disk error.

---

## Comments

Comments are the annotation layer (PRD §7.4, FR-4.1–FR-4.5/4.7): a user note
anchored to a region of a specific artifact version, optionally carrying an
agent resolution. A comment with a `null` anchor is a **direct follow-up
question** (FR-4.3) — it flows through the identical machinery.

### The anchor model (FR-4.4)

The `anchor` is JSON stored on the comment. It is the load-bearing contract
shared by the in-artifact bridge (which captures it), the re-attachment logic
(which re-locates it after edits), and the headless follow-up agents (which read
it via `get-context`). The canonical Rust definition lives in
`crates/conceptify-types` (`Anchor`, `TextAnchor`, `ElementAnchor`, `TextQuote`).

Design invariants:

- **Versioned.** Every anchor object carries an integer `v` (currently `1`).
  The server rejects an unsupported `v`. A breaking schema change bumps `v`.
- **Discriminated.** A `type` field selects the variant. Two exist today:
  `text` (a text selection, FR-4.1) and `element` (a whole `data-cfy-id`-bearing
  element, FR-4.2). New types are additive.
- **Extensible.** The server validates the envelope (`v`, `type`, required
  fields) but stores the object **verbatim** and tolerates unknown extra fields,
  so the bridge can add capture hints without a server change.
- **Field naming is `snake_case`**, matching the rest of the API.
- **`null` anchor** (the field absent/`null`) = a direct follow-up question.

Every anchor pairs a **primary** anchor (fast, exact) with a **fallback**
text-quote (W3C Web Annotation style) used to re-attach the comment after the
artifact is edited when the primary no longer resolves.

**`type: "text"`** (text selection). Primary = the nearest ancestor
`data-cfy-id` (`cfy_id`) plus `start`/`end` character offsets within that
element's normalized text content (precise definition: "visible text" under
[Bridge protocol → Conventions](#conventions)); fallback = `quote`. When the
selection has no id-bearing ancestor, `cfy_id`/`start`/`end` are omitted and
`quote` is the sole anchor.

```json
{
  "v": 1,
  "type": "text",
  "cfy_id": "sec-walkthrough",
  "start": 142,
  "end": 210,
  "quote": {
    "exact": "the token is refreshed here",
    "prefix": "I don't get why ",
    "suffix": " on every request"
  }
}
```

**`type: "element"`** (diagram node / heading click). Primary = the clicked
element's `cfy_id`; optional `quote` (the element's text) is a fallback for
re-attachment if the id later disappears — omitted for purely graphical nodes
with no text.

```json
{
  "v": 1,
  "type": "element",
  "cfy_id": "fig-auth-flow.token-service",
  "quote": { "exact": "Token Service" }
}
```

`quote` is a text-quote selector: required `exact`, plus optional `prefix` /
`suffix` context (absent at document edges) to disambiguate a repeated `exact`.

The `data-cfy-id` values referenced here are the artifact's anchorability API —
see [docs/artifact-spec.md](artifact-spec.md) §4 for the id grammar and the
cross-version stability contract that makes re-attachment work.

### Re-attachment across versions (FR-4.4)

Anchors must survive artifact updates. Whenever a **new artifact version is
saved** (`POST /threads/:thread_id/artifact`, version ≥ 2), the server runs a
re-attachment pass over the thread's comments, inside the same transaction as
the version insert (implementation: `src-tauri/src/anchoring.rs`). This is
what makes the M5 apply-to-artifact loop safe: applying one clarification can
never invisibly orphan the other comments.

**Which comments participate.** Every comment with `artifact_version` older
than the new version and status `open` or `answered` — including comments
currently flagged `moved` (they are retried on every save and heal when the
content returns). `applied` comments are **frozen history**: the apply that
resolved them typically rewrote the very text they anchored, so re-flagging
them "moved" would be noise; they keep their original `artifact_version`
(and version tag in the sidebar) forever. **Null-anchor comments** (direct
follow-ups) are version-agnostic and advance to the new version trivially.

**Per-comment verdict** (the measurement conventions are exactly the bridge's
— see [Conventions](#conventions); the server measures identically):

1. **Primary intact** — the anchor's `data-cfy-id` resolves in the new
   document (first match in document order) and, for text anchors, the
   stored `start`/`end` still select visible text equal to `quote.exact`:
   the anchor is kept **verbatim**.
2. **Quote fallback** (text anchors) — a W3C-style search for `quote.exact`
   over the body's visible text, scored by exact `prefix`/`suffix` context
   match, scoped to the original `cfy_id` element first, then document-wide.
   A **unique best** match re-points the primary fields: `start`/`end` are
   rewritten to the new position and `cfy_id` to the deepest id-bearing
   element containing the match (dropped to a quote-only anchor when none
   contains it). A tie is **ambiguous → `moved`** (unlike the bridge's
   best-effort highlighting, a persisted decision never guesses). The
   `quote` itself is never rewritten — it is the captured selection and
   remains the durable fallback. All other anchor fields (capture hints)
   are preserved verbatim.
3. **Quote fallback** (element anchors) — when the `cfy_id` vanished, the
   id-bearing element whose whitespace-collapsed text equals `quote.exact`
   is the new target (`cfy_id` rewritten). Multiple matches are ambiguous →
   `moved`, unless they form a strict nesting chain (innermost wins).
4. **Failure → `anchor_state: "moved"`** — the comment is flagged "reference
   moved", **never dropped**. Its `artifact_version` deliberately stays at
   the last version where the anchor resolved, so the (version, anchor) pair
   remains truthful: switching the viewer to that version still highlights
   it, and `get-context` still surfaces it to agents.

**On success** the comment's `artifact_version` advances to the new version
(and a prior `moved` flag clears) — this is what makes comments follow the
latest artifact automatically: the sidebar/highlight layer decorates comments
whose `artifact_version` equals the viewed version, so migrated comments
light up on the new version with no frontend special-casing.

One `comment-updated` event fires per comment whose row changed; an untouched
row (e.g. still-unresolvable and already `moved`) emits nothing.

### Status machine

A comment's `status` is one of `open` | `answered` | `applied`, and may only
**advance** along that order (or stay put) — a regression is rejected `409`:

```
open ──→ answered ──→ applied
  └──────────────────────┘   (open → applied directly is allowed)
```

- `open → answered`: the **Ask follow-ups** flow (FR-4.6) — an agent answers the
  comment in the sidebar (`answer_html` lands).
- `answered → applied` / `open → applied`: the **Apply to artifact** flow
  (FR-4.7) resolved-with-update. `open → applied` directly supports the M5
  `resolve-comment --applied` one-shot (apply without a separate answer step).

`resolved_at` is stamped the first time a comment leaves `open` and is stable
thereafter.

Separately, `anchor_state` is `anchored` | `moved`. `moved` is FR-4.4's
"reference moved" flag, set by the re-attachment pass (below) when a comment's
anchor can't be re-located in a newly saved artifact version — the comment is
flagged, never silently dropped. It is independent of the status machine, and
it can flip back to `anchored` when a later version restores the content.

### `POST /api/v1/comments`

Authenticated. Create a comment (FR-4.1/4.2/4.3). The target thread and the
`artifact_version` must exist (a comment always anchors to an artifact version
that already exists). New comments start `open` / `anchored`.

Request body (`anchor` is `null`/omitted for a direct follow-up):

```json
{
  "thread_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "artifact_version": 1,
  "anchor": {
    "v": 1,
    "type": "text",
    "cfy_id": "sec-walkthrough",
    "start": 142,
    "end": 210,
    "quote": { "exact": "the token is refreshed here", "prefix": "why ", "suffix": " each time" }
  },
  "body": "I don't get why the token is refreshed here."
}
```

Response `200 OK` (the created comment):

```json
{
  "id": "b3f1…",
  "thread_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "artifact_version": 1,
  "anchor": { "v": 1, "type": "text", "cfy_id": "sec-walkthrough", "start": 142, "end": 210, "quote": { "exact": "the token is refreshed here", "prefix": "why ", "suffix": " each time" } },
  "body": "I don't get why the token is refreshed here.",
  "status": "open",
  "answer_html": null,
  "anchor_state": "anchored",
  "created_at": "2026-07-04T12:34:56.789Z",
  "resolved_at": null
}
```

Response `400 Bad Request`: empty body, or a malformed anchor (not an object,
unsupported `v`, unknown/missing `type`, or a missing required field):

```json
{ "error": "invalid anchor: unsupported anchor schema version 2 (expected 1)" }
```

Response `404 Not Found`: unknown `thread_id`, or an `artifact_version` that
doesn't exist for the thread:

```json
{ "error": "artifact version 9 not found for thread 7c9e6679-…" }
```

Side effect: emits `comment-created` (see Events above).

Errors: `401` if bearer token missing/wrong; `400` empty body / bad anchor;
`404` unknown thread or version; `500` on database error.

---

### `GET /api/v1/comments`

Authenticated. List a thread's comments (FR-4.5, FR-6.4), oldest first (the
sidebar reading order). Serves both the UI and the M5 `list-comments` CLI.

Query parameters:
- `thread_id` (required): the thread whose comments to list.
- `status` (optional): filter to one of `open` | `answered` | `applied`.

An unknown `thread_id` returns an empty `comments` array rather than a 404. An
invalid `status` value is a `400`.

Response `200 OK`:

```json
{
  "comments": [
    {
      "id": "b3f1…",
      "thread_id": "7c9e6679-…",
      "artifact_version": 1,
      "anchor": { "v": 1, "type": "element", "cfy_id": "fig-auth-flow.token-service", "quote": { "exact": "Token Service" } },
      "body": "why does this node retry?",
      "status": "answered",
      "answer_html": "<p>Because the upstream can return 503…</p>",
      "anchor_state": "anchored",
      "created_at": "2026-07-04T12:34:56.789Z",
      "resolved_at": "2026-07-04T12:40:01.234Z"
    }
  ]
}
```

Errors: `401` if bearer token missing/wrong; `400` on an invalid `status`
filter; `500` on database error.

---

### `PATCH /api/v1/comments/:id`

Authenticated. Update a comment (FR-4.6/4.7); drives the M5 `resolve-comment`
CLI. Supply any subset of the fields below (at least one); omitted fields are
unchanged.

Request body:

```json
{
  "status": "answered",
  "answer_html": "<p>Because the refresh token rotates on every use…</p>",
  "anchor_state": "moved"
}
```

- `status` — target status; must be a legal advance (see the status machine).
- `answer_html` — the agent's resolution (rendered HTML/markdown fragment).
- `anchor_state` — `anchored` | `moved` (driven by re-attachment; independent of
  the status machine).

Response `200 OK`: the full updated comment (same shape as the create response).

Response `409 Conflict` (illegal status regression) — structured with the
offending `from`/`to`:

```json
{ "error": "illegal status transition: applied -> answered", "from": "applied", "to": "answered" }
```

Response `404 Not Found` (unknown comment id):

```json
{ "error": "comment not found: <id>" }
```

Response `400 Bad Request`: no fields supplied, or an invalid `status` /
`anchor_state` value.

Side effect: emits `comment-updated` (see Events above).

Errors: `401` if bearer token missing/wrong; `400` bad/empty update; `404`
unknown comment; `409` illegal status transition; `500` on database error.

---

## Open / focus

### `POST /api/v1/open`

Authenticated. Focus the app on a project or thread (PRD §5.2 `conceptify
open`). Validates the target exists, brings the main window to the front, and
emits a `navigate` event the frontend uses to route to the target.

Focus-on-open is part of UC1's feel: after an agent finishes generating an
artifact, `conceptify open --thread <id>` puts it on screen. Because the main
window hides (rather than quits) on close (§5.1 lifecycle), the handler
`show()`s it before `set_focus()`.

Request body — supply **exactly one** of `thread_id` / `project_id`:

```json
{ "thread_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7" }
```

or

```json
{ "project_id": "550e8400-e29b-41d4-a716-446655440000" }
```

If both are present, the more specific `thread_id` wins (the project is
resolved from the thread). The CLI enforces exactly-one before calling.

Response `200 OK`:

```json
{
  "ok": true,
  "project_id": "550e8400-e29b-41d4-a716-446655440000",
  "thread_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7"
}
```

`thread_id` is `null` when a project (not a specific thread) was opened. The
same `{ project_id, thread_id }` shape is emitted as the `navigate` event.

Response `400 Bad Request` (neither `thread_id` nor `project_id` supplied):

```json
{ "error": "must specify thread_id or project_id" }
```

Response `404 Not Found` (unknown target):

```json
{ "error": "thread not found: <id>" }
```

```json
{ "error": "project not found: <id>" }
```

Side effect: brings the `main` window to the front (`show` + `set_focus`) and
emits `navigate` (see Events above).

Errors: `401 Unauthorized` if bearer token missing/wrong; `400` if no target
is supplied; `404` if the referenced thread/project doesn't exist; `500` on
database error.

---

## The `artifact://` scheme (in-app viewer transport)

Not an HTTP endpoint: a custom Tauri URI scheme
(`register_asynchronous_uri_scheme_protocol`, PRD §5.4, §9 S2) that serves
stored artifacts into the viewer's sandboxed iframe. Only reachable from
inside the app's webviews — there is no network listener behind it.
Implementation: `src-tauri/src/artifact_protocol.rs`.

### URL contract

```
artifact://localhost/<thread-id>/<version>
```

- **`<thread-id>`** — the thread's DB id (UUID). Validated strictly:
  `[A-Za-z0-9_-]{1,128}`. No percent-decoding is performed on this scheme;
  ids never need encoding, and any `%`, `.`, `\`, or extra `/` in a segment
  is a `400`.
- **`<version>`** — one of:
  - a bare decimal integer ≥ 1 (e.g. `…/3`): serves the immutable
    `artifact.v3.html` for that thread. **Not** `v3` — no prefix.
  - the literal lowercase **`latest`**: resolves the thread's highest
    version **via the DB** (`MAX(version)` over `artifacts`) and serves that
    versioned file. The `artifact.html` disk copy is deliberately not
    served — files are written before the DB row commits (crash-safety
    ordering, PRD N4), so the copy can momentarily be ahead of the DB; the
    DB is the source of truth the version switcher and events read.
- **GET only** (405 otherwise). One document per request — artifacts are
  self-contained by spec, and subresource fetches through custom protocols
  are unreliable in WKWebView anyway (Appendix A, wry #168): never point a
  subresource (`<img src>`, etc.) at `artifact://`.

The viewer (`conceptify-nsy.4`) should embed
`<iframe src="artifact://localhost/<thread-id>/<version>"
sandbox="allow-scripts">` — **no `allow-same-origin`** — and reload on
`artifact-updated`, using the concrete `version` from the event payload.

### Response headers

Every response on the scheme — success or error page — carries:

| Header | Value |
|---|---|
| `Content-Type` | `text/html; charset=utf-8` |
| `Content-Security-Policy` | `default-src 'none'; script-src 'unsafe-inline' https://cdn.jsdelivr.net; style-src 'unsafe-inline' https://cdn.jsdelivr.net; font-src data: https://cdn.jsdelivr.net; img-src data:; connect-src 'none'` |
| `X-Content-Type-Options` | `nosniff` |
| `Cache-Control` | numeric version: `public, max-age=31536000, immutable` (write-once files); `latest` and all errors: `no-store` |

The CSP is exactly the reference policy of
[docs/artifact-spec.md](artifact-spec.md) §3 and stays in lockstep with the
§7.1 pinned-CDN allowlist (single host, `cdn.jsdelivr.net`; `font-src`
includes it for KaTeX). The load-bearing directive is `connect-src 'none'`:
artifact JS can never reach this API (PRD §9 S2). Changing the policy means
changing the spec first.

### Bridge injection

The handler splices one `<script data-cfy-bridge="v1">…</script>` tag into
the served bytes immediately before the closing `</body>` (appended at the
end if no close tag exists; idempotent via the reserved `data-cfy-bridge`
marker). The on-disk file is **never modified** — opened directly in a
browser it carries no Conceptify residue (G3). The script is the in-artifact
half of the comment interaction layer (source:
`src-tauri/assets/bridge.js`, inlined via `artifact_protocol::BRIDGE_TAG`);
the protocol it speaks is specified in [Bridge protocol](#bridge-protocol)
below. It defines the reserved global
`window.__cfyBridge = Object.freeze({ v: 1 })` and injects one
zero-specificity `<style data-cfy-bridge="style">` element (hover
affordances + highlight styles, all `:where()`-wrapped so artifact CSS
always wins). Artifacts must not rely on the bridge existing (spec §1/§3):
the same file opens bridge-less in a plain browser.

## Bridge protocol

The postMessage protocol between the app shell and the injected bridge
script inside the artifact iframe (PRD §5.4). Shell counterpart:
`src/lib/bridge.ts` (the only module that may talk to the frame); bridge
script: `src-tauri/assets/bridge.js`. Comment UI (popover, sidebar) rides on
this seam — beads `conceptify-94m.3/4/5/6`.

**Envelope.** Every message, both directions, is a plain object carrying
`cfy: 1` (protocol version) and a `type` string, plus type-specific fields.
Receivers on both sides ignore messages without the `cfy: 1` envelope and
silently drop unknown `type`s (forward compatibility; the shell logs them at
debug level). A breaking protocol change bumps `cfy`.

**Origin / source validation.** The artifact frame is opaque-origin
(sandboxed, cross-scheme), so:

- The **shell** accepts a message only when `event.origin === "null"` (an
  opaque origin serializes as the string `"null"`) **and** `event.source`
  is the registered viewer iframe's `contentWindow`.
- The **bridge** accepts a command only when `event.source ===
  window.parent`.
- Both sides must post with `targetOrigin "*"` — an opaque origin cannot be
  named. Nothing sensitive ever rides the channel in either direction:
  artifact→shell carries only anchors/quotes/rects, shell→artifact only
  anchors/keys.

**Trust model (S2 — containment, not adversarial).** Artifact JS lives in
the same frame as the bridge and *can* post protocol-shaped messages to the
shell (spoofing the bridge). This is accepted by the threat model: the shell
treats every inbound message as untrusted input (shape-validated, dropped if
malformed) and nothing a spoofed message triggers exceeds what the user
could do by hand in the UI. Artifact JS cannot, however, spoof
shell→artifact *commands* (its `event.source` is its own window, not
`window.parent`).

### Conventions

- **Anchors** are exactly the FR-4.4 anchor objects documented under
  [The anchor model](#the-anchor-model-fr-44) — snake_case, `v: 1`. The
  bridge emits them capture-ready; the shell adds `thread_id`,
  `artifact_version`, and `body` when creating the comment.
- **Text offsets** (`start`/`end` in `text` anchors — this is the precise
  meaning of the anchor model's "normalized text content"): UTF-16
  code-unit indices into the element's **visible text** — the concatenation
  of its `Text` node data in document order, **excluding** text inside
  `script`, `style`, `noscript`, and `template` subtrees, with **no
  whitespace normalization**. Excluding script/style keeps offsets stable
  against inline-JS/CSS churn (and against the injected bridge itself).
  Re-attachment (`conceptify-94m.7`) must measure identically.
- **Quotes**: `text`-anchor quotes are raw visible-text slices
  (`exact` = the selection verbatim; `prefix`/`suffix` = up to 32 chars of
  *document-wide* visible text on either side, omitted when empty at a
  document edge). `element`-anchor quotes are whitespace-collapsed +
  trimmed element text, omitted when empty (purely graphical node) or
  longer than 300 chars — they are re-attachment hints, not exact-match
  slices.
- **Rects** are `{x, y, width, height}` in the **iframe's viewport
  coordinate space** (CSS px, `getBoundingClientRect` values at send time).
  To position shell UI, add the iframe element's own bounding rect; rects
  go stale when the artifact scrolls (the popover should dismiss or
  re-request on scroll/`selection_cleared`).

### Artifact → shell

| `type` | Payload | Meaning |
|---|---|---|
| `ready` | — | The bridge booted. Sent once per **document load** — including every iframe reload (version switch, live refresh), which wipes all decorations. Consumers must (re)apply highlights on every `ready`; `bridge.ts` queues commands sent before the first `ready` of an attachment. |
| `selection` | `anchor` (a `text` anchor), `rect` (selection bounding rect) | A non-empty text selection settled (debounced ~180 ms). `cfy_id`/`start`/`end` are present when the selection has a `data-cfy-id` ancestor; `quote` always is. |
| `selection_cleared` | — | The previously reported selection collapsed/vanished (dismiss the popover). |
| `element_click` | `anchor` (an `element` anchor), `rect` (element bounding rect) | Click on a `data-cfy-id`-bearing element (nearest such ancestor of the click target). Suppressed for clicks that end a text selection and for clicks on interactive elements (`a[href]`, `button`, form controls, `summary`, `label`, `[contenteditable]`). |

### Shell → artifact

| `type` | Payload | Meaning |
|---|---|---|
| `set_highlights` | `highlights`: array of `{key, anchor}` (`key` = comment id) | **Full replacement** of the decoration set (`[]` clears). Element anchors get an outline on the resolved element; text anchors are wrapped in inline `<span data-cfy-hl="text">` spans via offsets (verified against the quote), falling back to a document-wide quote search, then to outlining the `cfy_id` host. Unresolvable anchors are skipped (flagging them `moved` is `conceptify-94m.7`). Decorations are non-destructive and fully reverted on the next `set_highlights`. |
| `scroll_to_anchor` | `anchor`, optional `key` | Smooth-scroll the anchored element/range into view (`block: "center"`) with a brief attention pulse. When `key` matches a live decoration the pulse lands exactly on it; otherwise the anchor is resolved fresh. |

### Hover affordance (v1 decision)

Commentable elements (`[data-cfy-id]`) always show a subtle dashed-outline
hover affordance — there is **no** "commenting enabled" toggle message in
v1. Rationale: commenting is always available in the viewer, the affordance
is zero-specificity (any artifact rule overrides it), and it doubles as
discoverability for click-to-comment. If a mode toggle is ever needed, add a
`set_mode` shell→artifact message rather than overloading an existing one.

### Reserved DOM footprint

Everything the bridge touches stays inside the reserved `data-cfy-*` /
`__cfy*` namespaces (artifact-spec §1): the `data-cfy-bridge` script/style
markers, `data-cfy-hl` / `data-cfy-hl-key` decoration attributes,
`data-cfy-pulse` for the scroll pulse, and the `cfy-pulse` keyframes name.
Text-node splits made while wrapping highlight spans are re-merged
(`Node.normalize()`) when the decoration is removed.

### Errors

Errors are small styled HTML pages (dark-mode aware, same CSP) designed to
render inside the viewer iframe:

| Status | Case |
|---|---|
| `400` | Malformed path: wrong segment count, bad charset, traversal attempts (`..`, `%2e%2e`, `\`), bad version syntax (`0`, `v3`, `Latest`) |
| `404` | Unknown thread; unknown version; `latest` on a thread with no artifact versions yet; DB row present but file missing on disk |
| `405` | Non-GET method |
| `500` | Database/read errors; DB-stored project id or slug fails path-segment validation (defense in depth — never expected) |

---

_Endpoints to be added by later beads: `get-context`, `list-comments`,
`resolve-comment`, `status` (§5.2). Each should get its own section here with
request/response shapes and any events it emits._
