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
| `navigate` | `POST /api/v1/open` | `{ project_id: string, thread_id: string \| null }` |

`artifact-updated` is the viewer's live-refresh trigger (PRD N2: save →
visible refresh < 500ms): the frontend reloads the artifact iframe for
`thread_id` at `version` when it arrives.

`GET /api/v1/debug/db-check`, `GET /api/v1/projects`, and `GET /api/v1/threads`
do not emit events — they're read-only.

Future mutation endpoints (comments) will add rows here as they land
(`comment-resolved`, per PRD §5.1).

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
`artifact-updated` (see Events above).

Errors: `401 Unauthorized` if bearer token missing/wrong; `404` unknown
thread; `413` body over the 60 MiB transport cap; `422` validation hard
failure; `500` on database or disk error.

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

_Endpoints to be added by later beads: `get-context`, `list-comments`,
`resolve-comment`, `status` (§5.2). Each should get its own section here with
request/response shapes and any events it emits._
