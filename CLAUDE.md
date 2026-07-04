# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:7510c1e2 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.

## Session Completion

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds
<!-- END BEADS INTEGRATION -->


## Build & Test

```bash
just dev          # app dev loop (npm install if needed + npm run tauri dev)
just build        # release build: Conceptify.app bundle + CLI
just install-cli  # release CLI symlinked onto PATH (~/.local/bin)
just check        # cargo check + clippy -D warnings (quality gate)
cargo test        # workspace tests
```

`cargo` lives at `$HOME/.cargo/bin`. Build from the repo root; artifacts land in the root `target/` (Cargo workspace).

## Architecture Overview

Tauri v2 macOS app (Preact + Tailwind v4 frontend, Rust core) with an embedded axum HTTP API on `127.0.0.1:4477` (bearer-token auth, port/token files under `~/Library/Application Support/conceptify/`) and a WAL-mode SQLite DB. Cargo workspace: `src-tauri` (app, binary `conceptify-app`), `crates/conceptify-cli` (binary `conceptify`), `crates/conceptify-types` (shared API types). Artifact HTML files live centrally under `~/Documents/conceptify/artifacts/<project-id>/`, never in target repos. See `prd.md` for the full spec, `docs/api.md` + `docs/cli.md` for the API/CLI surface, and `docs/startup.md` for the boot sequence.


### Using subagents for beads

We have three worker subagents, tiered by task complexity. When you, the orchestrator, are instructed to "work through the beads" (to a certain point or indefinitely), scan each upcoming bead and route it to the cheapest tier that can genuinely complete it — escalate only when the bead's content demands it, not because it's long.

| Tier | Agent | Model | Route these beads here |
|------|-------|-------|------------------------|
| High | `high-complexity-worker` | Fable 5 | Open design questions / decision-type beads; security- or isolation-relevant surfaces (auth, CSP, sandboxing, origin isolation); architecture-sensitive Rust core (protocol handlers, process management, async/state sharing); novel algorithms (anchor re-attachment, status machines, atomic versioning); anything expensive to unwind if the approach is wrong |
| Medium | `medium-complexity-worker` | Opus 4.8 | The workhorse for most feature beads: multi-module implementation on a settled design, non-trivial API endpoints + domain logic, meaty UI with real state, integration across existing layers (server ↔ DB ↔ frontend ↔ CLI), refactors with clear intent |
| Low | `low-complexity-worker` | Sonnet 4.5 | Fully-specified mechanical work: scaffolding, installs, config/justfile, docs/README, setup scripts, trivial CRUD wiring, copy/style tweaks |

**Triage heuristics:**
- Ask "what could a weaker model get *wrong* here?" If the answer is "nothing, the bead spells it out" → low. If "implementation details and edge cases" → medium. If "the actual approach" → high.
- Decision-type beads and anything the PRD flags as an open question go straight to high.
- Workers are told to escalate rather than guess: low → medium when real judgment appears, medium → high when design latitude or security surfaces appear. Take escalations seriously — re-dispatch, don't downplay.
- When in doubt between two tiers, prefer the higher one for foundation code others will build on, the lower one for leaf work that's easy to redo.

**Parallelism:** identify beads that don't interact (disjoint files/layers) and run workers concurrently — mixing tiers freely as the tasks dictate. Fence each worker's prompt with explicit file boundaries ("do not touch X, another agent is working there") when running in parallel; beads that converge on the same core files (e.g. `src-tauri/src/lib.rs`) should run sequentially.

**Orchestrator duties per bead:** claim it (`bd update <id> --claim`) before dispatch, tell the worker whether you or it will close it, spot-check the result against the acceptance criteria (don't rubber-stamp reports — verify at least the load-bearing claim), then close and commit per the session protocol.
