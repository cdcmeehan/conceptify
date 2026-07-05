//! Agent-agnostic invocation settings (PRD §5.5, G6, FR-7.1–7.4).
//!
//! This module owns the **data model** and **resolution logic** for the agent
//! adapter layer. It is deliberately split from the process spawner (bead
//! `conceptify-b12.2`), which consumes the [`ResolvedInvocation`] this module
//! produces. Nothing here spawns an agent — the one effectful function,
//! [`resolve_agent_binary`], only runs a cached login-shell `which` lookup.
//!
//! # Why an adapter template (G6)
//!
//! Every headless agent run (follow-ups, in-app asks, artifact updates) goes
//! through a settings-defined command template so nothing is hardcoded — Phase 1
//! ships and tests only the `claude` adapter, but a second adapter (e.g. Codex)
//! can be added *purely via config* with no code change. Per-purpose model
//! config satisfies "don't burn a frontier model on a small sidebar answer."
//!
//! # Storage & defaults philosophy (FR-7.4)
//!
//! Defaults live in **code** ([`AgentSettings::default`]), not in seeded DB
//! rows: a fresh install with `claude` on `PATH` works with zero configuration
//! and no `settings` row present. The single `settings` key `agent_settings`
//! holds a JSON blob written *only once the user overrides something*. On read
//! ([`get_settings`]) a missing row yields the code defaults; a present row is
//! deserialized with **field-level `serde` defaults**, so a blob written by an
//! older app version (missing a field added later) still fills that field from
//! code — overrides merge over defaults rather than replacing them wholesale.
//!
//! # JSON shape (camelCase, matches PRD §5.5)
//!
//! ```jsonc
//! {
//!   "adapters": {
//!     "claude": {
//!       "command": "claude",
//!       "args": ["-p", "{prompt}", "--model", "{model}",
//!                "--permission-mode", "acceptEdits", "--output-format", "stream-json",
//!                "--verbose", "--strict-mcp-config",
//!                "--allowedTools", "Bash", "Edit", "Write",
//!                "--disallowedTools", /* web + mutating-git + project-root writes,
//!                                        see default_adapters() */ "..."],
//!       "cwd": "{project_root}"
//!     }
//!   },
//!   "defaultAdapter": "claude",
//!   "models": { "followUp": "claude-haiku-4-5",
//!               "artifactUpdate": "claude-sonnet-5",
//!               "inAppAsk": "claude-sonnet-5" },
//!   "timeoutSecs": 1800,
//!   "agentBinaryPath": null,
//!   "appearance": "system",
//!   "autoProjectBaseDir": null
//! }
//! ```
//!
//! `appearance` (FR-7.2) is `system`|`light`|`dark`, applied by the app shell.
//! `autoProjectBaseDir` (FR-7.3) is the base dir under which "create a folder
//! for me" (FR-1.2) makes project dirs; `null`/empty means the built-in default
//! `~/Documents/conceptify/projects`.
//!
//! `timeoutSecs` is stored in **seconds** (default 1800 = 30 min, FR-5.3) so the
//! spawner can feed it straight into `Duration::from_secs`. `agentBinaryPath`
//! (FR-7.3) is an absolute-path override; `null`/empty means "resolve via
//! login-shell `which`."
//!
//! # Contract for the spawner (`conceptify-b12.2`)
//!
//! Two calls, in order:
//! ```ignore
//! let settings = settings::get_settings(&conn)?;
//! let inv = settings.resolve(Purpose::FollowUp, project_root, &prompt)?;
//! let program = settings::resolve_agent_binary(&inv.program, settings.agent_binary_path.as_deref())?;
//! // tokio::process::Command::new(program).args(&inv.args).current_dir(&inv.cwd) ...
//! //   — exec'd directly, NO shell: see substitution safety below.
//! let timeout = std::time::Duration::from_secs(settings.timeout_secs);
//! ```
//!
//! # Substitution safety
//!
//! Placeholder substitution ([`resolve`](AgentSettings::resolve)) is **per-arg,
//! whole-string, single-pass** and is *never* shell-interpreted. Each template
//! string in `args` (and `command`/`cwd`) is expanded independently into exactly
//! one output string; there is no whitespace splitting, so a `{prompt}` value
//! containing spaces/quotes/braces/newlines stays a single argv element. The
//! expander scans the *template* once and copies substituted values in verbatim
//! without re-scanning them, so prompt content that happens to contain the
//! literal text `{model}` or `{project_root}` is **not** re-substituted and can
//! never alter the argument structure. Combined with the spawner exec'ing the
//! program directly via `tokio::process` (no shell), this makes adversarial
//! prompt content inert (PRD §9 S3).

