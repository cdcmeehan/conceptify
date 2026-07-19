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
//! through a settings-defined command template so nothing is hardcoded — the
//! built-ins are `claude` (the default) and `codex` (bead `conceptify-e7m.2`;
//! selected per-run via [`RunOverride`] or, later, provider routing —
//! bead `conceptify-e7m.7`), and further adapters can still be added *purely
//! via config* with no code change. Per-purpose model config satisfies "don't
//! burn a frontier model on a small sidebar answer."
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
//!                "--allowedTools", "Bash", "Edit", "Write", "Read", "Glob", "Grep",
//!                "--disallowedTools", /* web + mutating-git + project-root writes,
//!                                        see default_adapters() */ "..."],
//!       "cwd": "{project_root}"
//!     },
//!     "codex": {
//!       "command": "codex",
//!       "args": ["exec", "--model", "{model}", "--sandbox", "workspace-write",
//!                "-c", "sandbox_workspace_write.network_access=true",
//!                "--skip-git-repo-check", "--ephemeral", "--ignore-user-config",
//!                "--color", "never", "--", "{prompt}"],
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

/// The `settings` key holding the OpenRouter API key (bead `conceptify-e7m.7`),
/// stored as a **separate row** — deliberately NOT a field of [`AgentSettings`].
///
/// # Key storage decision (recorded per the bead): settings row, not Keychain
///
/// macOS Keychain (via the `security-framework` or `keyring` crate) was
/// evaluated and rejected for this app:
/// - **Cost:** a new native dependency plus real dev-loop friction — an ad-hoc
///   signed debug binary changes identity on every rebuild, so Keychain access
///   re-prompts constantly under `just dev`, and tests would need a live
///   keychain session (CI/headless pain).
/// - **Benefit under our threat model:** marginal. PRD §9 scopes security to
///   *containment and hygiene, not adversarial hardening*; the DB already sits
///   under `~/Library/Application Support` with user-only file permissions —
///   the same local-user boundary the Keychain item would effectively have
///   once the app is authorized to read it non-interactively.
///
/// The hygiene obligations that DO matter are enforced structurally instead:
/// the key lives outside the `agent_settings` blob, so the whole
/// `get_agent_settings`/`set_agent_settings`/`get_agent_options` surface that
/// reaches the frontend can never carry it (nothing to mask — the type doesn't
/// contain it); the frontend gets only a has-key boolean and a set command; and
/// the run engine keeps it out of every log line, event payload, error string,
/// and run row (test-proven in `runs::tests`). Revisit Keychain only if the app
/// ever syncs its DB off-machine.
const OPENROUTER_KEY_SETTINGS_KEY: &str = "openrouter_api_key";
const LOCAL_ENDPOINT_KEY_SETTINGS_KEY: &str = "local_endpoint_api_key";

/// The `settings` key holding the HeyGen API key (video epic conceptify-z9y,
/// bead z9y.4), stored as a **separate write-only row** — deliberately NOT a
/// field of [`AgentSettings`], mirroring the OpenRouter-key pattern above
/// (including the recorded Keychain-vs-settings-row decision, which applies
/// unchanged). Structural no-leak guarantee: the raw key is read only by the
/// server-side render-job code in `crate::heygen` / `server::video_routes`;
/// every frontend/CLI surface learns at most the presence boolean from
/// [`has_heygen_api_key`]. `reset_agent_settings` leaves the key intact.
const HEYGEN_KEY_SETTINGS_KEY: &str = "heygen_api_key";

/// Preferred HeyGen avatar (look) id, used when an avatar-render request omits
/// `avatarId` (bead z9y.4). A plain namespaced string row — not a secret, no
/// write-only handling. Absent row = no default (requests must then pass an
/// explicit id; `conceptify list-avatars` discovers valid ids).
const HEYGEN_DEFAULT_AVATAR_SETTINGS_KEY: &str = "heygen.default_avatar_id";

/// Preferred HeyGen voice id, used when an avatar-render request omits
/// `voiceId` (bead z9y.4). Absent row = no default; HeyGen then uses the
/// avatar's own default voice.
const HEYGEN_DEFAULT_VOICE_SETTINGS_KEY: &str = "heygen.default_voice_id";

