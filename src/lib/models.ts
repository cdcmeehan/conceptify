// Presentation helpers for catalog models and provider families (epic
// conceptify-e7m). The catalog carries canonical family ids (`anthropic`,
// `mistralai`, `x-ai`, `meta-llama`, …); the UI wants readable labels and
// compact context-window tags. Shared by the Settings suite toggles and the
// reusable model combobox.

const PROVIDER_LABELS: Record<string, string> = {
  anthropic: "Anthropic",
  openai: "OpenAI",
  google: "Google",
  mistralai: "Mistral",
  "x-ai": "xAI",
  "meta-llama": "Meta Llama",
  qwen: "Qwen",
  deepseek: "DeepSeek",
  cohere: "Cohere",
  ai21: "AI21",
  perplexity: "Perplexity",
  microsoft: "Microsoft",
  nvidia: "NVIDIA",
  amazon: "Amazon",
};

/** Readable label for a provider family id. Known families get a curated name;
 *  anything else is title-cased from its slug (`some-vendor` → `Some Vendor`). */
export function providerLabel(provider: string): string {
  const known = PROVIDER_LABELS[provider];
  if (known != null) return known;
  return provider
    .split(/[-_/]/)
    .filter(Boolean)
    .map((s) => s.charAt(0).toUpperCase() + s.slice(1))
    .join(" ");
}

/** Compact token count for a picker tag: `128000` → `128K`, `1000000` → `1M`.
 *  Returns `null` when the source reported no usable context window. */
export function formatContextWindow(tokens?: number | null): string | null {
  if (tokens == null || !Number.isFinite(tokens) || tokens <= 0) return null;
  if (tokens >= 1_000_000) {
    const m = tokens / 1_000_000;
    return `${m % 1 === 0 ? m.toFixed(0) : m.toFixed(1)}M`;
  }
  if (tokens >= 1_000) return `${Math.round(tokens / 1000)}K`;
  return String(tokens);
}