// This module is the *invocation seam*: the resolution + binary-lookup half of
// bead `conceptify-b12.1`. Its consumers are the headless spawner
// (`conceptify-b12.2`) and the Settings UI (`conceptify-959.4`), neither of
// which exists yet — so `resolve`, `resolve_agent_binary`, `Purpose`,
// `ResolvedInvocation`, the `SubstCtx`/`expand` internals, the `BINARY_CACHE`,
// and the `BinaryNotFound`/`BinaryLookup` error variants have no in-crate caller
// in this build. The app lib compiles as a cdylib, where such unused items (and
// their private helpers) are reported as dead. Every one of them is exercised by
// this module's unit tests, so none is truly dead; drop this allow once b12.2
// wires the spawner to them. The live storage path (`get_settings`/
// `update_settings`/`AgentSettings`, used by the `get`/`set_agent_settings`
// commands) stays honestly dead-code-checked because it has real callers.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// The `settings` key under which the agent-settings JSON blob is stored. The
/// `settings` table is a shared key/value store (see `db::migrations`); this
/// module namespaces its config under one key so other settings (theme, editor,
/// …) added by other beads never collide.
const SETTINGS_KEY: &str = "agent_settings";

// --- Defaults (single source of truth, shared by `Default` + serde) ---------

/// The built-in `claude` adapter template (PRD §5.5), with the OQ3 permission
/// scoping decided by bead `b12.8` (PRD §12 OQ3, §9 right-sized security).
///
/// # Scoping rationale (verified against claude CLI 2.1.201, headless `-p`)
///
/// **`--permission-mode acceptEdits` alone does NOT work headless**: in print
/// mode only a small safelist of read-only Bash commands is auto-approved;
/// anything else (including `conceptify …`, `mktemp`, `d2`/`dot`/`node`
/// renders) is denied with "command requires approval" — which would break
/// every flow. Hence the explicit allows:
///
/// - `--allowedTools Bash Edit Write` — every flow needs arbitrary Bash (the
///   `conceptify` CLI contract, `mktemp`, diagram renderers) and Write/Edit
///   *outside* the cwd (answer files and artifact working copies live in
///   `mktemp -d` scratch dirs). A fine-grained Bash whitelist
///   (`Bash(conceptify:*)`, …) was rejected: prefix rules break on env
///   assignments/pipes/quoting and every headless denial is a wasted flail —
///   the "prison that breaks the product" §9 warns about.
///
/// Deny rules always win over allows + acceptEdits (verified), so the
/// dangerous-and-unneeded surface is subtracted:
///
/// - `WebFetch` / `WebSearch` — all flows are grounded in local code and the
///   artifact; web tools are pure accident/injection surface here. A denied
///   tool is removed from the toolset entirely (no flailing).
/// - `Bash(git <mutating>:*)` — no flow ever mutates the target repo; denying
///   `commit/push/add/rebase/merge/reset/checkout/switch/restore/stash/clean`
///   keeps agent accidents away from the user's history and working tree
///   while leaving git *reads* (`log`, `diff`, `blame`, `grep`) available for
///   grounding. Prefix matching, hygiene not adversarial containment.
/// - `Edit(/{project_root}/**)` / `Write(/{project_root}/**)` — the target
///   repo is **read-only** in every flow (answer: sidebar only; apply/ask:
///   edits happen in a temp working copy, artifact-dir writes go through the
///   CLI/server). `{project_root}` starts with `/`, so the resolved pattern
///   gains the `//…` absolute-path prefix the permission-rule syntax expects.
///
/// `--strict-mcp-config` (with no `--mcp-config`) keeps the user's personal
/// MCP servers (browser automation, doc fetchers, …) from being spawned into
/// every headless run — surface and startup-latency hygiene.
///
/// **Rejected** (recorded for OQ3): `bypassPermissions` (no containment at
/// all); `--tools` whitelisting (must enumerate every built-in incl.
/// Read/Glob/Grep/Task — brittle across CLI versions, disallows achieve the
/// same subtraction); per-purpose arg overrides such as denying
/// `Bash(conceptify save-artifact:*)` in answer runs (needs a new adapter
/// data-model dimension for an accident that is prompt-forbidden and, being
/// append-only versioning, recoverable); `sandbox-exec`/network firewalling
/// (adversarial-grade, §9 explicitly out of scope).
///
/// Each pattern is its own argv element (the variadic flags accept that, and
/// `Bash(git commit:*)` contains a space, so comma-joining is riskier); the
/// two list flags sit *after* the positional `{prompt}` so they can never
/// swallow it.
fn default_adapters() -> BTreeMap<String, Adapter> {
    let mut m = BTreeMap::new();
    m.insert(
        "claude".to_owned(),
        Adapter {
            command: "claude".to_owned(),
            args: vec![
                "-p".to_owned(),
                "{prompt}".to_owned(),
                "--model".to_owned(),
                "{model}".to_owned(),
                "--permission-mode".to_owned(),
                "acceptEdits".to_owned(),
                "--output-format".to_owned(),
                "stream-json".to_owned(),
                // The claude CLI requires --verbose whenever --print is
                // combined with --output-format=stream-json; without it the
                // process exits 1 immediately and every headless run fails
                // (found live in the M5 checkpoint, bead conceptify-b12.9).
                "--verbose".to_owned(),
                "--strict-mcp-config".to_owned(),
                "--allowedTools".to_owned(),
                "Bash".to_owned(),
                "Edit".to_owned(),
                "Write".to_owned(),
                "--disallowedTools".to_owned(),
                "WebFetch".to_owned(),
                "WebSearch".to_owned(),
                "Bash(git commit:*)".to_owned(),
                "Bash(git push:*)".to_owned(),
                "Bash(git add:*)".to_owned(),
                "Bash(git rebase:*)".to_owned(),
                "Bash(git merge:*)".to_owned(),
                "Bash(git reset:*)".to_owned(),
                "Bash(git checkout:*)".to_owned(),
                "Bash(git switch:*)".to_owned(),
                "Bash(git restore:*)".to_owned(),
                "Bash(git stash:*)".to_owned(),
                "Bash(git clean:*)".to_owned(),
                "Edit(/{project_root}/**)".to_owned(),
                "Write(/{project_root}/**)".to_owned(),
            ],
            cwd: default_cwd(),
        },
    );
    m
}

