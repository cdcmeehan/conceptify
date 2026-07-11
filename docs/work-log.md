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

## 2026-07-05 — M7 conversational interrogation + fixes + polish

**Compact-ask fix (`conceptify-pri`).** A trivial syntax question ran 8.5 min
through the full authoring pipeline. Fixed: sizing step in the skill
(compact/standard/deep, bias-to-compact), proportional self-review (4-frame
loop only for diagram-bearing artifacts), rate-limit noise filtered from the
progress feed + elapsed clock, and Read/Glob/Grep added to the headless
allowedTools (they were denied — pure permission friction, ~35 wasted turns).
Re-verified live: same-class question now lands in ~3 min with 0 warnings.

**M7 — threaded replies + Ask now (epic `conceptify-6xi`).** Replies backend
(`parent_id` migration #10, linear chains, user reply re-opens its root,
nested chains in get-context), exchange-history transcripts in answer prompts
(answer the latest unanswered message, build don't repeat), roots-only
targeting for batch/apply, threaded sidebar with reply composer, per-root
"Ask now" with inline run state + FR-4.9 disable rules, skill etiquette
update. Live checkpoint passed all scenarios with real agents (Ask now 30s,
chained contextual answer 40s, mixed batch 82s, apply → v3 with chain frozen
+ re-anchoring, cancel/failure paths); one defect found + fixed live
(answering the latest reply re-answers the root).

**UI/UX polish pass (`conceptify-vxc`).** Shell adopted the artifact design
system's tokens (warm paper/charcoal, terracotta accent, re-keyed to
data-theme), status color families, shared control primitives, serif display
moments, empty states + skeletons, focus-visible rings, Escape handlers,
AA-verified contrast in both themes, reduced-motion guards.

## 2026-07-06 — Selection UX overhaul + model selection epic

**Skill detail-level ask (`conceptify-vsg`).** Interactive invocations now ask
once — Quick & simple / Balanced (default) / Very detailed — mapping to
COMPACT/auto/DEEP sizing AND the authoring model (Quick/Deep delegate
authoring to a subagent with a model override; generated-by stays honest).
Explicit depth wording skips the question; headless runs never ask. Proven
live: a "quick, simple" ask produced a compact haiku-authored artifact.

**Selection & highlight UX (epic `conceptify-vu1`).** Bridge now reports text
selections only on gesture completion (pointerup/pointercancel; ~300 ms settle
for keyboard selections) — no more popover mid-drag. Two-stage popover: a
compact toolbar (Comment · Copy) hovers above the selection on release;
Comment opens the composer in place, Copy writes the exact selected text.
Highlights re-tinted terracotta and theme-aware (translucent fill + opaque
underline accent; element outline + soft ring; styled ::selection) — clearly
visible over prose, callouts, code blocks and tables in both themes. Live
checkpoint passed all nine scenarios and caught two integration defects
(post-save toolbar re-pop; Escape from iframe focus), both fixed.

**Model selection (epic `conceptify-e7m`).** The user picks a model; the
system derives the route: anthropic → claude CLI, openai → codex CLI
(built-in verified adapter — workspace-write sandbox with explicit network
enable), anything else → claude CLI pointed at OpenRouter via per-run env
(key stored write-only outside the settings blob; secrets proven absent from
logs/rows). Model catalog fetched on restart (LiteLLM + OpenRouter, 614 chat
models / 52 providers), disk-cached with TTL + bundled offline snapshot.
Settings: searchable grouped model pickers per purpose, provider suite
toggles, OpenRouter key UI, catalog freshness/refresh. Every ask trigger
(composer, batch follow-ups, apply, per-comment Ask now) gained a quiet
per-run override pill; retry reuses a real override but re-derives current
defaults otherwise, and the failure state says which model/route it will
use. Per-adapter scope notes keep prompts mechanism-accurate
(`conceptify-w9e`; structured codex progress parked). Live checkpoint: real
claude + codex runs through the UI, offline catalog boot, fake-key
clean-401 proof at the OpenRouter auth boundary — a genuine OpenRouter run
awaits a real API key in Settings.

## Next up

Parked ideas: `docs/future-improvements.md` (⌘K + FTS search, model picker
at ask time, version diffing, streaming answers). Phase-2 backlog:
`conceptify-qmy`.

