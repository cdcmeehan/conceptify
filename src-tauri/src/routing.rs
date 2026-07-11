//! Provider-routed execution (epic `conceptify-e7m`, bead `conceptify-e7m.7`).
//!
//! The user picks a **model**; this module derives the execution path. It sits
//! *above* [`AgentSettings::resolve_with_override`] — routing picks the
//! `(adapter, model, env)` triple, resolution stays a pure template expander —
//! and is consumed by the run engine (`crate::runs::start_reserved`) right
//! before the invocation is resolved, so every routing failure (missing
//! OpenRouter key, unroutable model) surfaces BEFORE any run row exists and
//! never wedges the FR-4.9 per-thread guard.
//!
//! # Routing table
//!
//! | model's provider | route (tag)  | adapter | extra per-run env            |
//! |------------------|--------------|---------|------------------------------|
//! | anthropic        | `anthropic`  | `claude`| none — today's native path   |
//! | openai           | `openai`     | `codex` | none — native codex path     |
//! | anything else    | `openrouter` | `claude`| Anthropic-protocol env → OpenRouter (below) |
//!
//! # Bypass precedence (highest first)
//!
//! 1. **Per-run adapter override** ([`RunOverride::adapter`]) — the advanced
//!    escape hatch: routing is skipped entirely (no derived adapter, no env
//!    injection — an explicit `adapter: claude` + a `google/...` model will
//!    genuinely hit the native Anthropic API and fail; that is the point of an
//!    escape hatch). Tag: `manual`.
//! 2. **Custom default adapter** — when `settings.default_adapter` is neither
//!    built-in (`claude`/`codex`), the user has configured their own harness
//!    (G6 config-only adapters); routing has no idea how that harness maps
//!    models to providers, so it respects the config verbatim. Tag: `manual`.
//!    This is also what keeps every pre-routing test/config byte-identical:
//!    the engine's fake-adapter fixtures set a custom `default_adapter`.
//! 3. Otherwise the **model alone** decides, per the table above.
//!
//! # Model id → provider derivation (decided + recorded per the bead)
//!
//! 1. **Slash-form** (`vendor/model`, incl. `~vendor/latest-alias`) → the
//!    OpenRouter route, *unconditionally*. A slash id IS an OpenRouter slug —
//!    that is the execution contract the catalog established (bead e7m.6:
//!    "id == execution id") — so even `anthropic/claude-sonnet-5` deliberately
//!    routes via OpenRouter: the user picked the OpenRouter-namespaced entry,
//!    and the native claude CLI does not accept slug-form ids. Native
//!    execution of Anthropic models uses the bare ids the catalog lists for
//!    provider `anthropic`.
//! 2. **Catalog lookup** (exact id, via the injected `provider_of` — backed by
//!    [`crate::catalog::provider_of`], the disk cache/bundled snapshot, never
//!    the network): `anthropic` → claude, `openai` → codex. Any *other*
//!    catalog provider on a bare id is **unroutable** — bare non-native ids
//!    are not OpenRouter slugs (OpenRouter needs `vendor/model`), and
//!    inventing a vendor prefix would be guessing — so this fails fast with
//!    the suggestion to pick the model's OpenRouter form.
//! 3. **Prefix heuristics** for custom ids the catalog doesn't know:
//!    `claude-*` (plus the claude CLI's `sonnet`/`opus`/`haiku` aliases) →
//!    anthropic; `gpt-*` / `codex-*` / `chatgpt-*` / `o<digit>…` → openai.
//! 4. Anything else → [`SettingsError::UnroutableModel`] (fail fast, never
//!    guess).
//!
//! # The OpenRouter route mechanism (verified live, claude CLI 2.1.201)
//!
//! Verified against a local capture server standing in for the endpoint
//! (2026-07-06, this bead) rather than assumed:
//!
//! - `--model <vendor/model>` passes the slug through **verbatim** as the
//!   request body's `model` field — no `ANTHROPIC_MODEL` env remap is needed,
//!   so the existing `--model {model}` template arg carries the OpenRouter id
//!   unchanged.
//! - `ANTHROPIC_BASE_URL=https://openrouter.ai/api` — the CLI appends
//!   `/v1/messages`, landing exactly on OpenRouter's Anthropic-compatible
//!   endpoint.
//! - `ANTHROPIC_AUTH_TOKEN=<key>` becomes `Authorization: Bearer <key>` (no
//!   `x-api-key` header is sent). `ANTHROPIC_API_KEY` is explicitly set to the
//!   empty string so a key from the parent env can never shadow the token.
//! - **The user's normal claude login is untouched:** these are per-child-
//!   process env vars (`tokio::process::Command::env`), never process-global
//!   or persisted; the CLI itself confirms the precedence is env-scoped
//!   ("another auth source is set and takes precedence over your claude.ai
//!   login"). An anthropic-routed run carries none of these vars.
//!
//! # Secret discipline
//!
//! `RouteDecision::env` VALUES may carry the OpenRouter key. **Never log,
//! persist, or embed this vec (or any env) in an error/event/row.** The engine
//! records route visibility as the token-free [`RouteTag`] (+ a base-url note
//! in the log header); `runs::tests` proves end-to-end that a routed run's log
//! file and DB row never contain the token. OQ3 permission-scoping flags stay
//! exactly as the claude template pins them — the OpenRouter route is still
//! the claude CLI harness (bead design note).