fn default_default_adapter() -> String {
    "claude".to_owned()
}

/// 30 minutes (FR-5.3), expressed in seconds. Raised from the PRD's original
/// 15-minute default after live QA (bead `conceptify-bc4`): a full in-app-ask
/// authoring loop (research + diagram renders + agent-browser visual
/// self-review) routinely costs 12–20 min, so a 15-min ceiling killed healthy,
/// near-complete runs (a run that finished in ~830 s under an 1800 s ceiling
/// had been SIGKILLed at 900 s the try before). This is only the zero-config
/// default; the value stays user-configurable per FR-5.3. Note the timeout is a
/// *kill ceiling*, not a fixed wait — short follow-up answers still exit in
/// seconds — so a generous global ceiling costs nothing for fast runs and only
/// spares long ones, which is why a per-purpose timeout map was not worth its
/// added settings/type/doc surface (recorded on the bead).
fn default_timeout_secs() -> u64 {
    30 * 60
}

/// A newly-added adapter that omits `cwd` runs in the project root — the only
/// sensible default and what every current flow wants.
fn default_cwd() -> String {
    "{project_root}".to_owned()
}

fn default_follow_up_model() -> String {
    "claude-haiku-4-5".to_owned()
}

fn default_artifact_update_model() -> String {
    "claude-sonnet-5".to_owned()
}

fn default_in_app_ask_model() -> String {
    "claude-sonnet-5".to_owned()
}

/// The built-in auto-project base directory, `~/Documents/conceptify/projects`
/// (FR-7.3 default). `None` if the platform Documents directory can't be
/// resolved (headless/unusual environments) — callers surface that as an error.
/// Uses the same `dirs::document_dir()` root as the artifacts store (§5.6), so
/// auto-created projects sit alongside their artifacts under `~/Documents/conceptify`.
pub fn default_auto_project_base_dir() -> Option<PathBuf> {
    dirs::document_dir().map(|d| d.join("conceptify").join("projects"))
}

// --- Data model -------------------------------------------------------------

/// App appearance preference (FR-7.2). `System` follows the OS
/// `prefers-color-scheme`; `Light`/`Dark` force that scheme in the app shell.
/// Serialized lowercase to match the frontend `"system" | "light" | "dark"`.
/// The artifact iframe keeps its own `prefers-color-scheme` regardless (S2
/// origin isolation — the shell can't reach into it), so `System` is the path
/// where the reading surface and shell always agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

/// One agent invocation template. `args`/`cwd`/`command` may contain the
/// placeholders `{prompt}`, `{model}`, `{project_root}`, substituted at
/// resolution time (see module docs on substitution safety).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Adapter {
    /// The executable to run — a bare name resolved via login-shell `which`
    /// (e.g. `"claude"`), or an absolute path.
    pub command: String,
    /// Argument template list. Each element becomes exactly one argv entry after
    /// whole-string placeholder substitution (never split on whitespace).
    pub args: Vec<String>,
    /// Working-directory template (e.g. `"{project_root}"`).
    #[serde(default = "default_cwd")]
    pub cwd: String,
}

/// Per-purpose model selection (PRD §5.5) so small sidebar answers don't burn a
/// frontier model. Field-level serde defaults let a partial stored blob (e.g.
/// only `followUp` overridden) fill the rest from code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PurposeModels {
    #[serde(default = "default_follow_up_model")]
    pub follow_up: String,
    #[serde(default = "default_artifact_update_model")]
    pub artifact_update: String,
    #[serde(default = "default_in_app_ask_model")]
    pub in_app_ask: String,
}

impl Default for PurposeModels {
    fn default() -> Self {
        Self {
            follow_up: default_follow_up_model(),
            artifact_update: default_artifact_update_model(),
            in_app_ask: default_in_app_ask_model(),
        }
    }
}

impl PurposeModels {
    /// The configured model id for a given invocation purpose.
    pub fn for_purpose(&self, purpose: Purpose) -> &str {
        match purpose {
            Purpose::FollowUp => &self.follow_up,
            Purpose::ArtifactUpdate => &self.artifact_update,
            Purpose::InAppAsk => &self.in_app_ask,
        }
    }
}

