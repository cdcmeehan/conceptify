// Reusable searchable, provider-grouped model combobox (epic conceptify-e7m).
//
// A plain <select> is unusable for hundreds of catalog models (bead e7m.3), so
// this is a filterable listbox built on the shell's cfy-input / cfy-popover
// primitives — no new UI dependency. It is deliberately SELF-CONTAINED and
// controlled (value / onChange) so the point-of-ask picker (bead e7m.4) can
// reuse it verbatim in a compact popover:
//
//   - `models` are the already-provider-filtered catalog models to offer.
//   - `disabledReason(model)` optionally marks an option non-selectable with a
//     hint (e.g. e7m.4 greying OpenRouter-runnable models when no key is set);
//     Settings leaves everything selectable and warns inline instead.
//   - A "Custom…" escape hatch lets the user commit any free-text id the
//     catalog doesn't list (routing then classifies it heuristically).
//
// Keyboard: type to filter; ArrowUp/Down move the active option; Enter commits
// it; Escape closes the popover only (it does not bubble to a parent overlay's
// Escape-to-close). Safari/WKWebView-safe: no Chromium-only APIs.

import type { ComponentChildren } from "preact";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "preact/hooks";
import type { CatalogModel } from "../lib/api";
import { formatContextWindow, providerLabel } from "../lib/models";

interface ProviderGroup {
  provider: string;
  items: { model: CatalogModel; disabledReason: string | null }[];
}

/** What the default trigger renders, exposed to a custom {@link ModelComboboxProps.renderTrigger}
 *  so the point-of-ask pill (bead e7m.4) can supply its own compact affordance
 *  while reusing this component's popover/search/keyboard logic verbatim. */
export interface ModelComboboxTriggerProps {
  /** Whether the popover is open. */
  open: boolean;
  /** Toggle the popover (a no-op while `disabled`). */
  toggle: () => void;
  /** The catalog model matching `value`, or `null` for a custom/absent id. */
  selected: CatalogModel | null;
  /** The raw current value (may be a custom id or `""`). */
  value: string;
  /** Whether the control is disabled. */
  disabled: boolean;
}

export interface ModelComboboxProps {
  /** Currently selected model id (catalog id or a custom free-text id). */
  value: string;
  /** Commit a new selection (a catalog id or a custom id). */
  onChange: (id: string) => void;
  /** Options to offer — already filtered to the enabled provider suites. */
  models: CatalogModel[];
  /** Optional per-option disable: return a hint string to grey it out and block
   *  selection, or `null` to leave it selectable. */
  disabledReason?: (model: CatalogModel) => string | null;
  /** Disable the whole control. */
  disabled?: boolean;
  /** Placeholder shown when `value` is empty. */
  placeholder?: string;
  /** Accessible label for the trigger (the visible <label> also associates). */
  ariaLabel?: string;
  /** Replace the default full-width field trigger with a custom one (bead
   *  e7m.4's compact pill). The popover, search, filtering and keyboard nav are
   *  unchanged — only the trigger's appearance differs. */
  renderTrigger?: (props: ModelComboboxTriggerProps) => ComponentChildren;
  /** Positioning/width classes for the popover. Defaults to stretching to the
   *  trigger width (`left-0 right-0`); the pill variant passes a fixed width and
   *  an alignment so a narrow trigger still gets a comfortable menu. */
  popoverClass?: string;
  /** Notified whenever the popover opens/closes. Lets a clipping ancestor (the
   *  comment card's `overflow-hidden`) relax while the menu is open. */
  onOpenChange?: (open: boolean) => void;
}

