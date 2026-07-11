// Point-of-ask model override pill (epic conceptify-e7m, bead e7m.4).
//
// A compact, deliberately quiet affordance placed at every run trigger (in-app
// ask, Ask follow-ups, Apply to artifact, per-comment Ask now). Collapsed it is
// a small pill showing the EFFECTIVE model for that purpose — the settings
// default, or the chosen override — with a subtle route hint ("sonnet-5 ·
// claude"). Clicking it opens the shared, searchable, provider-grouped
// combobox (ModelCombobox, bead e7m.3) filtered to the enabled provider suites,
// Custom… included. This component wraps ModelCombobox verbatim, supplying only
// the pill trigger and the default/override bookkeeping.
//
// The selection is PER-INVOCATION component state owned by the caller and is
// NEVER persisted to settings. The caller sends it as the flow command's
// `run_override` ONLY when it differs from the default — {@link runOverrideOf}
// turns the picker's value (null = "use default") into either `{ model }` or
// `null`, so an unchanged pick leaves the run row override-free and keeps it
// tracking settings on retry (bead e7m.1).
//
// Models routed via OpenRouter with no key stored are shown disabled with an
// "add key in Settings" hint (the picker never lets a keyless run start).
//
// Safari/WKWebView-safe: no Chromium-only APIs; the popover is the same
// absolute-positioned element ModelCombobox already ships.

import { useCallback, useEffect, useMemo, useState } from "preact/hooks";
import * as api from "../lib/api";
import type {
  AgentOptions,
  CatalogModel,
  CatalogResponse,
  PurposeModels,
  RunOverride,
} from "../lib/api";
import { routeForModel, type RouteTag } from "../lib/routing";
import { ModelCombobox } from "./ModelCombobox";

/** Which per-purpose default the pill resolves against (keys of `PurposeModels`). */
export type OverridePurpose = keyof PurposeModels;

/**
 * Turn a picker selection into the `run_override` command argument: `{ model }`
 * for a real override, or `null` when there is none (the caller's state is
 * already `null` whenever the pick equals the settings default — see
 * {@link ModelOverridePicker} — so this is a straight map). Keeping the
 * "null unless it differs" invariant in the picker means every call site can
 * pass its raw state through here without re-deriving the default.
 */
export function runOverrideOf(model: string | null): RunOverride | null {
  return model != null ? { model } : null;
}

// ---------------------------------------------------------------------------
// Shared picker data: the catalog (already provider-filtered server-side) plus
// the agent options (per-purpose defaults + whether an OpenRouter key is set).
// Cached at module level so a second pill never flickers, with a short TTL so a
// Settings change (default model, provider suite, OpenRouter key) is reflected
// the next time a pill opens. Both reads are cheap local Tauri calls (warm disk
// cache / settings row) — there is no settings-changed event to subscribe to,
// so a small refresh-on-mount TTL is the pragmatic freshness mechanism.
// ---------------------------------------------------------------------------

interface PickerData {
  catalog: CatalogResponse;
  options: AgentOptions;
  providerOf: (id: string) => string | undefined;
}

let dataCache: PickerData | null = null;
let cacheAt = 0;
let inflight: Promise<PickerData> | null = null;
const CACHE_TTL_MS = 10_000;

async function loadPickerData(): Promise<PickerData> {
  const [catalog, options] = await Promise.all([api.getModelCatalog(), api.getAgentOptions()]);
  const map = new Map<string, string>();
  for (const m of catalog.models) map.set(m.id, m.provider);
  const data: PickerData = { catalog, options, providerOf: (id) => map.get(id) };
  dataCache = data;
  cacheAt = Date.now();
  return data;
}

function useModelPickerData(): PickerData | null {
  const [data, setData] = useState<PickerData | null>(dataCache);
  useEffect(() => {
    let alive = true;
    if (dataCache != null && Date.now() - cacheAt < CACHE_TTL_MS) {
      setData(dataCache);
      return () => {
        alive = false;
      };
    }
    // Dedupe concurrent loads across simultaneously-mounted pills.
    const p =
      inflight ??
      (inflight = loadPickerData().finally(() => {
        inflight = null;
      }));
    p.then((d) => {
      if (alive) setData(d);
    }).catch(() => {
      // Quiet: the pill falls back to the raw default id + heuristic route.
    });
    return () => {
      alive = false;
    };
  }, []);
  return data;
}

/** Short route word for the pill hint, mirroring routing.rs' three routes. */
const ROUTE_WORD: Record<RouteTag, string> = {
  anthropic: "claude",
  openai: "codex",
  openrouter: "OpenRouter",
  local: "local",
  unroutable: "no route",
};