/// The invocation purpose, selecting which per-purpose model to use.
///
/// - `FollowUp` — batch sidebar answers (FR-4.6).
/// - `ArtifactUpdate` — apply-to-artifact runs (FR-4.7).
/// - `InAppAsk` — in-app "new thread" question composer (FR-5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    FollowUp,
    ArtifactUpdate,
    InAppAsk,
}

/// Full agent-settings model (PRD §5.5, FR-7.1–7.4). See module docs for the
/// storage/defaults philosophy and JSON shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSettings {
    /// name → adapter template. Phase 1 ships only `"claude"`.
    #[serde(default = "default_adapters")]
    pub adapters: BTreeMap<String, Adapter>,
    /// Which adapter [`resolve`](AgentSettings::resolve) uses. Must be a key of
    /// `adapters` (enforced by [`validate`](AgentSettings::validate)).
    #[serde(default = "default_default_adapter")]
    pub default_adapter: String,
    /// Per-purpose model ids.
    #[serde(default)]
    pub models: PurposeModels,
    /// Agent run timeout in seconds (FR-5.3, default 1800 = 30 min).
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// Absolute-path override for the agent binary (FR-7.3). `None`/empty means
    /// "resolve via login-shell `which`" (see [`resolve_agent_binary`]).
    #[serde(default)]
    pub agent_binary_path: Option<String>,
    /// App appearance (FR-7.2): follow the OS or force light/dark in the shell.
    #[serde(default)]
    pub appearance: Appearance,
    /// Base directory under which "create a folder for me" (FR-1.2/UC6) makes
    /// new project dirs (FR-7.3). `None`/empty means the built-in default
    /// [`default_auto_project_base_dir`]; only stored when the user overrides,
    /// so the zero-config default stays code-side (FR-7.4).
    #[serde(default)]
    pub auto_project_base_dir: Option<String>,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            adapters: default_adapters(),
            default_adapter: default_default_adapter(),
            models: PurposeModels::default(),
            timeout_secs: default_timeout_secs(),
            agent_binary_path: None,
            appearance: Appearance::System,
            auto_project_base_dir: None,
        }
    }
}

/// A fully-substituted invocation ready for the spawner to run. `program` is the
/// adapter's *command* (still a bare name unless it was already absolute) — the
/// spawner resolves it to a real path via [`resolve_agent_binary`], honoring the
/// [`AgentSettings::agent_binary_path`] override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedInvocation {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
}

/// Errors from settings storage / resolution. Command wrappers map these to
/// strings; the spawner matches on them.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("no adapter named '{0}' is configured")]
    UnknownAdapter(String),

    #[error("agent binary '{0}' not found on PATH; set a path override in Settings")]
    BinaryNotFound(String),

    #[error("failed to look up agent binary '{0}': {1}")]
    BinaryLookup(String, String),

    #[error("invalid settings JSON: {0}")]
    Deserialize(#[source] serde_json::Error),

    #[error("could not resolve the default auto-project base directory; set one in Settings")]
    NoAutoProjectBaseDir,

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
}

impl AgentSettings {
    /// Validate cross-field invariants: `default_adapter` must name an existing
    /// adapter (else every [`resolve`](Self::resolve) would fail). Called before
    /// persisting so a broken config never reaches the DB.
    pub fn validate(&self) -> Result<(), SettingsError> {
        if !self.adapters.contains_key(&self.default_adapter) {
            return Err(SettingsError::UnknownAdapter(self.default_adapter.clone()));
        }
        Ok(())
    }

    /// The effective auto-project base dir (FR-7.3): the user override when set
    /// and non-empty, else the built-in default
    /// ([`default_auto_project_base_dir`]). Errors only when neither is
    /// available (no override + no resolvable Documents dir). The path is
    /// returned as-is (not created here); the project-creation domain code
    /// (`crate::projects::create_auto_project`) makes the directory.
    pub fn resolved_auto_project_base_dir(&self) -> Result<PathBuf, SettingsError> {
        if let Some(p) = self
            .auto_project_base_dir
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Ok(PathBuf::from(p));
        }
        default_auto_project_base_dir().ok_or(SettingsError::NoAutoProjectBaseDir)
    }

    /// Resolve an invocation for `purpose` against `project_root` and `prompt`.
    /// Pure and total apart from the `UnknownAdapter` error: it does no I/O and
    /// spawns nothing. Placeholder substitution is whole-string / single-pass /
    /// never shell-interpreted (see module docs).
    pub fn resolve(
        &self,
        purpose: Purpose,
        project_root: &Path,
        prompt: &str,
    ) -> Result<ResolvedInvocation, SettingsError> {
        let adapter = self
            .adapters
            .get(&self.default_adapter)
            .ok_or_else(|| SettingsError::UnknownAdapter(self.default_adapter.clone()))?;

        let model = self.models.for_purpose(purpose);
        // macOS project roots (under ~/Documents/conceptify, §5.6) are UTF-8 in
        // practice; a lossy conversion here only affects a non-UTF-8 cwd path.
        let root = project_root.to_string_lossy();
        let ctx = SubstCtx {
            prompt,
            model,
            project_root: &root,
        };

        Ok(ResolvedInvocation {
            program: expand(&adapter.command, &ctx),
            args: adapter.args.iter().map(|a| expand(a, &ctx)).collect(),
            cwd: expand(&adapter.cwd, &ctx),
        })
    }
}