export function ModelCombobox({
  value,
  onChange,
  models,
  disabledReason,
  disabled = false,
  placeholder = "Select a model…",
  ariaLabel,
  renderTrigger,
  popoverClass,
  onOpenChange,
}: ModelComboboxProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);

  const rootRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);

  const selected = useMemo(() => models.find((m) => m.id === value) ?? null, [models, value]);
  const trimmedQuery = query.trim();

  // Case-insensitive substring filter over id + display name.
  const filtered = useMemo(() => {
    const q = trimmedQuery.toLowerCase();
    if (q === "") return models;
    return models.filter(
      (m) => m.id.toLowerCase().includes(q) || m.displayName.toLowerCase().includes(q),
    );
  }, [models, trimmedQuery]);

  // Offer the Custom… escape hatch when the typed text isn't already a real id.
  const showCustom = trimmedQuery !== "" && !models.some((m) => m.id === trimmedQuery);

  // Flat option list drives keyboard nav; index 0 is the custom row when shown.
  type Opt =
    | { kind: "custom"; id: string }
    | { kind: "model"; model: CatalogModel; disabledReason: string | null };
  const options = useMemo<Opt[]>(() => {
    const opts: Opt[] = [];
    if (showCustom) opts.push({ kind: "custom", id: trimmedQuery });
    for (const m of filtered) {
      opts.push({
        kind: "model",
        model: m,
        disabledReason: disabledReason ? disabledReason(m) : null,
      });
    }
    return opts;
  }, [filtered, showCustom, trimmedQuery, disabledReason]);

  // Group the model options (preserving the catalog's provider-then-id order)
  // for the rendered headers; the custom row sits above the groups.
  const groups = useMemo<ProviderGroup[]>(() => {
    const out: ProviderGroup[] = [];
    for (const m of filtered) {
      const reason = disabledReason ? disabledReason(m) : null;
      const last = out[out.length - 1];
      if (last != null && last.provider === m.provider) {
        last.items.push({ model: m, disabledReason: reason });
      } else {
        out.push({ provider: m.provider, items: [{ model: m, disabledReason: reason }] });
      }
    }
    return out;
  }, [filtered, disabledReason]);

  const firstSelectable = useMemo(() => {
    const i = options.findIndex((o) => o.kind === "custom" || o.disabledReason == null);
    return i < 0 ? 0 : i;
  }, [options]);

  // Reset the active option whenever the option set changes (open, new query).
  useEffect(() => {
    setActiveIndex(firstSelectable);
  }, [firstSelectable, open]);

  // Focus the search input on open.
  useEffect(() => {
    if (open) inputRef.current?.focus();
  }, [open]);

  // Notify the host of open/close transitions without re-firing when only the
  // callback identity changes (a ref keeps the effect keyed on `open` alone).
  const onOpenChangeRef = useRef(onOpenChange);
  onOpenChangeRef.current = onOpenChange;
  useEffect(() => {
    onOpenChangeRef.current?.(open);
  }, [open]);

  // Close on outside click.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setOpen(false);
        setQuery("");
      }
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open]);

  // Keep the active option scrolled into view during keyboard nav.
  useLayoutEffect(() => {
    if (!open) return;
    const el = listRef.current?.querySelector<HTMLElement>(`[data-opt="${activeIndex}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [activeIndex, open]);

  function commit(opt: Opt | undefined) {
    if (opt == null) return;
    if (opt.kind === "custom") {
      onChange(opt.id);
    } else if (opt.disabledReason == null) {
      onChange(opt.model.id);
    } else {
      return; // disabled option — ignore
    }
    setOpen(false);
    setQuery("");
  }

  function moveActive(delta: number) {
    if (options.length === 0) return;
    let i = activeIndex;
    for (let step = 0; step < options.length; step++) {
      i = (i + delta + options.length) % options.length;
      const o = options[i];
      if (o.kind === "custom" || o.disabledReason == null) break;
    }
    setActiveIndex(i);
  }

  function onInputKeyDown(e: KeyboardEvent) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      moveActive(1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      moveActive(-1);
    } else if (e.key === "Enter") {
      e.preventDefault();
      commit(options[activeIndex]);
    } else if (e.key === "Escape") {
      // Close only this popover — do not let the parent overlay's Escape handler
      // (SettingsView) also fire and close the whole sheet.
      e.stopPropagation();
      setOpen(false);
      setQuery("");
    }
  }

  const triggerLabel = selected ? selected.displayName : value !== "" ? value : placeholder;
  const isPlaceholder = selected == null && value === "";

  const toggle = () => {
    if (disabled) return;
    setOpen((o) => !o);
    setQuery("");
  };

  let renderIndex = showCustom ? 1 : 0; // model options start after the custom row

  return (
    <div ref={rootRef} class="relative">
      {renderTrigger ? (
        renderTrigger({ open, toggle, selected, value, disabled })
      ) : (
        <button
          type="button"
          disabled={disabled}
          aria-haspopup="listbox"
          aria-expanded={open}
          aria-label={ariaLabel}
          onClick={toggle}
          class="cfy-input flex items-center justify-between gap-2 px-2.5 py-1.5 text-left text-sm disabled:opacity-55"
        >
          <span class={`min-w-0 flex-1 truncate ${isPlaceholder ? "text-muted" : "text-ink"}`}>
            {triggerLabel}
          </span>
          <span class="flex shrink-0 items-center gap-1.5">
            {selected != null ? (
              <span class="cfy-chip bg-info-bg text-info">{providerLabel(selected.provider)}</span>
            ) : value !== "" ? (
              <span class="cfy-chip bg-warn-bg text-warn">custom</span>
            ) : null}
            <svg
              width="12"
              height="12"
              viewBox="0 0 12 12"
              aria-hidden="true"
              class="shrink-0 text-muted"
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
          </span>
        </button>
      )}

      {open && (
        <div
          class={`cfy-popover absolute z-40 mt-1 flex max-h-80 flex-col overflow-hidden ${popoverClass ?? "left-0 right-0"}`}
          role="dialog"
        >
          <div class="border-b border-line p-1.5">
            <input
              ref={inputRef}
              type="text"
              value={query}
              spellcheck={false}
              placeholder="Search models…"
              onInput={(e) => setQuery((e.currentTarget as HTMLInputElement).value)}
              onKeyDown={onInputKeyDown}
              class="cfy-input px-2 py-1 text-sm"
              role="combobox"
              aria-expanded="true"
              aria-controls="cfy-model-listbox"
            />
          </div>
          <ul
            ref={listRef}
            id="cfy-model-listbox"
            role="listbox"
            class="min-h-0 flex-1 overflow-y-auto py-1"
          >
            {showCustom && (
              <li role="option" aria-selected={activeIndex === 0} data-opt={0}>
                <button
                  type="button"
                  onMouseEnter={() => setActiveIndex(0)}
                  onClick={() => commit({ kind: "custom", id: trimmedQuery })}
                  class={`flex w-full items-center gap-2 px-2.5 py-1.5 text-left text-sm ${
                    activeIndex === 0 ? "bg-hover" : ""
                  }`}
                >
                  <span class="text-muted">Use custom id</span>
                  <span class="min-w-0 flex-1 truncate font-mono text-[12px] text-ink">
                    {trimmedQuery}
                  </span>
                </button>
              </li>
            )}

            {groups.length === 0 && !showCustom && (
              <li class="px-2.5 py-3 text-center text-xs text-muted">No models match.</li>
            )}

            {groups.map((group) => (
              <li key={group.provider}>
                <div class="cfy-label sticky top-0 bg-raised px-2.5 py-1">
                  {providerLabel(group.provider)}
                  <span class="ml-1 font-normal normal-case tracking-normal text-muted">
                    {group.items.length}
                  </span>
                </div>
                <ul role="group">
                  {group.items.map((item) => {
                    const idx = renderIndex++;
                    const isActive = idx === activeIndex;
                    const isSelected = item.model.id === value;
                    const ctx = formatContextWindow(item.model.contextWindow);
                    const disabledOpt = item.disabledReason != null;
                    return (
                      <li key={item.model.id} role="option" aria-selected={isSelected} data-opt={idx}>
                        <button
                          type="button"
                          disabled={disabledOpt}
                          title={item.disabledReason ?? undefined}
                          onMouseEnter={() => !disabledOpt && setActiveIndex(idx)}
                          onClick={() => commit({ kind: "model", model: item.model, disabledReason: item.disabledReason })}
                          class={`flex w-full items-center gap-2 px-2.5 py-1.5 text-left text-sm ${
                            disabledOpt ? "cursor-not-allowed opacity-50" : isActive ? "bg-hover" : ""
                          }`}
                        >
                          <span
                            class={`min-w-0 flex-1 truncate ${isSelected ? "font-semibold text-accent-ink" : "text-ink"}`}
                          >
                            {item.model.displayName}
                          </span>
                          {item.disabledReason != null && (
                            <span class="shrink-0 text-[11px] text-warn">{item.disabledReason}</span>
                          )}
                          {ctx != null && (
                            <span class="shrink-0 font-mono text-[11px] text-muted">{ctx}</span>
                          )}
                        </button>
                      </li>
                    );
                  })}
                </ul>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}
