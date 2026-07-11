//! Live model catalog service (epic conceptify-e7m, bead e7m.6).
//!
//! Owns the app's list of selectable models. On startup a background task
//! (spawned from `lib.rs`, never on the boot critical path — NFR cold start
//! ~310ms) fetches two public sources, normalizes them into a compact catalog,
//! and atomically caches the **normalized** form (not the multi-MB raw payloads)
//! under the app-support dir. Serving reads that cache; a fresh offline install
//! falls back to a small bundled snapshot. The whole fetch path is
//! failure-silent: any error logs and falls back, never a dialog (PRD N4 spirit).
//!
//! # Sources
//!
//! - **LiteLLM** `model_prices_and_context_window.json` (raw GitHub, ~1.5MB, a
//!   JSON object keyed by model id — each value carries `mode`,
//!   `litellm_provider`, `max_input_tokens`, ...). It is the source of the
//!   **native** family ids: `litellm_provider == "anthropic"` → bare claude ids
//!   (`claude-sonnet-5`) for the claude CLI, `== "openai"` → bare gpt/o ids
//!   (`gpt-5`) for the codex CLI.
//! - **OpenRouter** `GET /api/v1/models` (no auth, a `{ "data": [...] }` array;
//!   each entry has `id` like `google/gemini-3-pro`, `name`, `context_length`,
//!   `architecture.output_modalities`). Every model here is runnable via the
//!   OpenRouter route, so these are marked `openrouter_runnable = true`.
//!
//! # Normalization (see [`normalize_litellm`] / [`normalize_openrouter`])
//!
//! Both are filtered to **chat-capable** models and normalized to
//! [`CatalogModel`] `{ id, provider, display_name, context_window,
//! openrouter_runnable }`. `provider` is the model **family** for the
//! provider-suite toggles — a clean name (`anthropic`, `openai`, `google`,
//! `mistralai`, `meta-llama`, ...), never a LiteLLM backend-routing token
//! (`bedrock`, `azure`, `fireworks_ai`, ...). Because LiteLLM's
//! `litellm_provider` is overwhelmingly backend-routing names, only entries
//! whose provider is a recognized clean family ([`LITELLM_FAMILY_PROVIDERS`]) are
//! kept from LiteLLM; the runnable long tail (google/mistral/meta/...) comes from
//! OpenRouter. This keeps the catalog to attributable, mostly-runnable models
//! instead of ~2000 backend-namespaced near-duplicates. The two sources are
//! merged by exact `id` (OpenRouter presence forces `openrouter_runnable`).

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use conceptify_types::{CatalogModel, CatalogProvider, CatalogResponse};
use serde::{Deserialize, Serialize};

// --- Configuration ----------------------------------------------------------

/// LiteLLM's model metadata (raw GitHub `main`). Verified at impl time to resolve
/// and to be a model-id-keyed JSON object.
const LITELLM_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

/// OpenRouter's public model list (no auth). Verified `{ "data": [ ... ] }`.
const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/models";

/// Normalized-catalog cache filename under the app-support dir.
const CACHE_FILE: &str = "model_catalog.json";

/// Per-request network timeout for the fetch. The whole path is failure-silent,
/// so a slow/unreachable source degrades to the cache/snapshot, never a hang.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Cache time-to-live: skip the startup fetch when the cache is younger than
/// this, so rapid dev restarts don't hammer the sources.
fn cache_ttl() -> chrono::Duration {
    chrono::Duration::hours(24)
}

fn user_agent() -> String {
    format!("conceptify/{}", env!("CARGO_PKG_VERSION"))
}

/// LiteLLM `mode` values we treat as chat-capable (PRD "chat/completion").
/// Everything else — `embedding`, `image_generation`, `audio_*`, `rerank`,
/// `moderation`, `responses`, `realtime`, `video_generation`, ... — is dropped.
const LITELLM_CHAT_MODES: &[&str] = &["chat", "completion"];

