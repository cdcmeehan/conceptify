# Response intent contract

Status: accepted  
Decision: `conceptify-l9w.1`  
Date: 2026-07-11

This document defines the provider-neutral contract behind the question
composer. It describes the result a person wants, not which model or prompt
technique should produce it. Provider adapters may translate the contract, but
stored values and user-facing labels do not change with the selected model.

## Decision

Response intent has four independent dimensions. The controls use ordinary
language and a short example; the durable values are stable, lowercase enum
tokens.

| Dimension | User-facing choices | Durable values | Meaning |
| --- | --- | --- | --- |
| Depth | Quick · Balanced · Deep | `quick` · `balanced` · `deep` | Scope and treatment, not vocabulary difficulty |
| Language | Plain language · Familiar · Domain-native | `plain` · `familiar` · `domain_native` | Assumed terminology, not response length |
| Visuals | When useful · Prefer visuals · Text only | `auto` · `prefer` · `avoid` | Whether diagrams/charts should be proposed |
| Shape | Best fit · Walkthrough · Comparison · Reference | `auto` · `walkthrough` · `comparison` · `reference` | The main organization of the answer |

The labels deliberately avoid “simple/advanced,” “beginner/expert,” and
“short/long.” Those words conflate a person's identity with the response they
need. A domain expert can ask for a quick plain-language explanation; a novice
can ask for a deep treatment that introduces domain terms carefully.

### Examples shown in the composer

- **Quick:** “Give me the essential idea and why it matters.”
- **Balanced:** “Explain the core idea, trade-offs, and a useful example.”
- **Deep:** “Develop the model, edge cases, trade-offs, and connections.”
- **Plain language:** “Define necessary terms and avoid unexplained jargon.”
- **Familiar:** “Assume I know the basics; explain specialized terms.”
- **Domain-native:** “Use the field's normal terminology without recapping basics.”
- **When useful:** “Use a visual only when it clarifies the idea.”
- **Prefer visuals:** “Lead with an informative diagram, map, or comparison when possible.”
- **Text only:** “Do not generate a visual; use prose, lists, tables, or code as appropriate.”

Shape examples are “Choose the clearest structure,” “Teach it in ordered
steps,” “Put alternatives side by side,” and “Make it easy to scan later.”

## Stable wire format

Version 1 is a JSON object stored and transported as a unit:

```json
{
  "version": 1,
  "depth": "balanced",
  "language": "familiar",
  "visuals": "auto",
  "shape": "auto"
}
```

All five keys are required after preference resolution. Persisted partial
preferences may omit dimension keys, but a submitted run may not. Unknown
versions or enum values are rejected at the submission boundary with a
dimension-specific error; they are never coerced. Additive metadata may be
ignored by older readers, but changing the meaning of a value requires a new
contract version.

Model, provider, route, estimated duration, and skill choice are deliberately
outside this object. They are execution choices or companion run metadata, not
response intent. A later skill recommendation may read this object and record
its own recommendation and user override beside it.

## Defaults and inheritance

The product default is Balanced + Familiar + When useful + Best fit. It gives
the existing ordinary ask behavior a name without forcing setup. The composer
shows one compact “Balanced · Familiar” summary and keeps the remaining
controls behind progressive disclosure.

Each dimension inherits independently in this order:

1. explicit value for this question;
2. project preference;
3. user preference;
4. product default.

Changing one question does not silently change a project or user preference.
“Use for this project” and “Make my default” are separate explicit actions.
The submitted run stores the fully resolved object plus per-dimension origin
(`question`, `project`, `user`, or `product`) so the resulting thread can show
what it used and later preference changes cannot rewrite history.

Resetting a scoped preference removes that scope's value and immediately
reveals the next inherited value. A project may therefore override only
visuals while inheriting the user's depth, language, and shape.

## Combination and fallback rules

Every cross-dimension combination is valid. In particular:

- Quick + Domain-native means a concise answer using normal field terminology.
- Deep + Plain language means thorough coverage with terms introduced and
  explained, not a shortened answer.
- Prefer visuals + Reference asks for a scannable reference with useful visual
  structure; it does not require decorative imagery.
- Text only + Comparison can use a table or aligned prose because “visuals”
  controls generated explanatory graphics, not basic document structure.

Capabilities can make a requested presentation unavailable, but no adapter may
silently reinterpret the intent. `auto` always permits the clearest supported
result. If `prefer` cannot produce an accessible visual, the run records a
`visuals_unavailable` notice and uses an equivalent textual explanation. If a
specific shape is genuinely inapplicable (for example, Comparison with only
one subject), the answer briefly states that it used the closest useful
structure and the run records `shape_adjusted`. `avoid` is a hard constraint:
the run must not generate a diagram or image.

Safety, artifact validity, and accessibility requirements always outrank the
profile. A request is rejected only for an unknown contract value or when the
underlying question itself cannot be served; unusual but valid combinations
are not blocked.

## Prompt and artifact obligations

Prompt builders consume semantic values, not the English UI labels. Each run
captures the resolved contract before queueing and passes it unchanged through
retry and restart. Provider-specific instructions are implementation details
and must preserve these meanings.

The resulting thread exposes the four resolved choices. Published artifacts
record the contract version and resolved values as provenance. A retry copies
the original profile unless the user explicitly edits it; synthesis records
the profile used for the synthesis rather than pretending all source runs had
that profile.

## Accessibility and localization

Each dimension is a labelled single-select group with a visible selected
state, keyboard arrow navigation, and a short description connected with
`aria-describedby`. Choice is never communicated by color alone. The compact
summary is a button whose accessible name includes all four resolved choices
and whether any are inherited. Focus returns to that button when the expanded
controls close. No hover-only explanation is required to understand a choice.

Copy uses complete translation messages rather than concatenated fragments.
Stable message families are `responseIntent.depth.*`,
`responseIntent.language.*`, `responseIntent.visuals.*`, and
`responseIntent.shape.*`, each with `label` and `description` entries. Enum
tokens are never displayed directly and examples are localizable strings.
Layout must tolerate labels at twice the English length and 200% zoom without
truncating the selected value.

## Alternatives rejected

- A single simple-to-advanced slider was rejected because it couples depth,
  vocabulary, length, and perceived user expertise.
- Persona presets were rejected as the stored contract because they hide which
  property changes and make inheritance ambiguous. Presets may later be
  shortcuts that populate the four explicit dimensions.
- Skill-only configuration was rejected because skills describe capabilities,
  not the desired depth or assumed language.
- Raw prompt modifiers were rejected because they are difficult to discover,
  cannot inherit dimension by dimension, and expose provider-specific wording.

## Implementation obligations

Downstream beads must preserve and test:

- schema validation and forward-version rejection;
- independent dimension inheritance and immutable resolved run provenance;
- all cross-dimension combinations, especially Quick + Domain-native and Deep
  + Plain language;
- explicit fallback notices and the hard Text only constraint;
- keyboard operation, announced selection, focus return, zoom, and long-label
  behavior;
- prompt behavior without persisting provider-specific prompt text as the
  contract.