## 2026-07-11 — Phase 2 concurrency policy

Accepted the run concurrency and write-conflict contract
(`conceptify-k9z.1`; `docs/concurrency-policy.md`): durable queued/starting/
throttled/cancelling states, configurable provider pools, FIFO plus
cross-project fairness, honest restart recovery, concurrent exploration,
serialized same-thread mutation, and mandatory stale-base refusal before an
artifact version can publish. This decision unblocks the scheduler, activity,
notification, and safe compare/apply work in the concurrency epic.

**Concurrent scheduler + immediate question folio (`conceptify-k9z.2/.3`).**
Runs now enter a durable provider-aware queue before the submit call returns;
exploration overlaps, same-thread mutations serialize, capacity changes apply
live, queued/throttled work resumes after restart, cancellation wins spawn
races, provider limits requeue the same auditable row, and retries link to their
source attempt. The new project-keyed question folio keeps drafts across
navigation, offers an optional editable short-list review, launches each prompt
as its own trackable thread, and shows live queued/running/cancel states without
closing the composer. Browser QA covered constrained-pane layout, editing,
project switching, close/reopen persistence, and two-item batch state display.

**Global activity and richer run states (`conceptify-k9z.4`).** A compact
bottom-right tray makes every active run findable across projects, with queue
position, model, phase, elapsed time, and only the actions valid for that state:
jump, cancel, retry, or dismiss. Project rows quietly mark background work and
thread rows distinguish queued from generating, answering, applying, provider
wait, and cancellation. Failures remain until dismissed, ordinary terminal
history clears after 15 minutes, and active work can never be hidden. Browser QA
covered the real persisted history, cross-project jump, and mocked queued versus
admitted runs; the polite live summary reports aggregate counts without reading
streaming progress aloud.

**Background notifications and attention (`conceptify-k9z.5`).** Terminal work
now gains a durable unread marker: completion and attention badges survive
reload, opening the tray marks visible rows seen, and failures/conflicts remain
actionable until explicitly dismissed. Optional Tauri system notifications are
off by default, request OS permission only from the Settings opt-in, atomically
claim each run once to suppress duplicate lifecycle events, and deep-link back
to its project/thread. Lock-screen text contains only the project name and a
generic instruction; prompt, thread title, error, path, and model stay in-app.
Browser QA verified unread persistence, seen-on-open, retained attention, the
settings control, and the permission-free fallback path.

**Artifact version diff engine (`conceptify-3nn.1`).** Saved versions can now be
compared through matching Tauri and authenticated HTTP APIs. The engine works at
the existing `data-cfy-id` anchoring unit, ignores serialization-only whitespace
and attribute noise, returns word hunks for modified text, separates minimal
reorders from insertion/deletion, and supplies neighbor ids for removed-block
placement. Duplicate ids and changed id-less content degrade explicitly to a
document fallback; malformed HTML is error-recovered. Tests cover every change
class and a 150-block artifact well below the one-second budget.

**Viewer diff (`conceptify-3nn.2`).** Every viewed version with a predecessor
now offers “What changed”: a summary, changed-block list, next/previous jump,
layout-neutral color gutters, removed-block neighbor markers, and a unified
word-diff fallback. The bridge keeps diff overlays on a separate full-replacement
channel from comment highlights, so selections and comment decorations coexist
and both reapply independently after iframe reloads. Reduced-motion users get
instant jumps without pulses. Browser QA covered real v2→v3 apply output,
v1→v2 id-less fallback, version switching, jump, and clean exit; the rebuilt
native `Conceptify.app` repeated the real-apply checkpoint beside live comments.

**Safe concurrent mutation recovery (`conceptify-k9z.6`).** Headless saves now
bind to their durable run and immutable captured base. A stale save returns 409,
publishes no artifact, retains the validated candidate, and finishes as
`conflicted` even though its CLI exits nonzero. Conflict review compares current
and candidate side by side with agent/model/route/base provenance. Nothing
auto-merges: the user either starts a fresh-base, lineage-linked synthesis or
confirms a separate candidate version; recovered artifacts retain source run,
base, and resolution metadata. Browser QA covered modified/added comparisons
and the two-step separate-version guard; backend tests cover refusal, retention,
terminal-state preservation, provenance, and explicit recovery.