/// LiteLLM `litellm_provider` tokens that are genuine model families (as opposed
/// to backend routers like `bedrock`/`azure`/`fireworks_ai`). Only entries with
/// one of these providers are taken from LiteLLM; each maps to a canonical family
/// via [`family_alias`]. Kept deliberately small and unambiguous.
const LITELLM_FAMILY_PROVIDERS: &[&str] = &[
    "anthropic",
    "openai",
    "gemini",
    "mistral",
    "xai",
    "deepseek",
    "cohere",
    "ai21",
];

/// The bundled last-resort snapshot (a few KB of current anthropic/openai/google/
/// mistral/... chat models), so a fresh offline install still has a sensible
/// list. Regenerate occasionally from a live fetch; it is not meant to be
/// exhaustive.
const BUNDLED_SNAPSHOT: &str = include_str!("catalog_snapshot.json");

// --- On-disk cache model ----------------------------------------------------

/// The normalized catalog as cached on disk / bundled as the snapshot. Stores the
/// FULL model set (all providers); provider filtering happens at serve time
/// against the current settings, so toggling providers needs no re-fetch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedCatalog {
    /// RFC3339 timestamp of the network fetch that produced this catalog.
    pub fetched_at: String,
    pub models: Vec<CatalogModel>,
}

/// Errors from a network fetch. Internal only — every public boundary is
/// failure-silent and falls back, so these are logged, not surfaced.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("http error fetching {url}: {source}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("failed to build http client: {0}")]
    Client(#[source] reqwest::Error),
    #[error("failed to parse {url} as JSON: {source}")]
    Parse {
        url: String,
        #[source]
        source: serde_json::Error,
    },
}

// --- Provider derivation ----------------------------------------------------

/// Canonicalize a provider token to a single family name, unifying the few
/// well-known aliases the two sources spell differently (LiteLLM's `gemini` vs
/// OpenRouter's `google`, `mistral` vs `mistralai`, `xai` vs `x-ai`). Anything
/// else passes through lowercased — already-canonical OpenRouter prefixes
/// (`google`, `mistralai`, `x-ai`, `meta-llama`, `qwen`, ...) are unchanged.
fn family_alias(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "gemini" | "vertex_ai" | "vertex_ai_beta" | "palm" | "google" => "google".to_owned(),
        "mistral" | "mistralai" => "mistralai".to_owned(),
        "xai" | "x-ai" => "x-ai".to_owned(),
        "meta" | "llama" | "meta-llama" | "meta_llama" => "meta-llama".to_owned(),
        other => other.to_owned(),
    }
}

/// The family for a LiteLLM entry, or `None` when its `litellm_provider` is a
/// backend router we don't attribute to a family (so it is dropped).
fn litellm_family(litellm_provider: &str) -> Option<String> {
    let p = litellm_provider.trim().to_ascii_lowercase();
    if LITELLM_FAMILY_PROVIDERS.contains(&p.as_str()) {
        Some(family_alias(&p))
    } else {
        None
    }
}

/// The family for an OpenRouter id: the slug prefix before `/`, canonicalized. A
/// leading `~` (OpenRouter's namespace for auto-updating "latest" aliases such as
/// `~anthropic/claude-sonnet-latest`) is stripped for the family only — the id
/// keeps the `~` because that is the executable OpenRouter slug.
fn openrouter_family(id: &str) -> String {
    let prefix = id.split('/').next().unwrap_or(id);
    family_alias(prefix.strip_prefix('~').unwrap_or(prefix))
}

// --- Normalization ----------------------------------------------------------

/// Normalize the LiteLLM payload: keep chat-capable, clean-family entries only.
/// `openrouter_runnable` is `false` here (set during [`merge`] if the same id is
/// also on OpenRouter). Robust to the `sample_spec` sentinel and non-object
/// values.
pub fn normalize_litellm(payload: &serde_json::Value) -> Vec<CatalogModel> {
    let Some(obj) = payload.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (id, val) in obj {
        if id == "sample_spec" {
            continue; // documentation sentinel, not a model
        }
        let Some(entry) = val.as_object() else {
            continue;
        };
        let mode = entry.get("mode").and_then(serde_json::Value::as_str);
        if !mode.is_some_and(|m| LITELLM_CHAT_MODES.contains(&m)) {
            continue;
        }
        let provider_raw = entry
            .get("litellm_provider")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let Some(provider) = litellm_family(provider_raw) else {
            continue;
        };
        let context_window = entry
            .get("max_input_tokens")
            .and_then(serde_json::Value::as_u64)
            .or_else(|| entry.get("max_tokens").and_then(serde_json::Value::as_u64));
        out.push(CatalogModel {
            id: id.clone(),
            provider,
            display_name: id.clone(),
            context_window,
            openrouter_runnable: false,
        });
    }
    out
}