export interface ModelOverridePickerProps {
  /** The purpose whose settings default this run would otherwise use. */
  purpose: OverridePurpose;
  /** Current override id, or `null` to use the settings default for `purpose`.
   *  Owned by the caller (per-invocation state, never persisted). */
  value: string | null;
  /** Fired with the new override id, or `null` when the choice equals the
   *  default (the caller then sends `run_override` only for a real override). */
  onChange: (override: string | null) => void;
  /** Disable the pill (e.g. while a run is already active/starting). */
  disabled?: boolean;
  /** Which edge the popover aligns to. Right-align when the pill sits on the
   *  right of its row so the menu opens inward instead of off-screen. */
  menuAlign?: "left" | "right";
  /** Notified when the popover opens/closes — a host card can relax its
   *  `overflow-hidden` while the menu is open. */
  onOpenChange?: (open: boolean) => void;
  /** Accessible label for the trigger. */
  ariaLabel?: string;
}

export function ModelOverridePicker({
  purpose,
  value,
  onChange,
  disabled = false,
  menuAlign = "left",
  onOpenChange,
  ariaLabel,
}: ModelOverridePickerProps) {
  const data = useModelPickerData();

  const defaultModel = data?.options.models[purpose] ?? "";
  const effective = value ?? defaultModel;
  const overridden = value != null;

  const models = data?.catalog.models ?? [];
  const keySet = data?.options.openRouterKeySet ?? false;

  // `undefined` while loading → routeForModel falls back to its prefix
  // heuristics, which still classifies claude-*/gpt-* defaults correctly.
  const providerOf = useCallback((id: string) => data?.providerOf(id), [data]);

  const route = useMemo(() => routeForModel(effective, providerOf), [effective, providerOf]);

  // Keyless OpenRouter models are non-selectable with an inline hint; the fix
  // (adding a key) lives in Settings, so we point there rather than fail a run.
  const disabledReason = useCallback(
    (m: CatalogModel): string | null => {
      const r = routeForModel(m.id, providerOf);
      return r.tag === "openrouter" && !keySet ? "add key in Settings" : null;
    },
    [providerOf, keySet],
  );

  const selectedModel = useMemo(
    () => models.find((m) => m.id === effective) ?? null,
    [models, effective],
  );
  const label = selectedModel?.displayName ?? (effective || "Default model");

  // A pick equal to the current default is NOT an override: collapse it to
  // `null` so the run row stays override-free (bead e7m.4 "omit when unchanged").
  const handleChange = useCallback(
    (id: string) => onChange(id === defaultModel ? null : id),
    [onChange, defaultModel],
  );

  if (data == null) {
    // Brief, only for the first pill in a session (later pills seed from cache).
    return (
      <span class="inline-flex items-center gap-1 rounded-full border border-line bg-raised px-2 py-[3px] text-[11px] font-medium text-muted opacity-60">
        Model
      </span>
    );
  }

  return (
    <ModelCombobox
      value={effective}
      onChange={handleChange}
      models={models}
      disabledReason={disabledReason}
      disabled={disabled}
      ariaLabel={ariaLabel}
      onOpenChange={onOpenChange}
      popoverClass={`${menuAlign === "right" ? "right-0" : "left-0"} w-64 max-w-[80vw]`}
      renderTrigger={({ open, toggle, disabled: dis }) => (
        <span class="inline-flex items-center gap-0.5">
          <button
            type="button"
            disabled={dis}
            aria-haspopup="listbox"
            aria-expanded={open}
            aria-label={ariaLabel ?? "Model for this run"}
            title={
              overridden
                ? "Model for this run (overrides the default — click to change)"
                : "Model for this run (the settings default — click to override)"
            }
            onClick={toggle}
            class={`inline-flex max-w-[12rem] items-center gap-1 rounded-full border px-2 py-[3px] text-[11px] font-medium leading-none transition-colors disabled:opacity-55 ${
              overridden
                ? "border-accent/40 bg-accent-bg text-accent-ink"
                : "border-line bg-raised text-muted hover:bg-hover hover:text-ink"
            }`}
          >
            <span class="min-w-0 truncate">{label}</span>
            <span class={`shrink-0 ${route.tag === "unroutable" ? "text-warn" : "opacity-60"}`}>
              · {ROUTE_WORD[route.tag]}
            </span>
            <svg
              width="9"
              height="9"
              viewBox="0 0 12 12"
              aria-hidden="true"
              class="shrink-0 opacity-70"
            >
              <path
                d="M2.5 4.5L6 8l3.5-3.5"
                fill="none"
                stroke="currentColor"
                stroke-width="1.5"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
          </button>
          {overridden && !dis && (
            <button
              type="button"
              onClick={() => onChange(null)}
              title={`Use the default model (${defaultModel})`}
              aria-label="Use the default model"
              class="rounded-full p-0.5 text-muted transition-colors hover:bg-hover hover:text-ink"
            >
              <svg width="11" height="11" viewBox="0 0 12 12" aria-hidden="true">
                <path
                  d="M3.5 3.5l5 5m0-5l-5 5"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.4"
                  stroke-linecap="round"
                />
              </svg>
            </button>
          )}
        </span>
      )}
    />
  );
}