/// The `settings` key holding the chosen explanation-artifact theme (epic
/// conceptify-89k, bead 89k.2), stored as a **separate namespaced row** —
/// deliberately NOT a field of [`AgentSettings`], mirroring the OpenRouter-key
/// pattern above. Two reasons for the separate row: (1) `reset_agent_settings`
/// (FR-7.4) must leave the theme choice intact, exactly as it leaves the API
/// key; (2) the skill/CLI read it at authoring time without deserializing (or
/// depending on the shape of) the whole agent-settings blob. The value is one of
/// the [`ArtifactTheme`] wire ids; an absent row means `manuscript` (the default,
/// byte-identical to the current scaffold — see `skill/design-system.md`).
const ARTIFACT_THEME_SETTINGS_KEY: &str = "artifact.theme";

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
/// - `--allowedTools Bash Edit Write Read Glob Grep` — every flow needs
///   arbitrary Bash (the `conceptify` CLI contract, `mktemp`, diagram
///   renderers) and Write/Edit *outside* the cwd (answer files and artifact
///   working copies live in `mktemp -d` scratch dirs). Read/Glob/Grep are
///   read-only and strictly weaker than the already-allowed Bash (`cat`,
///   `find`, `grep`), so denying them added zero containment while costing
///   real turns: every skill-file/PNG Read was permission-denied, agents
///   fell back to `cat`-probing, and image review was impossible (found
///   live on bead conceptify-pri — a compact ask burned ~99% of 8 min on
///   35 permission-friction turns). A fine-grained Bash whitelist
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
///
/// # The `codex` adapter (bead `conceptify-e7m.2`)
///
/// Every flag below was verified live against **codex-cli 0.142.0**
/// (`codex --help` / `codex exec --help` + headless probes on this machine),
/// never assumed from prior knowledge:
///
/// - `exec` — the non-interactive mode; its approval policy is implicitly
///   `never` (`-a/--ask-for-approval` is not an `exec` flag, and the exec
///   banner prints `approval: never`), so sandbox denials are returned to the
///   model instead of hanging a headless run waiting for a human.
/// - `--sandbox workspace-write` — the scoping core, and the OQ3 equivalent.
///   Verified: writable roots are the working dir (`{project_root}`), `/tmp`
///   and `$TMPDIR` (banner: `sandbox: workspace-write [workdir, /tmp,
///   $TMPDIR]`; a `$HOME` write was denied, `mktemp -d` scratch writes
///   succeeded). That is *kernel-enforced* (Seatbelt) filesystem containment —
///   in that one dimension stronger than the claude template, whose allowed
///   `Bash` can technically write anywhere and whose repo protection is
///   tool-rule hygiene. The repo itself is writable here (claude denies
///   `Edit`/`Write` on `{project_root}`): accepted as the bead's
///   "repo-writable" level — every flow's prompt still forbids repo writes,
///   and versioned artifacts/comments live outside the repo entirely.
/// - `-c sandbox_workspace_write.network_access=true` — **required**.
///   Verified: with the Seatbelt properly applied, `workspace-write` denies
///   ALL network egress by default — loopback connects are refused, DNS
///   fails, even local socket *binds* get `EPERM` — which would break every
///   flow (the `conceptify` CLI reports back over `127.0.0.1`). With this
///   key set, loopback and external egress both work (verified live). The
///   resulting open-network posture matches the claude template's effective
///   posture (its dedicated `WebFetch`/`WebSearch` tools are denied but its
///   allowed `Bash` can `curl` freely); codex's native `web_search` tool is
///   opt-in via `--search`, which we do not pass — parity with the claude
///   `WebFetch`/`WebSearch` denies. A loopback-only mode does not exist on
///   0.142.0 (`experimental_network.*` is unstable and out of scope, §9).
///
///   **Measurement hazard (recorded so it is never re-litigated):** codex's
///   Seatbelt cannot nest inside another sandbox. Probes launched from an
///   already-sandboxed shell (e.g. a coding agent's) observe a DEGRADED codex
///   sandbox and wrongly conclude the network is open and the key unenforced
///   — this bead's first probe round did exactly that. All flags here were
///   re-verified from an unsandboxed parent, which is how the real app
///   spawns agents.
/// - **Mutating git** — codex 0.142.0 has no per-command deny (verified:
///   `git commit` succeeds under `workspace-write`; execpolicy `.rules` files
///   are user/project-scoped and inappropriate to ship into target repos), so
///   unlike the claude template's `Bash(git …:*)` denies this stays
///   **prompt-enforced only**. Recorded as the residual scoping difference.
/// - `--skip-git-repo-check` — `exec` refuses to run outside a git repo
///   without it; Conceptify project roots are not guaranteed to be repos and
///   the claude adapter imposes no such restriction.
/// - `--ephemeral` — headless runs must not pile session files into
///   `~/.codex/sessions`; the full transcript already lands in the run log.
/// - `--ignore-user-config` — the analog of claude's `--strict-mcp-config`,
///   necessarily broader: the user's `~/.codex/config.toml` would otherwise
///   spawn personal MCP servers, plugins, and `notify` hooks (observed live: a
///   GUI notifier binary fired per turn) into every headless run. Auth is
///   unaffected (it comes from `CODEX_HOME`; verified live) and CLI `-c`
///   overrides still apply on top.
/// - `--color never` — run logs must never contain ANSI escapes; piped stdout
///   would disable color anyway (`auto`), this pins it deterministically.
/// - `--` before `{prompt}` — `exec` has subcommands (`resume`, `review`) and
///   takes the prompt positionally; the separator guarantees the prompt is
///   never parsed as a flag or subcommand whatever its first token is.
///
/// **Output parsing decision (recorded per the bead):** plain stdout
/// passthrough — deliberately NOT `--json`. Verified stream shape: `codex
/// exec` writes the human-readable transcript (banner, `exec`/`codex`
/// blocks, token counts) to **stderr** and only the agent's final message to
/// **stdout**. The run engine already handles that with zero code change:
/// stderr lines land in the run log as `[err]` (full transcript preserved for
/// FR-4.8/FR-5.3 log-tail surfacing, human-readable), stdout lines degrade in
/// `classify_line` to `kind: "output"` run-progress events, and the frontend
/// shows the elapsed clock + log tail without claude-style progress kinds.
/// `--json` (JSONL events) was evaluated and rejected for v1: the event
/// schema is experimental/unversioned on 0.142.0, and JSONL in the log would
/// make the failure log-tail unreadable. A structured codex progress mode is
/// filed as a follow-up bead.
///
/// **Merge behavior (bead `conceptify-e7m.7`):** a stored settings blob that
/// already contains an `adapters` map replaces this map wholesale on
/// *deserialize* (field-level serde defaults do not merge inside the map), so
/// [`get_settings`] injects any missing built-in adapter **after**
/// deserializing — a blob written before `codex` existed still yields both
/// built-ins. Stored entries always win: a user override of a built-in key and
/// any user-defined adapters are preserved verbatim. Consequence: built-ins
/// cannot be *deleted* via config, only overridden — which provider routing
/// relies on (the `claude`/`codex` keys are always resolvable).
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
                // Emits `stream_event` / `content_block_delta` lines while
                // Claude is composing so answer-mode UI can show an ephemeral
                // live draft. Confirmed in Claude Code 2.1.207's CLI help.
                "--include-partial-messages".to_owned(),
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
                "Read".to_owned(),
                "Glob".to_owned(),
                "Grep".to_owned(),
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
    m.insert(
        "codex".to_owned(),
        Adapter {
            command: "codex".to_owned(),
            args: vec![
                "exec".to_owned(),
                "--model".to_owned(),
                "{model}".to_owned(),
                "--sandbox".to_owned(),
                "workspace-write".to_owned(),
                "-c".to_owned(),
                "sandbox_workspace_write.network_access=true".to_owned(),
                "--skip-git-repo-check".to_owned(),
                "--ephemeral".to_owned(),
                "--ignore-user-config".to_owned(),
                "--color".to_owned(),
                "never".to_owned(),
                "--".to_owned(),
                "{prompt}".to_owned(),
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

/// Provider suites enabled out of the box for the model catalog (epic
/// conceptify-e7m, bead e7m.6): Anthropic + OpenAI — the two natively-routable
/// families. Every other family (runnable via OpenRouter) is opt-in through the
/// Settings suite toggles. Fully-qualified `BTreeSet` so this addition touches
/// no import line.
fn default_enabled_providers() -> std::collections::BTreeSet<String> {
    ["anthropic".to_owned(), "openai".to_owned()]
        .into_iter()
        .collect()
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

/// The chosen explanation-artifact theme (epic conceptify-89k, bead 89k.1
/// design record in `skill/design-system.md`). Each variant names a complete
/// `@cfy:tokens` palette override plus a small set of component rules; the type
/// scale, spacing, and rhythm are shared. `Manuscript` is the default and is
/// byte-identical to the current scaffold, so an absent settings row resolves to
/// it (`#[default]`). Serialized lowercase to match the single wire id the CLI
/// (`conceptify status` `artifactTheme`), the `GET /settings/display` endpoint,
/// and the Settings UI all exchange. The CSS theme blocks + skill stamping that
/// consume this value are the downstream integration bead (89k.3); this type is
/// only the settings-plumbing identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactTheme {
    #[default]
    Manuscript,
    Blueprint,
    Sketchbook,
}

impl ArtifactTheme {
    /// The canonical lowercase wire id — what is stored, returned to the CLI, and
    /// shown in the frontend.
    pub fn as_str(&self) -> &'static str {
        match self {
            ArtifactTheme::Manuscript => "manuscript",
            ArtifactTheme::Blueprint => "blueprint",
            ArtifactTheme::Sketchbook => "sketchbook",
        }
    }

    /// Parse a wire theme id, rejecting anything outside the known set with a
    /// user-facing [`SettingsError::InvalidTheme`] (the validation the write path
    /// requires). Surrounding whitespace is trimmed, but — unlike a lenient parse
    /// — an unknown id is never silently coerced to the default.
    pub fn parse(s: &str) -> Result<Self, SettingsError> {
        match s.trim() {
            "manuscript" => Ok(ArtifactTheme::Manuscript),
            "blueprint" => Ok(ArtifactTheme::Blueprint),
            "sketchbook" => Ok(ArtifactTheme::Sketchbook),
            other => Err(SettingsError::InvalidTheme(other.to_owned())),
        }
    }
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

/// One Anthropic-compatible local gateway. Model ids are user-entered because
/// many Ollama/vLLM/LiteLLM installations do not expose discovery publicly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LocalEndpoint {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub models: Vec<String>,
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

/// Configurable provider-pool capacity for the durable run scheduler
/// (`docs/concurrency-policy.md`). The map is deliberately keyed by arbitrary
/// strings: routing may add a provider or a local endpoint without requiring a
/// settings-schema or UI-layout change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunConcurrency {
    /// Capacity used when no explicit pool entry exists.
    #[serde(default = "default_run_pool_limit")]
    pub default: usize,
    /// Stable provider-pool key → maximum simultaneous child processes.
    #[serde(default = "default_run_pool_limits")]
    pub pools: BTreeMap<String, usize>,
}

fn default_run_pool_limit() -> usize {
    1
}

fn default_run_pool_limits() -> BTreeMap<String, usize> {
    BTreeMap::from([
        ("anthropic".to_owned(), 2),
        ("openai".to_owned(), 2),
        ("openrouter".to_owned(), 3),
        // An explicit adapter bypass has no trustworthy upstream identity, so
        // share one conservative pool unless the user configures otherwise.
        ("manual".to_owned(), 1),
    ])
}

impl Default for RunConcurrency {
    fn default() -> Self {
        Self {
            default: default_run_pool_limit(),
            pools: default_run_pool_limits(),
        }
    }
}

impl RunConcurrency {
    pub fn limit_for(&self, pool: &str) -> usize {
        self.pools.get(pool).copied().unwrap_or(self.default)
    }
}

/// Full agent-settings model (PRD §5.5, FR-7.1–7.4). See module docs for the
/// storage/defaults philosophy and JSON shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSettings {
    /// name → adapter template. Built-ins: `"claude"` (default) and `"codex"`
    /// (bead `conceptify-e7m.2`); more can be added via config alone.
    #[serde(default = "default_adapters")]
    pub adapters: BTreeMap<String, Adapter>,
    /// Which adapter [`resolve`](AgentSettings::resolve) uses. Must be a key of
    /// `adapters` (enforced by [`validate`](AgentSettings::validate)).
    #[serde(default = "default_default_adapter")]
    pub default_adapter: String,
    /// Per-purpose model ids.
    #[serde(default)]
    pub models: PurposeModels,
    #[serde(default)]
    pub local_endpoint: Option<LocalEndpoint>,
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
    /// Enabled provider suites for the live model catalog (epic conceptify-e7m,
    /// bead e7m.6). The catalog API returns only models whose provider is in this
    /// set; the Settings UI (bead e7m.3) toggles membership. Defaults to
    /// Anthropic + OpenAI. A `BTreeSet` for natural dedup + deterministic order.
    #[serde(default = "default_enabled_providers")]
    pub enabled_providers: std::collections::BTreeSet<String>,
    /// Global execution capacity by provider-pool key. Generic keyed data keeps
    /// scheduler configuration independent of the set of providers shown by
    /// the current UI.
    #[serde(default)]
    pub run_concurrency: RunConcurrency,
    /// Opt-in native completion/attention notifications. Permission is requested
    /// by the frontend only when the user enables this setting; the default
    /// in-app activity badge requires no OS permission.
    #[serde(default)]
    pub system_notifications: bool,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            adapters: default_adapters(),
            default_adapter: default_default_adapter(),
            models: PurposeModels::default(),
            local_endpoint: None,
            timeout_secs: default_timeout_secs(),
            agent_binary_path: None,
            appearance: Appearance::System,
            auto_project_base_dir: None,
            enabled_providers: default_enabled_providers(),
            run_concurrency: RunConcurrency::default(),
            system_notifications: false,
        }
    }
}

