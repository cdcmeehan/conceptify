# Conceptify CLI

Reference for the `conceptify` command-line tool (PRD §5.2).

## Overview

The CLI is a thin Rust binary that communicates with the local Conceptify.app HTTP API. It handles application lifecycle automatically via the **launch-and-wait contract** and provides JSON output on stdout for consumption by agent scripts.

## Installation

The CLI binary is built as part of the workspace:

```bash
cargo build -p conceptify-cli
# Binary located at: target/debug/conceptify (or target/release/conceptify)
```

Install to PATH via symlink:

```bash
# Example (adjust target as needed):
ln -sf $(pwd)/target/release/conceptify /usr/local/bin/conceptify
```

## Launch-and-wait contract

Every command implements this flow automatically:

1. **Discover the port:** Read `~/Library/Application Support/conceptify/port` (fallback: 4477).
2. **Probe health:** Send `GET /health` to the discovered port.
3. **Launch if needed:** If the app doesn't respond, run `open -a Conceptify` and poll `GET /health` with 200ms intervals for up to 10 seconds.
4. **Proceed or fail:** If the app becomes healthy, continue with the command. Otherwise, exit non-zero with a clear stderr message.

This sidesteps macOS single-instance/argv-forwarding quirks — "launch and focus" is just HTTP (PRD §5.2).

## Discovery files

The CLI reads these files written by the server:

- **Port file:** `~/Library/Application Support/conceptify/port`  
  Plain text, just the port number (e.g., `4477`). May be stale if the app is not currently running — the CLI always verifies liveness via `GET /health`.

- **Bearer token:** `~/Library/Application Support/conceptify/token`  
  Random 32-byte hex string, mode `0600`. Required for all authenticated endpoints (everything except `/health`).

## Commands

### `conceptify status`

Prints app health, version, bound port, and the chosen artifact theme as JSON. Useful for agent scripts to verify the app is available; the artifact-authoring skill reads `artifactTheme` here in one call at authoring time (epic conceptify-89k, bead 89k.2).

**Output (stdout):**

```json
{
  "service": "conceptify",
  "status": "ok",
  "version": "0.1.0",
  "port": 4477,
  "artifactTheme": "manuscript"
}
```

`artifactTheme` is one of `manuscript` | `blueprint` | `sketchbook` (defaults to `manuscript` when unset). It is read via the authenticated `GET /api/v1/settings/display` endpoint after the health probe; if that read fails (health already proved liveness), `status` degrades to `manuscript` and prints a warning to stderr rather than failing.

**Errors (stderr):**

- `App not responding; attempting to launch...` — printed when the initial health probe fails.
- `warning: could not read artifact theme (<reason>); assuming manuscript` — printed when the display-settings read fails after a healthy probe.
- `Error: app did not become healthy within 10s` — printed when launch/polling times out.
- `Error: failed to launch app: <reason>` — printed when `open -a Conceptify` fails.

**Exit codes:**

- `0` — Success (app is healthy and responded).
- `1` — Failure (app could not be reached within the timeout).

**Performance:** < 50ms when the app is already running (N3, PRD §10).

---

### `conceptify doctor`

Checks all prerequisites needed for Conceptify to function properly (PRD FR-6.5). Runs all checks without failing mid-run and reports a summary at the end. Each check prints an actionable pass/fail line to stderr with install hints on failure. A machine-readable JSON summary is printed to stdout.

**Checks performed:**