/// Substitution context: the three placeholder values.
struct SubstCtx<'a> {
    prompt: &'a str,
    model: &'a str,
    project_root: &'a str,
}

impl SubstCtx<'_> {
    fn lookup(&self, name: &str) -> Option<&str> {
        match name {
            "prompt" => Some(self.prompt),
            "model" => Some(self.model),
            "project_root" => Some(self.project_root),
            _ => None,
        }
    }
}

/// Single-pass, whole-string placeholder expansion. Recognized `{name}` tokens
/// are replaced with their value **verbatim**; the scanner then continues in the
/// *template* past the token, never re-examining the inserted value — so a value
/// that itself contains `{...}` is never re-substituted. Unrecognized `{...}`
/// (and unbalanced `{`) are left literal. This function never splits on
/// whitespace: one template string → one output string.
fn expand(template: &str, ctx: &SubstCtx) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];

        if let Some(close) = after.find('}') {
            let name = &after[..close];
            if let Some(value) = ctx.lookup(name) {
                out.push_str(value);
                rest = &after[close + 1..];
                continue;
            }
        }

        // Not a recognized placeholder (unknown name or no closing brace):
        // emit the literal '{' and resume scanning just after it.
        out.push('{');
        rest = after;
    }

    out.push_str(rest);
    out
}

// --- Agent binary resolution (§5.1) -----------------------------------------

/// Cache of resolved binary paths keyed by command name, so the (slow)
/// login-shell lookup runs once per command per process. `OnceLock<Mutex<..>>`
/// gives lazy init without a global constructor; the spawner bead will call
/// [`resolve_agent_binary`] on every run, hitting this cache after the first.
static BINARY_CACHE: OnceLock<Mutex<BTreeMap<String, PathBuf>>> = OnceLock::new();

/// Resolve the executable path for an adapter `command` (§5.1).
///
/// Precedence:
/// 1. A non-empty `override_path` (the FR-7.3 settings override) is returned as
///    given — the user asserts it's correct; a bad path surfaces as a spawn
///    error in the spawner, not here.
/// 2. A `command` that is already absolute (starts with `/`) is returned as-is.
/// 3. Otherwise a login-shell `which` (`zsh -lc 'which <command>'`) resolves it
///    against the user's real `PATH` — GUI apps on macOS inherit a minimal
///    `PATH`, so a plain lookup would miss Homebrew/`~/.local` installs. The
///    result is cached for the process lifetime.
///
/// The `command` is the only value that ever reaches a shell here, and it comes
/// from local trusted settings (never from prompt/comment content); it is
/// additionally single-quoted before interpolation. Prompt content never touches
/// a shell — it flows through [`AgentSettings::resolve`] into the argv array the
/// spawner exec's directly.
pub fn resolve_agent_binary(
    command: &str,
    override_path: Option<&str>,
) -> Result<PathBuf, SettingsError> {
    if let Some(path) = override_path.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(path));
    }

    if command.starts_with('/') {
        return Ok(PathBuf::from(command));
    }

    let cache = BINARY_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(command)
        .cloned()
    {
        return Ok(hit);
    }

    let resolved = login_shell_which(command)?;
    cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(command.to_owned(), resolved.clone());
    Ok(resolved)
}

/// Run `zsh -lc 'which <command>'` and return the resolved absolute path.
/// A non-zero exit, empty output, or a first line that isn't an absolute path
/// (e.g. a zsh alias/function/builtin, or a "not found" message) maps to
/// [`SettingsError::BinaryNotFound`].
fn login_shell_which(command: &str) -> Result<PathBuf, SettingsError> {
    let script = format!("which {}", single_quote(command));
    let output = std::process::Command::new("zsh")
        .args(["-lc", &script])
        .output()
        .map_err(|e| SettingsError::BinaryLookup(command.to_owned(), e.to_string()))?;

    if !output.status.success() {
        return Err(SettingsError::BinaryNotFound(command.to_owned()));
    }

    let first_line = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();

    if first_line.starts_with('/') {
        Ok(PathBuf::from(first_line))
    } else {
        Err(SettingsError::BinaryNotFound(command.to_owned()))
    }
}

/// POSIX single-quote a string for safe interpolation into a shell command:
/// wrap in `'…'`, and turn any embedded `'` into `'\''`.
fn single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

// --- DB plumbing ------------------------------------------------------------

/// Read the agent settings: the stored override blob merged over code defaults,
/// or the pure code defaults when no row exists (FR-7.4 zero-config).
pub fn get_settings(conn: &Connection) -> Result<AgentSettings, SettingsError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [SETTINGS_KEY],
            |row| row.get(0),
        )
        .optional()?;

    match raw {
        None => Ok(AgentSettings::default()),
        Some(json) => serde_json::from_str(&json).map_err(SettingsError::Deserialize),
    }
}

