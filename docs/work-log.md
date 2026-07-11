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

**Concurrency checkpoint (`conceptify-k9z.7`).** The constrained-provider
simulation overlapped exploration across projects, filled the durable queue,
cancelled both queued and admitted work, resumed queued rows after restart,
retried a failed ask into its original thread, and forced two mutations to save
from the same artifact base. Queue ownership and FIFO/fairness survived capacity
changes; cancellation never spawned late work; restart reused the original run
row and captured base; retry preserved its source lineage; and the stale writer
retained a reviewable candidate instead of overwriting the newer version. The
production frontend build and activity/conflict browser checks stayed responsive,
keyboard-operable, and explicit about every recovery choice. No draft, ask, or
artifact candidate was lost.

## 2026-07-11 — Adaptive response intent

**Response-intent contract (`conceptify-l9w.1`).** Accepted a provider-neutral
v1 contract with four independent dimensions: depth, assumed language, visual
preference, and output shape. It defines plain-language labels and examples,
per-dimension user/project/question inheritance, immutable resolved provenance,
valid cross-combinations, explicit capability fallbacks, a hard text-only
constraint, stable localization keys, and keyboard/zoom/focus requirements.
The ordinary default remains Balanced + Familiar + When useful + Best fit.

**Skill capability catalog (`conceptify-l9w.2`).** Installed skills can now
declare a versioned local sidecar covering outcomes, supported intents, context,
outputs, latency, response-control compatibility, and recommendation signals.
The app discovers future sidecars from supported agent skill directories,
explains missing installations, preserves manual choices, and recommends with
deterministic in-process scoring; ordinary questions return no recommendation.
The Tauri/TypeScript API is ready for the progressive composer.

**Progressive response controls (`conceptify-l9w.3`).** The question folio now
keeps an editorial one-line profile summary in its ordinary fast path and
expands into native-radio depth, language, visual, and shape choices only on
demand. A local Skills section distinguishes suggested from manually chosen
capabilities, explains fit and output changes, supports search and unavailable
states, and offers an explicit no-skill mode. Profiles persist independently on
each staged question. Browser QA covered defaults, visual recommendation,
manual search/choice, no-skill mode, independent multi-question radio groups,
Escape/focus behavior, narrow single-column layout, and a clean console.

**Profile and skill propagation (`conceptify-l9w.5`).** Ask submission now
resolves the local skill policy before creating a thread, validates every
response dimension, and stores the effective profile plus skill id/name/schema
version/selection source on the durable queued run. The agent prompt translates
all four dimensions into explicit, provider-neutral obligations (including a
hard text-only constraint) and states that skills cannot override them. Retry
copies the original resolved metadata; artifact versions snapshot it, with
follow-up versions inheriting provenance when their mutation run has no new
profile. The thread header shows the effective profile and chosen/suggested
skills without exposing raw prompt plumbing. Tests cover every dimension,
retry identity, migration, conflict recovery publication, and skill validation.

**Response preference inheritance (`conceptify-l9w.4`).** User and project
defaults now persist as separate namespaced settings and resolve independently
per dimension over the product default. The composer labels each dimension's
origin, treats edits as question-only, and offers explicit Make my default, Use
for project, Reset to inherited, and clear-scope actions. Async preference load
does not overwrite a question already being edited; saving never clears its
text. Browser QA saved a Deep project default, preserved the active question,
verified inheritance after reload, then cleared it back to Balanced. Existing
thread metadata remains immutable when defaults change.

**Composer expectations (`conceptify-l9w.6`).** A compact note now translates
the selected profile into calm qualitative guidance: focused, balanced, deeper,
visual, capability-assisted, or deep visual treatment. Only the unusually
work-heavy Deep + Prefer visuals combination receives a caution, and it remains
fully submittable. No exact time, token, price, or queue promise is invented.
After submission, queued, provider-wait, submitting, and cancelling rows explain
that the question and history are durable. Browser QA verified live copy changes
from Quick to Deep + Prefer visuals, an enabled submit button, and a clean
console.

