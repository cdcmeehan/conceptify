# Quick-start information architecture

Status: accepted prototype  
Decision: `conceptify-vc1.1`  
Date: 2026-07-11

This is the interaction contract for starting a Conceptify project. It keeps
the current sidebar's one-action speed while making the context, destination,
and optional first ask predictable before backend expansion.

## The two intents

The first choice is about what the person wants to understand, not how storage
works:

| Choice | Supporting copy | Minimum input | Context created |
| --- | --- | --- | --- |
| **Explore a folder** | “Understand an existing codebase or set of files.” | One readable directory | Maps the chosen directory; name is inferred |
| **Learn a topic** | “Start with a subject; add sources now or later.” | Topic name | Creates an app-managed project folder |

“Project” remains the resulting object in navigation, but the setup does not
ask a topic learner to understand folders. “Codebase” was rejected as the first
label because valid context can be notes, documents, or an unversioned folder.
“Blank project” was rejected because it describes implementation, not intent.

The default is no preselected intent on first launch: both choices receive
equal weight and can be understood before acting. For a returning user, the
panel remembers only the last *intent tab*, never a path, topic, or question.
This saves one click without silently reusing sensitive context.

## Progressive single-sheet prototype

The sheet stays in the 14-rem sidebar on wide windows and becomes a centred
sheet when that width cannot preserve labels at 200% zoom.

```text
Start a project                                      ×
What would you like to understand?

┌ Explore a folder ────────────────────────────────┐
│ Existing code or files · Uses this folder only   │
└──────────────────────────────────────────────────┘
┌ Learn a topic ───────────────────────────────────┐
│ A subject such as cryptography · Sources optional│
└──────────────────────────────────────────────────┘
```

Selecting an intent expands in place; it does not open a wizard page.

### Explore a folder

```text
Folder
┌ Choose folder… ──────────────────────────────────┐
│ No folder selected                               │
└──────────────────────────────────────────────────┘

First question (optional)
┌ Give me a useful overview of this folder… ───────┐
└──────────────────────────────────────────────────┘

What Create does
Map this folder as “conceptify”. Files stay where they are.

Cancel                         Create project / Create & ask
```

The folder is the only required value. Name is inferred from its final path
component and can be changed later. After selection, the summary shows the
short path, detected context (“Git repository”, “Files”, or “Empty folder”),
and readiness. Full absolute paths are available in a title/accessible
description but do not dominate the narrow sheet.

### Learn a topic

```text
Topic
┌ Distributed systems ─────────────────────────────┐
└──────────────────────────────────────────────────┘

First question (optional)
┌ Give me a clear overview of distributed systems.┐
└──────────────────────────────────────────────────┘

Sources (optional)                         Add later
No source folder yet

What Create does
Create a private Conceptify folder named “Distributed systems”.

Cancel                         Create project / Create & ask
```

The topic name is the only required value. The first-question field gains the
editable suggestion “Give me a clear overview of {topic}.” once the topic loses
focus, but only while the question is untouched. Optional source context is a
progressive row, never a prerequisite. No external search is implied.

## Button and destination contract

The primary label is the exact outcome:

- Empty first question: **Create project**. Create/map, select it, and land on
  project home with the question composer focused.
- Non-empty first question: **Create & ask**. Create/map, persist the ask, and
  land on its generating thread immediately.
- Existing mapping: **Open existing project**. Never create a duplicate or
  change its name implicitly; preserve the drafted question so the user can
  choose **Open & ask**.

The always-visible “What Create does” sentence updates from actual inputs. It
names folder mapping versus app-managed folder creation and makes clear that an
ask will launch when present. There is no generic “Continue” or “Done.”

## State and recovery matrix

| Scenario | Prototype behavior | Recovery invariant |
| --- | --- | --- |
| First launch | Empty-state action opens the two equal intent cards; focus moves to “Explore a folder” | Escape/Cancel returns focus to “Create a project” |
| Returning user | New project opens on the last intent tab in its pristine state | No path, topic, question, or error is restored implicitly |
| Picker cancelled | Sheet remains open; selected-folder summary remains unchanged; no error appears | Focus returns to “Choose folder…” and every typed question remains |
| Unreadable/invalid folder | Inline error beneath Folder: “This folder can’t be read. Choose another folder.” | Topic/question/last good folder stay intact; primary action disabled only until valid context exists |
| Duplicate folder | Inline existing-project card names it and offers “Open existing project” / “Open & ask” | No duplicate row, rename, or archive mutation; draft is not lost |
| Topic-only | App-managed folder preview and optional overview question are shown | Sources can be added later; creation needs no native picker |
| Creation failure | Specific inline reason, inputs retained, primary action becomes “Try again” | Never close the sheet or leave an invisible half-created project |
| Creation succeeds, ask fails | Project remains selected; failed ask appears durably with Edit & retry | Creation is not rolled back and the question remains recoverable |

Busy state disables intent switching and dismissal only after the create
transaction begins. Picker time is not a busy state. Duplicate detection is a
successful resolution, not a red error.

## Defaults and terminology

- Sheet title: **Start a project**, launcher: **New project** (navigation noun).
- Folder action: **Choose folder…**, never “Browse” or “Upload.” Nothing is
  copied or uploaded.
- Topic input label: **Topic**, not “Project name”; the created project adopts
  the topic name.
- First ask label: **First question (optional)**. Empty remains a supported
  fast path.
- Context language: **Uses this folder only** / **Sources optional**. Agent,
  skill, model, embedding, and index terminology do not appear.

The response-profile summary from the ordinary question folio appears only
after the first-question field is non-empty. Its default stays compact; opening
advanced response options must not push the primary action off-screen.

## Accessibility contract

- Intent cards are a labelled radio group with arrow-key selection; Enter or
  Space expands the selected panel.
- Focus order follows intent → required input → optional question/context →
  explanation → Cancel → primary action. No hover-only information.
- Picker cancellation and validation messages use a polite live region;
  creation failure uses an assertive error only once.
- Invalid input receives `aria-invalid` and an `aria-describedby` link to the
  concrete recovery message. Focus stays on the invalid control after submit.
- At 200% zoom and a 320px content width, the sheet is one column with no
  truncated primary labels or horizontal scrolling.
- The dynamic primary label and “What Create does” sentence are announced
  together when the optional question crosses empty/non-empty.

## Validation notes

The existing sidebar prototype was walked as both a first-time empty state and
a returning project list. It already proves the desired compact footprint,
native picker cancellation (a no-op), topic-folder creation, duplicate mapping
idempotence, inline failure retention, Escape cancellation, and keyboard entry.
Its main comprehension gaps are the implementation-led “or make one” divider,
the ambiguous “Create folder” destination, no context readiness summary, and no
first ask. The accepted prototype keeps its speed while replacing those gaps
with the two-intent cards, outcome sentence, and optional first question above.

## Implementation boundaries

- `vc1.2` owns folder readiness and duplicate resolution.
- `vc1.3` owns topic-only storage and optional source context.
- `vc1.4` owns the atomic create-then-submit orchestration and recovery split.
- `vc1.5` owns the project-home destination; setup must not duplicate that
  screen's brief or activity detail.

All children must use the same intent names, primary-label rules, state matrix,
and focus behavior. Deviations require updating this decision first.
