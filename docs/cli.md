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

- `conceptify ensure-project --dir <path> [--name <name>]`
- `conceptify create-thread --project <id> --title <t> --question <q>`
- `conceptify save-artifact --thread <id> --file <path>`
- `conceptify get-context --thread <id>`
- `conceptify list-comments --thread <id> [--status open]`
- `conceptify resolve-comment --id <id> --answer-file <path> [--applied]`
- `conceptify open --thread <id> | --project <id>`

These will be added in later milestones as the API surface expands.
