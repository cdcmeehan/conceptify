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

Future mutation endpoints (projects, threads, artifacts, comments) will add
rows here as they land (`artifact-updated`, `comment-resolved`,
`thread-created`, per PRD §5.1).

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

---

_Endpoints to be added by later beads: `ensure-project`, `create-thread`,
`save-artifact`, `get-context`, `list-comments`, `resolve-comment`, `open`,
`status` (§5.2). Each should get its own section here with request/response
shapes and any events it emits._
