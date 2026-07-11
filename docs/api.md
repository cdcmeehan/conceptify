# Conceptify local HTTP API

Reference for the local HTTP API embedded in the Conceptify app (PRD §5.1,
§7.8). This is the one source of truth for endpoint shapes shared by the
`conceptify` CLI, the Claude Code skill, and the app's own frontend — extend
this doc alongside any route you add.

## Transport & discovery

- The server binds `127.0.0.1:4477` by default. It never listens on any
  non-loopback interface.
- If `4477` is taken, it probes the occupant's `GET /health`. If the
  occupant identifies itself as Conceptify (`"service":"conceptify"` in the
  JSON body), this process defers to it and does not start its own server
  (this should only happen if the single-instance guard is bypassed).
  Otherwise it walks `4478..=4487` looking for a free port.
- The actually-bound port is written to
  `~/Library/Application Support/conceptify/port` (plain text, just the
  port number) so the CLI and other local tools can discover it without
  guessing.

## Auth (PRD §9 S1)

- A random 32-byte token is generated on first run and persisted to
  `~/Library/Application Support/conceptify/token`, mode `0600`. It is
  reused across restarts.
- Every route **except `GET /health`** (at either path below) requires:

  ```
  Authorization: Bearer <token>
  ```

  Missing header, wrong scheme, or wrong token → `401 Unauthorized`.
- This is containment against other local processes / browser-page
  localhost probing, not adversarial hardening — the threat model is a
  single-user machine (PRD §9).

## Versioning

All endpoints beyond health live under `/api/v1/`. This path is versioned
from day one; breaking changes get a new `/api/v2/` rather than mutating
`/api/v1/` in place.

## Events

Mutating endpoints emit Tauri events (via the shared `AppHandle`) so the
webview updates live instead of polling. The frontend subscribes with
`@tauri-apps/api/event`'s `listen()`. Event names so far:

| Event | Emitted by | Payload |
|---|---|---|
| `api-ping` | `GET /api/v1/ping` (demo/health-check route) | `{ message: string, unix_ms: number }` |
| `projects-changed` | `POST /api/v1/projects/ensure`, `PATCH /api/v1/projects/:id`, `PUT /api/v1/projects/:id/archive` | `null` (no payload) |
| `thread-created` | `POST /api/v1/threads` | `{ project_id: string, thread_id: string }` |
| `artifact-updated` | `POST /api/v1/threads/:thread_id/artifact` | `{ project_id: string, thread_id: string, version: number }` |
| `comment-created` | `POST /api/v1/comments` (a comment or a reply) | `{ project_id: string, thread_id: string, comment_id: string }` |
| `comment-updated` | `PATCH /api/v1/comments/:id`; `POST /api/v1/threads/:thread_id/artifact` (one per re-attached comment); `POST /api/v1/comments` when a reply re-opens its root | `{ project_id: string, thread_id: string, comment_id: string, status: string }` |
| `navigate` | `POST /api/v1/open` | `{ project_id: string, thread_id: string \| null }` |
| `run-progress` | agent-run engine (`runs` module, not an HTTP route) | `{ run_id: string, thread_id: string, kind: string, detail: string }` |
| `run-state-changed` | durable run scheduler | `{ run_id: string, thread_id: string, status: string }` |
| `run-finished` | agent-run engine (`runs` module, not an HTTP route) | `{ run_id: string, thread_id: string, status: string }` |
| `thread-updated` | flow layer (`flows` module, not an HTTP route): thread status changes owned by the run lifecycle — apply-mode `updating` ↔ `ready`, and in-app-ask generation `generating` → `error` (or `generating` again on Retry), PRD §4 | `{ project_id: string, thread_id: string, status: string }` |
| `settings-changed` | `set_agent_settings` / `reset_agent_settings` (Tauri commands, not HTTP) | `null` (no payload) |

`artifact-updated` is the viewer's live-refresh trigger (PRD N2: save →
visible refresh < 500ms): the frontend reloads the artifact iframe for
`thread_id` at `version` when it arrives.

