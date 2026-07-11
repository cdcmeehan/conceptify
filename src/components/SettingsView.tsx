// Settings overlay (PRD §7.7, FR-7.1–7.4; model selection epic conceptify-e7m).
// A full-panel modal opened from the project sidebar footer. Sections:
//
//  - Appearance (FR-7.2): system | light | dark. Applied live via theme.ts;
//    persisted on Save. The artifact iframe keeps its own prefers-color-scheme
//    (§9 S2 isolation) — surfaced inline for honesty.
//  - Models (epic e7m): per-purpose model choice from the live catalog via
//    searchable, provider-grouped comboboxes (bead e7m.6 catalog; e7m.7
//    routing). Each row shows the RESOLVED ROUTE (via claude CLI / codex CLI /
//    OpenRouter) and inline validation (empty, disabled-suite, OpenRouter
//    choice with no key). A freshness row + Refresh keeps the catalog current.
//  - Providers (epic e7m): suite toggles with counts. Enabling/disabling a
//    suite persists immediately (enabledProviders) and re-filters every picker
//    app-wide (including the point-of-ask picker, bead e7m.4) — see the note on
//    onToggleProvider for why toggles auto-persist.
//  - OpenRouter (bead e7m.7): masked, write-only API key with set/replace/clear
//    states. Required for any model routed via OpenRouter.
//  - Paths (FR-7.3): auto-project base dir (empty = default).
//  - Advanced: the raw adapter templates + default adapter + binary path +
//    timeout — the escape hatch, collapsed and visually secondary (bead e7m.3).
//
// Defaults live in Rust (FR-7.4 zero-config); this UI is for overrides. "Reset
// to defaults" deletes the stored override (the OpenRouter key, stored in its
// own row, survives — bead e7m.7). Invalid command-template JSON, or a config
// the backend rejects, surfaces as an inline error.

import type { ComponentChildren } from "preact";
import { useEffect, useMemo, useState } from "preact/hooks";
import * as api from "../lib/api";
import type {
  AgentSettings,
  Appearance,
  CatalogModel,
  CatalogProvider,
  CatalogResponse,
} from "../lib/api";
import { appStore } from "../store/appStore";
import { setAppearance } from "../lib/theme";
import { relativeTime } from "../lib/time";
import { providerLabel } from "../lib/models";
import { familyOf, routeForModel, routeLabel, type RouteResult } from "../lib/routing";
import { ModelCombobox } from "./ModelCombobox";