use crate::settings::{AgentSettings, Purpose, RunOverride, SettingsError};

/// OpenRouter's Anthropic-compatible API base (the claude CLI appends
/// `/v1/messages`). Also surfaced token-free in the run-log header.
pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api";

/// The built-in adapters provider routing knows how to drive. A
/// `default_adapter` outside this set means a user-configured harness →
/// routing bypassed (see module docs, bypass rule 2).
const ROUTABLE_ADAPTERS: &[&str] = &["claude", "codex"];

/// Which execution path a run resolved to — recorded (token-free) on the run
/// row's `route` column and in the run-log header, so a user can always tell
/// which path executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteTag {
    /// Native Anthropic via the claude CLI (today's default path).
    Anthropic,
    /// Native OpenAI via the codex CLI.
    Openai,
    /// Any other provider: claude CLI pointed at OpenRouter via per-run env.
    Openrouter,
    /// Explicit `local/<id>` through the configured Anthropic-compatible gateway.
    Local,
    /// Routing bypassed — user-directed adapter (per-run override or a custom
    /// `default_adapter`).
    Manual,
}

impl RouteTag {
    pub fn as_str(self) -> &'static str {
        match self {
            RouteTag::Anthropic => "anthropic",
            RouteTag::Openai => "openai",
            RouteTag::Openrouter => "openrouter",
            RouteTag::Local => "local",
            RouteTag::Manual => "manual",
        }
    }
}

/// The routing outcome: which adapter template to resolve, the model id
/// (verbatim — no remap, see module docs), the extra per-run env (possibly
/// secret-bearing — see the secret-discipline note), and the token-free tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDecision {
    pub adapter: String,
    pub model: String,
    /// Applied by the engine on top of the flow env (`StartRun::env`), last-
    /// writer-wins. Empty for every route except OpenRouter.
    pub env: Vec<(String, String)>,
    pub tag: RouteTag,
}

/// Derived provider class for a model id (internal to routing).
enum Provider {
    Anthropic,
    Openai,
    /// A slash-form OpenRouter slug.
    Openrouter,
}

