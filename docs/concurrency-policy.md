# Run concurrency, queueing, and write-conflict policy

Status: accepted  
Decision: `conceptify-k9z.1`  
Date: 2026-07-11

This document is the contract for the concurrent run scheduler and every UI
that submits, displays, cancels, retries, or applies a run. It supersedes the
Phase 1 one-active-run-per-thread rule. The existing process supervision,
immutable run history, and atomic artifact-file writes remain in force.

## Goals and invariants

- A user may submit independent questions immediately. Lack of execution
  capacity queues work; it does not reject the submission.
- Artifact exploration may overlap. Artifact mutation is ordered and may not
  silently overwrite work derived from a different base version.
- Every accepted submission has a durable row before it can execute. Queue
  state, cancellation, interruption, retry, and completion remain auditable.
- Provider capacity is configuration, not UI structure. Adding a provider or
  changing a limit must not require a new queue or screen design.
- A terminal run never becomes non-terminal again. Retry creates a new run
  linked to the original.

## Run classification

Classification is explicit on the durable run record; the scheduler must not
infer it later from prompt text.

| Run class | Current modes | Artifact access | Scheduling rule |
| --- | --- | --- | --- |
| `exploration` | `answer` | Reads a captured artifact version; may append answers to comments, but does not publish an artifact version | May overlap with other runs, including runs for the same thread |
| `mutation` | `apply`; `ask` while publishing its initial artifact | Reads a captured base and may publish an artifact version | At most one executing mutation per target thread; stale-base check required before publish |

An `ask` targets a newly-created thread and therefore normally cannot conflict,
but it is still a mutation: initial publication is a write and follows the same
durability and cancellation rules. Future modes must declare a class and target
when they are introduced. Unknown classes are not executable.

Each submission captures these immutable scheduling inputs:

- `run_class`;
- `project_id` and `thread_id` target;
- `base_artifact_version` (nullable only when the target has no artifact yet);
- resolved provider-pool key and the model/route intent needed by retry;
- `submitted_at` and a monotonic queue sequence used to break timestamp ties;
- optional `retry_of_run_id`.

The captured base is the version used to assemble context. It is never silently
advanced while a run waits in the queue.

## Capacity and worker pools

Execution capacity is controlled by provider pools. The pool key is a stable
string derived during routing, for example `anthropic`, `openai`,
`openrouter`, or a future `local:<endpoint-id>`. Different routes that consume
the same upstream quota must resolve to the same pool key.

Limits are stored as a keyed map, not fixed columns or provider-specific UI:

```json
{
  "default": 1,
  "pools": {
    "anthropic": 2,
    "openai": 2,
    "openrouter": 3
  }
}
```

The settings surface may render rows from discovered pool keys, but the
scheduler accepts any non-empty key. A missing key uses `default`. Limits are
positive integers; invalid configuration falls back to the last valid value,
then to a compiled default of `1`. A reduced limit prevents new admissions but
does not kill already-running work. Capacity is counted across all projects and
run classes because it represents upstream process, rate, and cost pressure.

Provider throttling does not consume a worker in a retry loop. A retryable
rate-limit response moves the run to `throttled` with a durable `not_before`
time, releases the slot, and re-enters admission when eligible. The scheduler
uses provider guidance such as `Retry-After`, with bounded exponential backoff
and jitter when no guidance exists.

## Durable states and transitions

The non-terminal states are:

- `queued`: accepted and eligible once capacity and target guards allow;
- `starting`: atomically owns a provider slot and, for mutations, the target
  guard, but the child has not yet been confirmed spawned;
- `running`: child supervision is active;
- `throttled`: paused until `not_before` after a retryable provider limit;
- `cancelling`: cancellation is durable and process termination is in progress.

Terminal states are `completed`, `conflicted`, `failed`, `cancelled`, and
`timeout`. `conflicted` means generation finished but its artifact publication
was refused because the captured base was stale; it is a result that needs user
attention, not a process failure.

```text
submit -> queued -> starting -> running -> completed / conflicted
             |          |         |  \-> failed / timeout
             |          |         \----> cancelling -> cancelled
             |          \--------------> failed / cancelled
             \-> cancelled

running -> throttled -> queued
```

State changes use compare-and-set transactions (`WHERE status = <expected>`),
so two scheduler wakes cannot admit or finish the same run. Slot ownership and
the `starting` transition are committed together. Process spawn happens only
after that commit. A spawn failure ends as `failed`; it is never erased.

The scheduler publishes a state-change event after the transaction commits.
The event includes run id, state, queue position when meaningful, and a reason
code. Consumers must refresh from durable state after reconnect; events are a
low-latency hint, not the source of truth.

## Ordering, queue position, and fairness

There is one logical queue shown to the user and provider-pool capacity beneath
it. Admission considers only eligible `queued` rows for a pool.