1. **App installed**: Verifies Conceptify.app exists via `mdfind` (using bundle identifier from `tauri.conf.json`) or in `/Applications` or `~/Applications`. Also probes `GET /health` to detect a running instance (which may be a dev build). Reports both installed state and running state distinctly.
2. **CLI on PATH**: Checks if `conceptify` is resolvable on PATH. Notes if it's running from `target/` (dev build) and suggests `just install-cli`.
3. **d2 present**: Checks if `d2` is installed (hint: `brew install d2`).
4. **dot present**: Checks if `dot` (graphviz) is installed (hint: `brew install graphviz`).
5. **node present**: Checks if `node` is installed and version >= 20 (needed for Shiki; hint: `brew install node`).
6. **Agent binary resolvable**: Checks if `claude` is found via login-shell lookup (`zsh -lc 'which claude'`) per PRD §5.1 (hint: install Claude Code; note that settings can override the binary path later). `claude` is the default adapter, so its absence **fails** doctor.
7. **Codex binary resolvable** (informational): Checks if `codex` is found via the same login-shell lookup. Codex is an optional agent (only needed to route runs to OpenAI models — epic `conceptify-e7m`), so this check reports `ok: true` either way and never affects doctor's exit code; when missing, the detail says so and a hint is printed (`brew install codex` or `npm install -g @openai/codex`).

**Output (stdout):**

```json
{
  "ok": true,
  "checks": [
    {
      "name": "app-installed",
      "ok": true,
      "detail": "Conceptify.app found at /Applications/Conceptify.app",
      "hint": null
    },
    {
      "name": "cli-on-path",
      "ok": true,
      "detail": "conceptify is on PATH at /usr/local/bin/conceptify",
      "hint": null
    },
    {
      "name": "d2-present",
      "ok": true,
      "detail": "d2 is available at /opt/homebrew/bin/d2",
      "hint": null
    },
    {
      "name": "dot-present",
      "ok": true,
      "detail": "dot (graphviz) is available at /opt/homebrew/bin/dot",
      "hint": null
    },
    {
      "name": "node-present",
      "ok": true,
      "detail": "node v20.10.0 is available (>= v20)",
      "hint": null
    },
    {
      "name": "agent-binary-resolvable",
      "ok": true,
      "detail": "claude (default agent) is resolvable at /usr/local/bin/claude",
      "hint": null
    },
    {
      "name": "codex-binary-resolvable",
      "ok": true,
      "detail": "codex not found (optional — only needed to run OpenAI models)",
      "hint": "Install codex: brew install codex (or: npm install -g @openai/codex)"
    }
  ]
}
```

**Errors (stderr, with hints):**

```
[✗] d2-present: d2 not found on PATH
    Hint: Install d2: brew install d2
```

**Exit codes:**

- `0` — All checks passed.
- `1` — One or more checks failed.

**Notes:**

- This command does **not** launch the app or have side effects — it only probes existing state.
- The health probe uses a short timeout and does not trigger the launch-and-wait contract.

---

### `conceptify ensure-project --dir <path> [--name <name>]`

Find-or-create a project for a directory (PRD §5.2, maps to
`POST /api/v1/projects/ensure`). Idempotent: running it twice for the same
directory returns the same `projectId` with `created: false` the second time.

`--dir` is resolved to an absolute, symlink-free path **on the CLI side**
before the request is sent, because the server canonicalizes relative to its
own working directory (wherever the app launched), not the agent's. A path that
doesn't exist fails fast on the CLI before any HTTP call.

`--name` optionally overrides the project name (defaults to the directory
name, deduped with a numeric suffix if taken).

**Output (stdout):**

```json
{
  "projectId": "550e8400-e29b-41d4-a716-446655440000",
  "created": true
}
```

**Errors (stderr, exit 1):**

- `Error: path not found: <dir> (...)` — `--dir` doesn't exist / can't be resolved.
- `Error: ensure-project requires --dir <path>` — `--dir` missing.
- API errors are surfaced verbatim, e.g. `Error: path not found: ... (HTTP 400)`.

---

### `conceptify create-thread --project <id> --title <t> --question <q>`

Create a thread in a project (PRD §5.2, maps to `POST /api/v1/threads`). The
filesystem-safe `slug` (the artifact-folder name, §5.6) is derived server-side
from the title and deduped within the project; it's echoed in the output
because the skill needs it for `save-artifact`.