**Adaptive-ask checkpoint (`conceptify-l9w.7`).** The same borrow-checking
topic was exercised as Quick + Plain language, Deep + Plain language, Balanced
+ Domain-native + Text only, and Deep + Domain-native + Prefer visuals +
Comparison. The prompt matrix proves depth and assumed language remain
independent and that format constraints materially change agent obligations.
Live composer evidence covered the concise default, keyboard-native radio
groups, automatic recommendation, manual search/override, explicit no-skill,
qualitative expectations, project-default save/reload/clear, and narrow layout.
Backend evidence covered local selection privacy, unknown/unavailable skills,
durable queue metadata, retry identity, artifact provenance, and stale-result
recovery; the contract-accurate thread-header checkpoint displayed the exact
profile and skill schema version on a historical artifact. No raw prompt or
provider detail reached the UI.

## 2026-07-11 — Fast project kickoff

**Quick-start IA prototype (`conceptify-vc1.1`).** Accepted a progressive
single-sheet flow around two user intents: Explore a folder and Learn a topic.
The decision fixes minimum fields, inferred defaults, terminology, exact Create
versus Create & ask destinations, context/readiness summaries, first-launch and
returning focus behavior, and recovery for picker cancellation, invalid or
duplicate folders, creation failure, and topic-only setup. Validation against
the existing sidebar retained its compact strengths while identifying the
implementation-led divider, ambiguous destination, missing readiness, and
missing first ask that the child beads now replace.

**Folder context readiness (`conceptify-vc1.2`).** Folder-backed projects now
appear and select before a local background orientation scan. The bounded scan
detects Git versus folder context, counts the leading languages and inspected
files, excludes common generated/vendor trees, and stops after 5,000 files with
a clear “agents still read relevant files directly” fallback. A lightweight
root/Git fingerprint reuses unchanged results. The selected project exposes an
expandable readiness card with languages, exclusions, privacy/indexing copy,
and any warning; scanning and errors never block navigation.

**Topic-only projects (`conceptify-vc1.3`).** The compact creation path now says
“learn a topic” and “Start topic”; users never need to understand the managed
backing folder. Topic name alone remains sufficient. Progressive optional
context accepts notes, HTTP(S) reference links, and source files with remove
controls and clear privacy copy. Notes/links become a local context document,
selected files are copied into the managed project boundary, links are not
fetched, and clearing context removes its stored/materialized representation.
The resulting project uses the same question, run, and artifact paths as a
folder-backed project.

**Create and first ask (`conceptify-vc1.4`).** Both folder and topic setup now
offer an editable optional first question plus overview, architecture, key
concept, and learning-path starters. Empty keeps the create-only path; non-empty
changes the action and outcome copy to Create/Start & ask. Creation completes
first, then the existing durable ask flow submits exactly once with inherited
response preferences and opens its generating thread. A failed ask therefore
leaves the selected project and durable run available for retry, while the
core's existing title derivation turns an unlabelled starter/edit into a
sensible thread title.

**Project home (`conceptify-vc1.5`).** Selecting a project without a thread now
opens a scannable home instead of a bare instruction. It includes an editable
persistent learning brief, available-context summary and warnings, actionable
active runs, three recent result cards, and editable next-question suggestions
that launch through the same durable ask/profile path. Empty projects explain
the next action; established projects keep their full history in the adjacent
thread list rather than duplicating it on the home.