1. Order eligible rows by `submitted_at`, then queue sequence.
2. If the head row is blocked only by a mutation already executing for its
   target thread, scan forward for the first runnable row. A blocked target
   must not idle unrelated capacity.
3. After each admission from a project, prefer the oldest eligible row from a
   different project when one exists. This round-robin tie-break prevents a
   bulk submit in one project from starving another while preserving FIFO
   within each project.
4. `not_before` rows are ineligible. They become eligible without losing their
   original submission order.

Displayed queue position is a snapshot, not a promise. It is the row's rank
among currently eligible work in its provider pool, plus the count of earlier
temporarily blocked rows. The UI labels it “Queued · approximately Nth” and
updates it on state-change events or refresh. Running, starting, throttled, and
terminal rows do not have a queue position.

No user-visible priority lane exists in this phase. Retries are new submissions
and join the tail; cancellation never promotes a related run specially.

## Cancellation, timeout, and retry

- Cancelling `queued` or `throttled` work atomically marks it `cancelled`; no
  child is spawned.
- Cancelling `starting` records the request before touching the process. If
  spawn wins the race, the supervisor observes the durable cancellation and
  kills the process group immediately.
- Cancelling `running` first transitions to `cancelling`, then uses the existing
  process-group kill. The slot and mutation guard are released only when the
  supervisor has reaped or abandoned the child and committed `cancelled`.
- Repeated cancellation is idempotent for non-terminal states. Cancelling a
  terminal run is a no-op that returns its current state.
- Timeout follows the same release discipline and ends in `timeout`.
- Manual retry creates a new `queued` row with `retry_of_run_id`; it retains the
  original row and captured model override, but captures the artifact version
  that is current when the user retries.

## Restart and crash recovery

Queued and throttled rows survive restart. On startup, before admitting work:

1. `queued` remains `queued`.
2. `throttled` remains so when `not_before` is in the future; otherwise it
   transitions to `queued`.
3. `starting`, `running`, and `cancelling` from the previous process transition
   to `failed` with reason `app_interrupted`. They are not auto-replayed because
   an external agent might have performed side effects before the crash.
4. All in-memory provider slots and mutation guards are rebuilt empty from
   newly admitted work; stale rows never retain capacity.

The scheduler then performs one admission pass. A user may explicitly retry an
interrupted run, producing a linked row as above.

## Artifact mutation and conflict rules

Serialization and conflict detection solve different problems. The existing
database connection lock and atomic file rename prevent two files from claiming
the same version number. They do not make a stale generated result safe to
publish.

Before a mutation publishes, it compares its immutable
`base_artifact_version` with the thread's latest version in the same transaction
that reserves the next version:

- If they match, publication may proceed.
- If both are null, an initial artifact may publish.
- Otherwise the run completes its generation but enters a durable
  `conflicted` result condition and publishes no artifact version. The newer
  base version and the generated candidate remain available for compare/apply
  UX. The run must not be reported as an ordinary successful apply.

Only one mutation for a thread executes at a time, in queue order. This reduces
wasted generation but is not permission to rebase silently: two mutations may
both have captured version 3 before either ran, so the second must conflict
after the first publishes version 4.

Exploration results are anchored to their captured version and may finish after
a newer artifact is published. Their provenance shows that version. Appending
an answer to a still-existing comment is allowed; applying that answer later is
a new mutation with a fresh explicit base.

Resolution of a conflicted candidate is always a separate, auditable action:

- compare and explicitly apply it against the current version;
- regenerate/rebase from current context;
- synthesize alternatives into a new candidate; or
- preserve it as a separate version/branch once that feature exists.

Automatic merge is forbidden in this phase. Later support may propose only
demonstrably non-overlapping semantic changes, still with a preview and an
explicit user confirmation. No completion-order or last-writer-wins path may
publish a stale result.

## Consequences and rejected alternatives

- A single global serial queue was rejected because unrelated exploration
  would wait behind mutations and one slow provider would stall all others.
- Unlimited concurrency was rejected because CLI processes and upstream quotas
  are finite, throttling would amplify, and cost would be surprising.
- Fully independent per-project queues were rejected because provider limits
  are global; project-aware round robin supplies fairness without oversubscription.
- Optimistic last-writer-wins artifact saves were rejected because serialized
  version allocation cannot detect semantic overwrite.
- Cancelling interrupted rows and automatically replaying running work were
  rejected: cancellation misstates what happened, while replay can duplicate
  external side effects. `failed/app_interrupted` plus explicit retry is honest.

## Implementation obligations

The scheduler bead may choose concrete table and type names, but it must
preserve this contract and test at least:

- provider capacity, dynamic limit changes, and cross-project fairness;
- concurrent exploration on one thread;
- serialized same-thread mutations and runnable-work scan-around;
- compare-and-set admission/cancellation races;
- queued/throttled restart recovery and interrupted running work;
- cancellation releasing capacity only after supervision ends;
- retry lineage and fresh-base capture;
- stale-base publication refusal, including two mutations submitted from the
  same base.