/// Normalize the OpenRouter payload: keep chat-capable entries (output modalities
/// include `text`; a model that cannot emit text — pure image/audio generation —
/// is dropped). Every kept entry is `openrouter_runnable = true`.
pub fn normalize_openrouter(payload: &serde_json::Value) -> Vec<CatalogModel> {
    let Some(data) = payload.get("data").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in data {
        let Some(id) = entry.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        // Chat-capable = can output text. When architecture is absent we keep it
        // (assume a normal text model) rather than guess it away.
        let emits_text = match entry
            .get("architecture")
            .and_then(|a| a.get("output_modalities"))
            .and_then(serde_json::Value::as_array)
        {
            Some(mods) => mods
                .iter()
                .any(|m| m.as_str() == Some("text")),
            None => true,
        };
        if !emits_text {
            continue;
        }
        let display_name = entry
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(id)
            .to_owned();
        let context_window = entry
            .get("context_length")
            .and_then(serde_json::Value::as_u64);
        out.push(CatalogModel {
            id: id.to_owned(),
            provider: openrouter_family(id),
            display_name,
            context_window,
            openrouter_runnable: true,
        });
    }
    out
}

/// Merge the two normalized lists into the final catalog: union by exact `id`.
/// When an id appears in both, it collapses to one entry marked
/// `openrouter_runnable = true`, preferring OpenRouter's `name`/context/provider
/// (its labels are friendlier and its family prefix authoritative). Deterministic
/// order: sorted by `(provider, id)`.
pub fn merge(litellm: Vec<CatalogModel>, openrouter: Vec<CatalogModel>) -> Vec<CatalogModel> {
    let mut by_id: BTreeMap<String, CatalogModel> = BTreeMap::new();
    for m in litellm {
        by_id.insert(m.id.clone(), m);
    }
    for m in openrouter {
        match by_id.get_mut(&m.id) {
            Some(existing) => {
                existing.openrouter_runnable = true;
                existing.display_name = m.display_name;
                existing.provider = m.provider;
                if m.context_window.is_some() {
                    existing.context_window = m.context_window;
                }
            }
            None => {
                by_id.insert(m.id.clone(), m);
            }
        }
    }
    let mut models: Vec<CatalogModel> = by_id.into_values().collect();
    models.sort_by(|a, b| a.provider.cmp(&b.provider).then_with(|| a.id.cmp(&b.id)));
    models
}

// --- Fetch ------------------------------------------------------------------