**Output (stdout):**

```json
{
  "threadId": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "slug": "how-does-oauth-work"
}
```

**Errors (stderr, exit 1):**

- `Error: create-thread requires --project <id> --title <t> --question <q>` — a flag is missing.
- API errors are surfaced verbatim, e.g. `Error: project not found: <id> (HTTP 404)`,
  `Error: title must not be empty (HTTP 400)`.

---

### `conceptify open --thread <id> | --project <id>`

Focus the app on a project or thread (PRD §5.2, maps to `POST /api/v1/open`).
Brings the main window to the front and emits the `navigate` event the frontend
routes on — so the artifact is on screen when an agent finishes (UC1). Supply
**exactly one** of `--thread` / `--project`.

**Output (stdout):**

```json
{
  "ok": true,
  "projectId": "550e8400-e29b-41d4-a716-446655440000",
  "threadId": "7c9e6679-7425-40de-944b-e07fc1f90ae7"
}
```

`threadId` is `null` when opening a project without a specific thread.

**Errors (stderr, exit 1):**

- `Error: open requires exactly one of --thread <id> or --project <id>` — neither supplied.
- `Error: open takes only one of --thread or --project, not both` — both supplied.
- API errors are surfaced verbatim, e.g. `Error: thread not found: <id> (HTTP 404)`.

---

### `conceptify save-artifact --thread <id> --file <path>`

Save an artifact HTML file to a thread (PRD §5.2, maps to
`POST /api/v1/threads/:thread_id/artifact`). This is the final step in the UC1
flow — the agent authors the artifact, writes it to disk, and publishes it via
this command. The endpoint validates, stores, versions, and triggers live
refresh in the app.

The file is read on the CLI side and sent as raw HTML bytes to the server.
Validation failures (hard errors from docs/artifact-spec.md §8.1) are surfaced
with each violated rule's code and message. Warnings (§8.2) are printed to
stderr but the artifact is still stored.

After a successful save, the CLI focuses the app on the thread via
`POST /api/v1/open` so the artifact appears on screen immediately (UC1 feel).

**Output (stdout):**

```json
{
  "version": 2,
  "warningsCount": 1
}
```

`version` is the server-assigned version number (v1, v2, ...). `warningsCount`
is the number of spec warnings returned (details printed to stderr).

**Warnings (stderr):**

```
warning: W-ANCHOR-DIAGRAM: svg "fig-map" has thin anchor coverage: 8 shape elements but only 1 data-cfy-id bearers (need ≥ 3)
```

Each warning is printed as `warning: <code>: <message>` (agent-visible).

**Errors (stderr, exit 1):**

- `Error: failed to read <path>: ...` — file doesn't exist or can't be read.
- `Error: save-artifact requires --thread <id> --file <path>` — a flag is missing.
- Validation errors (HTTP 422) are printed with all violated rules:
  ```
  Error: script src "https://evil.example.com/x.js" is not on the Tier-2 CDN allowlist (E-EXTERNAL-CODE)
    E-EXTERNAL-CODE: script src "https://evil.example.com/x.js" is not on the Tier-2 CDN allowlist
  ```
- `Error: thread not found: <id> (HTTP 404)` — unknown thread.
- API errors are surfaced verbatim.

---

### `conceptify save-asset --thread <id> --file <path>`

> **Status: contract reserved, not yet implemented.** Ships with the
> video epic's app-side bead (conceptify-z9y.6); documented here as the
> fixed shape the skill's render pipeline builds against.

Upload a video clip into a thread's asset storage (artifact-spec §1.4;
maps to `PUT /api/v1/threads/:thread_id/assets/:sha256`). Run **before**
`save-artifact` for any artifact whose HTML references the clip — the
save-artifact validator rejects references to assets that were never
uploaded (`E-ASSET-REF`).

