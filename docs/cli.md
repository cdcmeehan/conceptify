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

### `conceptify doctor`

Checks all prerequisites needed for Conceptify to function properly (PRD FR-6.5). Runs all checks without failing mid-run and reports a summary at the end. Each check prints an actionable pass/fail line to stderr with install hints on failure. A machine-readable JSON summary is printed to stdout.

**Checks performed:**

1. **App installed**: Verifies Conceptify.app exists via `mdfind` (using bundle identifier from `tauri.conf.json`) or in `/Applications` or `~/Applications`. Also probes `GET /health` to detect a running instance (which may be a dev build). Reports both installed state and running state distinctly.
2. **CLI on PATH**: Checks if `conceptify` is resolvable on PATH. Notes if it's running from `target/` (dev build) and suggests `just install-cli`.
3. **d2 present**: Checks if `d2` is installed (hint: `brew install d2`).
4. **dot present**: Checks if `dot` (graphviz) is installed (hint: `brew install graphviz`).
5. **node present**: Checks if `node` is installed and version >= 20 (needed for Shiki; hint: `brew install node`).
6. **Agent binary resolvable**: Checks if `claude` is found via login-shell lookup (`zsh -lc 'which claude'`) per PRD §5.1 (hint: install Claude Code; note that settings can override the binary path later).

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
      "detail": "claude is resolvable at /usr/local/bin/claude",
      "hint": null
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

- `conceptify get-context --thread <id>`
- `conceptify list-comments --thread <id> [--status open]`
- `conceptify resolve-comment --id <id> --answer-file <path> [--applied]`

These will be added in later milestones as the API surface expands.