/// Resolve the route for one run. Pure apart from the injected `provider_of`
/// catalog lookup (disk-only); spawns nothing, mutates nothing.
///
/// `over` is the raw per-run override from the caller; `openrouter_key` is the
/// stored key (`settings::get_openrouter_api_key`), consumed only when the
/// route is OpenRouter — a missing key then fails fast
/// ([`SettingsError::OpenRouterKeyMissing`]) so no run row is ever created for
/// a run that cannot authenticate.
pub fn route_run(
    settings: &AgentSettings,
    purpose: Purpose,
    over: Option<&RunOverride>,
    provider_of: impl Fn(&str) -> Option<String>,
    openrouter_key: Option<&str>,
) -> Result<RouteDecision, SettingsError> {
    // The model the user (or the per-purpose default) selected. Validated here
    // so routing never classifies garbage (the resolver re-validates anyway).
    let model = match over.and_then(|o| o.model.as_deref()) {
        Some(m) => {
            crate::settings::validate_model(m)?;
            m.to_owned()
        }
        None => settings.models.for_purpose(purpose).to_owned(),
    };

    // Bypass 1: explicit per-run adapter override — the escape hatch.
    if let Some(adapter) = over.and_then(|o| o.adapter.as_deref()) {
        return Ok(RouteDecision {
            adapter: adapter.to_owned(),
            model,
            env: Vec::new(),
            tag: RouteTag::Manual,
        });
    }

    // Bypass 2: custom (non-built-in) default adapter — respect the config.
    if !ROUTABLE_ADAPTERS.contains(&settings.default_adapter.as_str()) {
        return Ok(RouteDecision {
            adapter: settings.default_adapter.clone(),
            model,
            env: Vec::new(),
            tag: RouteTag::Manual,
        });
    }

    if let Some(local_model) = model.strip_prefix("local/") {
        let endpoint = settings.local_endpoint.as_ref().ok_or_else(|| {
            SettingsError::InvalidLocalEndpoint("configure a local endpoint before selecting local models".into())
        })?;
        if !endpoint.models.iter().any(|candidate| candidate == local_model) {
            return Err(SettingsError::InvalidLocalEndpoint(format!("model '{local_model}' is not configured")));
        }
        return Ok(RouteDecision {
            adapter: "claude".into(),
            model: local_model.to_owned(),
            env: vec![
                ("ANTHROPIC_BASE_URL".into(), endpoint.base_url.trim_end_matches('/').to_owned()),
                // The engine replaces this non-secret sentinel with the
                // separately stored optional key immediately before spawn.
                ("ANTHROPIC_AUTH_TOKEN".into(), "local".into()),
                ("ANTHROPIC_API_KEY".into(), String::new()),
            ],
            tag: RouteTag::Local,
        });
    }

    // The model alone decides.
    match derive_provider(&model, &provider_of)? {
        Provider::Anthropic => Ok(RouteDecision {
            adapter: "claude".to_owned(),
            model,
            env: Vec::new(),
            tag: RouteTag::Anthropic,
        }),
        Provider::Openai => Ok(RouteDecision {
            adapter: "codex".to_owned(),
            model,
            env: Vec::new(),
            tag: RouteTag::Openai,
        }),
        Provider::Openrouter => {
            let Some(key) = openrouter_key.map(str::trim).filter(|k| !k.is_empty()) else {
                return Err(SettingsError::OpenRouterKeyMissing(model));
            };
            Ok(RouteDecision {
                adapter: "claude".to_owned(),
                model,
                env: vec![
                    (
                        "ANTHROPIC_BASE_URL".to_owned(),
                        OPENROUTER_BASE_URL.to_owned(),
                    ),
                    ("ANTHROPIC_AUTH_TOKEN".to_owned(), key.to_owned()),
                    // Explicitly emptied so an inherited key can never shadow
                    // the bearer token (verified: the CLI then sends only the
                    // Authorization header).
                    ("ANTHROPIC_API_KEY".to_owned(), String::new()),
                ],
                tag: RouteTag::Openrouter,
            })
        }
    }
}

/// Model id → provider, per the derivation order in the module docs:
/// slash-form → OpenRouter; catalog lookup; prefix heuristics; else fail fast.
fn derive_provider(
    model: &str,
    provider_of: &impl Fn(&str) -> Option<String>,
) -> Result<Provider, SettingsError> {
    if model.contains('/') {
        return Ok(Provider::Openrouter);
    }

    if let Some(provider) = provider_of(model) {
        return match provider.as_str() {
            "anthropic" => Ok(Provider::Anthropic),
            "openai" => Ok(Provider::Openai),
            other => Err(SettingsError::UnroutableModel(
                model.to_owned(),
                format!(
                    "provider '{other}' has no native runner here — use the \
                     model's OpenRouter id (like '{other}/{model}')"
                ),
            )),
        };
    }

    if is_anthropic_shaped(model) {
        return Ok(Provider::Anthropic);
    }
    if is_openai_shaped(model) {
        return Ok(Provider::Openai);
    }

    Err(SettingsError::UnroutableModel(
        model.to_owned(),
        "it is not in the model catalog and matches no known provider prefix \
         (claude-*, gpt-*, o<N>-*, codex-*, chatgpt-*)"
            .to_owned(),
    ))
}