/// An optional per-run override of the adapter and/or model, layered over the
/// configured defaults for a **single** invocation without mutating stored
/// settings (epic `conceptify-e7m`). Fallback chain, per field independently:
/// explicit override → per-purpose model ([`PurposeModels::for_purpose`]) /
/// [`AgentSettings::default_adapter`]. A `{model}`-only override keeps the
/// default adapter; an `{adapter}`-only override keeps the per-purpose model;
/// an empty override (both `None`) is byte-identical to no override at all.
///
/// **Model-centric** by design (epic e7m rescope): `model` is the primary
/// user-facing choice; `adapter` is the advanced escape hatch. Provider-routed
/// execution (bead `conceptify-e7m.7`) normally *derives* the adapter from the
/// chosen model's provider — this raw `{adapter}` form deliberately bypasses
/// that routing. No routing logic lives here: [`resolve_with_override`]
/// stays pure and only substitutes the selected adapter/model.
///
/// Serialized camelCase for the Tauri command surface (both fields are single
/// words, so the wire shape is `{ "adapter": …, "model": … }`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOverride {
    /// Adapter key to use instead of `default_adapter`. Must name an existing
    /// adapter (validated in [`AgentSettings::select`]); the escape hatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    /// Model id to use instead of the per-purpose default. Validated
    /// ([`validate_model`]): non-empty, no whitespace/control characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl RunOverride {
    /// True when neither field is set — indistinguishable from "no override".
    /// The run engine stores `NULL` (not an empty `{}` blob) on the run row in
    /// this case, so retry of an override-free run re-derives current defaults.
    pub fn is_empty(&self) -> bool {
        self.adapter.is_none() && self.model.is_none()
    }
}

/// Validate an explicit per-run model override. Structural argv safety is
/// already guaranteed by whole-string substitution (a model becomes exactly one
/// argv element regardless of its bytes — see the module docs), so this is not a
/// shell-injection guard; it rejects values that are never a real model id —
/// empty/whitespace-only, or carrying embedded whitespace/control characters —
/// so a bad override fails fast with a clear error instead of spawning a doomed
/// agent. Mirrors how [`AgentSettings::validate`] rejects an unknown adapter key
/// before use. Model ids with `/`, `.`, `-`, `:` (OpenRouter/LiteLLM shapes)
/// stay valid.
pub(crate) fn validate_model(model: &str) -> Result<(), SettingsError> {
    if model.trim().is_empty() || model.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(SettingsError::InvalidModel(model.to_owned()));
    }
    Ok(())
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

    #[error(
        "invalid model override '{0}': a model must be non-empty and contain no \
         whitespace or control characters"
    )]
    InvalidModel(String),

    /// The submitted OpenRouter API key is structurally invalid. Deliberately
    /// carries NO payload: an error string must never echo (even a malformed)
    /// secret back to logs or the UI.
    #[error(
        "invalid OpenRouter API key: it must be non-empty and contain no \
         whitespace or control characters"
    )]
    InvalidApiKey,

    #[error("invalid local endpoint API key: it must contain no whitespace or control characters")]
    InvalidLocalApiKey,

    /// The submitted HeyGen API key is structurally invalid. Like
    /// [`SettingsError::InvalidApiKey`], deliberately carries NO payload: an
    /// error string must never echo (even a malformed) secret back to logs or
    /// the UI.
    #[error(
        "invalid HeyGen API key: it must be non-empty and contain no \
         whitespace or control characters"
    )]
    InvalidHeygenApiKey,

    /// A HeyGen avatar/voice default id is structurally invalid (these are
    /// short opaque tokens; whitespace/control characters mean a paste
    /// accident). Not a secret, so the field name is echoed for clarity.
    #[error("invalid HeyGen {0} id: it must contain no whitespace or control characters")]
    InvalidHeygenId(&'static str),

    #[error("invalid local model endpoint: {0}")]
    InvalidLocalEndpoint(String),

    #[error("unknown artifact theme '{0}': choose one of manuscript, blueprint, sketchbook")]
    InvalidTheme(String),

    /// A run routed via OpenRouter (bead `conceptify-e7m.7`) but no key is
    /// stored. Raised BEFORE any run row exists (FR-4.9 guard freed).
    #[error(
        "model '{0}' runs via OpenRouter, but no OpenRouter API key is \
         configured — add one in Settings"
    )]
    OpenRouterKeyMissing(String),

    /// Provider routing (bead `conceptify-e7m.7`) could not derive an execution
    /// path for a model id. The second field is a pre-formatted reason clause
    /// (e.g. why the derived provider has no runner, or that no provider could
    /// be derived at all). Fail fast with an actionable message instead of
    /// guessing a route.
    #[error(
        "cannot route model '{0}': {1}. Pick a model from the catalog, use its \
         OpenRouter id ('vendor/model'), or set an explicit adapter override"
    )]
    UnroutableModel(String, String),

    #[error("agent binary '{0}' not found on PATH; set a path override in Settings")]
    BinaryNotFound(String),

    #[error("failed to look up agent binary '{0}': {1}")]
    BinaryLookup(String, String),

    #[error("invalid settings JSON: {0}")]
    Deserialize(#[source] serde_json::Error),

    #[error("invalid run concurrency settings: {0}")]
    InvalidRunConcurrency(String),

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
        if let Some(endpoint) = &self.local_endpoint {
            let url = reqwest::Url::parse(endpoint.base_url.trim()).map_err(|_| {
                SettingsError::InvalidLocalEndpoint("base URL must be an absolute http(s) URL".into())
            })?;
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() || url.password().is_some() || !url.username().is_empty() {
                return Err(SettingsError::InvalidLocalEndpoint("base URL must be an absolute http(s) URL without embedded credentials".into()));
            }
            if endpoint.models.is_empty() {
                return Err(SettingsError::InvalidLocalEndpoint("add at least one model id".into()));
            }
            for model in &endpoint.models { validate_model(model)?; }
        }
        if self.run_concurrency.default == 0 {
            return Err(SettingsError::InvalidRunConcurrency(
                "default capacity must be at least 1".to_owned(),
            ));
        }
        if let Some((pool, limit)) = self
            .run_concurrency
            .pools
            .iter()
            .find(|(pool, limit)| pool.trim().is_empty() || **limit == 0)
        {
            let reason = if pool.trim().is_empty() {
                "provider pool keys must not be blank".to_owned()
            } else {
                format!("provider pool '{pool}' capacity must be at least 1 (got {limit})")
            };
            return Err(SettingsError::InvalidRunConcurrency(reason));
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

    /// Resolve an invocation for `purpose` against `project_root` and `prompt`,
    /// using the configured defaults (no per-run override). Thin wrapper over
    /// [`resolve_with_override`] with `None` — so the no-override path is
    /// literally the override path with an empty override, which is what makes
    /// "omitted override == current behavior byte-identical" true by
    /// construction (the exact-string arg tests exercise it). Pure and total
    /// apart from the `UnknownAdapter` error: no I/O, spawns nothing.
    pub fn resolve(
        &self,
        purpose: Purpose,
        project_root: &Path,
        prompt: &str,
    ) -> Result<ResolvedInvocation, SettingsError> {
        self.resolve_with_override(purpose, project_root, prompt, None)
    }

    /// Resolve an invocation for `purpose`, honoring an optional per-run
    /// [`RunOverride`] (epic `conceptify-e7m`). Fallback chain per field:
    /// explicit override → per-purpose model / `default_adapter`. Validates the
    /// override (unknown adapter → [`SettingsError::UnknownAdapter`], bad model
    /// → [`SettingsError::InvalidModel`]) before substituting. Placeholder
    /// substitution is whole-string / single-pass / never shell-interpreted, so
    /// the selected model reaches `{model}` (and adapter command → `program`)
    /// verbatim as single argv elements (see module docs). Pure — no routing.
    pub fn resolve_with_override(
        &self,
        purpose: Purpose,
        project_root: &Path,
        prompt: &str,
        over: Option<&RunOverride>,
    ) -> Result<ResolvedInvocation, SettingsError> {
        let (_, adapter, model) = self.select(purpose, over)?;
        // macOS project roots (under ~/Documents/conceptify, §5.6) are UTF-8 in
        // practice; a lossy conversion here only affects a non-UTF-8 cwd path.
        let root = project_root.to_string_lossy();
        let ctx = SubstCtx {
            prompt,
            model: &model,
            project_root: &root,
        };

        Ok(ResolvedInvocation {
            program: expand(&adapter.command, &ctx),
            args: adapter.args.iter().map(|a| expand(a, &ctx)).collect(),
            cwd: expand(&adapter.cwd, &ctx),
        })
    }

    /// The `(adapter_key, model_id)` actually selected for `purpose` under
    /// `over` — what the run engine records as the `follow_up_runs.agent` /
    /// `.model` columns so a row honestly reflects what ran (not just the
    /// defaults). Same fallback + validation as [`resolve_with_override`].
    pub fn selection_for(
        &self,
        purpose: Purpose,
        over: Option<&RunOverride>,
    ) -> Result<(String, String), SettingsError> {
        let (key, _, model) = self.select(purpose, over)?;
        Ok((key, model))
    }

    /// Shared resolution core: pick the adapter (by key) and model for `purpose`
    /// under `over`, validating the override. Returns the owned adapter key (so
    /// no lifetime is tied to the borrowed `over`), a borrow of the chosen
    /// [`Adapter`] from `self`, and the owned model id.
    fn select(
        &self,
        purpose: Purpose,
        over: Option<&RunOverride>,
    ) -> Result<(String, &Adapter, String), SettingsError> {
        let adapter_key: String = over
            .and_then(|o| o.adapter.as_deref())
            .unwrap_or(self.default_adapter.as_str())
            .to_owned();
        let adapter = self
            .adapters
            .get(&adapter_key)
            .ok_or_else(|| SettingsError::UnknownAdapter(adapter_key.clone()))?;

        let model = match over.and_then(|o| o.model.as_deref()) {
            Some(m) => {
                validate_model(m)?;
                m.to_owned()
            }
            None => self.models.for_purpose(purpose).to_owned(),
        };

        Ok((adapter_key, adapter, model))
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
        Some(json) => {
            let mut settings: AgentSettings =
                serde_json::from_str(&json).map_err(SettingsError::Deserialize)?;
            // Built-in adapters merge ADDITIVELY over a stored `adapters` map
            // (bead conceptify-e7m.7, closing the e7m.2 caveat): serde replaces
            // the whole map on deserialize, so a blob written before a built-in
            // existed would otherwise hide it forever. Stored entries win —
            // user overrides of a built-in key and user-defined adapters are
            // untouched; only *missing* built-ins are injected. This also
            // guarantees provider routing can always resolve `claude`/`codex`.
            for (key, adapter) in default_adapters() {
                settings.adapters.entry(key).or_insert(adapter);
            }
            Ok(settings)
        }
    }
}

// --- OpenRouter API key (bead conceptify-e7m.7) ------------------------------
//
// Stored under its own `settings` row (see OPENROUTER_KEY_SETTINGS_KEY for the
// Keychain-vs-blob decision + rationale), so the AgentSettings type — and every
// command surface built on it — structurally cannot leak the key. Only these
// three functions touch it; the run engine reads it, the frontend only ever
// learns a boolean.

/// The stored OpenRouter API key, if any. Trimmed; a blank stored value reads
/// as `None`.
pub fn get_openrouter_api_key(conn: &Connection) -> Result<Option<String>, SettingsError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [OPENROUTER_KEY_SETTINGS_KEY],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty()))
}

