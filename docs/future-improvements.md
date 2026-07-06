# Future improvements

Candidate features beyond the current work, roughly in priority order. Each
should become a bead (or epic) when picked up — this file is the parking lot,
not the tracker.

## 1. ⌘K quick switcher + full-text search (FTS5)

As artifacts accumulate, the app becomes a personal knowledge base — but the
only navigation is project → thread list. Add SQLite **FTS5** over thread
titles, initial questions, artifact text content, comment bodies, and agent
answers, surfaced through a **⌘K quick switcher** (jump to project/thread) and
a search results view (hit → thread, scrolled to the matching section/comment
where possible). Worth building *before* content piles up. (PRD Phase 2 lists
this; index maintenance hooks belong in `save-artifact` and the comment
mutation paths.)

## 2. Model/depth picker at the point of asking — **picked up: epic `conceptify-e7m`**

Per-purpose models exist in settings (`followUp` / `artifactUpdate` /
`inAppAsk`) but are invisible in the moment. Add a small picker on the ask
composer, "Ask follow-ups", and "Apply to artifact" actions — e.g. *quick*
(haiku), *standard* (sonnet), *deep* (opus/fable) — overriding the per-purpose
default for that one run. Cheap: the adapter template already takes `{model}`
per invocation; the flow commands just need an optional model override
parameter plumbed through to `settings.resolve()`.

## 2b. Local / self-hosted models via a LiteLLM proxy

Provider-routed execution (bead `conceptify-e7m.7`) covers anthropic-native,
openai-native, and everything-else-via-OpenRouter. Local or self-hosted models
(Ollama, vLLM, …) were explicitly scoped OUT of that bead: they would ride the
same claude-CLI env mechanism (`ANTHROPIC_BASE_URL` pointed at a local LiteLLM
proxy exposing an Anthropic-compatible endpoint, per-run env already supported
by `StartRun::env` + `routing.rs`), but need a base-URL + optional-key settings
surface per endpoint and a way to attribute catalog entries to a local
provider. Cheap to add on top of the routing layer when wanted: a new
`RouteTag`/route arm with a configurable base URL instead of the hardcoded
OpenRouter one.

## 3. Artifact version diffing

After an apply run, v1 → v2 happens live but there's no way to see *what
changed* — a trust gap in the apply loop. Even a simple view would help:
changed-section highlighting in the viewer (diff the two HTML files
server-side, mark changed `data-cfy-id` blocks), or a side-by-side text diff
as a first cut. PRD lists visual diff as P2/nice-to-have (FR-2.4).

## 4. Streaming answers into the sidebar

Follow-up answers currently land wholesale per comment (`comment-updated`
fires when `resolve-comment` completes). Streaming the answer text
token-by-token into the sidebar row would make the interrogation loop feel
alive. Requires the headless agent to emit partial answers (or the run
`stream-json` events to carry assistant text deltas that the run-progress
pipeline forwards to a per-comment buffer). Polish, not structure — do after
the conversational-interrogation epic lands.
