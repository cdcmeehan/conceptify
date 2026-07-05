# Conceptify

A personal macOS app for deeply understanding concepts — codebases, code flows, unfamiliar languages/frameworks, and general technical questions. AI coding agents (Claude Code first) generate rich, visual, self-contained HTML "explanation artifacts" and publish them into Conceptify, where they are rendered in a beautiful reading experience.

**One-line pitch:** a personal "grokking workbench" — agents explain, Conceptify renders, you interrogate until it clicks.

## What makes it different

- **Durable, visual documents:** Not ephemeral chat transcripts — explanations are rich HTML artifacts with diagrams, syntax-highlighted code, and a baked-in design system.
- **Portable artifacts:** Every explanation is a self-contained `.html` file that renders correctly in Conceptify's viewer *and* standalone in any browser.
- **The interrogation loop:** Highlight text or diagram elements, leave comments, trigger a background agent to answer follow-ups in a sidebar, and when needed, update the artifact live.
- **Agent-native workflow:** A Claude Code skill + `conceptify` CLI make "use conceptify to explain this" a one-line request from any codebase.

## Architecture

Conceptify consists of four components working together:

```
┌─────────────────────────────────────────────────────────────┐
│ Conceptify.app (Tauri v2)                                    │
│                                                              │
│  ┌───────────────┐   ┌──────────────────────────────────┐   │
│  │ Rust core     │   │ Webview (Preact + Tailwind shell)│   │
│  │ · axum HTTP   │──▶│ · project/thread navigation      │   │
│  │   API :4477   │evt│ · artifact viewer (sandboxed     │   │
│  │ · rusqlite    │   │   iframe, artifact:// scheme)    │   │
│  │ · agent       │   │ · comments sidebar + popovers    │   │
│  │   spawner     │   │ · settings                       │   │
│  └───────▲───────┘   └──────────────────────────────────┘   │
└──────────┼───────────────────────────────────────────────────┘
           │ HTTP (127.0.0.1:4477, bearer token)
   ┌───────┴────────┐          ┌─────────────────────────┐
   │ conceptify CLI │◀─────────│ Agents                  │
   │ (thin wrapper, │  invoked │ · Claude Code + skill   │
   │ launches app   │  by      │   (interactive session) │
   │ if needed)     │          │ · headless `claude -p`  │
   └────────────────┘          │   (follow-up runs)      │
                               └─────────────────────────┘
```

- **Conceptify.app:** Tauri v2 macOS app with embedded axum HTTP API on `127.0.0.1:4477` (bearer-token auth) and WAL-mode SQLite DB.
- **conceptify CLI:** Thin Rust binary communicating with the app's HTTP API; handles launch-and-wait, artifact creation, status checks.
- **Claude Code skill:** Installed globally at `~/.claude/skills/conceptify`; enables "use conceptify to explain..." from any codebase.
- **Headless agent runs:** Background `claude -p` processes for answering comments and updating artifacts.

Artifact HTML files live centrally under `~/Documents/conceptify/artifacts/<project-id>/` (never in target repos) alongside the SQLite database.

## Prerequisites

Run `conceptify doctor` to verify your environment has everything needed:

- Conceptify.app (installed or running)
- `conceptify` CLI on PATH
- `d2` (diagram generation)
- `dot` / graphviz (graph rendering)
- `node` >= v20 (Shiki syntax highlighting)
- `claude` (Claude Code agent binary)

Install missing tools via Homebrew:

```bash
brew install d2 graphviz node
```

For Rust/Cargo (needed to build from source):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

For Claude Code, install from [claude.ai/code](https://claude.ai/code).

## Setup

### Automated setup

Run the setup script from the repo root:

```bash
./scripts/setup.sh
```

This will:
1. Check prerequisites (offers install commands for missing tools)
2. Build Conceptify in release mode (`just build`)
3. Install the CLI to PATH (`just install-cli`)
4. Install the Claude Code skill globally (`just install-skill`)
5. Run `conceptify doctor` to verify everything works

**Note:** The first build takes several minutes. If you've already built the release binaries, use `./scripts/setup.sh --skip-build`.

### Manual setup

If you prefer to run steps individually:

```bash
# Build the app bundle and CLI (release mode)
just build

# Install the CLI to PATH
just install-cli

# Install the Claude Code skill globally
just install-skill

# Verify everything is working
conceptify doctor
```

For development, use `just dev` to launch the app with hot reload.

## Usage

### Use case 1: Explain from a coding session

In any Claude Code session inside a codebase:

```
"Use conceptify to explain how authentication middleware works in this repo."
```

The skill ensures the app is running, creates a project for the repo (mapped by root path), generates a visual artifact, and opens it in Conceptify.

### Use case 2: Ask directly from the app

1. Launch Conceptify
2. Open a project (or create one for a directory)
3. Type a question
4. Agent runs headless from the project's mapped directory
5. Artifact appears when done (with streaming progress)

### Use case 3: Interrogate an artifact

While reading an explanation:

1. **Highlight text or click a diagram element** → add a comment
2. Click **Ask follow-ups** → background agent answers each comment in the sidebar
3. Choose **Apply to artifact** → agent edits the HTML live, comment marked resolved-with-update

### Use case 4: Direct CLI usage

```bash
# Check app status
conceptify status

# Verify prerequisites
conceptify doctor
```

## Core commands

Built using [just](https://github.com/casey/just):

```bash
just dev          # App dev loop (npm install if needed + npm run tauri dev)
just build        # Release build: Conceptify.app bundle + CLI
just install-cli  # Release CLI symlinked onto PATH (~/.local/bin or /usr/local/bin)
just install-skill # Install skill to ~/.claude/skills/conceptify
just check        # cargo check + clippy -D warnings (quality gate)
```

Rust binaries are built from the repo root; artifacts land in `target/`.

## Documentation

- [API Reference](docs/api.md) — HTTP API endpoints, request/response formats
- [CLI Reference](docs/cli.md) — `conceptify` command usage, launch-and-wait contract
- [Artifact Spec](docs/artifact-spec.md) — HTML artifact structure, design system, bridge API
- [Startup Walkthrough](docs/startup.md) — Boot sequence and initialization
- [PRD](prd.md) — Full product requirements document (goals, use cases, architecture, milestones)

## Project structure

```
conceptify/
├── src/                    # Preact frontend (Tailwind v4)
├── src-tauri/              # Tauri app + Rust core (axum API, SQLite, agent spawner)
├── crates/
│   ├── conceptify-cli/     # CLI binary
│   └── conceptify-types/   # Shared API types (serde)
├── skill/                  # Claude Code skill (conceptify.md + examples)
├── docs/                   # API, CLI, artifact spec, startup walkthrough
├── scripts/                # Setup script
├── justfile                # Build recipes
└── README.md
```

## Contributing

This is a personal project for a single user (Chris). Development is tracked using the [beads issue tracker](https://github.com/gastownhall/beads) (`bd` CLI). See `CLAUDE.md` for agent workflow instructions.

## License

MIT

---

Built with Tauri v2, Preact, Rust, and Claude Code.