/// `claude-*` plus the claude CLI's own short aliases.
fn is_anthropic_shaped(model: &str) -> bool {
    model.starts_with("claude-") || matches!(model, "sonnet" | "opus" | "haiku")
}

/// `gpt-*` / `codex-*` / `chatgpt-*` and the reasoning-series `o<digit>…` ids
/// (`o1`, `o3-mini`, `o4-mini-high`, ...). The digit requirement keeps this
/// from swallowing arbitrary `o...` names.
fn is_openai_shaped(model: &str) -> bool {
    if model.starts_with("gpt-") || model.starts_with("codex-") || model.starts_with("chatgpt-") {
        return true;
    }
    let mut chars = model.chars();
    chars.next() == Some('o') && chars.next().is_some_and(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::AgentSettings;

    /// A catalog lookup that knows nothing — forces heuristics/fail-fast.
    fn no_catalog(_: &str) -> Option<String> {
        None
    }

    fn over(adapter: Option<&str>, model: Option<&str>) -> RunOverride {
        RunOverride {
            adapter: adapter.map(str::to_owned),
            model: model.map(str::to_owned),
        }
    }

    // --- Routing table per provider -----------------------------------------

    #[test]
    fn default_settings_route_anthropic_natively_per_purpose() {
        // The byte-identity precondition: with pure defaults and no override,
        // every purpose routes anthropic → claude with the per-purpose model
        // and NO env — exactly today's path.
        let s = AgentSettings::default();
        for (purpose, model) in [
            (Purpose::FollowUp, "claude-haiku-4-5"),
            (Purpose::ArtifactUpdate, "claude-sonnet-5"),
            (Purpose::InAppAsk, "claude-sonnet-5"),
        ] {
            let d = route_run(&s, purpose, None, no_catalog, None).unwrap();
            assert_eq!(d.adapter, "claude");
            assert_eq!(d.model, model);
            assert!(d.env.is_empty());
            assert_eq!(d.tag, RouteTag::Anthropic);
        }
    }

    #[test]
    fn explicit_local_model_routes_to_configured_gateway_without_affecting_slash_heuristics() {
        let mut s = AgentSettings::default();
        s.local_endpoint = Some(crate::settings::LocalEndpoint {
            name: "Lab".into(),
            base_url: "http://127.0.0.1:4000/".into(),
            models: vec!["qwen-coder".into()],
        });
        let d = route_run(&s, Purpose::FollowUp, Some(&over(None, Some("local/qwen-coder"))), no_catalog, None).unwrap();
        assert_eq!(d.tag, RouteTag::Local);
        assert_eq!(d.adapter, "claude");
        assert_eq!(d.model, "qwen-coder");
        assert_eq!(d.env[0], ("ANTHROPIC_BASE_URL".into(), "http://127.0.0.1:4000".into()));
        assert_eq!(d.env[1].1, "local");

        let openrouter = route_run(&s, Purpose::FollowUp, Some(&over(None, Some("vendor/model"))), no_catalog, Some("key")).unwrap();
        assert_eq!(openrouter.tag, RouteTag::Openrouter);
    }

    #[test]
    fn openai_models_route_to_codex() {
        let s = AgentSettings::default();
        for model in ["gpt-5.4-mini", "o3", "o4-mini-high", "codex-mini-latest", "chatgpt-4o-latest"] {
            let o = over(None, Some(model));
            let d = route_run(&s, Purpose::FollowUp, Some(&o), no_catalog, None).unwrap();
            assert_eq!(d.adapter, "codex", "{model}");
            assert_eq!(d.model, model);
            assert!(d.env.is_empty());
            assert_eq!(d.tag, RouteTag::Openai);
        }
    }

    #[test]
    fn anthropic_heuristics_cover_aliases() {
        let s = AgentSettings::default();
        for model in ["claude-opus-9", "sonnet", "opus", "haiku"] {
            let o = over(None, Some(model));
            let d = route_run(&s, Purpose::InAppAsk, Some(&o), no_catalog, None).unwrap();
            assert_eq!(d.adapter, "claude", "{model}");
            assert_eq!(d.tag, RouteTag::Anthropic);
            assert!(d.env.is_empty());
        }
    }

    #[test]
    fn slash_form_routes_via_openrouter_with_exact_env() {
        let s = AgentSettings::default();
        for model in [
            "google/gemini-3-pro",
            "mistralai/mistral-large",
            "~anthropic/claude-sonnet-latest", // OR "latest" alias namespace
            "anthropic/claude-sonnet-5",       // OR-namespaced anthropic: still OR
        ] {
            let o = over(None, Some(model));
            let d =
                route_run(&s, Purpose::FollowUp, Some(&o), no_catalog, Some("sk-or-v1-k")).unwrap();
            assert_eq!(d.adapter, "claude", "{model}");
            assert_eq!(d.model, model, "slug passes through verbatim (no remap)");
            assert_eq!(d.tag, RouteTag::Openrouter);
            // The exact env contract verified live against claude CLI 2.1.201.
            assert_eq!(
                d.env,
                vec![
                    (
                        "ANTHROPIC_BASE_URL".to_owned(),
                        "https://openrouter.ai/api".to_owned()
                    ),
                    ("ANTHROPIC_AUTH_TOKEN".to_owned(), "sk-or-v1-k".to_owned()),
                    ("ANTHROPIC_API_KEY".to_owned(), String::new()),
                ]
            );
        }
    }

    // --- Catalog lookup ------------------------------------------------------

    #[test]
    fn catalog_provider_beats_heuristics_for_bare_ids() {
        let s = AgentSettings::default();
        let catalog = |id: &str| match id {
            "surprise-model" => Some("openai".to_owned()), // no gpt- prefix
            "davinci-002" => Some("openai".to_owned()),
            "claude-next" => Some("anthropic".to_owned()),
            _ => None,
        };
        let o = over(None, Some("surprise-model"));
        let d = route_run(&s, Purpose::FollowUp, Some(&o), catalog, None).unwrap();
        assert_eq!((d.adapter.as_str(), d.tag), ("codex", RouteTag::Openai));

        let o = over(None, Some("claude-next"));
        let d = route_run(&s, Purpose::FollowUp, Some(&o), catalog, None).unwrap();
        assert_eq!((d.adapter.as_str(), d.tag), ("claude", RouteTag::Anthropic));
    }

    #[test]
    fn bare_other_provider_id_is_unroutable_with_actionable_suggestion() {
        // A bare id the catalog attributes to a non-native provider must fail
        // fast (bare ids are not OpenRouter slugs) and point at the slash form.
        let s = AgentSettings::default();
        let catalog = |id: &str| (id == "gemini-3-pro").then(|| "google".to_owned());
        let o = over(None, Some("gemini-3-pro"));
        let err = route_run(&s, Purpose::FollowUp, Some(&o), catalog, Some("k")).unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, SettingsError::UnroutableModel(..)));
        assert!(msg.contains("google/gemini-3-pro"), "{msg}");
    }

    #[test]
    fn unknown_custom_id_fails_fast_not_guessed() {
        let s = AgentSettings::default();
        let o = over(None, Some("totally-custom-llm"));
        let err = route_run(&s, Purpose::FollowUp, Some(&o), no_catalog, Some("k")).unwrap_err();
        assert!(matches!(err, SettingsError::UnroutableModel(..)), "{err:?}");
        // Message is actionable: names the model and the ways out.
        let msg = err.to_string();
        assert!(msg.contains("totally-custom-llm"), "{msg}");
        assert!(msg.contains("adapter override"), "{msg}");
    }

    #[test]
    fn invalid_model_rejected_before_classification() {
        let s = AgentSettings::default();
        let o = over(None, Some("has space"));
        let err = route_run(&s, Purpose::FollowUp, Some(&o), no_catalog, None).unwrap_err();
        assert!(matches!(err, SettingsError::InvalidModel(_)), "{err:?}");
    }

    // --- Missing key ----------------------------------------------------------

    #[test]
    fn openrouter_route_without_key_fails_fast_and_never_leaks() {
        let s = AgentSettings::default();
        let o = over(None, Some("google/gemini-3-pro"));
        for key in [None, Some(""), Some("   ")] {
            let err = route_run(&s, Purpose::FollowUp, Some(&o), no_catalog, key).unwrap_err();
            assert!(
                matches!(err, SettingsError::OpenRouterKeyMissing(ref m) if m == "google/gemini-3-pro"),
                "{err:?}"
            );
            let msg = err.to_string();
            assert!(msg.contains("Settings"), "actionable: {msg}");
        }
    }

    // --- Bypass precedence -----------------------------------------------------

    #[test]
    fn per_run_adapter_override_bypasses_routing_entirely() {
        // Even an openai-shaped model + a slash model get NO routing and NO env
        // when the escape hatch names an adapter explicitly.
        let s = AgentSettings::default();
        let o = over(Some("claude"), Some("google/gemini-3-pro"));
        let d = route_run(&s, Purpose::FollowUp, Some(&o), no_catalog, Some("k")).unwrap();
        assert_eq!(d.adapter, "claude");
        assert_eq!(d.model, "google/gemini-3-pro");
        assert!(d.env.is_empty(), "manual bypass injects no env");
        assert_eq!(d.tag, RouteTag::Manual);

        let o = over(Some("codex"), Some("claude-sonnet-5"));
        let d = route_run(&s, Purpose::FollowUp, Some(&o), no_catalog, None).unwrap();
        assert_eq!((d.adapter.as_str(), d.tag), ("codex", RouteTag::Manual));
    }

    #[test]
    fn custom_default_adapter_bypasses_routing() {
        // A user-configured harness as default_adapter → routing keeps its
        // hands off (this is what keeps the fake-adapter engine tests, and any
        // real custom-adapter config, byte-identical to pre-routing behavior —
        // including models that would otherwise be unroutable).
        let mut s = AgentSettings::default();
        s.default_adapter = "my-agent".to_owned();
        let o = over(None, Some("totally-custom-llm"));
        let d = route_run(&s, Purpose::FollowUp, Some(&o), no_catalog, None).unwrap();
        assert_eq!(d.adapter, "my-agent");
        assert_eq!(d.model, "totally-custom-llm");
        assert!(d.env.is_empty());
        assert_eq!(d.tag, RouteTag::Manual);
    }

    #[test]
    fn codex_as_default_adapter_still_routes_by_model() {
        // Both built-ins are routing-aware: default_adapter=codex + an
        // anthropic model routes to claude (the model decides, not the
        // default), and vice versa openai models stay on codex.
        let mut s = AgentSettings::default();
        s.default_adapter = "codex".to_owned();
        let d = route_run(&s, Purpose::FollowUp, None, no_catalog, None).unwrap();
        assert_eq!((d.adapter.as_str(), d.tag), ("claude", RouteTag::Anthropic));

        let o = over(None, Some("gpt-5.4-mini"));
        let d = route_run(&s, Purpose::FollowUp, Some(&o), no_catalog, None).unwrap();
        assert_eq!((d.adapter.as_str(), d.tag), ("codex", RouteTag::Openai));
    }

    #[test]
    fn route_tags_are_stable_strings() {
        assert_eq!(RouteTag::Anthropic.as_str(), "anthropic");
        assert_eq!(RouteTag::Openai.as_str(), "openai");
        assert_eq!(RouteTag::Openrouter.as_str(), "openrouter");
        assert_eq!(RouteTag::Local.as_str(), "local");
        assert_eq!(RouteTag::Manual.as_str(), "manual");
    }
}
