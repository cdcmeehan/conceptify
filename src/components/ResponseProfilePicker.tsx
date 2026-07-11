import { useEffect, useId, useMemo, useRef, useState } from "preact/hooks";
import {
  listSkillCapabilities,
  recommendSkills,
  type ResponseIntent,
  type VisualPurpose,
  type SkillCapability,
  type SkillRecommendation,
} from "../lib/api";

type SkillMode = "auto" | "none" | "manual";

interface Props {
  question: string;
  intent: ResponseIntent;
  skillMode: SkillMode;
  selectedSkillIds: string[];
  origins?: Record<"depth" | "language" | "visuals" | "shape" | "visual_purpose", "product" | "user" | "project" | "question">;
  onChange: (patch: {
    responseIntent?: ResponseIntent;
    skillMode?: SkillMode;
    selectedSkillIds?: string[];
  }) => void;
  compact?: boolean;
  onSaveDefault?: (scope: "user" | "project") => Promise<void>;
  onResetToInherited?: () => void;
  onResetDefault?: (scope: "user" | "project") => Promise<void>;
}

const DEPTH = [
  ["quick", "Quick", "Essential idea and why it matters."],
  ["balanced", "Balanced", "Core idea, trade-offs, and an example."],
  ["deep", "Deep", "Full model, edge cases, and connections."],
] as const;
const LANGUAGE = [
  ["plain", "Plain language", "Define terms and avoid unexplained jargon."],
  ["familiar", "Familiar", "Assume the basics; explain specialist terms."],
  ["domain_native", "Domain-native", "Use the field’s normal terminology."],
] as const;
const VISUALS = [
  ["auto", "When useful", "Use a visual only when it earns its place."],
  ["prefer", "Prefer visuals", "Lead with a useful diagram or map when possible."],
  ["avoid", "Text only", "Do not generate diagrams or images."],
] as const;
const SHAPE = [
  ["auto", "Best fit", "Choose the clearest structure."],
  ["walkthrough", "Walkthrough", "Teach it in ordered steps."],
  ["comparison", "Comparison", "Put alternatives side by side."],
  ["reference", "Reference", "Make it easy to scan later."],
] as const;
const VISUAL_PURPOSE = [
  ["auto", "Automatic", "Choose the smallest visual that clarifies the question."],
  ["compare", "Compare", "Align alternatives so differences are visible."],
  ["sequence", "Sequence", "Show order, direction, or timing."],
  ["relationships", "Relationships", "Map meaningful connections between concepts."],
  ["hierarchy", "Hierarchy", "Show parent, child, and nested levels."],
  ["values", "Plot values", "Chart measured values and retain exact numbers."],
  ["interactive", "Interactive model", "Let inputs change only when that teaches something."],
] as const satisfies ReadonlyArray<readonly [VisualPurpose, string, string]>;

const LABELS = {
  depth: Object.fromEntries(DEPTH.map(([value, label]) => [value, label])),
  language: Object.fromEntries(LANGUAGE.map(([value, label]) => [value, label])),
  visuals: Object.fromEntries(VISUALS.map(([value, label]) => [value, label])),
  shape: Object.fromEntries(SHAPE.map(([value, label]) => [value, label])),
  visual_purpose: Object.fromEntries(VISUAL_PURPOSE.map(([value, label]) => [value, label])),
} as Record<keyof Omit<ResponseIntent, "version">, Record<string, string>>;

let catalogPromise: Promise<SkillCapability[]> | null = null;
function loadCatalog(): Promise<SkillCapability[]> {
  catalogPromise ??= listSkillCapabilities();
  return catalogPromise;
}

function profileSummary(intent: ResponseIntent): string {
  const purpose = intent.visuals !== "avoid" && intent.visual_purpose !== "auto"
    ? ` · ${LABELS.visual_purpose[intent.visual_purpose]}`
    : "";
  return `${LABELS.depth[intent.depth]} · ${LABELS.language[intent.language]} · ${LABELS.visuals[intent.visuals]}${purpose} · ${LABELS.shape[intent.shape]}`;
}