The CLI computes the file's SHA-256 locally, streams the raw bytes to
the endpoint, and prints the canonical `cfy-asset://` URL — the exact
string that belongs in the artifact's `<video src>`. Re-uploading an
already stored clip is idempotent (content-addressed).

**Output (stdout):**

```json
{
  "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
  "bytes": 8388608,
  "url": "cfy-asset://localhost/7c9e6679-…/e3b0c442…b855.mp4",
  "warningsCount": 0
}
```

**Warnings (stderr):** upload-time spec warnings (`W-ASSET-RES`,
`W-ASSET-LONG` — artifact-spec §8.3), printed as
`warning: <code>: <message>` like save-artifact.

**Errors (stderr, exit 1):** unreadable file; missing flags; validation
hard failures (HTTP 422 — `E-ASSET-HASH`, `E-ASSET-SIZE`,
`E-ASSET-TYPE`, `E-ASSET-DURATION`) printed with each violated rule's
code and message; `thread not found` (HTTP 404); API errors verbatim.

---

### `conceptify get-context --thread <id>`

Assemble a thread's **run context** for a headless follow-up (PRD §5.2, §5.5;
maps to `GET /api/v1/threads/:id/context`). One round-trip returns everything an
agent needs to answer the thread's open comments without touching the DB: the
question, the latest artifact's path on disk, the project root (the agent's
`cwd`), and each open comment with its anchor. This is the context Conceptify
assembles into the follow-up prompt.

**Output (stdout):**

```json
{
  "threadId": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "title": "How does OAuth work?",
  "question": "Explain the OAuth 2.0 authorization code flow.",
  "status": "ready",
  "slug": "how-does-oauth-work",
  "projectId": "550e8400-e29b-41d4-a716-446655440000",
  "projectName": "myrepo",
  "projectRoot": "/Users/chris/code/myrepo",
  "artifactVersion": 2,
  "artifactPath": "/Users/chris/Documents/conceptify/artifacts/550e8400-…/threads/how-does-oauth-work/artifact.v2.html",
  "openComments": [
    {
      "id": "b3f1…",
      "threadId": "7c9e6679-…",
      "parentId": null,
      "artifactVersion": 1,
      "anchor": { "v": 1, "type": "text", "cfy_id": "sec-walkthrough", "start": 142, "end": 210, "quote": { "exact": "the token is refreshed here" } },
      "body": "why is the token refreshed here?",
      "status": "open",
      "answerHtml": "<p>Because the refresh token rotates…</p>",
      "anchorState": "anchored",
      "createdAt": "2026-07-04T12:34:56.789Z",
      "resolvedAt": null,
      "replies": [
        {
          "id": "d7a2…",
          "threadId": "7c9e6679-…",
          "parentId": "b3f1…",
          "artifactVersion": 1,
          "anchor": null,
          "body": "I still don't get why it rotates every request.",
          "status": "open",
          "answerHtml": null,
          "anchorState": "anchored",
          "createdAt": "2026-07-04T12:40:00.000Z",
          "resolvedAt": null
        }
      ]
    }
  ]
}
```

`artifactVersion` / `artifactPath` are `null` when the thread has no artifact yet.
`openComments` contains only **root** comments still in the `open` state (a root
re-opened by a user reply is among them). Each carries a `replies` array — its
ordered reply chain (oldest first), the **exchange history** the follow-up prompt
builds on (original question + prior `answerHtml` + follow-up replies). `replies`
is `[]` when a root has no replies; each reply has `parentId` = the root's id and
a `null` `anchor`.

**Anchor passthrough:** top-level keys are camelCase (like every command), **but
each comment's `anchor` object is passed through verbatim** — its inner keys stay
`snake_case` (`cfy_id`, `quote`, …). The anchor is a stored cross-layer contract
(bridge ⇄ DB ⇄ re-attachment ⇄ agent, see docs/api.md → *The anchor model*) and
must round-trip byte-for-byte; the CLI never rewrites it.

**Errors (stderr, exit 1):**

