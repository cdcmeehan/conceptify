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

`GET /api/v1/debug/db-check` and `GET /api/v1/projects` do not emit events —
they're read-only.

Future mutation endpoints (threads, artifacts, comments) will add rows here
as they land (`artifact-updated`, `comment-resolved`, `thread-created`, per
PRD §5.1).

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

_Endpoints to be added by later beads: `create-thread`, `save-artifact`,
`get-context`, `list-comments`, `resolve-comment`, `open`, `status` (§5.2).
Each should get its own section here with request/response shapes and any
events it emits._