**Kickoff checkpoint (`conceptify-vc1.6`).** The create-to-submit paths were
walked against a small mixed Rust/TypeScript folder, the bounded 5,000-file
fallback, and a topic with notes/link/source context. From New project, the
folder happy path is three actions after choosing a starter (open, starter,
folder) and topic is four (open, topic, starter, Start & ask); neither opens
Settings. Project selection and durable submission occur immediately after the
native picker/create transaction, while orientation continues independently.
Automated gates prove bounded/unchanged scanning, excluded trees, durable ask
retry, profile inheritance, and project survival after generation failure.
Keyboard-native inputs/radios, Escape cancellation, retained inline errors,
single-column 320px composer layouts, and explicit scanning/queued/provider
states passed the accumulated browser checkpoints. No regression added an
empty intermediate screen or duplicate submit path.

## 2026-07-11 — Contextual artifact exploration

**Selection action menu (`conceptify-9lj.1`).** The shipped selection toolbar
retains Comment and Copy while adding Explain and Deepen on the compact first
row and Simplify, Visualise, Change, and Copy behind More. A truncated readable
target preview keeps the request grounded, with the full quote in its title.
Every action is a native keyboard-reachable control and advances to an anchored
request composer with a useful default request that can be submitted as-is or
edited. The enlarged toolbar keeps the existing viewport-edge clamp and
above/below flip, so scrolled selections remain usable.

**Semantic targets (`conceptify-9lj.2`).** Text and element anchors now carry a
bounded semantic summary for text, blocks, code, figures, images, and diagrams:
a readable label/excerpt, up to eight stable `data-cfy-id` values, and explicit
multi-block state. The original id/offset/quote selectors remain authoritative,
so existing re-attachment and visible “Reference moved” behavior are preserved;
the extra context survives storage and anchor rewrites verbatim.

**Anchored exploration (`conceptify-9lj.3`).** Explain, deepen, simplify, and
visualise open a pinned-target composer with an explicit destination: a compact
answer beside the artifact, the existing Comments conversation, or a durable
new thread. Inline cards persist as anchored comments, show live answer state,
and scroll/pulse their source target. Quick actions store a structured response
intent (depth, language, visuals, shape), and answer agents are instructed to
honor that profile. Cancelling the composer performs no write and starts no run.

**Targeted revision (`conceptify-9lj.4`).** Change—and Redraw for semantic
visual targets—now launches a dedicated proposal run. The agent edits only a
temporary copy and its run-aware save is always intercepted into validated
candidate storage; it cannot resolve the request or publish a version. The
review compares every semantic block against current content, calls out changes
outside the selected `cfy_ids`, and requires Apply or Reject. Apply creates one
immutable version and marks the request applied; Reject creates none. The
post-apply confirmation offers Undo, which creates a restoring version from the
prior artifact and reopens the request. Stale bases keep the existing conflict
review and re-synthesis path, with the synthesis retained for another explicit
preview rather than auto-published.

**Focus and history (`conceptify-9lj.5`).** The bridge now distinguishes warm
saved/open anchors from quiet dotted answered dig-ins in both themes; live text
selection remains stronger and transient, while moved references stay out of
the artifact and retain their warning badge in Comments. A compact history
restores each prior answer together with its target, pulses that anchor on
selection, and supports previous/next buttons plus Alt+Left/Alt+Right traversal.
Only one answer card is expanded at a time, and it can be hidden without
removing its durable history or leaving a filled colour wash over the artifact.

**Live contextual checkpoint (`conceptify-9lj.6`).** The real built shell,
production IPC bridge, production HTTP server, current CLI, real app database,
and real Claude runs exercised text, paragraph, code, and nested diagram-node
targets in light and dark themes. Explain/Deepen/Simplify, inline and new-thread
destinations, answer history/Alt+Arrow navigation, target pulse, Comment cancel,
Copy, diagram Redraw, proposal diff, Reject, Apply, Undo, and post-undo
reattachment all passed. Captured states included both-theme selection and
composer views, inline running/answered cards, diagram actions, and revision
review.