/// Store (or clear) the OpenRouter API key. `None`/blank deletes the row.
/// Validation rejects embedded whitespace/control characters — never a real
/// key, and catches paste accidents — with an error that deliberately does NOT
/// echo the value ([`SettingsError::InvalidApiKey`]). One upsert/delete
/// statement, atomic like [`update_settings`] (PRD N4).
pub fn set_openrouter_api_key(
    conn: &Connection,
    key: Option<&str>,
) -> Result<(), SettingsError> {
    let key = key.map(str::trim).filter(|s| !s.is_empty());
    match key {
        None => {
            conn.execute(
                "DELETE FROM settings WHERE key = ?1",
                [OPENROUTER_KEY_SETTINGS_KEY],
            )?;
        }
        Some(k) => {
            if k.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(SettingsError::InvalidApiKey);
            }
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                rusqlite::params![OPENROUTER_KEY_SETTINGS_KEY, k],
            )?;
        }
    }
    Ok(())
}

/// Whether an OpenRouter API key is stored — the ONLY key-related fact the
/// frontend is ever given.
pub fn has_openrouter_api_key(conn: &Connection) -> Result<bool, SettingsError> {
    Ok(get_openrouter_api_key(conn)?.is_some())
}

pub fn get_local_endpoint_api_key(conn: &Connection) -> Result<Option<String>, SettingsError> {
    let raw: Option<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        [LOCAL_ENDPOINT_KEY_SETTINGS_KEY], |row| row.get(0),
    ).optional()?;
    Ok(raw.map(|value| value.trim().to_owned()).filter(|value| !value.is_empty()))
}

pub fn set_local_endpoint_api_key(conn: &Connection, key: Option<&str>) -> Result<(), SettingsError> {
    let key = key.map(str::trim).filter(|value| !value.is_empty());
    match key {
        None => { conn.execute("DELETE FROM settings WHERE key = ?1", [LOCAL_ENDPOINT_KEY_SETTINGS_KEY])?; }
        Some(value) => {
            if value.chars().any(|c| c.is_whitespace() || c.is_control()) { return Err(SettingsError::InvalidLocalApiKey); }
            conn.execute(
                "INSERT INTO settings(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                rusqlite::params![LOCAL_ENDPOINT_KEY_SETTINGS_KEY, value],
            )?;
        }
    }
    Ok(())
}

// --- HeyGen API key + render defaults (video epic conceptify-z9y, bead z9y.4)
//
// The key follows the OpenRouter-key pattern exactly (see
// HEYGEN_KEY_SETTINGS_KEY for the rationale): its own settings row, write-only
// through the command surface, presence-boolean-only reads everywhere else.
// The avatar/voice defaults are ordinary namespaced string rows (not secrets).

/// The stored HeyGen API key, if any. Trimmed; a blank stored value reads as
/// `None`. **Server-side only**: never expose the returned value through any
/// Tauri command, HTTP response, event payload, or log line.
pub fn get_heygen_api_key(conn: &Connection) -> Result<Option<String>, SettingsError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [HEYGEN_KEY_SETTINGS_KEY],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty()))
}

/// Store (or clear) the HeyGen API key. `None`/blank deletes the row (the
/// clear/reset affordance, same as the OpenRouter key). Validation rejects
/// embedded whitespace/control characters with an error that deliberately does
/// NOT echo the value ([`SettingsError::InvalidHeygenApiKey`]). One
/// upsert/delete statement, atomic like [`update_settings`] (PRD N4).
pub fn set_heygen_api_key(conn: &Connection, key: Option<&str>) -> Result<(), SettingsError> {
    let key = key.map(str::trim).filter(|s| !s.is_empty());
    match key {
        None => {
            conn.execute(
                "DELETE FROM settings WHERE key = ?1",
                [HEYGEN_KEY_SETTINGS_KEY],
            )?;
        }
        Some(k) => {
            if k.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(SettingsError::InvalidHeygenApiKey);
            }
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                rusqlite::params![HEYGEN_KEY_SETTINGS_KEY, k],
            )?;
        }
    }
    Ok(())
}

/// Whether a HeyGen API key is stored — the ONLY key-related fact the
/// frontend/CLI are ever given (`heygenKeyConfigured`). Gates the avatar
/// render feature end-to-end: absent key = feature cleanly disabled.
pub fn has_heygen_api_key(conn: &Connection) -> Result<bool, SettingsError> {
    Ok(get_heygen_api_key(conn)?.is_some())
}

/// Read one optional HeyGen default-id row (trimmed; blank reads as `None`).
fn get_heygen_default(conn: &Connection, key: &str) -> Result<Option<String>, SettingsError> {
    let raw: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(raw
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty()))
}

/// Write (or, with `None`/blank, clear) one HeyGen default-id row. Ids are
/// opaque short tokens; embedded whitespace/control characters are paste
/// accidents and rejected ([`SettingsError::InvalidHeygenId`]).
fn set_heygen_default(
    conn: &Connection,
    key: &str,
    value: Option<&str>,
    field: &'static str,
) -> Result<(), SettingsError> {
    let value = value.map(str::trim).filter(|s| !s.is_empty());
    match value {
        None => {
            conn.execute("DELETE FROM settings WHERE key = ?1", [key])?;
        }
        Some(v) => {
            if v.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(SettingsError::InvalidHeygenId(field));
            }
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                rusqlite::params![key, v],
            )?;
        }
    }
    Ok(())
}

/// The preferred avatar (look) id used when a render request omits `avatarId`.
pub fn get_heygen_default_avatar_id(conn: &Connection) -> Result<Option<String>, SettingsError> {
    get_heygen_default(conn, HEYGEN_DEFAULT_AVATAR_SETTINGS_KEY)
}

/// Set/clear the preferred avatar (look) id.
pub fn set_heygen_default_avatar_id(
    conn: &Connection,
    value: Option<&str>,
) -> Result<(), SettingsError> {
    set_heygen_default(conn, HEYGEN_DEFAULT_AVATAR_SETTINGS_KEY, value, "avatar")
}

/// The preferred voice id used when a render request omits `voiceId`.
pub fn get_heygen_default_voice_id(conn: &Connection) -> Result<Option<String>, SettingsError> {
    get_heygen_default(conn, HEYGEN_DEFAULT_VOICE_SETTINGS_KEY)
}

/// Set/clear the preferred voice id.
pub fn set_heygen_default_voice_id(
    conn: &Connection,
    value: Option<&str>,
) -> Result<(), SettingsError> {
    set_heygen_default(conn, HEYGEN_DEFAULT_VOICE_SETTINGS_KEY, value, "voice")
}

// --- Artifact theme (epic conceptify-89k, bead 89k.2) ------------------------
//
// Stored under its own `artifact.theme` settings row (see
// ARTIFACT_THEME_SETTINGS_KEY for the separate-row rationale), so a
// `reset_agent_settings` never disturbs the theme and the value is readable
// without touching the agent-settings blob. Only these two functions touch it;
// the Tauri commands and the `GET /settings/display` route consume them.