export function ResponseProfilePicker({
  question,
  intent,
  skillMode,
  selectedSkillIds,
  origins,
  onChange,
  compact = false,
  onSaveDefault,
  onResetToInherited,
  onResetDefault,
}: Props) {
  const [open, setOpen] = useState(false);
  const [catalog, setCatalog] = useState<SkillCapability[]>([]);
  const [recommendations, setRecommendations] = useState<SkillRecommendation[]>([]);
  const [catalogError, setCatalogError] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [preferenceMessage, setPreferenceMessage] = useState<string | null>(null);
  const [savingPreference, setSavingPreference] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const controlId = useId();

  useEffect(() => {
    let active = true;
    void loadCatalog()
      .then((items) => {
        if (active) setCatalog(items);
      })
      .catch((error: unknown) => {
        if (active) setCatalogError(error instanceof Error ? error.message : String(error));
      });
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    if (!open || skillMode === "none") {
      setRecommendations([]);
      return;
    }
    let active = true;
    const timer = window.setTimeout(() => {
      void recommendSkills(
        question,
        intent,
        skillMode === "manual" ? selectedSkillIds : [],
      )
        .then((items) => {
          if (active) setRecommendations(items);
        })
        .catch((error: unknown) => {
          if (active) setCatalogError(error instanceof Error ? error.message : String(error));
        });
    }, 180);
    return () => {
      active = false;
      window.clearTimeout(timer);
    };
  }, [open, question, intent, skillMode, selectedSkillIds.join("\u0000")]);

  const recommendedIds = useMemo(
    () => new Set(recommendations.filter((item) => !item.selected_manually).map((item) => item.skill.id)),
    [recommendations],
  );
  const reasonById = useMemo(
    () => new Map(recommendations.map((item) => [item.skill.id, item.reason])),
    [recommendations],
  );
  const normalizedSearch = search.trim().toLowerCase();
  const visibleSkills = normalizedSearch === ""
    ? catalog
    : catalog.filter((skill) =>
        [skill.name, skill.outcome, ...skill.supported_intents]
          .join(" ")
          .toLowerCase()
          .includes(normalizedSearch),
      );

  const skillSummary = skillMode === "none"
    ? "No extra skill"
    : skillMode === "manual"
      ? selectedSkillIds.length === 0
        ? "Choose a skill"
        : `${selectedSkillIds.length} chosen`
      : recommendations.length > 0
        ? `${recommendations.length} suggested`
        : "Skills automatic";
  const summary = profileSummary(intent);
  const originSummary = origins == null
    ? null
    : new Set(Object.values(origins)).size === 1
      ? Object.values(origins)[0]
      : "mixed";

  function close() {
    setOpen(false);
    window.setTimeout(() => triggerRef.current?.focus(), 0);
  }

  function setIntent<K extends keyof Omit<ResponseIntent, "version">>(
    dimension: K,
    value: ResponseIntent[K],
  ) {
    onChange({ responseIntent: { ...intent, [dimension]: value } });
  }

  function setVisualPurpose(purpose: VisualPurpose) {
    const shape = purpose === "compare"
      ? "comparison"
      : purpose === "sequence"
        ? "walkthrough"
        : intent.shape;
    onChange({ responseIntent: { ...intent, visuals: purpose === "auto" ? intent.visuals : "prefer", visual_purpose: purpose, shape } });
  }

  function setSkillMode(next: SkillMode) {
    onChange({
      skillMode: next,
      selectedSkillIds: next === "manual" ? selectedSkillIds : [],
    });
  }

  function toggleSkill(id: string) {
    const selected = new Set(selectedSkillIds);
    if (selected.has(id)) selected.delete(id);
    else selected.add(id);
    onChange({ skillMode: "manual", selectedSkillIds: [...selected] });
  }

  async function saveDefault(scope: "user" | "project") {
    if (onSaveDefault == null) return;
    setSavingPreference(true);
    setPreferenceMessage(null);
    try {
      await onSaveDefault(scope);
      setPreferenceMessage(scope === "project" ? "Saved as this project’s default." : "Saved as your default.");
    } catch (error) {
      setPreferenceMessage(error instanceof Error ? error.message : String(error));
    } finally {
      setSavingPreference(false);
    }
  }

  async function clearDefault(scope: "user" | "project") {
    if (onResetDefault == null) return;
    setSavingPreference(true);
    setPreferenceMessage(null);
    try {
      await onResetDefault(scope);
      setPreferenceMessage(scope === "project" ? "Project default cleared." : "Your default cleared.");
    } catch (error) {
      setPreferenceMessage(error instanceof Error ? error.message : String(error));
    } finally {
      setSavingPreference(false);
    }
  }

  return (
    <div
      class="relative"
      onKeyDown={(event) => {
        if (event.key === "Escape" && open) {
          event.stopPropagation();
          close();
        }
      }}
    >
      <button
        ref={triggerRef}
        type="button"
        class={`group flex w-full items-center gap-2 rounded-ctl border border-line bg-well/45 text-left transition-colors hover:border-accent/40 hover:bg-well ${compact ? "px-2 py-1.5" : "px-2.5 py-2"}`}
        aria-expanded={open}
        aria-controls={`${controlId}-response-profile`}
        aria-label={`Response profile: ${summary}. ${skillSummary}. ${open ? "Close" : "Edit"} response options.`}
        onClick={() => setOpen((value) => !value)}
      >
        <span class="flex h-5 w-5 shrink-0 items-center justify-center rounded-full border border-accent/25 bg-accent-bg font-serif text-[11px] font-semibold text-accent-ink" aria-hidden="true">
          Aa
        </span>
        <span class="min-w-0 flex-1">
          <span class="block truncate text-[10px] font-semibold text-ink">{summary}</span>
          <span class="block truncate text-[9px] text-muted">{skillSummary}</span>
        </span>
        <span class={`text-[10px] text-muted transition-transform ${open ? "rotate-180" : ""}`} aria-hidden="true">⌄</span>
      </button>

      {open ? (
        <div
          id={`${controlId}-response-profile`}
          class="mt-1.5 overflow-hidden rounded-ctl border border-line bg-paper shadow-lg"
        >
          <div class="border-b border-line bg-well/35 px-3 py-2.5">
            <div class="flex items-start justify-between gap-3">
              <div>
                <p class="font-serif text-sm font-semibold text-ink">Shape the answer</p>
                <p class="mt-0.5 text-[10px] leading-relaxed text-muted">Depth and language are separate—you can ask for a deep explanation in plain words.</p>
              </div>
              <button type="button" onClick={close} class="cfy-btn cfy-btn-ghost h-6 px-2 text-[10px]">Done</button>
            </div>
          </div>

          <div class="grid gap-3 p-3 min-[520px]:grid-cols-2">
            <ChoiceGroup groupId={controlId} legend="Depth" origin={origins?.depth} dimension="depth" value={intent.depth} choices={DEPTH} onChange={setIntent} />
            <ChoiceGroup groupId={controlId} legend="Language" origin={origins?.language} dimension="language" value={intent.language} choices={LANGUAGE} onChange={setIntent} />
            <ChoiceGroup groupId={controlId} legend="Visuals" origin={origins?.visuals} dimension="visuals" value={intent.visuals} choices={VISUALS} onChange={setIntent} />
            <ChoiceGroup groupId={controlId} legend="Shape" origin={origins?.shape} dimension="shape" value={intent.shape} choices={SHAPE} onChange={setIntent} />
            <fieldset class="min-[520px]:col-span-2">
              <legend class="cfy-label mb-1.5">
                Visual purpose{origins?.visual_purpose != null ? <span class="ml-1 normal-case tracking-normal text-muted">· {origins.visual_purpose === "question" ? "this question" : origins.visual_purpose}</span> : null}
              </legend>
              <p class="mb-2 text-[9px] leading-snug text-muted">Start with the relationship you need to see. Choosing a purpose requests a fitting visual; Text only remains a hard constraint above.</p>
              <div class="grid gap-1 min-[520px]:grid-cols-2">
                {VISUAL_PURPOSE.map(([purpose, label, description]) => (
                  <label key={purpose} class={`flex cursor-pointer items-start gap-2 rounded-ctl border px-2 py-1.5 ${intent.visual_purpose === purpose ? "border-accent/40 bg-accent-bg/45" : "border-line bg-paper hover:bg-well/35"}`}>
                    <input type="radio" name={`${controlId}-visual-purpose`} value={purpose} checked={intent.visual_purpose === purpose} onChange={() => setVisualPurpose(purpose)} class="mt-0.5 accent-current" />
                    <span>
                      <span class="block text-[10px] font-semibold text-ink">{label}</span>
                      <span class="block text-[9px] leading-snug text-muted">{description}</span>
                    </span>
                  </label>
                ))}
              </div>
            </fieldset>
          </div>

          <div class="border-t border-line bg-well/25 p-3">
            <div class="mb-2 flex items-end justify-between gap-3">
              <div>
                <p class="cfy-label">Skills</p>
                <p class="mt-0.5 text-[10px] text-muted">Optional capabilities that change what gets made.</p>
              </div>
              <span class="cfy-chip bg-paper text-muted">Local matching</span>
            </div>

            <div class="grid grid-cols-3 gap-1 rounded-ctl bg-well p-1" role="radiogroup" aria-label="Skill selection mode">
              {(["auto", "none", "manual"] as const).map((mode) => (
                <label key={mode} class={`cursor-pointer rounded px-2 py-1.5 text-center text-[10px] font-medium ${skillMode === mode ? "bg-paper text-ink shadow-sm" : "text-muted hover:text-ink"}`}>
                  <input type="radio" name={`${controlId}-skill-mode`} value={mode} checked={skillMode === mode} onChange={() => setSkillMode(mode)} class="sr-only" />
                  {mode === "auto" ? "Suggest" : mode === "none" ? "No skill" : "Choose"}
                </label>
              ))}
            </div>

            {skillMode === "none" ? (
              <p class="mt-2 rounded-ctl border border-dashed border-line bg-paper px-2.5 py-2 text-[10px] leading-relaxed text-muted">No extra skill will be requested. Your response profile still applies.</p>
            ) : (
              <div class="mt-2">
                {skillMode === "manual" ? (
                  <input value={search} onInput={(event) => setSearch((event.currentTarget as HTMLInputElement).value)} type="search" class="cfy-input mb-2 h-8 text-[10px]" placeholder="Search skills by outcome…" aria-label="Search available skills" />
                ) : null}
                {catalogError != null ? (
                  <p class="rounded-ctl bg-danger-bg px-2.5 py-2 text-[10px] text-danger">Skills could not be loaded: {catalogError}</p>
                ) : visibleSkills.length === 0 ? (
                  <p class="rounded-ctl border border-dashed border-line bg-paper px-2.5 py-3 text-center text-[10px] text-muted">{catalog.length === 0 ? "Checking installed skills…" : "No skills match that search."}</p>
                ) : (
                  <ul class="flex max-h-56 flex-col gap-1.5 overflow-y-auto" aria-label="Available skills">
                    {visibleSkills.map((skill) => {
                      const chosen = selectedSkillIds.includes(skill.id);
                      const suggested = recommendedIds.has(skill.id);
                      const show = skillMode === "manual" || suggested;
                      if (!show) return null;
                      return (
                        <li key={skill.id}>
                          <label class={`block rounded-ctl border bg-paper px-2.5 py-2 ${chosen ? "border-accent/50 ring-1 ring-accent/15" : suggested ? "border-info/35" : "border-line"} ${skill.availability.available ? "cursor-pointer" : "cursor-not-allowed opacity-65"}`}>
                            <div class="flex items-start gap-2">
                              {skillMode === "manual" ? <input type="checkbox" checked={chosen} disabled={!skill.availability.available} onChange={() => toggleSkill(skill.id)} class="mt-0.5 accent-current" /> : null}
                              <span class="min-w-0 flex-1">
                                <span class="flex flex-wrap items-center gap-1.5">
                                  <span class="text-[11px] font-semibold text-ink">{skill.name}</span>
                                  {suggested ? <span class="cfy-chip bg-info-bg text-info">Suggested</span> : null}
                                  {chosen ? <span class="cfy-chip bg-accent-bg text-accent-ink">Chosen</span> : null}
                                  {!skill.availability.available ? <span class="cfy-chip bg-well text-muted">Unavailable</span> : null}
                                </span>
                                <span class="mt-0.5 block text-[10px] leading-relaxed text-muted">{reasonById.get(skill.id) ?? skill.outcome}</span>
                                <span class="mt-1 block text-[9px] text-muted">Makes: {skill.expected_outputs.join(" · ")} · {skill.latency_hint} setup</span>
                                {!skill.availability.available && skill.availability.reason != null ? <span class="mt-1 block text-[9px] text-warn">{skill.availability.reason}</span> : null}
                              </span>
                            </div>
                          </label>
                        </li>
                      );
                    })}
                  </ul>
                )}
                {skillMode === "auto" && recommendations.length === 0 && catalog.length > 0 ? (
                  <p class="rounded-ctl border border-dashed border-line bg-paper px-2.5 py-2 text-[10px] leading-relaxed text-muted">No extra skill fits yet. That is normal—ordinary questions need none.</p>
                ) : null}
              </div>
            )}
          </div>
          {onSaveDefault != null ? (
            <div class="border-t border-line bg-paper px-3 py-2.5">
              <div class="flex flex-wrap items-center justify-between gap-2">
                <div>
                  <p class="cfy-label">Defaults</p>
                  <p class="mt-0.5 text-[9px] text-muted">
                    {originSummary === "question" ? "Edited for this question only." : originSummary === "project" ? "Inherited from this project." : originSummary === "user" ? "Inherited from your default." : originSummary === "product" ? "Using Conceptify defaults." : "Inherited from more than one scope."}
                  </p>
                </div>
                <div class="flex flex-wrap gap-1.5">
                  {onResetToInherited != null && Object.values(origins ?? {}).includes("question") ? (
                    <button type="button" onClick={onResetToInherited} class="cfy-btn cfy-btn-ghost h-7 px-2 text-[9px]">Reset to inherited</button>
                  ) : null}
                  {onResetDefault != null && Object.values(origins ?? {}).includes("project") ? (
                    <button type="button" disabled={savingPreference} onClick={() => void clearDefault("project")} class="cfy-btn cfy-btn-ghost h-7 px-2 text-[9px]">Clear project default</button>
                  ) : null}
                  {onResetDefault != null && Object.values(origins ?? {}).includes("user") ? (
                    <button type="button" disabled={savingPreference} onClick={() => void clearDefault("user")} class="cfy-btn cfy-btn-ghost h-7 px-2 text-[9px]">Clear my default</button>
                  ) : null}
                  <button type="button" disabled={savingPreference} onClick={() => void saveDefault("user")} class="cfy-btn cfy-btn-secondary h-7 px-2 text-[9px]">Make my default</button>
                  <button type="button" disabled={savingPreference} onClick={() => void saveDefault("project")} class="cfy-btn cfy-btn-secondary h-7 px-2 text-[9px]">Use for project</button>
                </div>
              </div>
              {preferenceMessage != null ? <p class="mt-1.5 text-[9px] text-muted" role="status">{preferenceMessage}</p> : null}
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

function ChoiceGroup<K extends keyof Omit<ResponseIntent, "version">>({
  groupId,
  legend,
  origin,
  dimension,
  value,
  choices,
  onChange,
}: {
  groupId: string;
  legend: string;
  origin?: "product" | "user" | "project" | "question";
  dimension: K;
  value: ResponseIntent[K];
  choices: ReadonlyArray<readonly [ResponseIntent[K], string, string]>;
  onChange: <D extends keyof Omit<ResponseIntent, "version">>(dimension: D, value: ResponseIntent[D]) => void;
}) {
  return (
    <fieldset>
      <legend class="cfy-label mb-1.5">
        {legend}{origin != null ? <span class="ml-1 normal-case tracking-normal text-muted">· {origin === "question" ? "this question" : origin}</span> : null}
      </legend>
      <div class="flex flex-col gap-1">
        {choices.map(([choice, label, description]) => (
          <label key={String(choice)} class={`flex cursor-pointer items-start gap-2 rounded-ctl border px-2 py-1.5 ${value === choice ? "border-accent/40 bg-accent-bg/45" : "border-line bg-paper hover:bg-well/35"}`}>
            <input type="radio" name={`${groupId}-response-${dimension}`} value={String(choice)} checked={value === choice} onChange={() => onChange(dimension, choice)} class="mt-0.5 accent-current" />
            <span>
              <span class="block text-[10px] font-semibold text-ink">{label}</span>
              <span class="block text-[9px] leading-snug text-muted">{description}</span>
            </span>
          </label>
        ))}
      </div>
    </fieldset>
  );
}