async fn fetch_json(
    client: &reqwest::Client,
    url: &'static str,
) -> Result<serde_json::Value, CatalogError> {
    let resp = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|source| CatalogError::Http {
            url: url.to_owned(),
            source,
        })?;
    let bytes = resp.bytes().await.map_err(|source| CatalogError::Http {
        url: url.to_owned(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| CatalogError::Parse {
        url: url.to_owned(),
        source,
    })
}

/// Fetch both sources concurrently, normalize + merge, stamp `fetched_at = now`.
/// Requires **both** sources to succeed: a partial fetch (one source down) would
/// silently lose either native ids or the runnable flag, so we treat it as a
/// failure and let the caller fall back to the (last complete) cache/snapshot.
async fn fetch_normalized() -> Result<CachedCatalog, CatalogError> {
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .user_agent(user_agent())
        .build()
        .map_err(CatalogError::Client)?;

    let (litellm, openrouter) =
        tokio::try_join!(fetch_json(&client, LITELLM_URL), fetch_json(&client, OPENROUTER_URL))?;

    let models = merge(normalize_litellm(&litellm), normalize_openrouter(&openrouter));
    Ok(CachedCatalog {
        fetched_at: Utc::now().to_rfc3339(),
        models,
    })
}

// --- Cache I/O --------------------------------------------------------------

fn cache_path() -> io::Result<PathBuf> {
    Ok(crate::server::paths::app_support_dir()?.join(CACHE_FILE))
}

/// Read + parse the normalized cache from `dir`. Any error (missing/corrupt)
/// yields `None` — the caller falls back to the snapshot.
fn load_cache_from(dir: &Path) -> Option<CachedCatalog> {
    let raw = std::fs::read(dir.join(CACHE_FILE)).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Atomically write the normalized cache into `dir` via temp-file + rename, so a
/// crash mid-write never leaves a truncated/corrupt cache (PRD N4).
fn write_cache_atomic_in(dir: &Path, cat: &CachedCatalog) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(CACHE_FILE);
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(cat)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn load_cache() -> Option<CachedCatalog> {
    let dir = cache_path().ok()?.parent()?.to_path_buf();
    load_cache_from(&dir)
}

fn write_cache_atomic(cat: &CachedCatalog) -> io::Result<()> {
    let path = cache_path()?;
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no cache parent dir"))?;
    write_cache_atomic_in(dir, cat)
}

/// Parse the bundled snapshot. `expect` is intentional: the snapshot is a
/// compile-time `include_str!` asset validated by a unit test, so a parse
/// failure is a build/programmer error, not a runtime condition.
fn bundled_snapshot() -> CachedCatalog {
    serde_json::from_str(BUNDLED_SNAPSHOT).expect("bundled catalog snapshot must be valid JSON")
}

/// Whether `fetched_at` is younger than `ttl` relative to `now`. An unparseable
/// timestamp is treated as stale (forces a re-fetch).
fn is_fresh(fetched_at: &str, now: DateTime<Utc>, ttl: chrono::Duration) -> bool {
    match DateTime::parse_from_rfc3339(fetched_at) {
        Ok(ts) => now.signed_duration_since(ts.with_timezone(&Utc)) < ttl,
        Err(_) => false,
    }
}

// --- Public API (startup / serving / refresh) -------------------------------

/// Startup warm-up, spawned off the boot critical path from `lib.rs`. TTL-gated:
/// skips the fetch when the cache is younger than [`cache_ttl`]. Failure-silent —
/// on any fetch error the existing cache/snapshot stays in place. Never returns
/// an error and never blocks boot.
pub async fn refresh_on_startup() {
    if let Some(cache) = load_cache() {
        if is_fresh(&cache.fetched_at, Utc::now(), cache_ttl()) {
            eprintln!("[conceptify-catalog] cache is fresh; skipping startup fetch");
            return;
        }
    }
    match fetch_normalized().await {
        Ok(cat) => {
            let count = cat.models.len();
            match write_cache_atomic(&cat) {
                Ok(()) => eprintln!("[conceptify-catalog] refreshed catalog: {count} models cached"),
                Err(e) => eprintln!("[conceptify-catalog] fetched {count} models but cache write failed: {e}"),
            }
        }
        Err(e) => eprintln!("[conceptify-catalog] startup fetch failed (using cache/snapshot): {e}"),
    }
}

/// Force a re-fetch (the "refresh now" command/endpoint). On success writes the
/// cache and returns the fresh catalog tagged `"live"`. On failure falls back to
/// the cache (`"cache"`) or bundled snapshot (`"snapshot"`) — never errors.
pub async fn refresh_now() -> (CachedCatalog, &'static str) {
    match fetch_normalized().await {
        Ok(cat) => {
            if let Err(e) = write_cache_atomic(&cat) {
                eprintln!("[conceptify-catalog] refresh_now cache write failed: {e}");
            }
            (cat, "live")
        }
        Err(e) => {
            eprintln!("[conceptify-catalog] refresh_now fetch failed: {e}");
            load_for_serving()
        }
    }
}

/// The catalog to serve right now, without any network access: the disk cache if
/// present and parseable (`"cache"`), else the bundled snapshot (`"snapshot"`).
pub fn load_for_serving() -> (CachedCatalog, &'static str) {
    match load_cache() {
        Some(cat) => (cat, "cache"),
        None => (bundled_snapshot(), "snapshot"),
    }
}

/// The provider family the effective catalog (disk cache, else bundled
/// snapshot — never the network) records for an **exact** model id, or `None`
/// when the id is unknown. Provider routing's primary lookup (bead
/// `conceptify-e7m.7`); its callers fall back to prefix heuristics on `None`.
/// A fresh disk read per call — run starts are user-action-rate, and reading
/// keeps a mid-session catalog refresh visible without cache invalidation.
pub fn provider_of(model_id: &str) -> Option<String> {
    let (cat, _) = load_for_serving();
    cat.models
        .iter()
        .find(|m| m.id == model_id)
        .map(|m| m.provider.clone())
}

/// Project a [`CachedCatalog`] into the API response: models filtered to the
/// `enabled` providers (sorted by provider then id), plus every provider with its
/// full-catalog model count and enabled flag (for the settings toggles). Pure.
pub fn build_response(
    cat: &CachedCatalog,
    source: &str,
    enabled: &BTreeSet<String>,
) -> CatalogResponse {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for m in &cat.models {
        *counts.entry(m.provider.clone()).or_insert(0) += 1;
    }
    let providers = counts
        .into_iter()
        .map(|(provider, model_count)| CatalogProvider {
            enabled: enabled.contains(&provider),
            provider,
            model_count,
        })
        .collect();

    let mut models: Vec<CatalogModel> = cat
        .models
        .iter()
        .filter(|m| enabled.contains(&m.provider))
        .cloned()
        .collect();
    models.sort_by(|a, b| a.provider.cmp(&b.provider).then_with(|| a.id.cmp(&b.id)));

    CatalogResponse {
        fetched_at: cat.fetched_at.clone(),
        source: source.to_owned(),
        models,
        providers,
    }
}

pub fn add_local_endpoint(
    response: &mut CatalogResponse,
    endpoint: Option<&crate::settings::LocalEndpoint>,
    enabled: &BTreeSet<String>,
) {
    let Some(endpoint) = endpoint else { return; };
    response.providers.retain(|provider| provider.provider != "local");
    response.providers.push(CatalogProvider {
        provider: "local".into(),
        model_count: endpoint.models.len(),
        enabled: enabled.contains("local"),
    });
    if enabled.contains("local") {
        response.models.extend(endpoint.models.iter().map(|model| CatalogModel {
            id: format!("local/{model}"),
            provider: "local".into(),
            display_name: format!("{} · {model}", endpoint.name.trim()),
            context_window: None,
            openrouter_runnable: false,
        }));
        response.models.sort_by(|a, b| a.provider.cmp(&b.provider).then_with(|| a.id.cmp(&b.id)));
    }
    response.providers.sort_by(|a, b| a.provider.cmp(&b.provider));
}

#[cfg(test)]
mod tests {
    use super::*;

    // Small fixtures of BOTH source shapes (the real shapes verified live at impl
    // time). Kept inline so the normalization contract reads alongside the tests.

    const LITELLM_FIXTURE: &str = r#"{
      "sample_spec": {
        "litellm_provider": "one of: chat, embedding",
        "mode": "one of: chat, embedding, completion, image_generation"
      },
      "claude-sonnet-5": {
        "litellm_provider": "anthropic", "mode": "chat",
        "max_input_tokens": 200000, "max_tokens": 64000
      },
      "gpt-5": {
        "litellm_provider": "openai", "mode": "chat",
        "max_input_tokens": 272000, "max_tokens": 128000
      },
      "gemini/gemini-2.5-pro": {
        "litellm_provider": "gemini", "mode": "chat", "max_input_tokens": 1048576
      },
      "text-embedding-3-large": {
        "litellm_provider": "openai", "mode": "embedding", "max_input_tokens": 8191
      },
      "dall-e-3": {
        "litellm_provider": "openai", "mode": "image_generation"
      },
      "anthropic.claude-sonnet-5": {
        "litellm_provider": "bedrock", "mode": "chat", "max_input_tokens": 200000
      },
      "some-legacy": {
        "litellm_provider": "openai", "mode": "completion", "max_tokens": 4096
      },
      "no-mode-entry": {
        "litellm_provider": "anthropic"
      }
    }"#;

    #[test]
    fn configured_local_models_join_catalog_under_explicit_ids() {
        let mut response = build_response(&CachedCatalog {
            fetched_at: "now".into(), models: vec![],
        }, "snapshot", &BTreeSet::from(["local".into()]));
        let endpoint = crate::settings::LocalEndpoint {
            name: "Studio GPU".into(),
            base_url: "http://127.0.0.1:4000".into(),
            models: vec!["llama-3.3".into(), "qwen-coder".into()],
        };
        add_local_endpoint(&mut response, Some(&endpoint), &BTreeSet::from(["local".into()]));
        assert_eq!(response.models.iter().map(|model| model.id.as_str()).collect::<Vec<_>>(), vec!["local/llama-3.3", "local/qwen-coder"]);
        assert_eq!(response.providers[0].provider, "local");
        assert!(response.providers[0].enabled);
    }

    const OPENROUTER_FIXTURE: &str = r#"{
      "data": [
        {
          "id": "google/gemini-2.5-pro", "name": "Google: Gemini 2.5 Pro",
          "context_length": 1048576,
          "architecture": { "output_modalities": ["text"] }
        },
        {
          "id": "anthropic/claude-sonnet-5", "name": "Anthropic: Claude Sonnet 5",
          "context_length": 200000,
          "architecture": { "output_modalities": ["text"] }
        },
        {
          "id": "openai/image-only", "name": "Image Only",
          "context_length": 4096,
          "architecture": { "output_modalities": ["image"] }
        },
        {
          "id": "mistralai/mistral-large", "name": "Mistral: Large",
          "context_length": 128000
        },
        {
          "id": "~anthropic/claude-sonnet-latest", "name": "Anthropic: Claude Sonnet (latest)",
          "context_length": 200000,
          "architecture": { "output_modalities": ["text"] }
        }
      ]
    }"#;

    fn litellm() -> Vec<CatalogModel> {
        normalize_litellm(&serde_json::from_str(LITELLM_FIXTURE).unwrap())
    }
    fn openrouter() -> Vec<CatalogModel> {
        normalize_openrouter(&serde_json::from_str(OPENROUTER_FIXTURE).unwrap())
    }
    fn find<'a>(v: &'a [CatalogModel], id: &str) -> Option<&'a CatalogModel> {
        v.iter().find(|m| m.id == id)
    }

    #[test]
    fn litellm_keeps_chat_families_drops_backends_and_non_chat() {
        let m = litellm();
        let ids: BTreeSet<_> = m.iter().map(|x| x.id.as_str()).collect();
        // Kept: chat + completion under clean families.
        assert!(ids.contains("claude-sonnet-5"));
        assert!(ids.contains("gpt-5"));
        assert!(ids.contains("gemini/gemini-2.5-pro"));
        assert!(ids.contains("some-legacy")); // completion mode is chat-capable
        // Dropped: sentinel, embedding, image-gen, backend-router provider, no-mode.
        assert!(!ids.contains("sample_spec"));
        assert!(!ids.contains("text-embedding-3-large"));
        assert!(!ids.contains("dall-e-3"));
        assert!(!ids.contains("anthropic.claude-sonnet-5")); // bedrock backend
        assert!(!ids.contains("no-mode-entry"));
    }

    #[test]
    fn litellm_maps_family_and_context_and_is_not_runnable() {
        let m = litellm();
        let claude = find(&m, "claude-sonnet-5").unwrap();
        assert_eq!(claude.provider, "anthropic");
        assert_eq!(claude.context_window, Some(200000)); // from max_input_tokens
        assert!(!claude.openrouter_runnable);
        // gemini → google canonical family.
        assert_eq!(find(&m, "gemini/gemini-2.5-pro").unwrap().provider, "google");
        // completion entry falls back to max_tokens for context.
        assert_eq!(find(&m, "some-legacy").unwrap().context_window, Some(4096));
    }

    #[test]
    fn openrouter_marks_runnable_families_and_drops_image_only() {
        let m = openrouter();
        let ids: BTreeSet<_> = m.iter().map(|x| x.id.as_str()).collect();
        assert!(ids.contains("google/gemini-2.5-pro"));
        assert!(ids.contains("anthropic/claude-sonnet-5"));
        assert!(ids.contains("mistralai/mistral-large")); // no architecture → kept
        assert!(!ids.contains("openai/image-only")); // output image only → dropped
        let g = find(&m, "google/gemini-2.5-pro").unwrap();
        assert_eq!(g.provider, "google");
        assert!(g.openrouter_runnable);
        assert_eq!(g.display_name, "Google: Gemini 2.5 Pro");
        assert_eq!(g.context_window, Some(1048576));

        // OpenRouter "~latest" alias: family strips the leading `~` (groups under
        // anthropic), but the id keeps `~` (it is the executable slug).
        let latest = find(&m, "~anthropic/claude-sonnet-latest").unwrap();
        assert_eq!(latest.provider, "anthropic");
        assert!(latest.openrouter_runnable);
    }

    #[test]
    fn merge_dedups_by_id_and_ors_runnable() {
        let merged = merge(litellm(), openrouter());
        // google/gemini-2.5-pro is in BOTH (LiteLLM key "gemini/..." differs, but
        // OpenRouter "google/..." is distinct). Native "claude-sonnet-5" (LiteLLM)
        // and "anthropic/claude-sonnet-5" (OpenRouter) are DIFFERENT ids → both kept.
        assert!(find(&merged, "claude-sonnet-5").is_some());
        let native_claude = find(&merged, "claude-sonnet-5").unwrap();
        assert!(!native_claude.openrouter_runnable, "native id is not the OR slug");
        let or_claude = find(&merged, "anthropic/claude-sonnet-5").unwrap();
        assert!(or_claude.openrouter_runnable);

        // Construct a genuine exact-id collision to prove OR-flag wins.
        let a = vec![CatalogModel {
            id: "x/y".into(), provider: "x".into(), display_name: "litellm".into(),
            context_window: Some(1), openrouter_runnable: false,
        }];
        let b = vec![CatalogModel {
            id: "x/y".into(), provider: "x".into(), display_name: "openrouter".into(),
            context_window: Some(2), openrouter_runnable: true,
        }];
        let merged = merge(a, b);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].openrouter_runnable);
        assert_eq!(merged[0].display_name, "openrouter"); // OR label preferred
        assert_eq!(merged[0].context_window, Some(2));
    }

    #[test]
    fn merge_output_is_sorted_by_provider_then_id() {
        let merged = merge(litellm(), openrouter());
        let keys: Vec<(String, String)> =
            merged.iter().map(|m| (m.provider.clone(), m.id.clone())).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn ttl_fresh_and_stale() {
        let now = Utc::now();
        let ttl = chrono::Duration::hours(24);
        let recent = (now - chrono::Duration::hours(1)).to_rfc3339();
        let old = (now - chrono::Duration::hours(25)).to_rfc3339();
        assert!(is_fresh(&recent, now, ttl));
        assert!(!is_fresh(&old, now, ttl));
        assert!(!is_fresh("not-a-timestamp", now, ttl)); // unparseable → stale
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "conceptify-test-catalog-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn atomic_write_round_trips_and_leaves_no_temp() {
        let dir = temp_dir("atomic");
        let cat = CachedCatalog {
            fetched_at: Utc::now().to_rfc3339(),
            models: merge(litellm(), openrouter()),
        };
        write_cache_atomic_in(&dir, &cat).unwrap();
        let read = load_cache_from(&dir).unwrap();
        assert_eq!(read, cat);
        // No leftover temp file.
        assert!(!dir.join("model_catalog.json.tmp").exists());
        assert!(dir.join("model_catalog.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fallback_chain_cache_then_snapshot() {
        // No cache in an empty dir → None (would fall through to snapshot).
        let dir = temp_dir("fallback");
        assert!(load_cache_from(&dir).is_none());

        // Snapshot is always available and non-empty (the last-resort fallback).
        let snap = bundled_snapshot();
        assert!(!snap.models.is_empty());

        // Write a cache, then it is preferred.
        let cat = CachedCatalog {
            fetched_at: Utc::now().to_rfc3339(),
            models: litellm(),
        };
        write_cache_atomic_in(&dir, &cat).unwrap();
        assert_eq!(load_cache_from(&dir).unwrap(), cat);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_is_valid_has_native_families_and_is_small() {
        let snap = bundled_snapshot();
        let providers: BTreeSet<_> = snap.models.iter().map(|m| m.provider.as_str()).collect();
        assert!(providers.contains("anthropic"));
        assert!(providers.contains("openai"));
        // Native anthropic/openai ids present (the claude/codex routes need them).
        assert!(snap.models.iter().any(|m| m.id == "claude-sonnet-5"));
        assert!(snap.models.iter().any(|m| m.id == "gpt-5"));
        // A genuinely small asset (a few KB), not the whole catalog.
        assert!(BUNDLED_SNAPSHOT.len() < 8 * 1024, "snapshot should stay small");
    }

    #[test]
    fn build_response_filters_to_enabled_and_counts_all_providers() {
        let cat = CachedCatalog {
            fetched_at: "2026-07-05T00:00:00Z".into(),
            models: merge(litellm(), openrouter()),
        };
        let enabled: BTreeSet<String> =
            ["anthropic".to_owned(), "openai".to_owned()].into_iter().collect();
        let resp = build_response(&cat, "cache", &enabled);

        assert_eq!(resp.source, "cache");
        assert_eq!(resp.fetched_at, "2026-07-05T00:00:00Z");
        // Only enabled-provider models are returned.
        assert!(resp.models.iter().all(|m| m.provider == "anthropic" || m.provider == "openai"));
        assert!(resp.models.iter().any(|m| m.id == "claude-sonnet-5"));
        assert!(resp.models.iter().any(|m| m.id == "gpt-5"));
        assert!(!resp.models.iter().any(|m| m.provider == "google"));
        // Returned models are sorted by (provider, id).
        let keys: Vec<_> = resp.models.iter().map(|m| (m.provider.clone(), m.id.clone())).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);

        // Provider list covers ALL families (incl. disabled) with counts + flags.
        let google = resp.providers.iter().find(|p| p.provider == "google").unwrap();
        assert!(!google.enabled);
        assert!(google.model_count >= 1);
        let anthropic = resp.providers.iter().find(|p| p.provider == "anthropic").unwrap();
        assert!(anthropic.enabled);
    }

    #[test]
    fn build_response_empty_enabled_returns_no_models_but_all_providers() {
        let cat = bundled_snapshot();
        let resp = build_response(&cat, "snapshot", &BTreeSet::new());
        assert!(resp.models.is_empty());
        assert!(!resp.providers.is_empty());
        assert!(resp.providers.iter().all(|p| !p.enabled));
    }

    /// Live network probe — run once by hand to verify the real sources resolve
    /// and to record counts (`cargo test -p conceptify catalog -- --ignored
    /// --nocapture`). Ignored so the normal suite never hits the network.
    #[tokio::test]
    #[ignore = "hits the live network; run manually to verify source shapes"]
    async fn live_fetch_counts() {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .user_agent(user_agent())
            .build()
            .unwrap();
        let litellm = fetch_json(&client, LITELLM_URL).await.expect("litellm fetch");
        let openrouter = fetch_json(&client, OPENROUTER_URL).await.expect("openrouter fetch");
        let l = normalize_litellm(&litellm);
        let o = normalize_openrouter(&openrouter);
        let merged = merge(l.clone(), o.clone());
        let providers: BTreeSet<_> = merged.iter().map(|m| m.provider.clone()).collect();
        let runnable = merged.iter().filter(|m| m.openrouter_runnable).count();
        println!("LIVE litellm chat/family models: {}", l.len());
        println!("LIVE openrouter chat models: {}", o.len());
        println!("LIVE merged models: {}", merged.len());
        println!("LIVE openrouter_runnable: {runnable}");
        println!("LIVE providers ({}): {:?}", providers.len(), providers);
        assert!(merged.len() > 50);
    }
}