const APPEARANCE_OPTIONS: { value: Appearance; label: string }[] = [
  { value: "system", label: "System" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

const DEFAULT_BASE_DIR_HINT = "~/Documents/conceptify/projects";

export function SettingsView() {
  // The last-persisted settings — used to revert a live appearance preview if
  // the user closes without saving, and as the base for a provider-toggle write.
  const [saved, setSaved] = useState<AgentSettings | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  // Editable form state (mirrors AgentSettings, plus the raw adapters textarea
  // and a minutes view of the timeout).
  const [appearance, setAppearanceState] = useState<Appearance>("system");
  const [defaultAdapter, setDefaultAdapter] = useState("");
  const [followUp, setFollowUp] = useState("");
  const [artifactUpdate, setArtifactUpdate] = useState("");
  const [inAppAsk, setInAppAsk] = useState("");
  const [timeoutMins, setTimeoutMins] = useState("30");
  const [binaryPath, setBinaryPath] = useState("");
  const [autoBaseDir, setAutoBaseDir] = useState("");
  const [adaptersText, setAdaptersText] = useState("");
  const [enabledProviders, setEnabledProviders] = useState<Set<string>>(new Set());

  // Catalog + OpenRouter key state (epic e7m).
  const [catalog, setCatalog] = useState<CatalogResponse | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [providerBusy, setProviderBusy] = useState<string | null>(null);
  const [openRouterKeySet, setOpenRouterKeySet] = useState(false);
  const [keyInput, setKeyInput] = useState("");
  const [keyEditing, setKeyEditing] = useState(false);
  const [keyBusy, setKeyBusy] = useState(false);
  const [advancedOpen, setAdvancedOpen] = useState(false);

  const [error, setError] = useState<string | null>(null);
  const [savedFlash, setSavedFlash] = useState(false);
  const [busy, setBusy] = useState(false);

  function populate(s: AgentSettings) {
    setSaved(s);
    setAppearanceState(s.appearance);
    setDefaultAdapter(s.defaultAdapter);
    setFollowUp(s.models.followUp);
    setArtifactUpdate(s.models.artifactUpdate);
    setInAppAsk(s.models.inAppAsk);
    setTimeoutMins(String(Math.max(1, Math.round(s.timeoutSecs / 60))));
    setBinaryPath(s.agentBinaryPath ?? "");
    setAutoBaseDir(s.autoProjectBaseDir ?? "");
    setAdaptersText(JSON.stringify(s.adapters, null, 2));
    setEnabledProviders(new Set(s.enabledProviders));
    setError(null);
  }

  useEffect(() => {
    let alive = true;
    api
      .getAgentSettings()
      .then((s) => {
        if (alive) populate(s);
      })
      .catch((e) => {
        if (alive) setLoadError(String(e));
      });
    // Catalog + key-set flag load independently — a catalog miss must not block
    // the settings form (it degrades to Custom-only pickers), so failures here
    // stay quiet (never an error dialog, per bead e7m.3).
    api
      .getModelCatalog()
      .then((c) => {
        if (alive) setCatalog(c);
      })
      .catch((e) => console.warn("model catalog load failed", e));
    api
      .getAgentOptions()
      .then((o) => {
        if (alive) setOpenRouterKeySet(o.openRouterKeySet);
      })
      .catch((e) => console.warn("agent options load failed", e));
    return () => {
      alive = false;
    };
  }, []);

  // Appearance is previewed live for immediate feedback (FR-7.2 "without
  // restart"); Save persists it.
  function onAppearanceChange(value: Appearance) {
    setAppearanceState(value);
    setAppearance(value);
  }

  function close() {
    // Revert an unsaved appearance preview to the persisted value.
    if (saved != null && appearance !== saved.appearance) {
      setAppearance(saved.appearance);
    }
    appStore.closeSettings();
  }

  // Escape closes the overlay (standard macOS sheet behaviour). An open
  // combobox popover stops Escape from bubbling here, so it only closes itself.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        close();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [saved, appearance]);

  function buildSettings(): AgentSettings | null {
    let adapters: AgentSettings["adapters"];
    try {
      const parsed = JSON.parse(adaptersText);
      if (parsed == null || typeof parsed !== "object" || Array.isArray(parsed)) {
        throw new Error("expected a JSON object of adapter templates");
      }
      adapters = parsed as AgentSettings["adapters"];
    } catch (e) {
      setError(`Command templates: invalid JSON — ${String(e)}`);
      return null;
    }

    const mins = Number(timeoutMins);
    if (!Number.isFinite(mins) || mins < 1) {
      setError("Timeout must be a whole number of minutes (at least 1).");
      return null;
    }

    const trimmedBinary = binaryPath.trim();
    const trimmedBase = autoBaseDir.trim();

    return {
      adapters,
      defaultAdapter,
      models: { followUp, artifactUpdate, inAppAsk },
      timeoutSecs: Math.round(mins * 60),
      agentBinaryPath: trimmedBinary === "" ? null : trimmedBinary,
      appearance,
      autoProjectBaseDir: trimmedBase === "" ? null : trimmedBase,
      enabledProviders: [...enabledProviders].sort(),
      // The scheduler consumes a generic keyed map. Until this view gains a
      // capacity editor, round-trip it verbatim so saving appearance/models
      // cannot reset limits configured by another surface.
      runConcurrency: saved?.runConcurrency ?? {
        default: 1,
        pools: { anthropic: 2, openai: 2, openrouter: 3, manual: 1 },
      },
    };
  }

  async function onSave() {
    setError(null);
    setSavedFlash(false);
    const next = buildSettings();
    if (next == null) return;
    setBusy(true);
    try {
      await api.setAgentSettings(next);
      setSaved(next);
      setAppearance(next.appearance);
      setSavedFlash(true);
      window.setTimeout(() => setSavedFlash(false), 2000);
    } catch (e) {
      // Backend validation (e.g. defaultAdapter names no adapter) lands here.
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function onReset() {
    setError(null);
    setSavedFlash(false);
    setBusy(true);
    try {
      const defaults = await api.resetAgentSettings();
      populate(defaults);
      setAppearance(defaults.appearance);
      // Re-filter the pickers to the reset provider set.
      api.getModelCatalog().then(setCatalog).catch(() => {});
      setSavedFlash(true);
      window.setTimeout(() => setSavedFlash(false), 2000);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  /**
   * Toggle a provider suite. Provider filtering happens server-side against the
   * SAVED `enabledProviders` (get_model_catalog), and the point-of-ask picker
   * (bead e7m.4) reads that same saved setting — so a toggle only takes effect
   * app-wide by PERSISTING immediately, then re-fetching the catalog. The write
   * is based on the last-SAVED settings (not the live form), so a mid-edit model
   * change is not force-committed and a broken Advanced adapters-JSON can't block
   * a toggle; the main Save still persists model edits + enabledProviders together.
   */
  async function onToggleProvider(provider: string) {
    if (saved == null || providerBusy != null) return;
    const next = new Set(enabledProviders);
    if (next.has(provider)) next.delete(provider);
    else next.add(provider);
    setEnabledProviders(next);
    setProviderBusy(provider);
    setError(null);
    try {
      const nextSettings: AgentSettings = { ...saved, enabledProviders: [...next].sort() };
      await api.setAgentSettings(nextSettings);
      setSaved(nextSettings);
      const cat = await api.getModelCatalog();
      setCatalog(cat);
    } catch (e) {
      setEnabledProviders(new Set(saved.enabledProviders)); // revert on failure
      setError(`Could not update providers: ${String(e)}`);
    } finally {
      setProviderBusy(null);
    }
  }

  async function onRefreshCatalog() {
    setRefreshing(true);
    try {
      // Failure-silent server-side: a network miss degrades to cache/snapshot and
      // still resolves, so we just repaint the (possibly cached) state — no dialog.
      const cat = await api.refreshModelCatalog();
      setCatalog(cat);
    } catch (e) {
      console.warn("catalog refresh failed", e);
    } finally {
      setRefreshing(false);
    }
  }

  async function onSaveKey() {
    const k = keyInput.trim();
    if (k === "") return;
    setKeyBusy(true);
    setError(null);
    try {
      await api.setOpenRouterApiKey(k);
      setOpenRouterKeySet(true);
      setKeyInput("");
      setKeyEditing(false);
    } catch (e) {
      setError(String(e));
    } finally {
      setKeyBusy(false);
    }
  }

  async function onClearKey() {
    setKeyBusy(true);
    setError(null);
    try {
      await api.setOpenRouterApiKey(null);
      setOpenRouterKeySet(false);
      setKeyInput("");
      setKeyEditing(false);
    } catch (e) {
      setError(String(e));
    } finally {
      setKeyBusy(false);
    }
  }

  const adapterNames = (() => {
    try {
      return Object.keys(JSON.parse(adaptersText) as Record<string, unknown>);
    } catch {
      // While the textarea is mid-edit / invalid, fall back to the saved names
      // so the dropdown stays usable.
      return saved != null ? Object.keys(saved.adapters) : [];
    }
  })();

  // Exact id → provider family, for the route mirror + disabled-suite check.
  const providerOf = useMemo(() => {
    const map = new Map<string, string>();
    if (catalog != null) for (const m of catalog.models) map.set(m.id, m.provider);
    return (id: string) => map.get(id);
  }, [catalog]);

  // Options offered by the pickers: the catalog models whose suite is enabled.
  // Client-side filtering by the LIVE set gives instant "turn off" feedback even
  // before the re-fetch lands (the server only ever returns enabled models, so
  // this is a no-op narrowing once the re-fetch completes).
  const enabledModels = useMemo(
    () => (catalog != null ? catalog.models.filter((m) => enabledProviders.has(m.provider)) : []),
    [catalog, enabledProviders],
  );

  const sortedProviders = useMemo<CatalogProvider[]>(() => {
    if (catalog == null) return [];
    return [...catalog.providers].sort(
      (a, b) => b.modelCount - a.modelCount || a.provider.localeCompare(b.provider),
    );
  }, [catalog]);

  const anySuiteEnabled = enabledProviders.size > 0;

  const sourceNote =
    catalog == null
      ? ""
      : catalog.source === "snapshot"
        ? " · offline snapshot"
        : catalog.source === "cache"
          ? " · cached"
          : "";

  return (
    <div
      class="absolute inset-0 z-30 flex animate-[cfy-rise_180ms_ease-out] flex-col bg-well text-ink"
      role="dialog"
      aria-modal="true"
      aria-label="Settings"
    >
      <header class="flex items-center justify-between border-b border-line bg-paper px-5 py-3">
        <h1 class="font-serif text-[17px] font-semibold">Settings</h1>
        <div class="flex items-center gap-3">
          {savedFlash && <span class="text-xs font-medium text-ok">Saved</span>}
          <button
            type="button"
            onClick={close}
            class="cfy-btn cfy-btn-secondary px-3 py-1.5 text-sm"
          >
            Close
          </button>
        </div>
      </header>

      <div class="min-h-0 flex-1 overflow-y-auto px-5 py-6">
        <div class="mx-auto flex max-w-2xl flex-col gap-8">
          {loadError != null ? (
            <p class="rounded-ctl bg-danger-bg px-3 py-2 text-sm text-danger">
              Failed to load settings: {loadError}
            </p>
          ) : saved == null ? (
            <div class="flex flex-col gap-2.5" aria-hidden="true">
              <div class="cfy-skeleton w-1/3" />
              <div class="cfy-skeleton w-2/3" />
              <div class="cfy-skeleton w-1/2" />
            </div>
          ) : (
            <>
              {/* Appearance (FR-7.2) */}
              <Section title="Appearance" description="Light, dark, or follow the system setting.">
                <div class="flex gap-1.5">
                  {APPEARANCE_OPTIONS.map((opt) => (
                    <button
                      key={opt.value}
                      type="button"
                      onClick={() => onAppearanceChange(opt.value)}
                      aria-pressed={appearance === opt.value}
                      class={`cfy-btn px-3 py-1.5 text-sm ${
                        appearance === opt.value ? "cfy-btn-accent" : "cfy-btn-secondary"
                      }`}
                    >
                      {opt.label}
                    </button>
                  ))}
                </div>
                <p class="mt-2 text-xs text-muted">
                  The artifact viewer keeps the system light/dark setting even when the app is
                  forced — its sandboxed frame is isolated from the app shell.
                </p>
              </Section>

              {/* Models (epic conceptify-e7m) */}
              <Section
                title="Models"
                description="Which model answers each kind of request. The route is derived from the model's provider."
              >
                <PurposeField
                  label="Follow-up answers"
                  hint="Batch sidebar answers (FR-4.6)."
                  value={followUp}
                  onChange={setFollowUp}
                  models={enabledModels}
                  providerOf={providerOf}
                  providers={catalog?.providers ?? []}
                  enabledProviders={enabledProviders}
                  openRouterKeySet={openRouterKeySet}
                />
                <PurposeField
                  label="Artifact updates"
                  hint="Apply-to-artifact runs (FR-4.7)."
                  value={artifactUpdate}
                  onChange={setArtifactUpdate}
                  models={enabledModels}
                  providerOf={providerOf}
                  providers={catalog?.providers ?? []}
                  enabledProviders={enabledProviders}
                  openRouterKeySet={openRouterKeySet}
                />
                <PurposeField
                  label="In-app asks"
                  hint="New-thread generation (FR-5.1)."
                  value={inAppAsk}
                  onChange={setInAppAsk}
                  models={enabledModels}
                  providerOf={providerOf}
                  providers={catalog?.providers ?? []}
                  enabledProviders={enabledProviders}
                  openRouterKeySet={openRouterKeySet}
                />

                <div class="mt-1 flex items-center justify-between gap-3 border-t border-line pt-3">
                  <span class="text-[11px] text-muted">
                    {catalog == null
                      ? "Loading model list…"
                      : `Model list updated ${relativeTime(catalog.fetchedAt)}${sourceNote}`}
                  </span>
                  <button
                    type="button"
                    onClick={onRefreshCatalog}
                    disabled={refreshing}
                    class="cfy-btn cfy-btn-secondary px-2.5 py-1 text-xs"
                  >
                    {refreshing ? "Refreshing…" : "Refresh"}
                  </button>
                </div>
              </Section>

              {/* Provider suites (epic conceptify-e7m) */}
              <Section
                title="Providers"
                description="Which provider suites appear in the model pickers. Changes apply immediately, everywhere."
              >
                {catalog == null ? (
                  <div class="flex flex-col gap-2" aria-hidden="true">
                    <div class="cfy-skeleton w-2/3" />
                  </div>
                ) : (
                  <>
                    <div class="flex flex-wrap gap-1.5">
                      {sortedProviders.map((p) => {
                        const on = enabledProviders.has(p.provider);
                        return (
                          <button
                            key={p.provider}
                            type="button"
                            onClick={() => onToggleProvider(p.provider)}
                            aria-pressed={on}
                            disabled={providerBusy != null}
                            class={`cfy-btn px-2.5 py-1 text-xs ${
                              on ? "cfy-btn-accent" : "cfy-btn-secondary"
                            } ${providerBusy != null ? "opacity-60" : ""}`}
                          >
                            {on && (
                              <svg width="11" height="11" viewBox="0 0 12 12" aria-hidden="true">
                                <path
                                  d="M2.5 6.2l2.3 2.3L9.5 3.5"
                                  fill="none"
                                  stroke="currentColor"
                                  stroke-width="1.6"
                                  stroke-linecap="round"
                                  stroke-linejoin="round"
                                />
                              </svg>
                            )}
                            {providerLabel(p.provider)}
                            <span class="opacity-70">{p.modelCount}</span>
                          </button>
                        );
                      })}
                    </div>
                    {!anySuiteEnabled && (
                      <p class="text-[11px] text-warn">
                        No suites enabled — every model picker is empty. Turn at least one on.
                      </p>
                    )}
                  </>
                )}
              </Section>

              {/* OpenRouter key (bead conceptify-e7m.7) */}
              <Section
                title="OpenRouter"
                description="Required to run any model routed via OpenRouter (any suite other than Anthropic or OpenAI). The key is write-only — it is never displayed again."
              >
                {!openRouterKeySet || keyEditing ? (
                  <div class="flex items-center gap-2">
                    <input
                      type="password"
                      value={keyInput}
                      spellcheck={false}
                      autocomplete="off"
                      placeholder="sk-or-…"
                      onInput={(e) => setKeyInput((e.currentTarget as HTMLInputElement).value)}
                      class="cfy-input flex-1 px-2.5 py-1.5 font-mono text-sm"
                    />
                    <button
                      type="button"
                      onClick={onSaveKey}
                      disabled={keyInput.trim() === "" || keyBusy}
                      class="cfy-btn cfy-btn-primary px-3 py-1.5 text-sm"
                    >
                      {keyBusy ? "Saving…" : "Save key"}
                    </button>
                    {keyEditing && (
                      <button
                        type="button"
                        onClick={() => {
                          setKeyEditing(false);
                          setKeyInput("");
                        }}
                        disabled={keyBusy}
                        class="cfy-btn cfy-btn-ghost px-2.5 py-1.5 text-sm"
                      >
                        Cancel
                      </button>
                    )}
                  </div>
                ) : (
                  <div class="flex items-center gap-2">
                    <span class="cfy-input flex flex-1 items-center gap-2 px-2.5 py-1.5 text-sm text-muted">
                      <span class="cfy-chip bg-ok-bg text-ok">Key stored</span>
                      <span class="font-mono tracking-widest">••••••••••••</span>
                    </span>
                    <button
                      type="button"
                      onClick={() => setKeyEditing(true)}
                      class="cfy-btn cfy-btn-secondary px-3 py-1.5 text-sm"
                    >
                      Replace
                    </button>
                    <button
                      type="button"
                      onClick={onClearKey}
                      disabled={keyBusy}
                      class="cfy-btn cfy-btn-danger px-3 py-1.5 text-sm"
                    >
                      {keyBusy ? "…" : "Clear"}
                    </button>
                  </div>
                )}
              </Section>

              {/* Paths (FR-7.3) */}
              <Section title="Paths" description="Where auto-created project folders live.">
                <Field
                  label="Auto-project base directory"
                  hint={`Empty = default (${DEFAULT_BASE_DIR_HINT}).`}
                >
                  <TextInput
                    value={autoBaseDir}
                    onInput={setAutoBaseDir}
                    placeholder={DEFAULT_BASE_DIR_HINT}
                  />
                </Field>
              </Section>

              {/* Advanced — the raw escape hatch, collapsed + secondary (e7m.3) */}
              <section>
                <button
                  type="button"
                  onClick={() => setAdvancedOpen((o) => !o)}
                  aria-expanded={advancedOpen}
                  class="flex w-full items-center gap-1.5 text-left"
                >
                  <svg
                    width="12"
                    height="12"
                    viewBox="0 0 12 12"
                    aria-hidden="true"
                    class={`text-muted transition-transform ${advancedOpen ? "rotate-90" : ""}`}
                  >
                    <path
                      d="M4.5 2.5L8 6l-3.5 3.5"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.5"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                    />
                  </svg>
                  <span class="cfy-label">Advanced</span>
                </button>
                <p class="mb-3 ml-[18px] mt-0.5 text-xs text-muted">
                  Raw adapter templates and run limits. The model routing above normally handles
                  this — edit only if you run a custom harness.
                </p>
                {advancedOpen && (
                  <div class="ml-[18px] flex flex-col gap-3 border-l border-line pl-3.5">
                    <Field label="Default adapter" hint="Bypasses routing when set to a custom (non-built-in) harness.">
                      <select
                        value={defaultAdapter}
                        onChange={(e) =>
                          setDefaultAdapter((e.currentTarget as HTMLSelectElement).value)
                        }
                        class="cfy-input px-2.5 py-1.5 text-sm"
                      >
                        {!adapterNames.includes(defaultAdapter) && defaultAdapter !== "" && (
                          <option value={defaultAdapter}>{defaultAdapter} (not in templates)</option>
                        )}
                        {adapterNames.map((n) => (
                          <option key={n} value={n}>
                            {n}
                          </option>
                        ))}
                      </select>
                    </Field>

                    <Field
                      label="Command templates"
                      hint="Raw adapter JSON. Placeholders: {prompt} {model} {project_root}. Validated on save."
                    >
                      <textarea
                        value={adaptersText}
                        onInput={(e) =>
                          setAdaptersText((e.currentTarget as HTMLTextAreaElement).value)
                        }
                        rows={12}
                        spellcheck={false}
                        class="cfy-input resize-y px-2.5 py-2 font-mono text-xs"
                      />
                    </Field>

                    <Field
                      label="Agent binary path"
                      hint="Absolute path override. Empty = resolve on PATH (FR-7.3)."
                    >
                      <TextInput
                        value={binaryPath}
                        onInput={setBinaryPath}
                        placeholder="auto (login-shell which)"
                      />
                    </Field>

                    <Field label="Timeout (minutes)" hint="Kills a run that runs long (FR-5.3).">
                      <input
                        type="number"
                        min={1}
                        value={timeoutMins}
                        onInput={(e) => setTimeoutMins((e.currentTarget as HTMLInputElement).value)}
                        class="cfy-input w-28 px-2.5 py-1.5 text-sm"
                      />
                    </Field>
                  </div>
                )}
              </section>

              {error != null && (
                <p class="whitespace-pre-wrap rounded-ctl bg-danger-bg px-3 py-2 text-sm text-danger">
                  {error}
                </p>
              )}

              <div class="flex items-center gap-2 border-t border-line pt-5">
                <button
                  type="button"
                  onClick={onSave}
                  disabled={busy}
                  class="cfy-btn cfy-btn-primary px-4 py-2 text-sm"
                >
                  {busy ? "Saving…" : "Save"}
                </button>
                <button
                  type="button"
                  onClick={onReset}
                  disabled={busy}
                  class="cfy-btn cfy-btn-secondary px-4 py-2 text-sm"
                >
                  Reset to defaults
                </button>
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

/**
 * One per-purpose model row: the searchable combobox, the resolved route (a
 * faithful mirror of routing.rs — see src/lib/routing.ts), and inline
 * validation. Validation is a soft warning, never a hard block: the bead's rule
 * is that a fixable choice (an OpenRouter model with no key, a disabled suite)
 * should nudge, not stop, since the fix lives on the same screen.
 */
function PurposeField({
  label,
  hint,
  value,
  onChange,
  models,
  providerOf,
  providers,
  enabledProviders,
  openRouterKeySet,
}: {
  label: string;
  hint: string;
  value: string;
  onChange: (v: string) => void;
  models: CatalogModel[];
  providerOf: (id: string) => string | undefined;
  providers: CatalogProvider[];
  enabledProviders: Set<string>;
  openRouterKeySet: boolean;
}) {
  const route: RouteResult = routeForModel(value, providerOf);
  const trimmed = value.trim();

  // Warnings, most-actionable first.
  const warnings: string[] = [];
  if (trimmed === "") {
    warnings.push("Pick a model for this purpose.");
  } else {
    // A saved model whose suite the user turned off (known family, disabled).
    const family =
      route.tag === "anthropic"
        ? "anthropic"
        : route.tag === "openai"
          ? "openai"
          : route.tag === "openrouter"
            ? familyOf(value)
            : null;
    if (
      family != null &&
      !enabledProviders.has(family) &&
      providers.some((p) => p.provider === family)
    ) {
      warnings.push(`The ${providerLabel(family)} suite is turned off — re-enable it or pick another model.`);
    }
    if (route.tag === "unroutable" && route.reason != null) {
      warnings.push(`Can't route this model — ${route.reason}.`);
    }
    if (route.tag === "openrouter" && !openRouterKeySet) {
      warnings.push("Routed via OpenRouter — add an OpenRouter key below to run it.");
    }
  }

  const routeTone =
    route.tag === "unroutable"
      ? "text-warn"
      : route.tag === "openrouter"
        ? "text-accent-ink"
        : "text-muted";

  return (
    <div class="flex flex-col gap-1">
      <span class="text-xs font-medium text-ink">{label}</span>
      <ModelCombobox
        value={value}
        onChange={onChange}
        models={models}
        ariaLabel={`${label} model`}
        placeholder="Select or type a model id…"
      />
      <div class="flex flex-wrap items-center gap-x-2 gap-y-0.5">
        <span class="text-[11px] text-muted">{hint}</span>
        {trimmed !== "" && route.tag !== "unroutable" && (
          <span class={`text-[11px] ${routeTone}`}>· Runs {routeLabel(route.tag)}</span>
        )}
      </div>
      {warnings.map((w) => (
        <span key={w} class="text-[11px] text-warn">
          {w}
        </span>
      ))}
    </div>
  );
}

function Section({
  title,
  description,
  children,
}: {
  title: string;
  description: string;
  children: ComponentChildren;
}) {
  return (
    <section>
      <h2 class="font-serif text-[15px] font-semibold text-ink">{title}</h2>
      <p class="mb-3 mt-0.5 text-xs text-muted">{description}</p>
      <div class="flex flex-col gap-3">{children}</div>
    </section>
  );
}

function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: ComponentChildren;
}) {
  return (
    <label class="flex flex-col gap-1">
      <span class="text-xs font-medium text-ink">{label}</span>
      {children}
      {hint != null && <span class="text-[11px] text-muted">{hint}</span>}
    </label>
  );
}

function TextInput({
  value,
  onInput,
  placeholder,
}: {
  value: string;
  onInput: (v: string) => void;
  placeholder?: string;
}) {
  return (
    <input
      type="text"
      value={value}
      spellcheck={false}
      placeholder={placeholder}
      onInput={(e) => onInput((e.currentTarget as HTMLInputElement).value)}
      class="cfy-input px-2.5 py-1.5 text-sm"
    />
  );
}