/// Persist the agent settings (validated first, so a config whose
/// `default_adapter` names no adapter never lands in the DB). Written as one
/// upsert statement — SQLite applies it atomically, so a crash mid-write can't
/// corrupt the row (PRD N4).
pub fn update_settings(conn: &Connection, settings: &AgentSettings) -> Result<(), SettingsError> {
    settings.validate()?;
    let json = serde_json::to_string(settings).map_err(SettingsError::Deserialize)?;
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![SETTINGS_KEY, json],
    )?;
    Ok(())
}

/// Delete the stored settings override (FR-7.4 "reset to defaults"): afterwards
/// [`get_settings`] returns the pure code defaults, exactly as a fresh install
/// with no `settings` row. A missing row is a no-op (already at defaults).
pub fn clear_settings(conn: &Connection) -> Result<(), SettingsError> {
    conn.execute("DELETE FROM settings WHERE key = ?1", [SETTINGS_KEY])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_memory_settings_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
        .unwrap();
        conn
    }

    // --- Substitution safety (adversarial) ---------------------------------

    #[test]
    fn expand_substitutes_known_placeholders() {
        let ctx = SubstCtx {
            prompt: "hello",
            model: "claude-x",
            project_root: "/tmp/proj",
        };
        assert_eq!(expand("{prompt}", &ctx), "hello");
        assert_eq!(expand("{model}", &ctx), "claude-x");
        assert_eq!(expand("{project_root}", &ctx), "/tmp/proj");
        assert_eq!(expand("--model={model}", &ctx), "--model=claude-x");
    }

    #[test]
    fn expand_leaves_unknown_and_unbalanced_literal() {
        let ctx = SubstCtx {
            prompt: "p",
            model: "m",
            project_root: "r",
        };
        assert_eq!(expand("{unknown}", &ctx), "{unknown}");
        assert_eq!(expand("a{prompt", &ctx), "a{prompt"); // no closing brace
        assert_eq!(expand("{}", &ctx), "{}");
        assert_eq!(expand("literal", &ctx), "literal");
    }

    #[test]
    fn expand_never_re_substitutes_inserted_values() {
        // A prompt containing the literal text of OTHER placeholders must NOT be
        // re-expanded — the classic sequential-replace injection bug.
        let ctx = SubstCtx {
            prompt: "explain {model} and {project_root} literally",
            model: "SECRET-MODEL",
            project_root: "/SECRET/ROOT",
        };
        assert_eq!(
            expand("{prompt}", &ctx),
            "explain {model} and {project_root} literally"
        );
    }

    #[test]
    fn resolve_keeps_adversarial_prompt_as_one_arg() {
        // Prompt with spaces, quotes, braces, newlines, shell metacharacters, and
        // embedded placeholder-looking text.
        let evil = "a b\"c'd; rm -rf / | $(whoami) `id`\n{model}{project_root}\t--not-a-flag";
        let settings = AgentSettings::default();
        let inv = settings
            .resolve(Purpose::FollowUp, Path::new("/tmp/proj"), evil)
            .unwrap();

        // The claude template puts {prompt} at args[1]; it must be exactly the
        // prompt, verbatim, as a SINGLE element — structure unchanged.
        assert_eq!(inv.program, "claude");
        assert_eq!(inv.args[0], "-p");
        assert_eq!(inv.args[1], evil);
        // The default template has 30 args; nothing was injected/split.
        assert_eq!(inv.args.len(), 30);
        // The model/permission structure is intact and untouched by the prompt.
        assert_eq!(inv.args[2], "--model");
        assert_eq!(inv.args[3], "claude-haiku-4-5");
        assert_eq!(inv.args[5], "acceptEdits");
    }

    #[test]
    fn default_claude_scoping_exact() {
        // OQ3/b12.8: pin the whole default scoping so an accidental template
        // edit is caught. Verified against claude CLI 2.1.201 headless probes:
        // acceptEdits alone denies non-safelisted Bash in -p mode, allows are
        // required, denies win over allows, and `X(/{project_root}/**)`
        // resolves to the `//…` absolute-path rule form.
        let settings = AgentSettings::default();
        let inv = settings
            .resolve(Purpose::ArtifactUpdate, Path::new("/tmp/proj"), "q")
            .unwrap();
        assert_eq!(
            inv.args,
            vec![
                "-p",
                "q",
                "--model",
                "claude-sonnet-5",
                "--permission-mode",
                "acceptEdits",
                "--output-format",
                "stream-json",
                "--verbose",
                "--strict-mcp-config",
                "--allowedTools",
                "Bash",
                "Edit",
                "Write",
                "--disallowedTools",
                "WebFetch",
                "WebSearch",
                "Bash(git commit:*)",
                "Bash(git push:*)",
                "Bash(git add:*)",
                "Bash(git rebase:*)",
                "Bash(git merge:*)",
                "Bash(git reset:*)",
                "Bash(git checkout:*)",
                "Bash(git switch:*)",
                "Bash(git restore:*)",
                "Bash(git stash:*)",
                "Bash(git clean:*)",
                "Edit(//tmp/proj/**)",
                "Write(//tmp/proj/**)",
            ]
        );
    }

    // --- Defaults / storage -------------------------------------------------

    #[test]
    fn defaults_when_db_empty() {
        let conn = in_memory_settings_db();
        let s = get_settings(&conn).unwrap();
        assert_eq!(s, AgentSettings::default());
        assert_eq!(s.default_adapter, "claude");
        assert!(s.adapters.contains_key("claude"));
        assert_eq!(s.models.follow_up, "claude-haiku-4-5");
        assert_eq!(s.models.artifact_update, "claude-sonnet-5");
        assert_eq!(s.models.in_app_ask, "claude-sonnet-5");
        assert_eq!(s.timeout_secs, 1800);
        assert_eq!(s.agent_binary_path, None);
        assert_eq!(s.appearance, Appearance::System);
        assert_eq!(s.auto_project_base_dir, None);
    }

    #[test]
    fn round_trip_through_storage() {
        let conn = in_memory_settings_db();
        let mut s = AgentSettings::default();
        s.models.follow_up = "custom-fast".to_owned();
        s.timeout_secs = 60;
        s.agent_binary_path = Some("/opt/claude".to_owned());
        s.appearance = Appearance::Dark;
        s.auto_project_base_dir = Some("/custom/projects".to_owned());

        update_settings(&conn, &s).unwrap();
        let read = get_settings(&conn).unwrap();
        assert_eq!(read, s);
    }

    #[test]
    fn appearance_serializes_lowercase() {
        // The stored JSON must use the lowercase tags the frontend sends.
        let mut s = AgentSettings::default();
        s.appearance = Appearance::Dark;
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""appearance":"dark""#), "{json}");

        // And a stored lowercase tag deserializes back to the enum.
        let parsed: AgentSettings =
            serde_json::from_str(r#"{"appearance":"light"}"#).unwrap();
        assert_eq!(parsed.appearance, Appearance::Light);
    }

    #[test]
    fn resolved_auto_project_base_dir_prefers_override() {
        let mut s = AgentSettings::default();
        // A whitespace-only override is treated as "unset" → the built-in default.
        s.auto_project_base_dir = Some("   ".to_owned());
        assert_eq!(
            s.resolved_auto_project_base_dir().unwrap(),
            default_auto_project_base_dir().unwrap(),
        );

        // A real override wins and is trimmed.
        s.auto_project_base_dir = Some("  /my/projects  ".to_owned());
        assert_eq!(
            s.resolved_auto_project_base_dir().unwrap(),
            PathBuf::from("/my/projects"),
        );
    }

    #[test]
    fn default_auto_project_base_dir_ends_in_conceptify_projects() {
        // On any host with a resolvable Documents dir (macOS test env), the
        // default lands under ~/Documents/conceptify/projects.
        let dir = default_auto_project_base_dir().expect("documents dir on test host");
        assert!(dir.ends_with("conceptify/projects"), "{}", dir.display());
    }

    #[test]
    fn partial_override_merges_over_defaults() {
        // A blob that overrides only followUp — every other field must fall back
        // to the code default (forward/back compat).
        let conn = in_memory_settings_db();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('agent_settings', ?1)",
            [r#"{"models":{"followUp":"only-this"}}"#],
        )
        .unwrap();

        let s = get_settings(&conn).unwrap();
        assert_eq!(s.models.follow_up, "only-this");
        // Untouched fields still default:
        assert_eq!(s.models.artifact_update, "claude-sonnet-5");
        assert_eq!(s.models.in_app_ask, "claude-sonnet-5");
        assert_eq!(s.default_adapter, "claude");
        assert!(s.adapters.contains_key("claude"));
        assert_eq!(s.timeout_secs, 1800);
        // Fields added after this blob shape still fill from code defaults.
        assert_eq!(s.appearance, Appearance::System);
        assert_eq!(s.auto_project_base_dir, None);
    }

    #[test]
    fn adapter_missing_cwd_defaults_to_project_root() {
        let conn = in_memory_settings_db();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('agent_settings', ?1)",
            [r#"{"adapters":{"claude":{"command":"claude","args":["-p","{prompt}"]}}}"#],
        )
        .unwrap();
        let s = get_settings(&conn).unwrap();
        assert_eq!(s.adapters["claude"].cwd, "{project_root}");
    }

    // --- Per-purpose model resolution --------------------------------------

    #[test]
    fn per_purpose_model_selects_correct_arg() {
        let settings = AgentSettings::default();
        let root = Path::new("/tmp/proj");

        let follow = settings.resolve(Purpose::FollowUp, root, "q").unwrap();
        let artifact = settings.resolve(Purpose::ArtifactUpdate, root, "q").unwrap();
        let ask = settings.resolve(Purpose::InAppAsk, root, "q").unwrap();

        assert_eq!(follow.args[3], "claude-haiku-4-5");
        assert_eq!(artifact.args[3], "claude-sonnet-5");
        assert_eq!(ask.args[3], "claude-sonnet-5");
    }

    #[test]
    fn swapping_purpose_model_changes_command() {
        let mut settings = AgentSettings::default();
        settings.models.follow_up = "swapped-model".to_owned();
        let inv = settings
            .resolve(Purpose::FollowUp, Path::new("/tmp/proj"), "q")
            .unwrap();
        assert_eq!(inv.args[3], "swapped-model");
    }

    #[test]
    fn cwd_placeholder_expands_to_project_root() {
        let settings = AgentSettings::default();
        let inv = settings
            .resolve(Purpose::FollowUp, Path::new("/some/where/proj"), "q")
            .unwrap();
        assert_eq!(inv.cwd, "/some/where/proj");
    }

    // --- Adapter lookup / fallback -----------------------------------------

    #[test]
    fn unknown_default_adapter_errors() {
        let mut settings = AgentSettings::default();
        settings.default_adapter = "does-not-exist".to_owned();
        let err = settings
            .resolve(Purpose::FollowUp, Path::new("/tmp"), "q")
            .unwrap_err();
        assert!(matches!(err, SettingsError::UnknownAdapter(_)));
        // validate() also rejects it, so update_settings won't persist it.
        assert!(settings.validate().is_err());
    }

    #[test]
    fn second_adapter_via_config_only() {
        // Prove a new adapter needs no code change: add "codex", point
        // defaultAdapter at it, and resolution uses its command/args template.
        let mut settings = AgentSettings::default();
        settings.adapters.insert(
            "codex".to_owned(),
            Adapter {
                command: "codex".to_owned(),
                args: vec![
                    "exec".to_owned(),
                    "--model".to_owned(),
                    "{model}".to_owned(),
                    "{prompt}".to_owned(),
                ],
                cwd: "{project_root}".to_owned(),
            },
        );
        settings.default_adapter = "codex".to_owned();
        settings.models.follow_up = "gpt-x".to_owned();
        assert!(settings.validate().is_ok());

        let inv = settings
            .resolve(Purpose::FollowUp, Path::new("/tmp/proj"), "prompt text")
            .unwrap();
        assert_eq!(inv.program, "codex");
        assert_eq!(inv.args, vec!["exec", "--model", "gpt-x", "prompt text"]);
        assert_eq!(inv.cwd, "/tmp/proj");
    }

    // --- Agent binary resolution -------------------------------------------

    #[test]
    fn binary_override_wins_and_is_trimmed() {
        let path = resolve_agent_binary("claude", Some("  /custom/bin/claude  ")).unwrap();
        assert_eq!(path, PathBuf::from("/custom/bin/claude"));
    }

    #[test]
    fn empty_override_is_ignored() {
        // An empty/whitespace override falls through to command resolution; an
        // absolute command is returned directly (no shell needed).
        let path = resolve_agent_binary("/usr/bin/env", Some("   ")).unwrap();
        assert_eq!(path, PathBuf::from("/usr/bin/env"));
    }

    #[test]
    fn absolute_command_returned_directly() {
        let path = resolve_agent_binary("/opt/homebrew/bin/claude", None).unwrap();
        assert_eq!(path, PathBuf::from("/opt/homebrew/bin/claude"));
    }

    #[test]
    fn missing_binary_reports_not_found() {
        // A command that cannot exist on PATH → BinaryNotFound (deterministic;
        // does not depend on what IS installed).
        let err = resolve_agent_binary("conceptify-no-such-binary-zzz", None).unwrap_err();
        assert!(matches!(err, SettingsError::BinaryNotFound(_)));
    }

    #[test]
    fn single_quote_escapes_embedded_quote() {
        assert_eq!(single_quote("a'b"), "'a'\\''b'");
        assert_eq!(single_quote("claude"), "'claude'");
    }

    // --- Real-migration integration ----------------------------------------

    /// Exercises get/update against the *real* migration output (`db::init_at`
    /// runs the full `migrations()` chain, including the M0 `SETTINGS` table),
    /// proving the shipped schema matches what this module's SQL expects. The
    /// unit tests above use a hand-written in-memory `settings` table; this one
    /// closes the gap the same way the threads/comments modules do in `lib.rs`.
    #[test]
    fn settings_round_trip_against_real_migrations() {
        let db_path = std::env::temp_dir().join(format!(
            "conceptify-test-settings-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let db_handle = crate::db::init_at(&db_path).expect("test db should init and migrate");
        {
            let conn = db_handle.lock().unwrap();

            // Empty DB → code defaults.
            assert_eq!(get_settings(&conn).unwrap(), AgentSettings::default());

            // Persist an override and read it back through the real schema.
            let mut s = AgentSettings::default();
            s.timeout_secs = 42;
            s.agent_binary_path = Some("/opt/claude".to_owned());
            update_settings(&conn, &s).unwrap();
            assert_eq!(get_settings(&conn).unwrap(), s);

            // Reset (FR-7.4): clearing the row returns to pure code defaults.
            clear_settings(&conn).unwrap();
            assert_eq!(get_settings(&conn).unwrap(), AgentSettings::default());
        }

        drop(db_handle);
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
        let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
    }
}