- `Error: get-context requires --thread <id>` — `--thread` missing.
- `Error: thread not found: <id> (HTTP 404)` — unknown thread.

---

### `conceptify list-comments --thread <id> [--status open|answered|applied]`

List a thread's comments with anchors (PRD §5.2; maps to
`GET /api/v1/comments`). Optionally filter by `--status`. Output is a bare JSON
**array** — each comment is the same camelCase shape as one entry of
`get-context`'s `openComments` (minus the nested `replies`), with the `anchor`
passed through verbatim. This is a **flat** list: replies appear as their own
entries, each with `parentId` set to its root (roots have `parentId: null`).

**Output (stdout):**

```json
[
  {
    "id": "b3f1…",
    "threadId": "7c9e6679-…",
    "parentId": null,
    "artifactVersion": 1,
    "anchor": { "v": 1, "type": "element", "cfy_id": "fig-auth-flow.token-service", "quote": { "exact": "Token Service" } },
    "body": "why does this node retry?",
    "status": "answered",
    "answerHtml": "<p>Because the upstream can return 503…</p>",
    "anchorState": "anchored",
    "createdAt": "2026-07-04T12:34:56.789Z",
    "resolvedAt": "2026-07-04T12:40:01.234Z"
  }
]
```

An unknown thread returns an empty array (`[]`), not an error.

**Errors (stderr, exit 1):**

- `Error: list-comments requires --thread <id>` — `--thread` missing.
- `Error: invalid status filter "<x>" (expected open|answered|applied) (HTTP 400)`
  — an unrecognized `--status` value.

---

### `conceptify resolve-comment --id <id> --answer-file <path> [--applied]`

Answer (or, with `--applied`, apply) a comment (PRD §5.2, FR-4.6/4.7; maps to
`PATCH /api/v1/comments/:id`). The answer is read from `--answer-file` on the CLI
side and stored as the comment's `answer_html`, advancing its status to
`answered` (default) or `applied` (with `--applied`). The sidebar updates live
via the `comment-updated` event the server emits.

The answer file may be an **HTML fragment or markdown** — it is sent verbatim;
the sidebar renders it. Use `--applied` for the *apply-to-artifact* flow, where
the run also saved a new artifact version and is resolving the comment as
applied in one shot (`open → applied` is a legal transition).

**Output (stdout):**

```json
{ "ok": true, "id": "b3f1…", "status": "answered" }
```

**Errors (stderr, exit 1):**

- `Error: resolve-comment requires --id <id> --answer-file <path> [--applied]` — a
  required flag is missing.
- `Error: failed to read <path>: ...` — the answer file doesn't exist or can't be
  read.
- `Error: comment not found: <id> (HTTP 404)` — unknown comment id.
- `Error: illegal status transition: applied -> answered (HTTP 409)` — the status
  would regress (a comment may only advance `open → answered → applied`).

---

## Output contract

All commands print stable, parseable JSON to **stdout** on success and
human-readable errors to **stderr** with a non-zero exit code — these are the
exact commands the M3 Claude Code skill drives. Note the CLI output keys are
camelCase (`projectId`, `threadId`) even though the underlying HTTP API uses
snake_case; the CLI is the stable contract for scripts.

---

## Workspace layout

The CLI is part of a Cargo workspace:

```
/Cargo.toml                         — workspace root
/crates/conceptify-types/           — shared request/response types
/crates/conceptify-cli/             — CLI binary crate
/src-tauri/                         — Tauri app crate
```

Shared types (e.g., `HealthResponse`) live in `conceptify-types` and are used by both the CLI and server, avoiding duplication.

---

## Command coverage

Every command in the PRD §5.2 table is now implemented: `status`, `doctor`,
`ensure-project`, `create-thread`, `open`, `save-artifact`, `get-context`,
`list-comments`, and `resolve-comment`. `save-asset` is documented above as
a reserved contract (video epic) and is not yet implemented.