`comment-updated` is the live-sidebar trigger for every mutation to a comment —
status transitions (the M5 `resolve-comment` flow), landing `answer_html`, and
the `anchor_state` "reference moved" flip. Besides `PATCH /comments/:id`, the
save-artifact endpoint emits it once per comment its re-attachment pass
changed (advanced to the new version, anchor repaired, and/or flagged
`moved`), and `POST /comments` emits it for a **root re-opened by a user reply**
(epic conceptify-6xi: the reply's own `comment-created` fires alongside a
`comment-updated` carrying the root's id and `status: "open"`). Symmetrically,
a `PATCH` that answers the **latest reply** of a chain also flips its re-opened
root back to `answered` in the same transaction (root status reflects the
latest exchange state; the root's own `answer_html` is untouched) and emits a
second `comment-updated` carrying the root's id and `status: "answered"`.
Answering an earlier (superseded) reply does not flip the root. It generalizes
what PRD §5.1 sketched
as `comment-resolved`: one event covers all comment mutations (consistent with
`artifact-updated`), and the frontend scopes its refetch by the `project_id` /
`thread_id` in the payload.

### Run events (headless agent runs)

`run-progress` / `run-finished` come from the durable headless run scheduler
(`src-tauri/src/runs.rs`, PRD §5.1 agent spawner / FR-4.8), not from an HTTP
endpoint. Submission creates a `queued` row before returning; provider-aware
admission drives `queued → starting → running`. A genuine provider limit moves
the same row through `throttled → queued` after `not_before`, releasing its
worker slot. Queued/throttled rows resume after app restart; interrupted
starting/running work fails with `status_reason = app_interrupted` and is never
replayed implicitly.

- `run-progress` — one per agent **stdout** line (the `claude` adapter emits
  `--output-format stream-json`, one JSON object per line). `kind` is the
  line's raw `type` field (`"output"` for non-JSON lines); `detail` is the
  line's `subtype` when present, else the raw line truncated to 200 chars.
  Deliberately shallow parsing — richer rendering is a frontend concern.
  stderr lines are not forwarded (they land in the run log only).
  **One structured exception:** for a `rate_limit_event` (which has no
  `subtype`), `detail` carries the nested `rate_limit_info` object as compact
  JSON (`{"status":…,"isUsingOverage":…,"resetsAt":…,…}`) instead of the raw
  line, so the frontend can parse it and decide whether to surface it. The
  frontend drops the purely informational `status: "allowed"` heartbeats and
  shows only genuine limiting; the display filtering/formatting policy lives
  there (one place), never in the core.
- `run-state-changed` — emitted after each durable scheduler transition that
  matters to live UI (`queued`, `starting`, `running`, `throttled`, and a
  cancellation request). Unlike stdout progress, this fires even for a silent
  child, so queue-versus-running labels cannot drift.
- `run-finished` — exactly once per run, emitted **after** the DB row reached
  its terminal state. `status` is one of `completed` (exit 0), `conflicted`
  (a stale mutation result retained without publication), `failed`
  (nonzero exit / spawn failure / abnormal supervision end), `cancelled`
  (user cancel), `timeout` (the FR-5.3 timeout killed the process group).
  Treat `failed` and `timeout` uniformly as the FR-5.3 error class.

The full transcript of every run is retained at
`~/Documents/conceptify/artifacts/<project-id>/threads/<slug>/runs/<run-id>.log`
(§5.6), with `[out]`/`[err]` stream tags and `[run]` lifecycle markers.
Cancellation is exposed to the frontend as the `cancel_run` Tauri command
(`invoke('cancel_run', { run_id })`), which SIGKILLs the run's whole process
group. Cancelling queued/throttled work marks it terminal without spawning;
cancelling running work releases capacity only after the process is reaped.
Exploration runs may overlap on one thread. Mutations for the same thread are
serialized and retain their captured base version for conflict checks.

Mutation children inherit an opaque `CONCEPTIFY_RUN_ID`; the CLI forwards it
as `X-Conceptify-Run-Id` only on `save-artifact`. The server resolves the
captured base from the durable row, never from a child-supplied version. If it
differs from current, validated HTML is retained as
`runs/<run-id>.candidate.html`, the run becomes `conflicted`, and save returns
`409` with `code: "STALE_BASE"`, `run_id`, `base_version`, and
`current_version`. No artifact row/version or update event is created, and
supervisor finalization preserves `conflicted` despite the CLI's nonzero exit.
Because agent CLIs may sanitize environment variables before tool subprocesses,
generated mutation prompts also prefix their final save command with the same
non-secret run id. The inherited environment and explicit command prefix are
deliberate redundant paths to the identical authenticated provenance header.
Contextual revisions use the same validated candidate store even on a current
base (`status_reason: preview_required:<comment-id>`): this is a review state,
not an overwrite. A stale contextual proposal becomes `stale_preview` and a
rebase is itself retained for a fresh preview.

### Flow commands (Tauri, not HTTP — FR-4.6/4.7/4.8)

The follow-up flows live in `src-tauri/src/flows.rs` on top of the run engine
and are invoked by the app shell as Tauri commands (snake_case args):

- `ask_follow_ups { thread_id, run_override? }` → `{ run_id, thread_id,
  mode: "answer", target_comment_ids }` — FR-4.6: ONE headless run answers every open **root**
  comment via `conceptify resolve-comment`; the artifact is never modified in
  this mode. Targets are open roots only (a root re-opened by a reply is
  included; its replies ride along as that root's exchange history, not as
  separate targets — epic conceptify-6xi); `target_comment_ids` is the root
  ids. Errors (user-facing strings): no artifact or no open comments.
- `ask_single_comment { thread_id, root_comment_id, run_override? }` → same shape with
  `mode: "answer"` — epic conceptify-6xi "Ask now": fire a single-root answer
  run immediately without gathering the batch. The target must be a **root**
  (not a reply) and **open** (re-opening by a reply counts). `target_comment_ids`
  is the single root id; the prompt directs the agent at the latest unanswered
  message in that root's chain, so the actual `resolve-comment` may land on a
  reply row. Errors (user-facing strings): no artifact; comment not found on
  this thread; target is a reply (reply to its root instead); target root not
  open.
- `apply_to_artifact { thread_id, comment_ids, run_override? }` → same shape with
  `mode: "apply"` — FR-4.7: `comment_ids` empty targets every **answered root**
  (roots only — `resolve-comment --applied` on a reply is rejected, so answered
  replies are never apply targets); explicit ids may be `open` or `answered`
  (never `applied`, never a reply). The run edits a working copy, marks each
  target `applied`, then publishes ONE new version via `conceptify
  save-artifact`. Exception: an anchor carrying `exploration.action: "change"`
  selects proposal mode—the comment stays open, the candidate is retained, and
  publication is possible only through the explicit review command.
- `get_active_run { thread_id }` → the newest non-terminal run summary
  (`{ run_id, thread_id, mode, status }`, where status is `queued`, `starting`,
  `running`, `throttled`, or `cancelling`) or `null` — the UI
  re-attaches to an in-flight run on thread switch. Target ids are not
  persisted, so a re-attached run renders indeterminate progress.
- `list_run_activity {}` → the global activity read model, with project/thread
  identity, mode/status/model/provider, queue position, queue/execution/finish
  timestamps, failure reason, and retry lineage for every active run. It also
  returns undismissed failures/timeouts/conflicts until the user acts and keeps
  ordinary completed/cancelled work visible for 15 minutes. The shell uses
  this to make work locatable across projects without exposing run logs.
- `dismiss_run_activity { run_id }` → `true` when a terminal run was dismissed,
  `false` for an unknown or still-active run. Dismissal is durable; active runs
  cannot be hidden. The shell may call this once per row to clear recent
  completion history.
- `mark_run_activity_seen { run_ids }` → number of terminal rows marked seen.
  Opening the tray calls this for visible unseen terminal work; the durable
  marker suppresses the in-app unread badge across reloads without dismissing
  failures or conflicts that still need action. Active rows are never marked.
- `claim_system_run_notification { run_id }` → a privacy-filtered notification
  routing record (`run/project/thread ids`, project name, status/reason), or
  `null`. The atomic claim succeeds once for completed/failed/timeout/conflict
  rows, providing at-most-once native delivery across duplicate lifecycle
  events and restarts. It is called only after opt-in and an OS permission check.
- `get_conflict_review { run_id }` → retained candidate metadata and a semantic
  diff from newer current content to the candidate: project/thread,
  agent/model/route, captured base, current version, resolution state, and the
  same block/hunk shape as `diff_versions`, plus `kind` (`revision` or
  `stale_base`) and the selected target ids used to identify spillover. Pending
  conflicts/revisions cannot be dismissed from activity.
- `rebase_conflict { run_id }` → a new queued `apply` run linked through
  `retry_of_run_id`. It captures the now-current base, preserves the source
  model/profile, and synthesizes candidate intent onto current content. The
  source records `rebase:<new-run-id>`.
- `publish_conflict_candidate { run_id }` → new artifact version number. This
  explicit, confirmed recovery publishes the retained candidate as a separate
  next version and records `source_run_id`, original `source_base_version`, and
  `resolution: "separate"`. It never runs automatically.
- `reject_conflict_candidate { run_id }` → boolean. Records an explicit reject
  and dismisses the candidate without creating an artifact version.
- `restore_artifact_version { thread_id, version, run_id? }` → new artifact
  version number. Copies the chosen historical content into a new latest
  version (immutable history is retained); when undoing a reviewed revision,
  the source request is reopened.
- `get_run_log_tail { run_id, max_lines? }` →
  `{ run_id, log_path, lines }` (default last 30 lines) — FR-4.8 failure
  surfacing; `log_path` is always returned even when the file is unreadable.

The in-app ask flow (FR-5.1/5.2/5.3, bead 959.1/959.2) adds three more:

- `ask_from_app { project_id, title?, question, run_override?, response_intent,
  skill_mode, selected_skill_ids }` → `{ run_id,
  thread_id }` — FR-5.1: creates a thread (status `generating`) and spawns ONE headless
  `ask` (`mode: "ask"`, `Purpose::InAppAsk`) generation run whose contract is
  to author an artifact per the installed Conceptify skill and publish it via
  `conceptify save-artifact` into that thread. `title` is optional (derived
  from the question's first words when blank). `cwd` = project root. Errors
  are returned before queueing when validation fails. The response-intent v1
  object carries `depth`, `language`, `visuals`, `shape`, and additive
  `visual_purpose` (`auto | compare | sequence | relationships | hierarchy |
  values | interactive`; missing on older stored v1 objects defaults to
  `auto`). `skill_mode` is `auto`, `none`,
  or `manual`; automatic selection runs locally before thread creation, while
  unknown/unavailable manual skills fail visibly. The resolved profile and
  versioned skills become immutable run metadata and explicit agent
  instructions. Errors (user-facing strings): invalid profile/skill, empty
  question, unknown project, missing agent/CLI.
- `retry_ask { thread_id }` → `{ run_id, thread_id }` — FR-5.3: re-spawns the
  SAME question into the SAME thread, moving it back to `generating`. A fresh
  `follow_up_runs` row is created; the failed run's history stays on disk.
  **Takes no `run_override`** — it reuses the ORIGINAL run's persisted override
  (see below), so a retried generation runs with the same adapter/model choice
  the user made, robustly across app restarts. It also reuses the original
  resolved response profile and skill versions; retry never re-runs current
  recommendations or silently adopts changed preferences.
- `get_latest_run { thread_id }` → `{ run_id, mode, status, model, route,
  overridden }` or `null` — the most recent run row for a thread (any
  mode/status). The FR-5.3 error state uses it to resolve the failed
  generation run's id for `get_run_log_tail`; unlike `get_active_run` (live
  runs only) it works after an app restart. `model` and `route` (`anthropic |
  openai | openrouter | manual`, `null` on pre-routing rows) are the failed
  run's resolved selection, shown on the error state; `overridden: true`
  means a per-run override was recorded — Retry re-applies it verbatim —
  while `false` means Retry re-derives the *current* settings defaults, so
  the UI only promises reuse when an override actually exists.

Learning-path commands are local and explicit:

- `list_learning_suggestions { project_id }` returns active semantic branches
  extracted from saved artifacts, including source thread/version/anchor,
  branch kind, editable question, and why it relates.
- `dismiss_learning_suggestion { id }` hides one low-value branch without
  launching work.
- `record_learning_trail { suggestion_id, launched_thread_id,
  edited_question }` marks an explicitly launched branch and stores the edited
  wording as a durable source→destination link. Both threads must share a
  project.
- `get_learning_trail { thread_id }` returns the source thread/version/anchor,
  branch, question, and reason for backtracking, or `null` for a root thread.

**Per-run override (`run_override?`, epic conceptify-e7m):** every run-starting
flow command (`ask_follow_ups`, `ask_single_comment`, `apply_to_artifact`,
`ask_from_app`) accepts an optional `run_override` object `{ adapter?, model? }`
that overrides the configured defaults for that ONE invocation without mutating
stored settings. Fallback chain, per field independently: explicit override →
per-purpose model (`models.for_purpose`) / `default_adapter`. **Omitting it (or
sending `{}`) is byte-identical to the pre-override behavior.** The override is
**model-centric**: `model` is the primary choice; `adapter` is an advanced
escape hatch (provider-routed execution, bead conceptify-e7m.7, normally derives
the adapter FROM the model's provider). Validation: `adapter` must name an
existing adapter (else an `UnknownAdapter` error); `model` must be non-empty and
free of whitespace/control characters (else an `InvalidModel` error) — both
rejected before any run row is created. The RESOLVED adapter/model are recorded
on the `follow_up_runs` row's `agent`/`model` columns, and the override itself
is persisted in the new nullable `override_json` column (`NULL` for an
override-free run) so `retry_ask` re-applies it without the frontend re-passing
it. The adapter/model picker the UI reads for this comes from `get_agent_options`
(below) plus the live model catalog (bead conceptify-e7m.6).

**Provider-routed execution (bead conceptify-e7m.7):** the user picks a
**model**; the run engine derives the execution path from the model's provider
(`src-tauri/src/routing.rs`, applied inside `start_run` before any row exists):

| model's provider | route tag    | adapter  | per-run env |
|------------------|--------------|----------|-------------|
| anthropic        | `anthropic`  | `claude` | none (today's native path) |
| openai           | `openai`     | `codex`  | none (native codex path)   |
| anything else    | `openrouter` | `claude` | `ANTHROPIC_BASE_URL=https://openrouter.ai/api`, `ANTHROPIC_AUTH_TOKEN=<stored OpenRouter key>`, `ANTHROPIC_API_KEY=` (explicitly empty) |

Provider derivation (decided + tested, in order): a **slash-form** id
(`vendor/model`, incl. `~vendor/...` aliases) is an OpenRouter slug → the
OpenRouter route unconditionally (even `anthropic/...` — the user picked the
OpenRouter-namespaced catalog entry; native runs use bare ids); else an exact
**catalog lookup** (disk cache/snapshot, never network); else **prefix
heuristics** for custom ids (`claude-*`/`sonnet`/`opus`/`haiku` → anthropic;
`gpt-*`/`codex-*`/`chatgpt-*`/`o<digit>…` → openai); anything else fails fast
with an actionable `UnroutableModel` error — never a guessed route. A bare id
the catalog attributes to a non-native provider is likewise unroutable (bare
ids are not OpenRouter slugs); the error suggests the `vendor/model` form.

Bypass precedence (highest first): (1) an explicit `run_override.adapter` — the
advanced escape hatch: routing is skipped entirely, **no env is injected**;
(2) a custom (non-built-in) `defaultAdapter` — the user configured their own
harness, routing respects it verbatim. Both record route tag `manual`. With
either built-in as `defaultAdapter`, the model alone decides (e.g.
`defaultAdapter: codex` + a `claude-*` model still routes to the claude CLI).

The OpenRouter route was verified live against claude CLI 2.1.201: `--model`
passes the slug through verbatim (no `ANTHROPIC_MODEL` remap needed);
`ANTHROPIC_AUTH_TOKEN` becomes the `Authorization: Bearer` header; the env is
per-child-process only, so the user's normal claude login is untouched on every
other route. Routing failures happen **before** any run row exists (FR-4.9
guard freed): `OpenRouterKeyMissing` ("add one in Settings") when an
openrouter-routed run has no stored key, `UnroutableModel` as above.

**Route visibility:** the resolved route tag (`anthropic` | `openai` |
`openrouter` | `manual`) is recorded on the run row's new nullable `route`
column (`NULL` = pre-routing row) and in the run-log header
(`route=… [base_url=…]`) — always token-free. The OpenRouter key itself never
appears in any log line, event payload, error string, command response, or DB
column (test-proven); it reaches the child process env only.

**Exchange-history prompts (epic conceptify-6xi):** the answer prompt (both
`ask_follow_ups` and `ask_single_comment`) renders each targeted root as an
exchange transcript — the root body with its anchor, any answer already given,
then each reply in order with its `[status]` and any answer. The agent is told
to address the LATEST unanswered message in each chain, build on prior answers
without repeating them, and `resolve-comment` against that message's id (the
reply row when answering a reply, the root row for a fresh root). The context
aggregation supplies this via `open_comment_threads` (open roots + ordered
reply chains); the flat `open_comments` — which now also carries open replies —
is no longer used for targeting.

**Apply ordering (FR-4.7 × FR-4.4):** the apply prompt instructs the agent to
finish all edits, then run `resolve-comment --applied` for **every** target
comment, then `save-artifact` exactly once, last. `applied` comments are
frozen at their capture version and excluded from save-time re-attachment, so
this order keeps re-anchoring away from the very text the apply rewrote. The
ordering is prompt-enforced (marking comments applied server-side before the
run could succeed would lie on failure); an agent that saves first anyway
degrades gracefully — re-attachment just processes the not-yet-applied
targets (noisier, never corrupting).

**Thread status:** apply runs set the thread to `updating` on start and
conditionally restore `ready` when the run terminates (a `ready` already set
by the agent's mid-run `save-artifact` is never regressed); each change emits
`thread-updated`. Answer runs never touch thread status, and **no follow-up
run ever sets thread status `error`** — that state belongs to generation runs
(FR-5.3); follow-up failures are surfaced by the run-status UI instead.

**Ask (generation) status (FR-5.2/5.3):** an in-app ask leaves the thread
`generating` until the agent's mid-run `save-artifact` flips it to `ready`
(that endpoint owns the `→ ready` transition and emits `artifact-updated`,
which swaps the viewer in). A completion watcher then does ONE conditional
`generating → error` transition on any terminal run outcome — this both
surfaces a crash / timeout / cancel AND treats a run that exited 0 but never
saved (completed-without-artifact) as an error, while never regressing a
`ready` the save already set (the conditional no-ops from `ready`). N4: a
start that fails *after* the thread row exists (missing CLI, gone cwd, spawn
failure) also flips the thread to `error`, so a generation is never stranded
in `generating`. Retry re-enters `generating` and re-spawns.

Every durable state transition emits `run-state-changed`. The shell refetches
the global activity read model from that event rather than reconstructing
queue truth client-side. Project rows signal active background work, thread
rows distinguish queued position from generating/answering/applying/provider
wait/cancelling, and the global tray offers only valid jump, cancel, retry, and
dismiss actions. Its polite live region announces aggregate active and
attention counts, avoiding per-progress-message screen-reader noise.

**Child environment:** the flow layer resolves the `conceptify` CLI
(`CONCEPTIFY_CLI` env override → binary sibling of the app executable →
login-shell `which`, cached) and prepends its directory to the spawned
agent's `PATH` — Finder-launched GUI apps inherit a minimal `PATH` (PRD
§5.1), and the prompts' contract is that plain `conceptify` works.

**Permission scoping (PRD §12 OQ3, decided by b12.8):** the default `claude`
adapter template (`src-tauri/src/settings.rs`, `default_adapters()`) runs
headless as `--permission-mode acceptEdits` **plus** `--allowedTools Bash
Edit Write` — required, because in `-p` (print) mode `acceptEdits` alone
auto-approves only a safelist of read-only Bash commands, and everything the
flows depend on (`conceptify …`, `mktemp`, `d2`/`dot`/`node` renders,
temp-dir writes outside the cwd) would be denied headlessly. Subtracted from
that via `--disallowedTools` (denies always beat allows):

- `WebFetch` / `WebSearch` — flows are grounded in local code + the artifact;
  web tools are unneeded accident/injection surface.
- Mutating git (`commit`, `push`, `add`, `rebase`, `merge`, `reset`,
  `checkout`, `switch`, `restore`, `stash`, `clean`) — no flow touches the
  repo's history or working tree; git *reads* (`log`/`diff`/`blame`/`grep`)
  stay available for grounding.
- `Edit(/{project_root}/**)` + `Write(/{project_root}/**)` — the target repo
  is **read-only in every mode**: answer runs write only sidebar answer
  files, apply/ask runs edit a temp working copy, and artifact-dir writes go
  through the CLI/server, never the agent's file tools.

`--strict-mcp-config` keeps user-configured MCP servers out of headless runs.
This is §9 right-sized containment (hygiene against accidents, not
adversarial sandboxing): rejected alternatives — `bypassPermissions` (no
containment), a fine-grained Bash whitelist (prefix rules break on pipes/env
assignments and every headless denial is a wasted flail), `--tools`
whitelisting (brittle enumeration of built-ins), per-purpose arg overrides
(a new adapter dimension for a prompt-forbidden, versioning-recoverable
accident), and `sandbox-exec`/network firewalls (adversarial-grade, out of
scope). All behavior verified against claude CLI 2.1.201 with headless
probes; the prompts tell the agent its toolset is scoped so it does not
flail against denials.

**Codex adapter scoping (bead e7m.2, verified against codex-cli 0.142.0):**
the built-in `codex` adapter runs `codex exec` (implicitly approval-`never`
headless; sandbox denials return to the model) with `--sandbox
workspace-write`: kernel-enforced (Seatbelt) filesystem containment where
only the project root, `/tmp` and `$TMPDIR` are writable — verified live
(`$HOME` writes denied, `mktemp -d` scratch writes fine). Network under
`workspace-write` is **fully blocked by default** — loopback connects
refused, DNS fails, local binds `EPERM` — so the template pins
`-c sandbox_workspace_write.network_access=true` (verified live: loopback +
external open with it), without which the loopback `conceptify` CLI
reporting path breaks in every flow. The resulting open network matches the
claude template's effective posture (denied `WebFetch`/`WebSearch`, but
allowed `Bash` can `curl`); codex's native `web_search` tool stays off
(opt-in `--search`, not passed). Probing caveat: codex's Seatbelt cannot
nest inside another sandbox, so probes launched from an already-sandboxed
shell observe a degraded codex sandbox (network wrongly appears open) — all
of the above was verified from an unsandboxed parent, which is how the app
spawns agents. Mutating git is
**not** per-command deniable on codex (verified: `git commit` succeeds in
the sandbox), so it remains prompt-enforced — the one residual scoping
difference from the claude template. Hygiene flags: `--skip-git-repo-check`
(project roots need not be git repos), `--ephemeral` (no session files piled
into `~/.codex`), `--ignore-user-config` (the `--strict-mcp-config` analog —
keeps personal MCP servers/plugins/notify-hooks out of headless runs; auth
still comes from `CODEX_HOME`), `--color never` (escape-free logs), and a
`--` separator so the prompt can never parse as a flag/subcommand. Output
parsing: **plain stdout passthrough** (no `--json` — its event schema is
experimental): codex writes its transcript to stderr (run log `[err]` lines)
and only the final message to stdout, so `run-progress` degrades to
`kind: "output"` and the UI shows the elapsed clock + log tail. Full
rationale in `settings.rs` `default_adapters()` docs.

Read-only routes (`GET /api/v1/debug/db-check`, `GET /api/v1/projects`,
`GET /api/v1/threads`, `GET /api/v1/threads/:id/context`, `GET /api/v1/comments`)
emit no events.

### App-shell commands (Tauri, not HTTP — FR-1.2 / FR-7.x, thread hygiene)

More app-only Tauri commands (`src-tauri/src/commands.rs`), invoked by the shell
with snake_case args. Like the other command mutations they emit **no** Tauri
event — the invoking window refetches after awaiting (the axum routes are the
cross-surface event source; there is no CLI/API equivalent for these yet). The
settings commands are the exception (they emit `settings-changed`, above).

**Project creation (FR-1.2 / UC6, beads 959.3):**

- `ensure_project { root_path, name? }` → `{ id, name, root_path, created }` —
  map an existing directory as a project (native dir-picker path). Thin wrapper
  over the same `projects::ensure_project` canonicalize → find-or-create path as
  `POST /api/v1/projects/ensure`; picking an already-mapped directory lands on
  the existing project (`created: false`), never an error. `name` overrides the
  display name (the picker leaves it unset → directory name). The frontend gets
  `root_path` from `@tauri-apps/plugin-dialog`'s `open({ directory: true })`.
- `scan_project_context { project_id }` → `{ status, repository, languages,
  included_files, excluded_paths, fingerprint, scanned_at, warning, unchanged
  }` — performs a local, bounded orientation scan after the project already
  appears. It skips generated/vendor directories, stops at 5,000 files with a
  non-blocking fallback, and reuses stored results when the root/Git fingerprint
  is unchanged. This is explicitly not a content index; raw file contents and
  paths are not transmitted.
- `create_project_folder { name }` → same shape — "create a folder for me": makes
  a fresh slugified, disk-deduped folder under the configured auto-project base
  dir (FR-7.3, default `~/Documents/conceptify/projects`) and maps it, with
  `name` as the display name. Errors (strings): empty name, unresolvable base
  dir, mkdir failure.
- `get_topic_context { project_id }` / `set_topic_context { project_id, notes,
  links, files }` → `{ notes, links, files }` — optional topic-only context.
  Notes and validated HTTP(S) reference links are materialized into a private
  `.conceptify-context.md`; selected source files are copied under
  `.conceptify-sources/` so headless agents can read them inside the project
  boundary. Empty context removes both stored metadata and the context file;
  links are never fetched automatically.

**Thread hygiene (bead 0kt):**

- `delete_thread { thread_id }` → `void` — delete a thread and all its data: the
  DB row (cascading to its artifacts/comments/follow_up_runs via the schema
  `ON DELETE CASCADE` FKs) and, best-effort, its on-disk artifact directory
  (`~/Documents/conceptify/artifacts/<project>/threads/<slug>/`). Errors
  (string) only on unknown thread / DB failure; a dir-removal failure is logged,
  not surfaced. The hygiene valve for a thread stuck in `generating` with no
  artifact (and the general delete affordance).

**Artifact diffing (`conceptify-3nn.1`):**

- `diff_versions { thread_id, from_version, to_version }` → the same
  `ArtifactVersionDiffResponse` as the authenticated HTTP endpoint below. It is
  a read-only command and emits no event.

`list_artifact_versions` also returns `response_intent` and `skills` for each
historical version. Both are snapshots copied from the publishing run (or
inherited from the preceding artifact for an unprofiled follow-up), allowing
the thread header to show effective provenance without exposing raw prompts.

**Skill capabilities (`conceptify-l9w.2`):**

- `list_skill_capabilities {}` → `SkillCatalogEntry[]` — reads local
  `capabilities.json` sidecars from supported agent skill directories and the
  bundled Conceptify descriptor. Each entry describes its outcome, supported
  intents, context requirements, expected outputs, latency hint, compatible
  response controls, recommendation signals, manual-selectability, and
  `{ available, reason }` installation state.
- `recommend_skills { question, intent, selected_skill_ids }` →
  `SkillRecommendation[]` — validates the response-intent v1 object and scores
  the local catalog deterministically. Results include score, a plain-language
  reason, and `selected_manually`; unavailable manual choices remain visible
  with their installation reason. Question text is processed in-process and is
  never sent to a model or service merely for selection. An ordinary question
  may correctly return an empty list.

Capability sidecar schema version 1 is exemplified by
`skill/capabilities.json`. New skills need no service code when they use the
same schema; invalid or unknown-version sidecars are not executable.

**Response preferences (`conceptify-l9w.4`):**

- `get_response_preferences { project_id }` → `{ intent, origins, user,
  project }` resolves each dimension independently in project → user → product
  order. `origins` names the winning scope for depth, language, visuals, and
  shape; partial stored scopes are returned for inspection.
- `save_response_preference { project_id, scope, intent }` → the same resolved
  shape. `scope` is `user` or `project`; saving is an explicit action and
  validates the full v1 contract.
- `reset_response_preference { project_id, scope }` → the newly resolved shape
  after deleting that scope. Existing run/artifact snapshots are untouched.

These commands use namespaced rows in the existing settings key/value table;
model/provider settings remain separate.

**Project brief (`conceptify-vc1.5`):** `get_project_goal { project_id }` and
`set_project_goal { project_id, goal }` read/write the short project-home
learning brief in the settings store. Clearing the text removes the setting;
thread questions and artifact provenance are never rewritten.

**Settings (FR-7.1–7.4, beads 959.4):**

- `get_agent_settings {}` → `AgentSettings` — stored overrides merged over code
  defaults, or pure defaults when nothing is saved (FR-7.4 zero-config). Shape
  (camelCase): `{ adapters: { name → { command, args, cwd } }, defaultAdapter,
  models: { followUp, artifactUpdate, inAppAsk }, timeoutSecs, agentBinaryPath,
  appearance: "system"|"light"|"dark", autoProjectBaseDir,
  systemNotifications, runConcurrency }`. `agentBinaryPath`
  and `autoProjectBaseDir` are `null` when unset (code default applies).
  Built-in adapters merge **additively** on read (bead conceptify-e7m.7): a
  stored `adapters` map written before a built-in existed still yields it; user
  overrides of a built-in key and user-defined adapters win. The OpenRouter API
  key is **not** part of this shape (see `set_openrouter_api_key`).
  `systemNotifications` defaults to `false`; the frontend requests native
  permission only from the user's explicit enable action. In-app badges and
  actionable activity never depend on this setting or OS permission.
- `set_agent_settings { settings }` → `void` — persist (validated: `defaultAdapter`
  must name an existing adapter, else a user-facing error). Emits `settings-changed`.
- `reset_agent_settings {}` → `AgentSettings` — delete the stored override so the
  next read returns pure code defaults (FR-7.4 reset); returns those defaults.
  Emits `settings-changed`.
- `get_agent_options {}` → `{ adapters: string[], defaultAdapter, models:
  { followUp, artifactUpdate, inAppAsk }, openRouterKeySet: boolean }`
  (camelCase) — a UI-friendly view of the run-selection options a per-ask
  override picker (bead conceptify-e7m.4) needs: the configured adapter
  **keys** (sorted, e.g. `["claude", "codex"]`) rather than the full
  `{ command, args, cwd }` templates `get_agent_settings` returns, plus the
  default adapter, the per-purpose default models (the fallback baseline when a
  run carries no override), and whether an OpenRouter API key is stored — the
  ONLY key-derived fact the frontend is ever given. Distinct from the live
  model *catalog* (bead conceptify-e7m.6); read-only, never mutates settings.
- `set_openrouter_api_key { key? }` → `void` — store (or clear, with
  `null`/blank) the OpenRouter API key used by openrouter-routed runs (bead
  conceptify-e7m.7). **Write-only**: no command returns the key. Stored in its
  own settings row — never inside the `agent_settings` blob, so
  `reset_agent_settings` leaves it intact — and never surfaced in logs, events,
  error strings, or run rows (see the Keychain-vs-blob decision recorded in
  `settings.rs`). Validation: no embedded whitespace/control characters, with
  an error that never echoes the value. Emits `settings-changed`.

## Endpoints

### `GET /health`

Unauthenticated. Also mirrored at `GET /api/v1/health`. Used by:

- the CLI's launch-and-wait contract (§5.2): probe → `open -a Conceptify` on
  failure → poll until healthy;
- this server's own occupant-probe when a port is already taken.

Response `200 OK`:

```json
{
  "service": "conceptify",
  "status": "ok",
  "version": "0.1.0"
}
```

### `GET /api/v1/ping`

Authenticated. Demo/smoke-test route: confirms the bearer token works and
that an axum handler can emit a Tauri event received by the webview.

Response `200 OK`:

```json
{ "pong": true }
```

Side effect: emits an `api-ping` event (see Events above).

Errors: `401 Unauthorized` if the bearer token is missing or wrong.

### `GET /api/v1/debug/db-check`

Authenticated. Demo/smoke-test route (PRD §5.1, §4): confirms the SQLite
connection held in Tauri managed state (`db::DbHandle`) is also reachable
from an axum handler, alongside the equivalent `db_check` `#[tauri::command]`
used from the frontend. Runs `SELECT count(*) FROM projects` through
`db::with_conn` (off the async runtime's worker thread — see `src-tauri/src/db/mod.rs`).

Response `200 OK`:

```json
{ "ok": true, "project_count": 0 }
```

Response `500 Internal Server Error` (query failed):

```json
{ "ok": false, "error": "..." }
```

Errors: `401 Unauthorized` if the bearer token is missing or wrong.

---

## Projects

### `POST /api/v1/projects/ensure`

Authenticated. Ensure-project by directory (PRD FR-1.1): given a root path,
canonicalize it and return the existing project or create one, defaulting name
to the directory name (deduped with numeric suffix if taken). Symlinks and
trailing slashes resolve to one identity via canonicalization.

Request body:

```json
{
  "root_path": "/Users/chris/code/myrepo",
  "name": "Optional Name Override"
}
```

The `name` field is optional. If omitted, defaults to the directory name.

Response `200 OK`:

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "name": "myrepo",
  "root_path": "/Users/chris/code/myrepo",
  "created_at": "2026-07-03T12:34:56.789Z",
  "archived": false,
  "created": true
}
```

`created` is `true` if this call created a new project, `false` if it already
existed.

Response `400 Bad Request` (path does not exist):

```json
{ "error": "path not found: /nonexistent/path" }
```

Side effect: emits `projects-changed` event if a new project was created.

Errors: `401 Unauthorized` if bearer token missing/wrong; `400` if path
doesn't exist or can't be canonicalized; `500` on database error.

---

### `GET /api/v1/projects`

Authenticated. List all projects with thread counts and last activity. Excludes
archived by default; pass `?archived=true` to include them.

Query parameters:
- `archived` (optional, boolean, default `false`): include archived projects.

Response `200 OK`:

```json
{
  "projects": [
    {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "name": "myrepo",
      "root_path": "/Users/chris/code/myrepo",
      "created_at": "2026-07-03T12:34:56.789Z",
      "archived": false,
      "thread_count": 5,
      "last_activity": "2026-07-03T15:20:10.456Z"
    }
  ]
}
```

`last_activity` is `max(threads.updated_at)` or `project.created_at` if the
project has no threads yet.

Errors: `401 Unauthorized` if bearer token missing/wrong; `500` on database
error.

---

### `PATCH /api/v1/projects/:id`

Authenticated. Rename a project.

Request body:

```json
{ "name": "New Project Name" }
```

Response `200 OK`:

```json
{ "ok": true }
```

Response `404 Not Found` (unknown project id):

```json
{ "error": "project not found: <id>" }
```

Side effect: emits `projects-changed` event.

Errors: `401 Unauthorized` if bearer token missing/wrong; `404` if project
not found; `500` on database error.

---

### `PUT /api/v1/projects/:id/archive`

Authenticated. Archive or unarchive a project. Archived projects are hidden
from the default list (they remain in the database; deletion is not
implemented).

Request body:

```json
{ "archived": true }
```

Set `archived: false` to unarchive.

Response `200 OK`:

```json
{ "ok": true }
```

Response `404 Not Found` (unknown project id):

```json
{ "error": "project not found: <id>" }
```

Side effect: emits `projects-changed` event.

Errors: `401 Unauthorized` if bearer token missing/wrong; `404` if project
not found; `500` on database error.

---

## Threads

### `POST /api/v1/threads`

Authenticated. Create a thread (PRD FR-2.1). A filesystem-safe `slug` for the
artifact folder (§5.6) is derived server-side from `title` and deduped to be
unique within the project (`slug`, `slug-2`, `slug-3`, ...); the caller never
supplies it. New threads start in status `generating` (OQ4: create early;
`error`/retry transitions land in later milestones).

Request body:

```json
{
  "project_id": "550e8400-e29b-41d4-a716-446655440000",
  "title": "How does OAuth work?",
  "initial_question": "Explain the OAuth 2.0 authorization code flow."
}
```

Response `200 OK`:

```json
{
  "id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "project_id": "550e8400-e29b-41d4-a716-446655440000",
  "title": "How does OAuth work?",
  "slug": "how-does-oauth-work",
  "initial_question": "Explain the OAuth 2.0 authorization code flow.",
  "status": "generating",
  "created_at": "2026-07-04T12:34:56.789Z",
  "updated_at": "2026-07-04T12:34:56.789Z"
}
```

Response `400 Bad Request` (empty/whitespace-only title):

```json
{ "error": "title must not be empty" }
```

Response `404 Not Found` (unknown `project_id`):

```json
{ "error": "project not found: <id>" }
```

Side effect: emits `thread-created` (see Events above).

Errors: `401 Unauthorized` if bearer token missing/wrong; `400` on empty
title; `404` if the project doesn't exist; `500` on database error.

---

### `GET /api/v1/threads`

Authenticated. List a project's threads (PRD FR-2.2), sorted by last activity
(`updated_at`, most recent first), each with its status and open-comment count.

Query parameters:
- `project_id` (required): the project whose threads to list.

Response `200 OK`:

```json
{
  "threads": [
    {
      "id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
      "project_id": "550e8400-e29b-41d4-a716-446655440000",
      "title": "How does OAuth work?",
      "slug": "how-does-oauth-work",
      "initial_question": "Explain the OAuth 2.0 authorization code flow.",
      "status": "generating",
      "created_at": "2026-07-04T12:34:56.789Z",
      "updated_at": "2026-07-04T12:34:56.789Z",
      "open_comment_count": 0
    }
  ]
}
```

`open_comment_count` counts comments still in the `open` state (a real
`LEFT JOIN` on `comments`; 0 until the comments backend starts inserting rows).
An unknown `project_id` returns an empty `threads` array rather than a 404.

Errors: `401 Unauthorized` if bearer token missing/wrong; `500` on database
error.

---

### `GET /api/v1/threads/:id/context`

Authenticated. **get-context** (PRD §5.2, §5.5): the one-round-trip aggregate a
headless follow-up run needs to answer a thread's open comments without touching
the DB directly — the thread, its owning project, the latest artifact on disk,
and the open comments (with anchors). The `conceptify get-context` CLI wraps
this; internal server-side prompt assembly (the headless spawner) reuses the
same aggregation.

Path parameter:
- `id` (required): the thread to assemble context for.

Response `200 OK`:

```json
{
  "thread": {
    "id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
    "title": "How does OAuth work?",
    "initial_question": "Explain the OAuth 2.0 authorization code flow.",
    "status": "ready",
    "slug": "how-does-oauth-work"
  },
  "project": {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "name": "myrepo",
    "root_path": "/Users/chris/code/myrepo"
  },
  "latest_artifact": {
    "version": 2,
    "file_path": "/Users/chris/Documents/conceptify/artifacts/550e8400-…/threads/how-does-oauth-work/artifact.v2.html"
  },
  "open_comments": [
    {
      "id": "b3f1…",
      "thread_id": "7c9e6679-…",
      "parent_id": null,
      "artifact_version": 1,
      "anchor": { "v": 1, "type": "text", "cfy_id": "sec-walkthrough", "start": 142, "end": 210, "quote": { "exact": "the token is refreshed here" } },
      "body": "why is the token refreshed here?",
      "status": "open",
      "answer_html": "<p>Because the refresh token rotates…</p>",
      "anchor_state": "anchored",
      "created_at": "2026-07-04T12:34:56.789Z",
      "resolved_at": null,
      "replies": [
        {
          "id": "d7a2…",
          "thread_id": "7c9e6679-…",
          "parent_id": "b3f1…",
          "artifact_version": 1,
          "anchor": null,
          "body": "I still don't get why it rotates every request.",
          "status": "open",
          "answer_html": null,
          "anchor_state": "anchored",
          "created_at": "2026-07-04T12:40:00.000Z",
          "resolved_at": null
        }
      ]
    }
  ]
}
```

- `latest_artifact` is the highest artifact version on disk (`file_path` is the
  absolute path of that immutable `artifact.vN.html`), or **`null`** when the
  thread has no artifact yet (still `generating`).
- `open_comments` lists only **root** comments still in the `open` state, oldest
  first — the questions the run must answer (a root re-opened by a user reply is
  among them; see [Threaded replies](#threaded-replies-fr-46-epic-conceptify-6xi)).
  Each entry is the full comment shape (identical to `GET /api/v1/comments`, so
  its `anchor` is carried **verbatim** — see
  [The anchor model](#the-anchor-model-fr-44)) **plus** a `replies` array: the
  root's ordered reply chain (oldest first, by `created_at` then insertion) — the
  **exchange history** (original question + its prior `answer_html` + follow-up
  replies) a run builds its answer on. `replies` is `[]` for a root with no
  replies; each reply carries `parent_id` = the root's id and a `null` `anchor`.

Response `404 Not Found` (unknown thread id):

```json
{ "error": "thread not found: <id>" }
```

Errors: `401 Unauthorized` if bearer token missing/wrong; `404` if the thread
doesn't exist; `500` on database error.

---

## Artifacts

### `POST /api/v1/threads/:thread_id/artifact`

Authenticated. **save-artifact** (PRD FR-3.6, §5.6): validate the submitted
HTML file and store it as the thread's next artifact version (v1, v2, …; prior
versions retained). This endpoint owns the thread's `→ ready` status
transition.

**Request body: the raw artifact HTML bytes** — not JSON, not multipart. Send
`Content-Type: text/html` (not enforced). Rationale: a JSON wrapper cannot
carry invalid UTF-8 (the validator's `E-UTF8` rule would be unreachable) and
needlessly escapes multi-megabyte payloads; the CLI just streams the file it
was given. Bodies over 60 MiB are rejected at the transport layer with a bare
`413`; the spec's 50 MiB cap (`E-SIZE-MAX`) applies below that with a
structured error.

```
POST /api/v1/threads/7c9e6679-7425-40de-944b-e07fc1f90ae7/artifact
Authorization: Bearer <token>
Content-Type: text/html

<!doctype html> ...
```

**Validation** runs the rule set from
[docs/artifact-spec.md](artifact-spec.md) §8 — that doc is the contract; rule
IDs (`E-*` hard failures, `W-*` warnings) are stable identifiers and are not
restated here. Hard failures reject the save (nothing is stored, no version
consumed); warnings are returned in the success response for the CLI to print
to stderr. `W-VERSION-MISMATCH` is checked against the server-assigned
version, which is authoritative over the file's `cfy:version` meta.

**Versioning & storage** (§5.6): the file is stored as
`~/Documents/conceptify/artifacts/<project-id>/threads/<thread-slug>/artifact.vN.html`,
and `artifact.html` in the same directory is atomically replaced with a copy
of the new version (temp + rename, never a symlink — PRD N4; a `runs/`
directory is also created, reserved for headless-agent transcripts). All
writes are temp + rename; a crash mid-save never leaves a partial file visible
or a DB row pointing at a missing file.

**`created_by` is inferred, never caller-supplied**: version 1 → `initial`,
version ≥ 2 → `follow_up`.

Response `200 OK` (stored — possibly with warnings):

```json
{
  "thread_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "project_id": "550e8400-e29b-41d4-a716-446655440000",
  "version": 2,
  "created_by": "follow_up",
  "file_path": "/Users/chris/Documents/conceptify/artifacts/550e8400-…/threads/how-does-oauth-work/artifact.v2.html",
  "warnings": [
    { "code": "W-ANCHOR-DIAGRAM", "message": "svg \"fig-map\" has thin anchor coverage: 8 shape elements but only 1 data-cfy-id bearers (need ≥ 3)" }
  ]
}
```

Response `422 Unprocessable Entity` (validation hard failure — nothing
stored). `error`/`code` carry the first violated rule (the shape promised by
artifact-spec.md §8); `errors` lists every hard failure found:

```json
{
  "error": "script src \"https://evil.example.com/x.js\" is not on the Tier-2 CDN allowlist",
  "code": "E-EXTERNAL-CODE",
  "errors": [
    { "code": "E-EXTERNAL-CODE", "message": "script src \"https://evil.example.com/x.js\" is not on the Tier-2 CDN allowlist" }
  ]
}
```

Response `404 Not Found` (unknown `thread_id`):

```json
{ "error": "thread not found: <id>" }
```

Side effects: thread status → `ready` (and `updated_at` bumped), emits
`artifact-updated` (see Events above). For follow-up saves (version ≥ 2) the
FR-4.4 **re-attachment pass** runs in the same transaction (see
[Re-attachment across versions](#re-attachment-across-versions-fr-44)): earlier-version
comments are migrated onto the new version or flagged `moved`, and one
`comment-updated` event is emitted per comment whose row changed.

Errors: `401 Unauthorized` if bearer token missing/wrong; `404` unknown
thread; `413` body over the 60 MiB transport cap; `422` validation hard
failure; `500` on database or disk error.

---

### `GET /api/v1/threads/:thread_id/artifacts/diff`

Authenticated. Query parameters: `from_version` and `to_version` (integer saved
versions). Returns only changed or moved `data-cfy-id` blocks, so identical
versions have `changes: []`, plus `unchanged_count`, `degraded`, and `warnings`.
Each change carries `kind` (`unchanged` only when moved, `modified`, `added`, or
`removed`), the orthogonal `moved` flag, old/new document indices, nearest
surviving neighbor ids, old/new normalized text, and word-level
`equal`/`added`/`removed` hunks.

Normalization compares visible descendant text, collapses Unicode whitespace
to single spaces, and trims it. Attribute order, comments, indentation, and
other HTML serialization noise are ignored; consequently whitespace-only code
formatting is also considered unchanged. Reordering uses the longest common
subsequence of ids, so insertion/deletion does not falsely mark every later
block moved. Duplicate ids use the first occurrence with a warning. Changed
visible text outside every `data-cfy-id` block returns one synthetic change
with `cfy_id: null` and `degraded: true`. HTML5 error recovery makes malformed
hand edits deterministic and non-panicking.

```json
{
  "thread_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "from_version": 1,
  "to_version": 2,
  "changes": [{
    "cfy_id": "sec-queue",
    "kind": "modified",
    "moved": false,
    "old_index": 3,
    "new_index": 3,
    "previous_cfy_id": "sec-policy",
    "next_cfy_id": "fig-flow",
    "old_text": "One request runs at a time.",
    "new_text": "Independent requests run concurrently.",
    "hunks": [
      { "kind": "removed", "text": "One request runs at a time." },
      { "kind": "added", "text": "Independent requests run concurrently." }
    ]
  }],
  "unchanged_count": 8,
  "degraded": false,
  "warnings": []
}
```

Errors: `401` without the bearer token; `404` for an unknown thread or missing
version; `500` for database or artifact-file read failures.

---

## Comments

Comments are the annotation layer (PRD §7.4, FR-4.1–FR-4.5/4.7): a user note
anchored to a region of a specific artifact version, optionally carrying an
agent resolution. A comment with a `null` anchor is a **direct follow-up
question** (FR-4.3) — it flows through the identical machinery.

### The anchor model (FR-4.4)

The `anchor` is JSON stored on the comment. It is the load-bearing contract
shared by the in-artifact bridge (which captures it), the re-attachment logic
(which re-locates it after edits), and the headless follow-up agents (which read
it via `get-context`). The canonical Rust definition lives in
`crates/conceptify-types` (`Anchor`, `TextAnchor`, `ElementAnchor`, `TextQuote`).

Design invariants:

- **Versioned.** Every anchor object carries an integer `v` (currently `1`).
  The server rejects an unsupported `v`. A breaking schema change bumps `v`.
- **Discriminated.** A `type` field selects the variant. Two exist today:
  `text` (a text selection, FR-4.1) and `element` (a whole `data-cfy-id`-bearing
  element, FR-4.2). New types are additive.
- **Extensible.** The server validates the envelope (`v`, `type`, required
  fields) but stores the object **verbatim** and tolerates unknown extra fields,
  so the bridge can add capture hints without a server change.
- **Field naming is `snake_case`**, matching the rest of the API.
- **`null` anchor** (the field absent/`null`) = a direct follow-up question.

Every anchor pairs a **primary** anchor (fast, exact) with a **fallback**
text-quote (W3C Web Annotation style) used to re-attach the comment after the
artifact is edited when the primary no longer resolves.

Both variants may also carry a `target` summary. This is presentation and
agent context rather than a third re-attachment mechanism: the primary id and
offsets plus `quote` remain authoritative. The summary identifies the semantic
kind (`text`, `block`, `code`, `figure`, `image`, or `diagram`), provides a
short readable `label` and `excerpt`, and lists the intersected stable ids in
document order. Diagram-element targets may additionally carry a short `role`
and a `relationships` string list when the artifact exposes them through SVG
classes/titles, ARIA, or `data-cfy-role` / `data-cfy-relationships` metadata.
Both fields are optional: unlabeled shapes still produce a useful target from
their stable id and element kind. The summary deliberately never duplicates
the whole artifact.

```json
{
  "target": {
    "kind": "code",
    "label": "Refresh-token example",
    "excerpt": "const session = await refresh(token)",
    "cfy_ids": ["sec-auth", "example-refresh"],
    "multi_block": true
  }
}
```

For a selection spanning more than one id-bearing region, `cfy_ids` contains
at most the first eight intersected ids and `multi_block` is `true`. The bridge
still records primary offsets only when the range has one common id-bearing
host; the exact quote is always retained as the cross-block fallback. A whole
element target has one primary `cfy_id`, a one-item `cfy_ids` list, and
`multi_block: false`.

An exploration request may add an `exploration` object beside `target`. It
records the selected quick action, destination (`inline`, `sidebar`, `thread`,
or `revision`), source thread/version, and resolved `response_intent`. Answer runs
honor that structured depth/language/visuals/shape profile; the action's display
label is not treated as a brittle prompt contract. Like `target`, this additive
metadata is stored verbatim and survives re-attachment.

**`type: "text"`** (text selection). Primary = the nearest ancestor
`data-cfy-id` (`cfy_id`) plus `start`/`end` character offsets within that
element's normalized text content (precise definition: "visible text" under
[Bridge protocol → Conventions](#conventions)); fallback = `quote`. When the
selection has no id-bearing ancestor, `cfy_id`/`start`/`end` are omitted and
`quote` is the sole anchor.

```json
{
  "v": 1,
  "type": "text",
  "cfy_id": "sec-walkthrough",
  "start": 142,
  "end": 210,
  "quote": {
    "exact": "the token is refreshed here",
    "prefix": "I don't get why ",
    "suffix": " on every request"
  }
}
```

**`type: "element"`** (diagram node / heading click). Primary = the clicked
element's `cfy_id`; optional `quote` (the element's text) is a fallback for
re-attachment if the id later disappears — omitted for purely graphical nodes
with no text.

```json
{
  "v": 1,
  "type": "element",
  "cfy_id": "fig-auth-flow.token-service",
  "quote": { "exact": "Token Service" }
}
```

`quote` is a text-quote selector: required `exact`, plus optional `prefix` /
`suffix` context (absent at document edges) to disambiguate a repeated `exact`.

The `data-cfy-id` values referenced here are the artifact's anchorability API —
see [docs/artifact-spec.md](artifact-spec.md) §4 for the id grammar and the
cross-version stability contract that makes re-attachment work.

### Re-attachment across versions (FR-4.4)

Anchors must survive artifact updates. Whenever a **new artifact version is
saved** (`POST /threads/:thread_id/artifact`, version ≥ 2), the server runs a
re-attachment pass over the thread's comments, inside the same transaction as
the version insert (implementation: `src-tauri/src/anchoring.rs`). This is
what makes the M5 apply-to-artifact loop safe: applying one clarification can
never invisibly orphan the other comments.

**Which comments participate.** Every comment with `artifact_version` older
than the new version and status `open` or `answered` — including comments
currently flagged `moved` (they are retried on every save and heal when the
content returns). `applied` comments are **frozen history**: the apply that
resolved them typically rewrote the very text they anchored, so re-flagging
them "moved" would be noise; they keep their original `artifact_version`
(and version tag in the sidebar) forever. **Null-anchor comments** (direct
follow-ups) are version-agnostic and advance to the new version trivially.
**Replies** (`parent_id` set, epic conceptify-6xi) never participate: they carry
no anchor and inherit their parent's version, so they are excluded outright (they
keep their inherited `artifact_version` across saves).

**Per-comment verdict** (the measurement conventions are exactly the bridge's
— see [Conventions](#conventions); the server measures identically):

1. **Primary intact** — the anchor's `data-cfy-id` resolves in the new
   document (first match in document order) and, for text anchors, the
   stored `start`/`end` still select visible text equal to `quote.exact`:
   the anchor is kept **verbatim**.
2. **Quote fallback** (text anchors) — a W3C-style search for `quote.exact`
   over the body's visible text, scored by exact `prefix`/`suffix` context
   match, scoped to the original `cfy_id` element first, then document-wide.
   A **unique best** match re-points the primary fields: `start`/`end` are
   rewritten to the new position and `cfy_id` to the deepest id-bearing
   element containing the match (dropped to a quote-only anchor when none
   contains it). A tie is **ambiguous → `moved`** (unlike the bridge's
   best-effort highlighting, a persisted decision never guesses). The
   `quote` itself is never rewritten — it is the captured selection and
   remains the durable fallback. All other anchor fields (capture hints)
   are preserved verbatim.
3. **Quote fallback** (element anchors) — when the `cfy_id` vanished, the
   id-bearing element whose whitespace-collapsed text equals `quote.exact`
   is the new target (`cfy_id` rewritten). Multiple matches are ambiguous →
   `moved`, unless they form a strict nesting chain (innermost wins).
4. **Failure → `anchor_state: "moved"`** — the comment is flagged "reference
   moved", **never dropped**. Its `artifact_version` deliberately stays at
   the last version where the anchor resolved, so the (version, anchor) pair
   remains truthful: switching the viewer to that version still highlights
   it, and `get-context` still surfaces it to agents.

**On success** the comment's `artifact_version` advances to the new version
(and a prior `moved` flag clears) — this is what makes comments follow the
latest artifact automatically: the sidebar/highlight layer decorates comments
whose `artifact_version` equals the viewed version, so migrated comments
light up on the new version with no frontend special-casing.

One `comment-updated` event fires per comment whose row changed; an untouched
row (e.g. still-unresolvable and already `moved`) emits nothing.

### Status machine

A comment's `status` is one of `open` | `answered` | `applied`, and may only
**advance** along that order (or stay put) — a regression is rejected `409`:

```
open ──→ answered ──→ applied
  └──────────────────────┘   (open → applied directly is allowed)
```

- `open → answered`: the **Ask follow-ups** flow (FR-4.6) — an agent answers the
  comment in the sidebar (`answer_html` lands).
- `answered → applied` / `open → applied`: the **Apply to artifact** flow
  (FR-4.7) resolved-with-update. `open → applied` directly supports the M5
  `resolve-comment --applied` one-shot (apply without a separate answer step).

`resolved_at` is stamped the first time a comment leaves `open` and is stable
thereafter.

Separately, `anchor_state` is `anchored` | `moved`. `moved` is FR-4.4's
"reference moved" flag, set by the re-attachment pass (below) when a comment's
anchor can't be re-located in a newly saved artifact version — the comment is
flagged, never silently dropped. It is independent of the status machine, and
it can flip back to `anchored` when a later version restores the content.

### Threaded replies (FR-4.6, epic conceptify-6xi)

A comment can carry a **reply**: a follow-up in the same conversation ("I still
don't get why X"). A reply is a `comments` row whose `parent_id` names the ROOT
comment it answers, created by `POST /api/v1/comments` with a `parent_id` (see
below). The model is deliberately narrow:

- **Linear chains, one level deep.** A reply's parent must be a **root**
  (`parent_id IS NULL`); replying to a reply is rejected (`400`). Reply to the
  root instead — a conversation is a flat chain under one root, ordered
  `created_at` then insertion, not a tree.
- **No anchor of its own.** A reply attaches to a comment, not to a region of the
  artifact. Sending an `anchor` alongside `parent_id` is a `400`. `anchor_state`
  stays `anchored` (a reply can never "move").
- **Inherits the parent's `artifact_version`.** A reply is pinned to where its
  conversation is anchored — the caller-supplied `artifact_version` is ignored for
  replies.
- **Re-opens its root.** Root `status` tracks the conversation state (`open` =
  "needs agent attention"). A user reply on an `answered`/`applied` root flips the
  **root** back to `open` (its `answer_html` is kept as history, `resolved_at`
  clears) in the same transaction as the reply insert, and emits a
  `comment-updated` for the root in addition to the reply's `comment-created`. A
  reply on an already-`open` root just creates. This keeps the batch **Ask
  follow-ups** flow (FR-4.6) and the sidebar open-comment counts working
  unchanged — a re-opened root is a normal open question again, and its open
  replies never inflate the count (only open **roots** are counted).
- **Its own status.** A reply has its own `status` and advances `open` →
  `answered` via `PATCH /api/v1/comments/:id` (the agent answers the reply row).
  `applied` is **root-only** (it tracks the artifact-apply flow) — `applied` on a
  reply is a `400`.

Replies are **excluded from re-attachment** (they have no anchor and inherit
their parent's version, so they never participate — see
[Re-attachment across versions](#re-attachment-across-versions-fr-44)).

Reply chains surface to agents nested under each open root in
`GET /api/v1/threads/:id/context` (the exchange history), and `parent_id` is
included on every comment in the list/create/update responses below.

### `POST /api/v1/comments`

Authenticated. Create a comment (FR-4.1/4.2/4.3) — or, with `parent_id`, a
threaded **reply** (epic conceptify-6xi; see
[Threaded replies](#threaded-replies-fr-46-epic-conceptify-6xi)). The target
thread and the `artifact_version` must exist (a comment always anchors to an
artifact version that already exists). New comments start `open` / `anchored`.

Request body (`anchor` is `null`/omitted for a direct follow-up):

```json
{
  "thread_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "artifact_version": 1,
  "anchor": {
    "v": 1,
    "type": "text",
    "cfy_id": "sec-walkthrough",
    "start": 142,
    "end": 210,
    "quote": { "exact": "the token is refreshed here", "prefix": "why ", "suffix": " each time" }
  },
  "body": "I don't get why the token is refreshed here."
}
```

To post a **reply**, set `parent_id` (the root comment's id) and omit `anchor`.
The reply inherits the parent's `artifact_version` (any supplied value is
ignored):

```json
{
  "thread_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "parent_id": "b3f1…",
  "artifact_version": 1,
  "body": "I still don't get why it rotates every request."
}
```

Response `200 OK` (the created comment; `parent_id` is `null` for a root, or the
parent's id for a reply):

```json
{
  "id": "b3f1…",
  "thread_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "parent_id": null,
  "artifact_version": 1,
  "anchor": { "v": 1, "type": "text", "cfy_id": "sec-walkthrough", "start": 142, "end": 210, "quote": { "exact": "the token is refreshed here", "prefix": "why ", "suffix": " each time" } },
  "body": "I don't get why the token is refreshed here.",
  "status": "open",
  "answer_html": null,
  "anchor_state": "anchored",
  "created_at": "2026-07-04T12:34:56.789Z",
  "resolved_at": null
}
```

Response `400 Bad Request`: empty body; a malformed anchor (not an object,
unsupported `v`, unknown/missing `type`, or a missing required field); a reply
that carries an `anchor`; a reply-to-a-reply; or a `parent_id` in a different
thread than `thread_id`:

```json
{ "error": "invalid anchor: unsupported anchor schema version 2 (expected 1)" }
```
```json
{ "error": "cannot reply to a reply (<id> is itself a reply); reply to the root comment" }
```

Response `404 Not Found`: unknown `thread_id`; an `artifact_version` that
doesn't exist for the thread; or a `parent_id` that names no comment:

```json
{ "error": "artifact version 9 not found for thread 7c9e6679-…" }
```
```json
{ "error": "parent comment not found: <id>" }
```

Side effect: emits `comment-created` for the new comment; when a user reply
re-opens an `answered`/`applied` root, **also** emits `comment-updated` for that
root (`status: "open"`). See Events above.

Errors: `401` if bearer token missing/wrong; `400` empty body / bad anchor /
reply-rule violation; `404` unknown thread, version, or parent; `500` on database
error.

---

### `GET /api/v1/comments`

Authenticated. List a thread's comments (FR-4.5, FR-6.4), oldest first (the
sidebar reading order). Serves both the UI and the M5 `list-comments` CLI.

Query parameters:
- `thread_id` (required): the thread whose comments to list.
- `status` (optional): filter to one of `open` | `answered` | `applied`.

An unknown `thread_id` returns an empty `comments` array rather than a 404. An
invalid `status` value is a `400`.

Response `200 OK`:

```json
{
  "comments": [
    {
      "id": "b3f1…",
      "thread_id": "7c9e6679-…",
      "parent_id": null,
      "artifact_version": 1,
      "anchor": { "v": 1, "type": "element", "cfy_id": "fig-auth-flow.token-service", "quote": { "exact": "Token Service" } },
      "body": "why does this node retry?",
      "status": "answered",
      "answer_html": "<p>Because the upstream can return 503…</p>",
      "anchor_state": "anchored",
      "created_at": "2026-07-04T12:34:56.789Z",
      "resolved_at": "2026-07-04T12:40:01.234Z"
    }
  ]
}
```

This is a **flat** list: replies appear as their own rows, each with
`parent_id` set to its root (roots have `parent_id: null`). The client groups
them into chains by `parent_id` (get-context does this nesting server-side). The
list is unfiltered by depth — a `status` filter applies to roots and replies
alike.

Errors: `401` if bearer token missing/wrong; `400` on an invalid `status`
filter; `500` on database error.

---

### `PATCH /api/v1/comments/:id`

Authenticated. Update a comment (FR-4.6/4.7); drives the M5 `resolve-comment`
CLI. Supply any subset of the fields below (at least one); omitted fields are
unchanged.

Request body:

```json
{
  "status": "answered",
  "answer_html": "<p>Because the refresh token rotates on every use…</p>",
  "anchor_state": "moved"
}
```

- `status` — target status; must be a legal advance (see the status machine).
  This is the path an agent uses to answer a **reply** (`open → answered` on the
  reply row); `applied` is root-only (`applied` on a reply is a `400`).
- `answer_html` — the agent's resolution (rendered HTML/markdown fragment).
- `anchor_state` — `anchored` | `moved` (driven by re-attachment; independent of
  the status machine).

Response `200 OK`: the full updated comment (same shape as the create response,
including `parent_id`).

Response `409 Conflict` (illegal status regression) — structured with the
offending `from`/`to`:

```json
{ "error": "illegal status transition: applied -> answered", "from": "applied", "to": "answered" }
```

Response `404 Not Found` (unknown comment id):

```json
{ "error": "comment not found: <id>" }
```

Response `400 Bad Request`: no fields supplied; an invalid `status` /
`anchor_state` value; or `applied` targeted at a reply (`applied` is root-only):

```json
{ "error": "cannot apply a reply (<id>); the `applied` status is root-only" }
```

Side effect: emits `comment-updated` (see Events above).

Errors: `401` if bearer token missing/wrong; `400` bad/empty update; `404`
unknown comment; `409` illegal status transition; `500` on database error.

---

## Open / focus

### `POST /api/v1/open`

Authenticated. Focus the app on a project or thread (PRD §5.2 `conceptify
open`). Validates the target exists, brings the main window to the front, and
emits a `navigate` event the frontend uses to route to the target.

Focus-on-open is part of UC1's feel: after an agent finishes generating an
artifact, `conceptify open --thread <id>` puts it on screen. Because the main
window hides (rather than quits) on close (§5.1 lifecycle), the handler
`show()`s it before `set_focus()`.

Request body — supply **exactly one** of `thread_id` / `project_id`:

```json
{ "thread_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7" }
```

or

```json
{ "project_id": "550e8400-e29b-41d4-a716-446655440000" }
```

If both are present, the more specific `thread_id` wins (the project is
resolved from the thread). The CLI enforces exactly-one before calling.

Response `200 OK`:

```json
{
  "ok": true,
  "project_id": "550e8400-e29b-41d4-a716-446655440000",
  "thread_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7"
}
```

`thread_id` is `null` when a project (not a specific thread) was opened. The
same `{ project_id, thread_id }` shape is emitted as the `navigate` event.

Response `400 Bad Request` (neither `thread_id` nor `project_id` supplied):

```json
{ "error": "must specify thread_id or project_id" }
```

Response `404 Not Found` (unknown target):

```json
{ "error": "thread not found: <id>" }
```

```json
{ "error": "project not found: <id>" }
```

Side effect: brings the `main` window to the front (`show` + `set_focus`) and
emits `navigate` (see Events above).

Errors: `401 Unauthorized` if bearer token missing/wrong; `400` if no target
is supplied; `404` if the referenced thread/project doesn't exist; `500` on
database error.

---

## Catalog

The live model catalog (epic conceptify-e7m, bead e7m.6): the set of selectable
models, grouped by provider **family**, that backs the Settings model dropdowns
+ provider-suite toggles (e7m.3), the point-of-ask picker (e7m.4), and execution
routing (e7m.7). Implementation: `src-tauri/src/catalog.rs`; shared types
`CatalogModel` / `CatalogProvider` / `CatalogResponse` in `conceptify-types`.

**Sources & normalization.** The catalog is fetched from two public sources and
normalized to `{ id, provider, displayName, contextWindow, openrouterRunnable }`:

- **LiteLLM** `model_prices_and_context_window.json` (raw GitHub) — kept: entries
  whose `mode` is `chat`/`completion` **and** whose `litellm_provider` is a clean
  model family (anthropic, openai, gemini→google, mistral→mistralai, xai→x-ai,
  deepseek, cohere, ai21). Backend-routing providers (bedrock, azure,
  fireworks_ai, …) are dropped — they are duplicative and not attributable to a
  family. This is the source of the **native** ids (`claude-sonnet-5`, `gpt-5`).
- **OpenRouter** `GET /api/v1/models` (no auth) — kept: entries that can emit text
  (`architecture.output_modalities` includes `text`; pure image/audio generators
  dropped). Every kept entry is `openrouterRunnable: true`. `provider` is the id
  slug prefix, canonicalized (a leading `~` on "latest" aliases like
  `~anthropic/claude-sonnet-latest` is stripped for the family; the id keeps it).

The two are merged by exact `id`; an id present in OpenRouter forces
`openrouterRunnable: true`. `id` is the **execution id** (the value passed to the
agent): a bare native id for the claude/codex routes, or the OpenRouter slug for
the OpenRouter route (bead e7m.7 uses `openrouterRunnable` to choose the route).

**Caching / offline.** On startup a background task (off the boot critical path)
fetches + atomically caches the *normalized* catalog under
`~/Library/Application Support/conceptify/model_catalog.json` with a `fetchedAt`
stamp. The startup fetch is skipped when the cache is <24h old. The fallback
chain for both fetching and serving is **live fetch → disk cache → bundled
snapshot** (a small offline asset of current anthropic/openai/google/mistral/…
models). The whole path is failure-silent: errors are logged, never shown as a
dialog, and never delay boot.

**Provider filtering.** Responses return only models whose `provider` is in the
enabled provider suites (`enabledProviders` in agent settings — default
`["anthropic","openai"]`, toggled via the Settings UI), plus the full provider
list with per-family counts (over the whole catalog) and each family's `enabled`
flag, so the toggles can render "google (29)" even while disabled.

### `GET /api/v1/catalog/models`

Authenticated. The catalog filtered to the enabled provider suites. Reads the
disk cache (or bundled snapshot) — never the network — so it is instant and
always succeeds.

Response `200 OK`:

```json
{
  "fetchedAt": "2026-07-05T09:12:44.512Z",
  "source": "cache",
  "models": [
    { "id": "claude-sonnet-5", "provider": "anthropic", "displayName": "claude-sonnet-5", "contextWindow": 200000, "openrouterRunnable": false },
    { "id": "anthropic/claude-sonnet-5", "provider": "anthropic", "displayName": "Anthropic: Claude Sonnet 5", "contextWindow": 200000, "openrouterRunnable": true },
    { "id": "gpt-5", "provider": "openai", "displayName": "gpt-5", "contextWindow": 272000, "openrouterRunnable": false }
  ],
  "providers": [
    { "provider": "anthropic", "modelCount": 18, "enabled": true },
    { "provider": "google", "modelCount": 29, "enabled": false },
    { "provider": "openai", "modelCount": 74, "enabled": true }
  ]
}
```

`source` is `live` | `cache` | `snapshot`. `models` is sorted by `(provider, id)`
and includes only enabled-provider models; `providers` always lists every family
in the full catalog. `contextWindow` is omitted when the source reports none.

Errors: `401 Unauthorized` if the bearer token is missing/wrong; `500` if the
settings read fails.

### `POST /api/v1/catalog/refresh`

Authenticated. Force a live re-fetch of both sources, update the cache, and
return the fresh catalog (same shape as `GET /catalog/models`, typically with
`"source": "live"`). Failure-silent: a network error degrades to the
cache/snapshot (`"source": "cache"`/`"snapshot"`) rather than erroring. Emits the
`catalog-refreshed` event so live views repaint. No request body.

Errors: `401 Unauthorized` if the bearer token is missing/wrong; `500` if the
settings read fails.

> The same two operations are exposed to the app shell as the Tauri commands
> `get_model_catalog` and `refresh_model_catalog` (identical `CatalogResponse`
> shape), following the command/HTTP parity the rest of the API keeps.

---

## The `artifact://` scheme (in-app viewer transport)

Not an HTTP endpoint: a custom Tauri URI scheme
(`register_asynchronous_uri_scheme_protocol`, PRD §5.4, §9 S2) that serves
stored artifacts into the viewer's sandboxed iframe. Only reachable from
inside the app's webviews — there is no network listener behind it.
Implementation: `src-tauri/src/artifact_protocol.rs`.

### URL contract

```
artifact://localhost/<thread-id>/<version>
```

- **`<thread-id>`** — the thread's DB id (UUID). Validated strictly:
  `[A-Za-z0-9_-]{1,128}`. No percent-decoding is performed on this scheme;
  ids never need encoding, and any `%`, `.`, `\`, or extra `/` in a segment
  is a `400`.
- **`<version>`** — one of:
  - a bare decimal integer ≥ 1 (e.g. `…/3`): serves the immutable
    `artifact.v3.html` for that thread. **Not** `v3` — no prefix.
  - the literal lowercase **`latest`**: resolves the thread's highest
    version **via the DB** (`MAX(version)` over `artifacts`) and serves that
    versioned file. The `artifact.html` disk copy is deliberately not
    served — files are written before the DB row commits (crash-safety
    ordering, PRD N4), so the copy can momentarily be ahead of the DB; the
    DB is the source of truth the version switcher and events read.
- **GET only** (405 otherwise). One document per request — artifacts are
  self-contained by spec, and subresource fetches through custom protocols
  are unreliable in WKWebView anyway (Appendix A, wry #168): never point a
  subresource (`<img src>`, etc.) at `artifact://`.

The viewer (`conceptify-nsy.4`) should embed
`<iframe src="artifact://localhost/<thread-id>/<version>"
sandbox="allow-scripts">` — **no `allow-same-origin`** — and reload on
`artifact-updated`, using the concrete `version` from the event payload.

### Response headers

Every response on the scheme — success or error page — carries:

| Header | Value |
|---|---|
| `Content-Type` | `text/html; charset=utf-8` |
| `Content-Security-Policy` | `default-src 'none'; script-src 'unsafe-inline' https://cdn.jsdelivr.net; style-src 'unsafe-inline' https://cdn.jsdelivr.net; font-src data: https://cdn.jsdelivr.net; img-src data:; connect-src 'none'` |
| `X-Content-Type-Options` | `nosniff` |
| `Cache-Control` | numeric version: `public, max-age=31536000, immutable` (write-once files); `latest` and all errors: `no-store` |

The CSP is exactly the reference policy of
[docs/artifact-spec.md](artifact-spec.md) §3 and stays in lockstep with the
§7.1 pinned-CDN allowlist (single host, `cdn.jsdelivr.net`; `font-src`
includes it for KaTeX). The load-bearing directive is `connect-src 'none'`:
artifact JS can never reach this API (PRD §9 S2). Changing the policy means
changing the spec first.

### Bridge injection

The handler splices one `<script data-cfy-bridge="v1">…</script>` tag into
the served bytes immediately before the closing `</body>` (appended at the
end if no close tag exists; idempotent via the reserved `data-cfy-bridge`
marker). The on-disk file is **never modified** — opened directly in a
browser it carries no Conceptify residue (G3). The script is the in-artifact
half of the comment interaction layer (source:
`src-tauri/assets/bridge.js`, inlined via `artifact_protocol::BRIDGE_TAG`);
the protocol it speaks is specified in [Bridge protocol](#bridge-protocol)
below. It defines the reserved global
`window.__cfyBridge = Object.freeze({ v: 1 })` and injects one
zero-specificity `<style data-cfy-bridge="style">` element (hover
affordances + highlight styles, all `:where()`-wrapped so artifact CSS
always wins). Artifacts must not rely on the bridge existing (spec §1/§3):
the same file opens bridge-less in a plain browser.

## Bridge protocol

The postMessage protocol between the app shell and the injected bridge
script inside the artifact iframe (PRD §5.4). Shell counterpart:
`src/lib/bridge.ts` (the only module that may talk to the frame); bridge
script: `src-tauri/assets/bridge.js`. Comment UI (popover, sidebar) rides on
this seam — beads `conceptify-94m.3/4/5/6`.

**Envelope.** Every message, both directions, is a plain object carrying
`cfy: 1` (protocol version) and a `type` string, plus type-specific fields.
Receivers on both sides ignore messages without the `cfy: 1` envelope and
silently drop unknown `type`s (forward compatibility; the shell logs them at
debug level). A breaking protocol change bumps `cfy`.

**Origin / source validation.** The artifact frame is opaque-origin
(sandboxed, cross-scheme), so:

- The **shell** accepts a message only when `event.origin === "null"` (an
  opaque origin serializes as the string `"null"`) **and** `event.source`
  is the registered viewer iframe's `contentWindow`.
- The **bridge** accepts a command only when `event.source ===
  window.parent`.
- Both sides must post with `targetOrigin "*"` — an opaque origin cannot be
  named. Nothing sensitive ever rides the channel in either direction:
  artifact→shell carries only anchors/quotes/rects, shell→artifact only
  anchors/keys.

**Trust model (S2 — containment, not adversarial).** Artifact JS lives in
the same frame as the bridge and *can* post protocol-shaped messages to the
shell (spoofing the bridge). This is accepted by the threat model: the shell
treats every inbound message as untrusted input (shape-validated, dropped if
malformed) and nothing a spoofed message triggers exceeds what the user
could do by hand in the UI. Artifact JS cannot, however, spoof
shell→artifact *commands* (its `event.source` is its own window, not
`window.parent`).

### Conventions

- **Anchors** are exactly the FR-4.4 anchor objects documented under
  [The anchor model](#the-anchor-model-fr-44) — snake_case, `v: 1`. The
  bridge emits them capture-ready; the shell adds `thread_id`,
  `artifact_version`, and `body` when creating the comment.
- **Text offsets** (`start`/`end` in `text` anchors — this is the precise
  meaning of the anchor model's "normalized text content"): UTF-16
  code-unit indices into the element's **visible text** — the concatenation
  of its `Text` node data in document order, **excluding** text inside
  `script`, `style`, `noscript`, and `template` subtrees, with **no
  whitespace normalization**. Excluding script/style keeps offsets stable
  against inline-JS/CSS churn (and against the injected bridge itself).
  Re-attachment (`conceptify-94m.7`) must measure identically.
- **Quotes**: `text`-anchor quotes are raw visible-text slices
  (`exact` = the selection verbatim; `prefix`/`suffix` = up to 32 chars of
  *document-wide* visible text on either side, omitted when empty at a
  document edge). `element`-anchor quotes are whitespace-collapsed +
  trimmed element text, omitted when empty (purely graphical node) or
  longer than 300 chars — they are re-attachment hints, not exact-match
  slices.
- **Rects** are `{x, y, width, height}` in the **iframe's viewport
  coordinate space** (CSS px, `getBoundingClientRect` values at send time).
  To position shell UI, add the iframe element's own bounding rect; rects
  go stale when the artifact scrolls (the popover should dismiss or
  re-request on scroll/`selection_cleared`).

### Artifact → shell

| `type` | Payload | Meaning |
|---|---|---|
| `ready` | — | The bridge booted. Sent once per **document load** — including every iframe reload (version switch, live refresh), which wipes all decorations. Consumers must (re)apply highlights on every `ready`; `bridge.ts` queues commands sent before the first `ready` of an attachment. |
| `selection` | `anchor` (a `text` anchor), `rect` (selection bounding rect) | A non-empty text selection reported on **gesture completion**, never mid-drag: pointer selections post once on release (`pointerup`/`pointercancel`); keyboard selections (shift+arrows, Cmd+A), which have no release, post after a ~300 ms settle debounce. `cfy_id`/`start`/`end` are present when the selection has a `data-cfy-id` ancestor; `quote` always is. |
| `selection_cleared` | — | The previously reported selection collapsed/vanished (dismiss the popover). Also sent when the user presses Escape inside the artifact: the iframe holds keyboard focus after a drag, so the bridge translates Escape into clearing the live selection (the shell's own Escape handler can't see the key). A `set_highlights` application never re-reports the still-live selection its DOM mutations perturb (the bridge suppresses its own mutation-induced `selectionchange`). |
| `element_click` | `anchor` (an `element` anchor), `rect` (element bounding rect) | Click, Enter, or Space on a `data-cfy-id`-bearing element (nearest such ancestor of the click target). Suppressed for clicks that end a text selection and for clicks on interactive elements (`a[href]`, `button`, form controls, `summary`, `label`, `[contenteditable]`). Anchored SVG nodes are made focusable; arrow keys move among nodes in the same diagram. Diagram activation is consumed before click-to-advance handlers, so inspecting a node cannot advance a slide-like artifact. |
| `suggestion_click` | `cfy_id`, `question`, `reason`, `branch`, `rect` | Explicit activation of a `data-cfy-next-question` branch. The shell opens an editable launch dialog; the bridge consumes the gesture so it cannot also comment, navigate, or advance a slide. No run starts from this message alone. |

### Shell → artifact

| `type` | Payload | Meaning |
|---|---|---|
| `set_highlights` | `highlights`: array of `{key, anchor, state?}` (`key` = comment id; `state` = `saved` or `answered`) | **Full replacement** of the decoration set (`[]` clears). Element anchors get an outline on the resolved element; text anchors are wrapped in inline `<span data-cfy-hl="text">` spans via offsets (verified against the quote), falling back to a document-wide quote search, then to outlining the `cfy_id` host. Saved/open anchors use the persistent warm highlight; answered dig-ins use a quiet dotted cool accent with no fill. Live selection remains the stronger native `::selection` state, while moved anchors are omitted and shown as stale in shell UI. Decorations are non-destructive and fully reverted on the next `set_highlights`. |
| `set_diff_markers` | `markers`: array of `{key, cfy_id, kind}` | **Full replacement** of version-diff gutter markers (`[]` clears). Markers are pointer-transparent document overlays positioned beside resolved blocks, never attributes/styles/wrappers on artifact content, so they coexist with comment highlights and selection without triggering `selectionchange`. Kinds are `modified`, `added`, `moved`, or `removed` (the last targets the nearest surviving neighbor). Re-send on every `ready`. |
| `scroll_to_anchor` | `anchor`, optional `key` | Scroll the anchored element/range into view (`block: "center"`) with a brief attention pulse. When `key` matches a live decoration the pulse lands exactly on it; otherwise the anchor is resolved fresh. Smooth motion and the pulse are disabled under `prefers-reduced-motion: reduce`. |

### Hover and focus affordances (v1 decision)

Commentable elements (`[data-cfy-id]`) always show a subtle dashed-outline
hover affordance, and keyboard-focusable diagram nodes show a solid focus ring
— there is **no** "commenting enabled" toggle message in
v1. Rationale: commenting is always available in the viewer, the affordance
is zero-specificity (any artifact rule overrides it), and it doubles as
discoverability for click-to-comment. If a mode toggle is ever needed, add a
`set_mode` shell→artifact message rather than overloading an existing one.

### Reserved DOM footprint

Everything the bridge touches stays inside the reserved `data-cfy-*` /
`__cfy*` namespaces (artifact-spec §1): the `data-cfy-bridge` script/style
markers, `data-cfy-hl` / `data-cfy-hl-key` decoration attributes,
`data-cfy-pulse` for the scroll pulse, and the `cfy-pulse` keyframes name.
Decorated wrappers/elements may also carry `data-cfy-hl-state` (`saved` or
`answered`) so state tinting remains inside the reserved namespace.
Text-node splits made while wrapping highlight spans are re-merged
(`Node.normalize()`) when the decoration is removed.

### Errors

Errors are small styled HTML pages (dark-mode aware, same CSP) designed to
render inside the viewer iframe:

| Status | Case |
|---|---|
| `400` | Malformed path: wrong segment count, bad charset, traversal attempts (`..`, `%2e%2e`, `\`), bad version syntax (`0`, `v3`, `Latest`) |
| `404` | Unknown thread; unknown version; `latest` on a thread with no artifact versions yet; DB row present but file missing on disk |
| `405` | Non-GET method |
| `500` | Database/read errors; DB-stored project id or slug fails path-segment validation (defense in depth — never expected) |

---

_Endpoints to be added by later beads: `get-context`, `list-comments`,
`resolve-comment`, `status` (§5.2). Each should get its own section here with
request/response shapes and any events it emits._