The checkpoint found and fixed three integration defects: iframe focus loss
could race an opened composer, diagram clicks bypassed the action toolbar, and
an older live-harness CLI did not forward run provenance so a proposal published
implicitly. Composer intent is now synchronously pinned, semantic elements use
the full action menu (without text-only Copy), mutation prompts carry an explicit
run-id prefix in addition to inherited environment, and Undo re-runs normal
anchor reattachment after reopening the request. The accidental live saves were
restored through immutable versions; the final reopened request is anchored on
the restored latest version.

## 2026-07-11 — Layered visual exploration

**Interactive diagram drill-in (`conceptify-dqb.3`).** Anchored SVG nodes,
edges, groups, and diagram roots are now discoverable by pointer and keyboard:
the bridge adds a zero-specificity pointer affordance, a visible focus ring,
Enter/Space activation, and wrapping arrow-key traversal within each diagram.
Node clicks are consumed in capture phase so a diagram inspection cannot also
trigger a slide artifact's click-to-advance behavior. Existing links and form
controls retain their native interaction.

Element anchors now preserve optional diagram role and relationship summaries
derived from explicit `data-cfy-*` metadata, ARIA descriptions, and common
Graphviz-style node/edge classes and titles, with readable id/text fallbacks
when metadata is absent. The shell presents that context with Explain, Deepen,
Compare, Comment, and Redraw actions; Compare uses the structured comparison
response profile, while all answers and revision previews retain the original
node anchor. A browser bridge probe verified focus order, node/edge metadata,
pointer and keyboard activation, and zero accidental slide advances.

**Layered artifact structure (`conceptify-dqb.1`).** The authoring contract now
requires STANDARD/DEEP artifacts to lead with a concise, always-visible
orientation and core explanation, reserve native disclosures for already-
generated optional depth, and provide a sticky semantic hash-link outline.
Linked targets carry matching native `id` and stable `data-cfy-id` values, so
the same document navigates correctly in a standalone browser with no bridge.

The bridge enhances that baseline with active-location state, disclosure
opening before deep-link navigation, and hash/back/forward restoration. Native
details preserve keyboard and assistive-technology semantics; browser search
still finds closed content, and `beforematch` opens its containing disclosure
where supported. Print hides the outline and exposes all deep-dive bodies. The
reference artifact browser probe verified sticky layout, initial/current
outline state, deep-link opening, history restoration, native target ids,
closed-content search, and coherent print rules.

**Visualization intent (`conceptify-dqb.2`).** The response profile now asks
what relationship a visual should clarify: compare, sequence, relationships,
hierarchy, measured values, or an interactive model, while preserving automatic
selection and the existing hard Text only constraint. A specific purpose
requests visuals and aligns answer shape for comparisons/sequences. The same
purpose picker appears in anchored Visualise actions, and both paths persist the
choice with the source question/anchor.

The provider-neutral run contract carries `visual_purpose` through inherited
preferences, submissions, retries, run/artifact provenance, and prompt
generation; stored v1 profiles without the additive field default safely to
`auto`. Prompts map each purpose to the smallest fitting supported format,
require accessible descriptions and textual/static fallbacks, preserve exact
chart values, and explicitly reject decorative diagrams. Unsupported forms
must be called out briefly rather than silently substituted.

**Next questions and trails (`conceptify-dqb.4`).** STANDARD/DEEP artifact
authors now provide two to four semantic next branches—example,
counterexample, mechanism, trade-off, or prerequisite—with an editable question
and a concise reason, rather than generic “more detail” filler. Save-time
extraction persists these hints without another model run; a newer artifact
supersedes only still-active hints while preserving launched/dismissed history.

Suggestions are reusable from both the artifact and project home. Choosing one
only fills an editor: no work starts until Launch/Ask, and the edited wording is
what the new thread receives. Readers can dismiss weak branches, inspect their
source, and backtrack from a launched thread to the exact source
thread/version/anchor and reason. A bridge probe verified the suggestion
gesture emits only its bounded semantic payload and cannot also comment or
advance an artifact. Domain tests cover extraction, invalid-branch rejection,
editing, dismissal, same-project launch validation, and durable backtracking.
