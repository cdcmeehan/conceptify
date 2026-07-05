// Settings overlay (PRD §7.7, FR-7.1–7.4). A full-panel modal opened from the
// project sidebar footer. Three sections:
//
//  - Appearance (FR-7.2): system | light | dark. Applied live via theme.ts;
//    persisted on Save. NOTE the artifact iframe keeps its own
//    prefers-color-scheme (§9 S2 isolation) — surfaced inline for honesty.
//  - Agent (FR-7.1): default adapter, per-purpose models, timeout, binary path
//    override, and raw command templates (adapters JSON — validated on Save).
//  - Paths (FR-7.3): auto-project base dir (empty = default).
//
// Defaults live in Rust (FR-7.4 zero-config); this UI is for overrides. "Reset
// to defaults" deletes the stored override so the app behaves as a fresh
// install. Invalid command-template JSON, or a config the backend rejects
// (e.g. defaultAdapter naming no adapter), surfaces as an inline error.

import type { ComponentChildren } from "preact";
import { useEffect, useState } from "preact/hooks";
import * as api from "../lib/api";
import type { AgentSettings, Appearance } from "../lib/api";
import { appStore } from "../store/appStore";
import { setAppearance } from "../lib/theme";

const APPEARANCE_OPTIONS: { value: Appearance; label: string }[] = [
  { value: "system", label: "System" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

const DEFAULT_BASE_DIR_HINT = "~/Documents/conceptify/projects";

export function SettingsView() {
  // The last-persisted settings — used to revert a live appearance preview if
  // the user closes without saving.
  const [saved, setSaved] = useState<AgentSettings | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  // Editable form state (mirrors AgentSettings, plus the raw adapters textarea
  // and a minutes view of the timeout).
  const [appearance, setAppearanceState] = useState<Appearance>("system");
  const [defaultAdapter, setDefaultAdapter] = useState("");
  const [followUp, setFollowUp] = useState("");
  const [artifactUpdate, setArtifactUpdate] = useState("");
  const [inAppAsk, setInAppAsk] = useState("");
  const [timeoutMins, setTimeoutMins] = useState("15");
  const [binaryPath, setBinaryPath] = useState("");
  const [autoBaseDir, setAutoBaseDir] = useState("");
  const [adaptersText, setAdaptersText] = useState("");

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
      setSavedFlash(true);
      window.setTimeout(() => setSavedFlash(false), 2000);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
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

  return (
    <div
      class="absolute inset-0 z-30 flex flex-col bg-neutral-100 text-neutral-900 dark:bg-neutral-900 dark:text-neutral-100"
      role="dialog"
      aria-modal="true"
      aria-label="Settings"
    >
      <header class="flex items-center justify-between border-b border-neutral-200 bg-white px-5 py-3 dark:border-neutral-800 dark:bg-neutral-950">
        <h1 class="text-lg font-semibold">Settings</h1>
        <div class="flex items-center gap-3">
          {savedFlash && (
            <span class="text-xs font-medium text-emerald-600 dark:text-emerald-400">Saved</span>
          )}
          <button
            type="button"
            onClick={close}
            class="rounded-md px-3 py-1.5 text-sm font-medium text-neutral-600 transition-colors hover:bg-neutral-200 dark:text-neutral-300 dark:hover:bg-neutral-800"
          >
            Close
          </button>
        </div>
      </header>

      <div class="min-h-0 flex-1 overflow-y-auto px-5 py-6">
        <div class="mx-auto flex max-w-2xl flex-col gap-8">
          {loadError != null ? (
            <p class="rounded-md bg-rose-100 px-3 py-2 text-sm text-rose-700 dark:bg-rose-500/15 dark:text-rose-300">
              Failed to load settings: {loadError}
            </p>
          ) : saved == null ? (
            <p class="text-sm text-neutral-400">Loading…</p>
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
                      class={`rounded-md border px-3 py-1.5 text-sm font-medium transition-colors ${
                        appearance === opt.value
                          ? "border-blue-400 bg-blue-600/10 text-blue-700 dark:border-blue-500/50 dark:bg-blue-500/15 dark:text-blue-300"
                          : "border-neutral-300 bg-white text-neutral-700 hover:bg-neutral-100 dark:border-neutral-700 dark:bg-neutral-900 dark:text-neutral-200 dark:hover:bg-neutral-800"
                      }`}
                    >
                      {opt.label}
                    </button>
                  ))}
                </div>
                <p class="mt-2 text-xs text-neutral-400">
                  The artifact viewer keeps the system light/dark setting even when the app is
                  forced — its sandboxed frame is isolated from the app shell.
                </p>
              </Section>

              {/* Agent (FR-7.1) */}
              <Section title="Agent" description="Adapters, models, and run limits (§5.5).">
                <Field label="Default adapter">
                  <select
                    value={defaultAdapter}
                    onChange={(e) => setDefaultAdapter((e.currentTarget as HTMLSelectElement).value)}
                    class="w-full rounded-md border border-neutral-300 bg-white px-2.5 py-1.5 text-sm text-neutral-800 focus:border-blue-400 focus:outline-none dark:border-neutral-700 dark:bg-neutral-950 dark:text-neutral-100"
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

                <Field label="Follow-up model" hint="Batch sidebar answers (FR-4.6).">
                  <TextInput value={followUp} onInput={setFollowUp} placeholder="claude-haiku-4-5" />
                </Field>
                <Field label="Artifact-update model" hint="Apply-to-artifact runs (FR-4.7).">
                  <TextInput
                    value={artifactUpdate}
                    onInput={setArtifactUpdate}
                    placeholder="claude-sonnet-5"
                  />
                </Field>
                <Field label="In-app ask model" hint="New-thread generation (FR-5.1).">
                  <TextInput value={inAppAsk} onInput={setInAppAsk} placeholder="claude-sonnet-5" />
                </Field>

                <Field label="Timeout (minutes)" hint="Kills a run that runs long (FR-5.3).">
                  <input
                    type="number"
                    min={1}
                    value={timeoutMins}
                    onInput={(e) => setTimeoutMins((e.currentTarget as HTMLInputElement).value)}
                    class="w-28 rounded-md border border-neutral-300 bg-white px-2.5 py-1.5 text-sm text-neutral-800 focus:border-blue-400 focus:outline-none dark:border-neutral-700 dark:bg-neutral-950 dark:text-neutral-100"
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

                <Field
                  label="Command templates"
                  hint="Raw adapter JSON. Placeholders: {prompt} {model} {project_root}. Validated on save."
                >
                  <textarea
                    value={adaptersText}
                    onInput={(e) => setAdaptersText((e.currentTarget as HTMLTextAreaElement).value)}
                    rows={12}
                    spellcheck={false}
                    class="w-full resize-y rounded-md border border-neutral-300 bg-white px-2.5 py-2 font-mono text-xs text-neutral-800 focus:border-blue-400 focus:outline-none dark:border-neutral-700 dark:bg-neutral-950 dark:text-neutral-100"
                  />
                </Field>
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

              {error != null && (
                <p class="whitespace-pre-wrap rounded-md bg-rose-100 px-3 py-2 text-sm text-rose-700 dark:bg-rose-500/15 dark:text-rose-300">
                  {error}
                </p>
              )}

              <div class="flex items-center gap-2 border-t border-neutral-200 pt-5 dark:border-neutral-800">
                <button
                  type="button"
                  onClick={onSave}
                  disabled={busy}
                  class="rounded-md bg-blue-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-700 disabled:opacity-50"
                >
                  {busy ? "Saving…" : "Save"}
                </button>
                <button
                  type="button"
                  onClick={onReset}
                  disabled={busy}
                  class="rounded-md border border-neutral-300 px-4 py-2 text-sm font-medium text-neutral-700 transition-colors hover:bg-neutral-100 disabled:opacity-50 dark:border-neutral-700 dark:text-neutral-200 dark:hover:bg-neutral-800"
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
      <h2 class="text-sm font-semibold text-neutral-800 dark:text-neutral-100">{title}</h2>
      <p class="mb-3 text-xs text-neutral-400">{description}</p>
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
      <span class="text-xs font-medium text-neutral-600 dark:text-neutral-300">{label}</span>
      {children}
      {hint != null && <span class="text-[11px] text-neutral-400">{hint}</span>}
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
      class="w-full rounded-md border border-neutral-300 bg-white px-2.5 py-1.5 text-sm text-neutral-800 placeholder:text-neutral-400 focus:border-blue-400 focus:outline-none dark:border-neutral-700 dark:bg-neutral-950 dark:text-neutral-100"
    />
  );
}
