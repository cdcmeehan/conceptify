// Route-display mirror of `src-tauri/src/routing.rs` (epic conceptify-e7m,
// bead e7m.7). routing.rs is the SOURCE OF TRUTH — it decides the real
// execution route at run time. This module exists only so the Settings UI
// (e7m.3) and the point-of-ask picker (e7m.4) can SHOW the route a chosen model
// will take, without a round-trip. Keep it in sync with `derive_provider` /
// `route_run` in routing.rs; the logic below is a faithful port of that
// function's model-id classification (adapter-override / custom-default-adapter
// bypasses are irrelevant to a per-purpose default, so they are not mirrored).

/** The route a model resolves to, mirroring routing.rs `RouteTag` plus an
 *  `unroutable` display state (routing.rs returns `SettingsError::UnroutableModel`
 *  for these — we render a warning rather than a route). */
export type RouteTag = "anthropic" | "openai" | "openrouter" | "local" | "unroutable";

export interface RouteResult {
  tag: RouteTag;
  /** True only for the OpenRouter route — it needs a stored OpenRouter key. */
  needsKey: boolean;
  /** Human explanation for the `unroutable` case; omitted otherwise. */
  reason?: string;
}

/** How each route reads in the UI (matches the bead's wording: "via claude CLI",
 *  "via codex CLI", "via OpenRouter"). */
const ROUTE_LABELS: Record<RouteTag, string> = {
  anthropic: "via claude CLI",
  openai: "via codex CLI",
  openrouter: "via OpenRouter",
  local: "via local endpoint",
  unroutable: "no route",
};

export function routeLabel(tag: RouteTag): string {
  return ROUTE_LABELS[tag];
}

/**
 * Best-effort provider *family* for a model id — the OpenRouter slug prefix for
 * a slash-form id (`google/gemini-3-pro` → `google`), else `null` for a bare id
 * whose family only the catalog knows. Mirrors the catalog's canonical family
 * names (OpenRouter slug prefixes already ARE those families). A leading `~`
 * (OpenRouter "…-latest" alias) is stripped for the family, matching
 * routing.rs / the catalog normalizer. Used by the Settings disabled-provider
 * check when a saved model belongs to a suite the user has turned off.
 */
export function familyOf(id: string): string | null {
  const trimmed = id.trim();
  const slash = trimmed.indexOf("/");
  if (slash <= 0) return null;
  const prefix = trimmed.slice(0, slash);
  return prefix.startsWith("~") ? prefix.slice(1) : prefix;
}

/**
 * Compute the route the backend would resolve for `id`, given a `providerOf`
 * lookup over the loaded catalog (exact id → family, or `undefined` when the
 * catalog doesn't list it). Faithful mirror of routing.rs `derive_provider`:
 *
 *  1. slash-form id → OpenRouter, unconditionally (a slash id IS an OR slug).
 *  2. exact catalog family: `anthropic` → claude CLI, `openai` → codex CLI;
 *     any OTHER family on a bare id → unroutable (its OpenRouter slug form is
 *     needed — bare non-native ids are not OR slugs).
 *  3. prefix heuristics for custom ids the catalog doesn't know:
 *     `claude-*` / `sonnet` / `opus` / `haiku` → anthropic;
 *     `gpt-*` / `codex-*` / `chatgpt-*` / `o<digit>…` → openai.
 *  4. otherwise → unroutable (fail fast, never guess).
 */
export function routeForModel(
  id: string,
  providerOf: (id: string) => string | undefined,
): RouteResult {
  const model = id.trim();
  if (model === "") {
    return { tag: "unroutable", needsKey: false, reason: "no model selected" };
  }

  // 1. Slash-form id is an OpenRouter slug.
  if (model.startsWith("local/") && model.length > 6) return { tag: "local", needsKey: false };
  if (model.includes("/")) return { tag: "openrouter", needsKey: true };

  // 2. Exact catalog family.
  const provider = providerOf(model);
  if (provider != null) {
    if (provider === "anthropic") return { tag: "anthropic", needsKey: false };
    if (provider === "openai") return { tag: "openai", needsKey: false };
    return {
      tag: "unroutable",
      needsKey: false,
      reason: `${provider} has no native runner here — pick this model's OpenRouter form (like ${provider}/${model})`,
    };
  }

  // 3. Prefix heuristics for custom ids.
  if (isAnthropicShaped(model)) return { tag: "anthropic", needsKey: false };
  if (isOpenaiShaped(model)) return { tag: "openai", needsKey: false };

  // 4. Give up — matches no known prefix and is not in the catalog.
  return {
    tag: "unroutable",
    needsKey: false,
    reason:
      "not in the catalog and matches no known provider prefix (claude-*, gpt-*, o<N>-*, codex-*, chatgpt-*)",
  };
}

/** `claude-*` plus the claude CLI's own short aliases (routing.rs
 *  `is_anthropic_shaped`). */
function isAnthropicShaped(model: string): boolean {
  return (
    model.startsWith("claude-") ||
    model === "sonnet" ||
    model === "opus" ||
    model === "haiku"
  );
}

/** `gpt-*` / `codex-*` / `chatgpt-*` and the reasoning-series `o<digit>…` ids
 *  (routing.rs `is_openai_shaped`; the digit requirement keeps it from
 *  swallowing arbitrary `o…` names). */
function isOpenaiShaped(model: string): boolean {
  if (
    model.startsWith("gpt-") ||
    model.startsWith("codex-") ||
    model.startsWith("chatgpt-")
  ) {
    return true;
  }
  return model.length >= 2 && model[0] === "o" && model[1] >= "0" && model[1] <= "9";
}
