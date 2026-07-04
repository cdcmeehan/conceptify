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

Prints app health, version, and bound port as JSON. Useful for agent scripts to verify the app is available.

**Output (stdout):**

```json
{
  "service": "conceptify",
  "status": "ok",
  "version": "0.1.0",
  "port": 4477
}
```

**Errors (stderr):**

- `App not responding; attempting to launch...` — printed when the initial health probe fails.
- `Error: app did not become healthy within 10s` — printed when launch/polling times out.
- `Error: failed to launch app: <reason>` — printed when `open -a Conceptify` fails.

**Exit codes:**

- `0` — Success (app is healthy and responded).
- `1` — Failure (app could not be reached within the timeout).

**Performance:** < 50ms when the app is already running (N3, PRD §10).

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

## Future commands

The following commands are specified in PRD §5.2 but not yet implemented:

- `conceptify save-artifact --thread <id> --file <path>`
- `conceptify get-context --thread <id>`
- `conceptify list-comments --thread <id> [--status open]`
- `conceptify resolve-comment --id <id> --answer-file <path> [--applied]`

These will be added in later milestones as the API surface expands.
