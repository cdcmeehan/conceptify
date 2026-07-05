# Work log

Brief running log of what's been built, for orientation. Details live in the
beads (`bd show <id>`), git history, and docs/.

## 2026-07-02 → 2026-07-03 — M0 Foundations

- Tauri v2 scaffold (Preact + Tailwind v4 + Vite), embedded axum API on
  127.0.0.1:4477 (bearer token + port file), rusqlite WAL DB + migrations,
  single-instance / hide-on-close lifecycle, CLI skeleton with the
  launch-and-wait contract, justfile. PRD, agent instructions, bead tracker.
- OQ2 resolved: artifacts stored centrally under
  `~/Documents/conceptify/artifacts/<project-id>/`, never in target repos.

## 2026-07-04 — Phase 1 complete (M1–M6, all beads closed)

**M1 — Projects & threads.** Projects backend (ensure/list/rename/archive),
threads backend (create/list, persisted per-project-unique slugs, status
machine), CLI `ensure-project` / `create-thread` / `open` (+ `POST /open`
with window focus + `navigate` event), app shell UI (sidebar, thread list,
status chips, missing-dir re-map), centralized Tauri event wiring for live
list updates.

**M2 — Artifact pipeline & viewer.** `docs/artifact-spec.md` (the FR-3.x
contract: id grammar, cfy:src embedding + escaping, validator rule set,
Tier-2 CDN allowlist), save-artifact endpoint (spec-§8 validator, atomic
versioned storage, thread→ready + `artifact-updated`), `artifact://`
protocol handler (per-response CSP, bridge injection seam, immutable
caching, traversal defenses), design-system CSS scaffold + golden demo
artifact (OQ1: system-stack "quiet print editorial"), CLI `save-artifact`,
sandboxed opaque-origin viewer (version switcher, open-in-browser, strict
app-shell CSP). Checkpoint verified across Chrome/WebKit × light/dark ×
offline; narrow-pane overflow found + fixed.

**M3 — Claude Code skill.** `skill/` authored + installed globally
(`just install-skill`): flow, authoring guide, tiered visual strategy with
verified render commands (d2/dot; Shiki via `scripts/highlight.mjs`;
`scripts/postprocess-svg.mjs` for id-stamping), self-review loop (headless
screenshots + checklist + overflow tripwire), `conceptify doctor`. OQ5:
Mermaid rejected from the authoring toolchain (positional ids break
anchoring). Quality iteration across three real repos (conceptify, axum,
ripgrep) fed fixes back into the guides; OQ4 confirmed create-thread-early.
The real app now contains the meta artifact "How Conceptify works, end to
end".

**M4 — Comments.** Comments backend (versioned anchor JSON schema, monotonic
status machine, events), postMessage bridge v1 (selection/element-click
anchor capture, highlights, scroll-to-anchor, hardened channel), selection +
diagram popovers, comments sidebar (filters, answers, moved badge) + direct
follow-up composer, anchor re-attachment across versions (comments follow
latest; failures flag "reference moved", applied comments freeze).

**M5 — Follow-up loop.** Agent adapter + per-purpose model settings
(injection-safe template resolution), headless run engine
(spawn/stream/timeout/cancel/logs, process-group kill, one-run-per-thread
guard, boot reconciliation), `get-context`/`list-comments`/`resolve-comment`
CLI+API, ask-follow-ups + apply-to-artifact flows (freeze-before-save
ordering), run status UI (progress, cancel, log tail), skill follow-up
guidance, OQ3 resolved (verified scoping: `--allowedTools Bash Edit Write`,
repo read-only, no web, no mutating git — and the load-bearing find that
`--verbose` is required with `-p --output-format stream-json`). UC4
checkpoint passed live with real haiku/sonnet runs (3 answers, 1 apply → v2,
re-attachment verified, cancel + failure paths).

**M6 — Self-sufficiency.** Ask-from-app (composer, streaming generation
view, error/timeout/retry — FR-5.3), `ask` run mode migration, in-app
project creation (native picker + auto-create), settings UI (adapters,
models, timeout, appearance light/dark/system, paths, reset), thread delete
+ stalled-chip hygiene, README + `scripts/setup.sh` + `just setup`. Final QA
passed all six use cases; NFRs measured green (cold start ~310 ms,
save→refresh ~30 ms, warm CLI ~5 ms, offline artifacts). Post-QA fixes:
default run timeout 900→1800 s; test artifact-dir leak structurally fixed.

**State at Phase 1 close:** 60/61 beads closed (the Phase-2 placeholder epic
stays open), 217 workspace tests, fresh bundle in /Applications, release CLI
on PATH, skill installed. No git remote configured — history is local.

## Next up

M7 epic `conceptify-6xi` — conversational interrogation (threaded replies +
per-comment "Ask now"). Parked ideas: `docs/future-improvements.md`.