/// The stored artifact theme, or [`ArtifactTheme::Manuscript`] when no row
/// exists (FR-7.4 zero-config default). A stored value is validated on write, so
/// a stored value that fails to parse — only reachable via external DB
/// tampering — falls back to the default rather than erroring an otherwise
/// healthy read.
pub fn get_artifact_theme(conn: &Connection) -> Result<ArtifactTheme, SettingsError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [ARTIFACT_THEME_SETTINGS_KEY],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw
        .and_then(|s| ArtifactTheme::parse(&s).ok())
        .unwrap_or_default())
}

/// Persist the artifact theme, validated first: an unknown id is rejected with
/// the user-facing [`SettingsError::InvalidTheme`] and never stored. The
/// canonical lowercase id ([`ArtifactTheme::as_str`]) is written as one upsert
/// statement — SQLite applies it atomically, so a crash mid-write cannot corrupt
/// the row (PRD N4).
pub fn set_artifact_theme(conn: &Connection, theme: &str) -> Result<(), SettingsError> {
    let theme = ArtifactTheme::parse(theme)?;
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![ARTIFACT_THEME_SETTINGS_KEY, theme.as_str()],
    )?;
    Ok(())
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
        // The default template has 34 args; nothing was injected/split.
        assert_eq!(inv.args.len(), 34);
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
                "--include-partial-messages",
                "--verbose",
                "--strict-mcp-config",
                "--allowedTools",
                "Bash",
                "Edit",
                "Write",
                "Read",
                "Glob",
                "Grep",
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

    #[test]
    fn default_codex_scoping_exact() {
        // e7m.2: pin the whole default codex template so an accidental edit is
        // caught. Every flag verified against codex-cli 0.142.0 live probes
        // from an UNSANDBOXED parent (see the measurement-hazard note on
        // default_adapters): exec is approval-never; workspace-write = workdir
        // + /tmp + $TMPDIR writable, $HOME denied; network FULLY BLOCKED
        // (loopback/DNS/bind) unless network_access=true — hence the -c key,
        // without which the conceptify CLI can never report back; git commit
        // NOT denied (prompt-enforced only); stderr carries the transcript,
        // stdout only the final message.
        let settings = AgentSettings::default();
        let over = RunOverride {
            adapter: Some("codex".to_owned()),
            model: Some("gpt-5.4-mini".to_owned()),
        };
        let inv = settings
            .resolve_with_override(
                Purpose::FollowUp,
                Path::new("/tmp/proj"),
                "q",
                Some(&over),
            )
            .unwrap();
        assert_eq!(inv.program, "codex");
        assert_eq!(inv.cwd, "/tmp/proj");
        assert_eq!(
            inv.args,
            vec![
                "exec",
                "--model",
                "gpt-5.4-mini",
                "--sandbox",
                "workspace-write",
                "-c",
                "sandbox_workspace_write.network_access=true",
                "--skip-git-repo-check",
                "--ephemeral",
                "--ignore-user-config",
                "--color",
                "never",
                "--",
                "q",
            ]
        );
    }

    #[test]
    fn codex_template_keeps_adversarial_prompt_as_one_trailing_arg() {
        // The codex template ends `-- {prompt}`: the prompt must arrive verbatim
        // as the single final argv element, after the `--` separator, so no
        // prompt content (flag-like, subcommand-like, placeholder-like) can
        // alter the parsed command structure.
        let evil = "--model hacked\nresume --last; rm -rf / {project_root}";
        let settings = AgentSettings::default();
        let over = RunOverride {
            adapter: Some("codex".to_owned()),
            model: None,
        };
        let inv = settings
            .resolve_with_override(Purpose::FollowUp, Path::new("/tmp/proj"), evil, Some(&over))
            .unwrap();
        assert_eq!(inv.args.len(), 14);
        assert_eq!(inv.args[inv.args.len() - 2], "--");
        assert_eq!(inv.args[inv.args.len() - 1], evil);
        // Per-purpose model still selected normally (no model override).
        assert_eq!(inv.args[2], "claude-haiku-4-5");
    }

    // --- Defaults / storage -------------------------------------------------

    #[test]
    fn defaults_when_db_empty() {
        let conn = in_memory_settings_db();
        let s = get_settings(&conn).unwrap();
        assert_eq!(s, AgentSettings::default());
        assert_eq!(s.default_adapter, "claude");
        assert!(s.adapters.contains_key("claude"));
        // codex ships as a built-in adapter (e7m.2) — but never as the default.
        assert!(s.adapters.contains_key("codex"));
        assert_eq!(s.adapters.len(), 2);
        assert_eq!(s.models.follow_up, "claude-haiku-4-5");
        assert_eq!(s.models.artifact_update, "claude-sonnet-5");
        assert_eq!(s.models.in_app_ask, "claude-sonnet-5");
        assert_eq!(s.timeout_secs, 1800);
        assert_eq!(s.agent_binary_path, None);
        assert_eq!(s.appearance, Appearance::System);
        assert_eq!(s.auto_project_base_dir, None);
        assert_eq!(s.run_concurrency.default, 1);
        assert_eq!(s.run_concurrency.limit_for("anthropic"), 2);
        assert_eq!(s.run_concurrency.limit_for("new-provider"), 1);
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
        assert_eq!(s.run_concurrency, RunConcurrency::default());
    }

    #[test]
    fn run_concurrency_is_generic_round_trippable_and_validated() {
        let conn = in_memory_settings_db();
        let mut s = AgentSettings::default();
        s.run_concurrency.default = 2;
        s.run_concurrency.pools.insert("local:lab".to_owned(), 4);
        update_settings(&conn, &s).unwrap();

        let read = get_settings(&conn).unwrap();
        assert_eq!(read.run_concurrency.limit_for("local:lab"), 4);
        assert_eq!(read.run_concurrency.limit_for("unlisted"), 2);

        let mut zero_default = AgentSettings::default();
        zero_default.run_concurrency.default = 0;
        assert!(matches!(
            zero_default.validate(),
            Err(SettingsError::InvalidRunConcurrency(_))
        ));

        let mut zero_pool = AgentSettings::default();
        zero_pool
            .run_concurrency
            .pools
            .insert("anthropic".to_owned(), 0);
        assert!(matches!(
            zero_pool.validate(),
            Err(SettingsError::InvalidRunConcurrency(_))
        ));

        let mut blank_pool = AgentSettings::default();
        blank_pool.run_concurrency.pools.insert("  ".to_owned(), 1);
        assert!(matches!(
            blank_pool.validate(),
            Err(SettingsError::InvalidRunConcurrency(_))
        ));
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

    // --- Built-in adapters merge additively (bead conceptify-e7m.7) ---------

    #[test]
    fn stored_adapters_map_gains_missing_builtins() {
        // A blob written before `codex` existed (its `adapters` map only has a
        // user-tweaked `claude`) must yield BOTH built-ins on read, with the
        // user's claude override preserved verbatim.
        let conn = in_memory_settings_db();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('agent_settings', ?1)",
            [r#"{"adapters":{"claude":{"command":"/opt/my-claude","args":["-p","{prompt}"]}}}"#],
        )
        .unwrap();

        let s = get_settings(&conn).unwrap();
        // The user's override of the built-in wins…
        assert_eq!(s.adapters["claude"].command, "/opt/my-claude");
        assert_eq!(s.adapters["claude"].args, vec!["-p", "{prompt}"]);
        // …and the missing built-in is injected with its code default.
        assert_eq!(s.adapters["codex"], default_adapters()["codex"]);
        assert_eq!(s.adapters.len(), 2);
    }

    #[test]
    fn user_defined_adapters_survive_builtin_injection() {
        // A map with ONLY a custom adapter keeps it AND gains both built-ins;
        // a custom default_adapter still validates and resolves.
        let conn = in_memory_settings_db();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('agent_settings', ?1)",
            [r#"{"adapters":{"my-agent":{"command":"my-agent","args":["{prompt}"]}},
                 "defaultAdapter":"my-agent"}"#],
        )
        .unwrap();

        let s = get_settings(&conn).unwrap();
        assert_eq!(s.adapters.len(), 3);
        assert_eq!(s.adapters["my-agent"].command, "my-agent");
        assert_eq!(s.adapters["claude"], default_adapters()["claude"]);
        assert_eq!(s.adapters["codex"], default_adapters()["codex"]);
        assert_eq!(s.default_adapter, "my-agent");
        assert!(s.validate().is_ok());
    }

    // --- OpenRouter API key storage (bead conceptify-e7m.7) -----------------

    #[test]
    fn openrouter_key_round_trip_and_clear() {
        let conn = in_memory_settings_db();
        assert_eq!(get_openrouter_api_key(&conn).unwrap(), None);
        assert!(!has_openrouter_api_key(&conn).unwrap());

        set_openrouter_api_key(&conn, Some("  sk-or-v1-abc123  ")).unwrap();
        assert_eq!(
            get_openrouter_api_key(&conn).unwrap().as_deref(),
            Some("sk-or-v1-abc123"), // trimmed
        );
        assert!(has_openrouter_api_key(&conn).unwrap());

        // None and blank both clear.
        set_openrouter_api_key(&conn, Some("   ")).unwrap();
        assert_eq!(get_openrouter_api_key(&conn).unwrap(), None);
        set_openrouter_api_key(&conn, Some("k2")).unwrap();
        set_openrouter_api_key(&conn, None).unwrap();
        assert!(!has_openrouter_api_key(&conn).unwrap());
    }

    #[test]
    fn openrouter_key_rejects_embedded_whitespace_without_echoing_it() {
        let conn = in_memory_settings_db();
        for bad in ["sk with space", "sk\nnewline", "sk\ttab", "sk\u{0}nul"] {
            let err = set_openrouter_api_key(&conn, Some(bad)).unwrap_err();
            assert!(matches!(err, SettingsError::InvalidApiKey), "{bad:?}");
            // The error string must never echo the (mis)pasted secret.
            assert!(!err.to_string().contains("sk"), "{err}");
        }
        assert!(!has_openrouter_api_key(&conn).unwrap());
    }

    #[test]
    fn openrouter_key_never_enters_the_agent_settings_blob() {
        // The key lives in its own row: saving/reading AgentSettings cannot
        // carry it, and clearing agent settings does not clear the key.
        let conn = in_memory_settings_db();
        set_openrouter_api_key(&conn, Some("sk-or-v1-SECRET")).unwrap();

        let s = get_settings(&conn).unwrap();
        let blob = serde_json::to_string(&s).unwrap();
        assert!(!blob.contains("SECRET"), "{blob}");
        update_settings(&conn, &s).unwrap();

        let stored_blob: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'agent_settings'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!stored_blob.contains("SECRET"), "{stored_blob}");

        clear_settings(&conn).unwrap();
        assert!(has_openrouter_api_key(&conn).unwrap(), "reset keeps the key");
    }

    // --- HeyGen key + defaults (bead z9y.4) --------------------------------

    #[test]
    fn heygen_key_round_trip_and_clear() {
        let conn = in_memory_settings_db();
        assert_eq!(get_heygen_api_key(&conn).unwrap(), None);
        assert!(!has_heygen_api_key(&conn).unwrap());

        set_heygen_api_key(&conn, Some("  hg_abc123  ")).unwrap();
        assert_eq!(get_heygen_api_key(&conn).unwrap().as_deref(), Some("hg_abc123"));
        assert!(has_heygen_api_key(&conn).unwrap());

        // Blank clears, like `None`.
        set_heygen_api_key(&conn, Some("   ")).unwrap();
        assert_eq!(get_heygen_api_key(&conn).unwrap(), None);
        set_heygen_api_key(&conn, Some("k2")).unwrap();
        set_heygen_api_key(&conn, None).unwrap();
        assert!(!has_heygen_api_key(&conn).unwrap());
    }

    #[test]
    fn heygen_key_rejects_embedded_whitespace_without_echoing_it() {
        let conn = in_memory_settings_db();
        for bad in ["hg SECRET", "hg\tSECRET", "hg\nSECRET"] {
            let err = set_heygen_api_key(&conn, Some(bad)).unwrap_err();
            // The error string must never echo the (mis)pasted secret.
            assert!(!err.to_string().contains("SECRET"), "{err}");
        }
        assert!(!has_heygen_api_key(&conn).unwrap());
    }

    #[test]
    fn heygen_key_never_enters_the_agent_settings_blob() {
        // Write-only isolation, mirroring the OpenRouter test: the key lives in
        // its own row, so saving/reading AgentSettings structurally cannot
        // carry it, and clearing agent settings does not clear the key.
        let conn = in_memory_settings_db();
        set_heygen_api_key(&conn, Some("hg-SECRET")).unwrap();

        let s = get_settings(&conn).unwrap();
        let blob = serde_json::to_string(&s).unwrap();
        assert!(!blob.contains("SECRET"), "{blob}");
        update_settings(&conn, &s).unwrap();

        let stored_blob: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'agent_settings'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!stored_blob.contains("SECRET"), "{stored_blob}");

        clear_settings(&conn).unwrap();
        assert!(has_heygen_api_key(&conn).unwrap(), "reset keeps the key");
    }

    #[test]
    fn heygen_defaults_round_trip_and_clear_independently() {
        let conn = in_memory_settings_db();
        assert_eq!(get_heygen_default_avatar_id(&conn).unwrap(), None);
        assert_eq!(get_heygen_default_voice_id(&conn).unwrap(), None);

        set_heygen_default_avatar_id(&conn, Some(" lk_abc ")).unwrap();
        set_heygen_default_voice_id(&conn, Some("vc_123")).unwrap();
        assert_eq!(
            get_heygen_default_avatar_id(&conn).unwrap().as_deref(),
            Some("lk_abc")
        );
        assert_eq!(
            get_heygen_default_voice_id(&conn).unwrap().as_deref(),
            Some("vc_123")
        );

        // Clearing one leaves the other.
        set_heygen_default_voice_id(&conn, None).unwrap();
        assert_eq!(get_heygen_default_voice_id(&conn).unwrap(), None);
        assert_eq!(
            get_heygen_default_avatar_id(&conn).unwrap().as_deref(),
            Some("lk_abc")
        );

        // Paste accidents rejected, with the field named (not a secret).
        let err = set_heygen_default_avatar_id(&conn, Some("lk abc")).unwrap_err();
        assert!(err.to_string().contains("avatar"), "{err}");
    }

    // --- Artifact theme (bead 89k.2) ---------------------------------------

    #[test]
    fn artifact_theme_defaults_to_manuscript_when_absent() {
        let conn = in_memory_settings_db();
        assert_eq!(
            get_artifact_theme(&conn).unwrap(),
            ArtifactTheme::Manuscript
        );
    }

    #[test]
    fn artifact_theme_round_trips_each_known_theme() {
        let conn = in_memory_settings_db();
        for (id, expected) in [
            ("manuscript", ArtifactTheme::Manuscript),
            ("blueprint", ArtifactTheme::Blueprint),
            ("sketchbook", ArtifactTheme::Sketchbook),
            ("  blueprint  ", ArtifactTheme::Blueprint), // trimmed on write
        ] {
            set_artifact_theme(&conn, id).unwrap();
            assert_eq!(get_artifact_theme(&conn).unwrap(), expected, "{id:?}");
        }
    }

    #[test]
    fn artifact_theme_rejects_unknown_id_with_clear_error() {
        let conn = in_memory_settings_db();
        set_artifact_theme(&conn, "blueprint").unwrap();
        for bad in ["", "  ", "Manuscript", "vellum", "manuscript ."] {
            let err = set_artifact_theme(&conn, bad).unwrap_err();
            assert!(matches!(err, SettingsError::InvalidTheme(_)), "{bad:?}");
            let msg = err.to_string();
            assert!(msg.contains("manuscript"), "{msg}");
            assert!(msg.contains("blueprint"), "{msg}");
            assert!(msg.contains("sketchbook"), "{msg}");
        }
        // A rejected write never mutates the stored value.
        assert_eq!(get_artifact_theme(&conn).unwrap(), ArtifactTheme::Blueprint);
    }

    #[test]
    fn artifact_theme_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&ArtifactTheme::Sketchbook).unwrap(),
            "\"sketchbook\""
        );
        assert_eq!(ArtifactTheme::default(), ArtifactTheme::Manuscript);
    }

    #[test]
    fn artifact_theme_stays_out_of_the_agent_settings_blob_and_survives_reset() {
        // Same isolation guarantee as the OpenRouter key: the theme lives in its
        // own row, so clearing agent settings (FR-7.4 reset) leaves it intact.
        let conn = in_memory_settings_db();
        set_artifact_theme(&conn, "sketchbook").unwrap();

        let s = get_settings(&conn).unwrap();
        let blob = serde_json::to_string(&s).unwrap();
        assert!(!blob.contains("sketchbook"), "{blob}");

        clear_settings(&conn).unwrap();
        assert_eq!(
            get_artifact_theme(&conn).unwrap(),
            ArtifactTheme::Sketchbook,
            "reset keeps the theme"
        );
    }

    #[test]
    fn artifact_theme_tampered_stored_value_falls_back_to_default() {
        let conn = in_memory_settings_db();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('artifact.theme', 'bogus')",
            [],
        )
        .unwrap();
        assert_eq!(
            get_artifact_theme(&conn).unwrap(),
            ArtifactTheme::Manuscript
        );
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
    fn extra_adapter_via_config_only() {
        // Prove a new adapter needs no code change: add a custom one (a key
        // that is NOT a built-in, so this exercises pure config extension),
        // point defaultAdapter at it, and resolution uses its template.
        let mut settings = AgentSettings::default();
        settings.adapters.insert(
            "my-agent".to_owned(),
            Adapter {
                command: "my-agent".to_owned(),
                args: vec![
                    "run".to_owned(),
                    "--model".to_owned(),
                    "{model}".to_owned(),
                    "{prompt}".to_owned(),
                ],
                cwd: "{project_root}".to_owned(),
            },
        );
        settings.default_adapter = "my-agent".to_owned();
        settings.models.follow_up = "gpt-x".to_owned();
        assert!(settings.validate().is_ok());

        let inv = settings
            .resolve(Purpose::FollowUp, Path::new("/tmp/proj"), "prompt text")
            .unwrap();
        assert_eq!(inv.program, "my-agent");
        assert_eq!(inv.args, vec!["run", "--model", "gpt-x", "prompt text"]);
        assert_eq!(inv.cwd, "/tmp/proj");
    }

    // --- Per-run override (epic conceptify-e7m) -----------------------------

    #[test]
    fn override_model_wins_over_purpose_default() {
        // A `{model}`-only override reaches `{model}` (args[3]) verbatim and
        // keeps the default adapter; the per-purpose default is bypassed.
        let settings = AgentSettings::default();
        let over = RunOverride {
            adapter: None,
            model: Some("claude-opus-9".to_owned()),
        };
        let inv = settings
            .resolve_with_override(Purpose::FollowUp, Path::new("/tmp/proj"), "q", Some(&over))
            .unwrap();
        assert_eq!(inv.program, "claude"); // default adapter unchanged
        assert_eq!(inv.args[3], "claude-opus-9"); // NOT the FollowUp default
        // selection_for records the same for the run row.
        assert_eq!(
            settings.selection_for(Purpose::FollowUp, Some(&over)).unwrap(),
            ("claude".to_owned(), "claude-opus-9".to_owned())
        );
    }

    #[test]
    fn override_adapter_wins_over_default_adapter() {
        // A `{adapter}`-only override swaps the whole template (the escape
        // hatch) while keeping the per-purpose model. Uses the BUILT-IN codex
        // adapter, so this doubles as the override→codex selection path the
        // run engine takes (e7m.2/e7m.7).
        let settings = AgentSettings::default();
        let over = RunOverride {
            adapter: Some("codex".to_owned()),
            model: None,
        };
        let inv = settings
            .resolve_with_override(Purpose::InAppAsk, Path::new("/tmp/proj"), "q", Some(&over))
            .unwrap();
        assert_eq!(inv.program, "codex");
        // model still the InAppAsk per-purpose default (no model override).
        assert_eq!(inv.args[1], "--model");
        assert_eq!(inv.args[2], "claude-sonnet-5");
        assert_eq!(
            settings.selection_for(Purpose::InAppAsk, Some(&over)).unwrap(),
            ("codex".to_owned(), "claude-sonnet-5".to_owned())
        );
    }

    #[test]
    fn override_adapter_and_model_together() {
        let settings = AgentSettings::default();
        let over = RunOverride {
            adapter: Some("codex".to_owned()),
            model: Some("gpt-5".to_owned()),
        };
        let inv = settings
            .resolve_with_override(Purpose::FollowUp, Path::new("/tmp/proj"), "q", Some(&over))
            .unwrap();
        assert_eq!(inv.program, "codex");
        assert_eq!(inv.args[2], "gpt-5");
    }

    #[test]
    fn override_unknown_adapter_rejected() {
        let settings = AgentSettings::default();
        let over = RunOverride {
            adapter: Some("does-not-exist".to_owned()),
            model: None,
        };
        let err = settings
            .resolve_with_override(Purpose::FollowUp, Path::new("/tmp"), "q", Some(&over))
            .unwrap_err();
        assert!(matches!(err, SettingsError::UnknownAdapter(a) if a == "does-not-exist"));
        // selection_for rejects it identically (the engine calls both).
        assert!(matches!(
            settings.selection_for(Purpose::FollowUp, Some(&over)),
            Err(SettingsError::UnknownAdapter(_))
        ));
    }

    #[test]
    fn override_invalid_model_rejected() {
        let settings = AgentSettings::default();
        for bad in ["", "   ", "has space", "new\nline", "tab\ttab", "ctrl\u{0}null"] {
            let over = RunOverride {
                adapter: None,
                model: Some(bad.to_owned()),
            };
            let err = settings
                .resolve_with_override(Purpose::FollowUp, Path::new("/tmp"), "q", Some(&over))
                .unwrap_err();
            assert!(
                matches!(err, SettingsError::InvalidModel(_)),
                "model {bad:?} should be rejected, got {err:?}"
            );
        }
        // A slash/dot/dash/colon model (OpenRouter/LiteLLM shape) is accepted.
        let over = RunOverride {
            adapter: None,
            model: Some("anthropic/claude-3.5-sonnet:beta".to_owned()),
        };
        assert!(settings
            .resolve_with_override(Purpose::FollowUp, Path::new("/tmp"), "q", Some(&over))
            .is_ok());
    }

    #[test]
    fn omitted_and_empty_override_are_byte_identical_to_default() {
        // The acceptance guarantee: no override, `None`, and an all-`None`
        // RunOverride must all produce the exact same invocation as `resolve`.
        let settings = AgentSettings::default();
        let root = Path::new("/tmp/proj");
        let base = settings.resolve(Purpose::ArtifactUpdate, root, "q").unwrap();

        let via_none = settings
            .resolve_with_override(Purpose::ArtifactUpdate, root, "q", None)
            .unwrap();
        let empty = RunOverride::default();
        assert!(empty.is_empty());
        let via_empty = settings
            .resolve_with_override(Purpose::ArtifactUpdate, root, "q", Some(&empty))
            .unwrap();

        assert_eq!(base, via_none);
        assert_eq!(base, via_empty);
    }

    #[test]
    fn run_override_serde_shape_and_is_empty() {
        // Wire shape is camelCase-single-word `{ "adapter", "model" }`; omitted
        // fields deserialize to None; an all-None override is `is_empty`.
        let parsed: RunOverride = serde_json::from_str(r#"{"model":"m1"}"#).unwrap();
        assert_eq!(parsed.model.as_deref(), Some("m1"));
        assert!(parsed.adapter.is_none());
        assert!(!parsed.is_empty());

        let empty: RunOverride = serde_json::from_str("{}").unwrap();
        assert!(empty.is_empty());
        // Empty override serializes to `{}` (both fields skipped when None).
        assert_eq!(serde_json::to_string(&empty).unwrap(), "{}");

        let full = RunOverride {
            adapter: Some("codex".to_owned()),
            model: Some("gpt-5".to_owned()),
        };
        assert_eq!(
            serde_json::to_string(&full).unwrap(),
            r#"{"adapter":"codex","model":"gpt-5"}"#
        );
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

    // --- LIVE proof: real codex headless run (bead conceptify-e7m.2) --------

    /// End-to-end live proof of the built-in `codex` adapter defaults: a REAL
    /// `codex exec` run (real binary, real auth, real model) answers a trivial
    /// follow-up through the REAL `conceptify` CLI against an isolated temp DB
    /// — comment ends `answered` in the DB, run row ends `completed`.
    ///
    /// Isolation: nothing here touches the user's real app DB or the running
    /// Conceptify instance. The `conceptify` CLI child discovers the API via
    /// `$HOME/Library/Application Support/conceptify/{port,token}`, so the run
    /// env sets `HOME` to a scratch dir holding files that point at an
    /// in-test axum server (fronting `comments::update_comment` on the temp
    /// DB) on an ephemeral port; `CODEX_HOME` is pinned to the real
    /// `~/.codex` so codex keeps its auth despite the fake `HOME`.
    ///
    /// The invocation is the SHIPPED template: `AgentSettings::default()` +
    /// `RunOverride { adapter: codex }` — no test-only args. The prompt is the
    /// real flow prompt (`flows::build_answer_prompt`), not a fork.
    ///
    /// Residual isolation caveat (accepted): if the `conceptify` CLI child ever
    /// fails to reach the in-test server, its launch-and-wait fallback runs
    /// `open -a Conceptify` — harmless error when no app bundle is installed
    /// (observed live), and even against a running real app the PATCH would
    /// 404 (this test's comment ids don't exist there), so no real data can
    /// change. Multi-thread runtime + the pre-spawn self-probe below exist to
    /// keep the in-test server responsive so that path is never taken.
    ///
    /// Ignored by default (needs codex installed + authenticated, network, and
    /// a built CLI). Run manually **from a plain, unsandboxed terminal**:
    /// codex's Seatbelt cannot nest inside another sandbox, so launching this
    /// from an already-sandboxed shell (e.g. a coding agent's) degrades
    /// codex's sandbox and produces misleading results (see the
    /// measurement-hazard note on `default_adapters`).
    /// ```sh
    /// cargo build -p conceptify-cli
    /// cargo test -p conceptify settings::tests::live_codex -- --ignored --nocapture
    /// ```
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "live: needs codex CLI installed+authenticated and target/debug/conceptify built"]
    async fn live_codex_answers_follow_up_end_to_end() {
        use crate::comments::{CommentStatus, CommentThread};
        use crate::runs::{self, RunMode, RunRegistry, RunStatus, StartRun};

        // -- Preconditions (fail loudly: this test is opt-in).
        let codex = resolve_agent_binary("codex", None)
            .expect("codex CLI not resolvable via login shell — install/authenticate codex");
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cli = std::env::var("CONCEPTIFY_CLI")
            .map(PathBuf::from)
            .ok()
            .filter(|p| p.is_file())
            .or_else(|| {
                ["debug", "release"]
                    .iter()
                    .map(|profile| manifest.join("../target").join(profile).join("conceptify"))
                    .find(|p| p.is_file())
            })
            .expect("conceptify CLI binary not found — run `cargo build -p conceptify-cli` first");
        let real_home = dirs::home_dir().expect("home dir");
        assert!(
            real_home.join(".codex/auth.json").is_file(),
            "~/.codex/auth.json missing — codex is not authenticated"
        );
        eprintln!("[live] codex = {}", codex.display());
        eprintln!("[live] cli   = {}", cli.display());

        // -- Isolated world: scratch project repo, fake HOME, temp DB.
        let unique = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let base = std::env::temp_dir().join(format!("conceptify-live-codex-{unique}"));
        let project_root = base.join("repo");
        let fake_home = base.join("home");
        let support = fake_home.join("Library/Application Support/conceptify");
        std::fs::create_dir_all(&project_root).unwrap();
        std::fs::create_dir_all(&support).unwrap();
        std::fs::write(
            project_root.join("README.md"),
            "# Demo project\n\nThe local HTTP API listens on TCP port 4477 (loopback only).\n",
        )
        .unwrap();

        let db_path = base.join("app.db");
        let db = crate::db::init_at(&db_path).expect("temp db init");
        let project_id = format!("proj-live-{unique}");
        let question = "According to README.md in this project, which TCP port does the local HTTP API listen on? Answer in one short sentence.";
        let (thread_id, comment) = {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO projects (id, name, root_path) VALUES (?1, 'Live', ?2)",
                rusqlite::params![project_id, project_root.to_string_lossy()],
            )
            .unwrap();
            let thread =
                crate::threads::create_thread(&conn, &project_id, "Live Codex", "port question")
                    .unwrap();
            crate::artifacts::save_artifact(
                &conn,
                &crate::artifacts::test_artifacts_root(),
                &thread.id,
                br#"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<title>Live</title>
<meta name="cfy:question" content="port question">
<meta name="cfy:version" content="1">
<meta name="cfy:generated-by" content="claude-code/test">
</head><body><h1 data-cfy-id="sec-api">The API listens locally.</h1></body></html>"#,
            )
            .unwrap();
            let ctx = crate::comments::create_comment(&conn, &thread.id, 1, None, question).unwrap();
            (thread.id, ctx.comment)
        };
        let (artifact_path, artifact_version) = {
            let conn = db.lock().unwrap();
            conn.query_row(
                "SELECT file_path, version FROM artifacts WHERE thread_id = ?1
                 ORDER BY version DESC LIMIT 1",
                [&thread_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .unwrap()
        };

        // -- In-test API server: /health + PATCH /api/v1/comments/{id}, exactly
        //    the surface `conceptify resolve-comment` needs, fronting the REAL
        //    domain logic (comments::update_comment) on the temp DB. The real
        //    router isn't reachable from here (private module), and spawning it
        //    would clobber the real app's port/token files — this stays fully
        //    isolated instead.
        let token = format!("live-test-token-{unique}");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        #[derive(Clone)]
        struct LiveState {
            db: crate::db::DbHandle,
            token: String,
        }
        async fn patch_comment(
            axum::extract::State(state): axum::extract::State<LiveState>,
            axum::extract::Path(id): axum::extract::Path<String>,
            headers: axum::http::HeaderMap,
            axum::Json(req): axum::Json<conceptify_types::UpdateCommentRequest>,
        ) -> Result<axum::Json<serde_json::Value>, axum::http::StatusCode> {
            let auth = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if auth != format!("Bearer {}", state.token) {
                return Err(axum::http::StatusCode::UNAUTHORIZED);
            }
            let status = match req.status.as_deref() {
                None => None,
                Some(s) => Some(
                    crate::comments::CommentStatus::parse(s)
                        .ok_or(axum::http::StatusCode::BAD_REQUEST)?,
                ),
            };
            let ctx = {
                let conn = state.db.lock().unwrap();
                crate::comments::update_comment(&conn, &id, status, req.answer_html.as_deref(), None)
                    .map_err(|_| axum::http::StatusCode::BAD_REQUEST)?
            };
            let c = &ctx.comment;
            // Shape matches conceptify_types::CommentResponse (snake_case).
            Ok(axum::Json(serde_json::json!({
                "id": c.id,
                "thread_id": c.thread_id,
                "parent_id": ctx.parent_id,
                "artifact_version": c.artifact_version,
                "anchor": c.anchor,
                "body": c.body,
                "status": c.status.as_str(),
                "answer_html": c.answer_html,
                "anchor_state": c.anchor_state.as_str(),
                "created_at": c.created_at,
                "resolved_at": c.resolved_at,
            })))
        }
        let router = axum::Router::new()
            .route(
                "/health",
                axum::routing::get(|| async {
                    axum::Json(serde_json::json!({
                        "service": "conceptify",
                        "status": "ok",
                        "version": "live-test",
                    }))
                }),
            )
            .route("/api/v1/comments/{id}", axum::routing::patch(patch_comment))
            .with_state(LiveState {
                db: db.clone(),
                token: token.clone(),
            });
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        std::fs::write(support.join("port"), port.to_string()).unwrap();
        std::fs::write(support.join("token"), &token).unwrap();

        // Sanity self-probe: the in-test API must answer /health BEFORE codex
        // is spawned — a dead/starved server would otherwise send the CLI into
        // its launch-the-app fallback (see the isolation caveat above).
        {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            let mut healthy = false;
            for _ in 0..50 {
                if let Ok(mut s) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                    let _ = s
                        .write_all(
                            b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
                        )
                        .await;
                    let mut buf = String::new();
                    let _ = s.read_to_string(&mut buf).await;
                    if buf.contains(r#""status":"ok""#) {
                        healthy = true;
                        break;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            assert!(healthy, "in-test API server did not come up on 127.0.0.1:{port}");
        }

        // -- The real flow prompt (no fork), single-exchange answer mode.
        let prompt = crate::flows::build_answer_prompt(&crate::flows::AnswerPromptContext {
            thread_id: &thread_id,
            title: "Live Codex",
            question: "port question",
            project_root: &project_root.to_string_lossy(),
            artifact_path: &artifact_path,
            artifact_version,
            exchanges: std::slice::from_ref(&CommentThread {
                root: comment.clone(),
                replies: vec![],
            }),
        });

        // -- Run env: CLI dir on PATH (what flows::child_env does), fake HOME
        //    for the CLI's port/token discovery, real CODEX_HOME for auth.
        let cli_dir = cli.parent().unwrap().to_string_lossy().into_owned();
        let path_value = format!(
            "{cli_dir}:{}",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into())
        );
        let env = vec![
            ("PATH".to_owned(), path_value),
            ("HOME".to_owned(), fake_home.to_string_lossy().into_owned()),
            (
                "CODEX_HOME".to_owned(),
                real_home.join(".codex").to_string_lossy().into_owned(),
            ),
        ];

        // -- Spawn through the real engine with the SHIPPED codex defaults.
        let app = tauri::test::mock_builder()
            .manage(db.clone())
            .manage(RunRegistry::default())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let handle = app.handle().clone();
        let started = runs::start_run(
            &handle,
            StartRun {
                thread_id: thread_id.clone(),
                mode: RunMode::Answer,
                prompt,
                env,
                run_override: Some(RunOverride {
                    adapter: Some("codex".to_owned()),
                    model: Some("gpt-5.4-mini".to_owned()),
                }),
                retry_of_run_id: None,
                response_metadata: None,
            },
        )
        .await
        .expect("start codex run");
        eprintln!("[live] run {} started on thread {thread_id}", started.run_id);

        let fin = tokio::time::timeout(std::time::Duration::from_secs(600), started.finished)
            .await
            .expect("codex run did not finish within 600s")
            .expect("finished channel dropped");
        let log = std::fs::read_to_string(&fin.log_path).unwrap_or_default();
        let tail: String = {
            let lines: Vec<&str> = log.lines().collect();
            lines[lines.len().saturating_sub(40)..].join("\n")
        };
        assert_eq!(
            fin.status,
            RunStatus::Completed,
            "codex run ended {:?} (exit {:?}); log tail:\n{tail}",
            fin.status,
            fin.exit_code
        );

        // -- The comment was answered IN THE DB through the real CLI + real
        //    domain logic, and the run row is terminal-success.
        let (c_status, c_answer, run_status, run_agent, run_model) = {
            let conn = db.lock().unwrap();
            let (cs, ca): (String, Option<String>) = conn
                .query_row(
                    "SELECT status, answer_html FROM comments WHERE id = ?1",
                    [&comment.id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            let (rs, ra, rm): (String, String, String) = conn
                .query_row(
                    "SELECT status, agent, model FROM follow_up_runs WHERE id = ?1",
                    [&started.run_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            (cs, ca, rs, ra, rm)
        };
        assert_eq!(run_status, "completed");
        assert_eq!(run_agent, "codex");
        assert_eq!(run_model, "gpt-5.4-mini");
        assert_eq!(
            c_status,
            CommentStatus::Answered.as_str(),
            "comment not answered; log tail:\n{tail}"
        );
        let answer = c_answer.expect("answer_html stored");
        assert!(
            answer.contains("4477"),
            "answer should ground on the README's port; got: {answer}"
        );
        eprintln!("[live] SUCCESS — answer: {answer}");

        // -- Cleanup (only on success; failures keep the evidence around).
        drop(db);
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(
            crate::artifacts::test_artifacts_root().join(&project_id),
        );
    }
}
