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

## 2. Local / self-hosted models via a LiteLLM proxy

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

## 5. Structured codex run-progress parsing

The `codex` adapter ships plain stdout passthrough: `codex exec` writes its
human-readable transcript to **stderr** and only the final message to stdout,
so `classify_line` degrades codex stdout to `kind: "output"` events and the
frontend shows the elapsed clock + log tail instead of claude-style progress
kinds (tool-use, thinking, …). This was deferred from bead `conceptify-w9e`
part 2 because codex's `--json` JSONL event schema is experimental and
unversioned on codex-cli 0.142.0 — parsing it now would couple the run engine
to a shape that will churn, and raw JSONL in the run log would make the
FR-4.8/FR-5.3 failure log-tail unreadable. When that schema stabilizes, add an
adapter-aware parse mode in `runs.rs` (claude `stream-json` vs codex JSONL)
keyed off the routed adapter, so codex runs surface real progress kinds while
the log stays human-readable. See the `codex` adapter block in settings.rs
`default_adapters()` for the verified 0.142.0 stream shape and the reasons
`--json` was rejected for v1.
