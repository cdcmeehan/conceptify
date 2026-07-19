//! Conceptify CLI — thin, fast client for the local HTTP API (PRD §5.2).
//!
//! Every command implements the launch-and-wait contract: probe GET /health
//! (using the port file, fallback 4477) → on failure run `open -a Conceptify`
//! → poll up to ~10s → proceed. JSON output on stdout for agent consumption;
//! human-readable errors on stderr; non-zero exit when the app can't be
//! reached or the API returns an error.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::thread;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::json;

use conceptify_types::{
    CommentResponse, CreateThreadRequest, CreateThreadResponse, DisplaySettingsResponse,
    EnsureProjectRequest, EnsureProjectResponse, HealthResponse, ListCommentsResponse, OpenRequest,
    OpenResponse, SaveArtifactErrorResponse, SaveArtifactResponse, ThreadContextResponse,
    UpdateCommentRequest,
};

const DEFAULT_PORT: u16 = 4477;
const POLL_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(200);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

const USAGE: &str = "\
Usage: conceptify <command> [args...]

Commands:
  status                                              app/API health, version, port
  doctor                                              check prerequisites (app, CLI, d2, dot, node, agents)
  ensure-project --dir <path> [--name <name>]         find-or-create a project by directory
  create-thread  --project <id> --title <t> --question <q>   create a thread
  open           --thread <id> | --project <id>       focus the app on a project/thread
  save-artifact  --thread <id> --file <path>          save an artifact file to a thread
  get-context    --thread <id>                         run context for a headless follow-up (JSON)
  list-comments  --thread <id> [--status open|answered|applied]   list a thread's comments (JSON)
  resolve-comment --id <id> --answer-file <path> [--applied]      answer/apply a comment";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    }

    let command = &args[1];
    let rest = &args[2..];

    match command.as_str() {
        "status" => cmd_status(),
        "doctor" => cmd_doctor(),
        "ensure-project" => cmd_ensure_project(rest),
        "create-thread" => cmd_create_thread(rest),
        "open" => cmd_open(rest),
        "save-artifact" => cmd_save_artifact(rest),
        "get-context" => cmd_get_context(rest),
        "list-comments" => cmd_list_comments(rest),
        "resolve-comment" => cmd_resolve_comment(rest),
        _ => {
            eprintln!("Unknown command: {}", command);
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

/// Returns the path to the port file written by the server.
fn port_file_path() -> PathBuf {
    let data_dir = dirs::data_dir().expect("failed to determine user data directory");
    data_dir.join("conceptify").join("port")
}

/// Returns the path to the bearer token file.
fn token_file_path() -> PathBuf {
    let data_dir = dirs::data_dir().expect("failed to determine user data directory");
    data_dir.join("conceptify").join("token")
}

/// Reads the port from the port file, returning the default (4477) if the
/// file doesn't exist or contains invalid data. Note: the port file may be
/// stale if the app is not currently running.
fn read_port_file() -> u16 {
    match fs::read_to_string(port_file_path()) {
        Ok(contents) => contents.trim().parse().unwrap_or(DEFAULT_PORT),
        Err(_) => DEFAULT_PORT,
    }
}

/// Reads the bearer token from the token file. Returns an error if the file
/// doesn't exist or can't be read (the app hasn't run yet, or permissions are
/// wrong). Callers should only read the token after `ensure_app_healthy`, by
/// which point the running server has written it.
fn read_token() -> io::Result<String> {
    fs::read_to_string(token_file_path()).map(|s| s.trim().to_string())
}

/// Probes GET /health at the given port. Returns Ok(response) if the endpoint
/// responds with a 200 and valid JSON matching the HealthResponse shape.
fn probe_health(port: u16) -> Result<HealthResponse, String> {
    let url = format!("http://127.0.0.1:{}/health", port);

    match ureq::get(&url).timeout(Duration::from_secs(2)).call() {
        Ok(response) => match response.into_json::<HealthResponse>() {
            Ok(health) => Ok(health),
            Err(e) => Err(format!("health endpoint returned invalid JSON: {}", e)),
        },
        Err(ureq::Error::Status(code, _)) => {
            Err(format!("health endpoint returned status {}", code))
        }
        Err(ureq::Error::Transport(e)) => Err(format!("failed to reach health endpoint: {}", e)),
    }
}

/// Attempts to launch the Conceptify app via `open -a Conceptify`. Returns
/// Ok(()) if the command was invoked successfully (not a guarantee the app
/// actually launched or will become healthy — the caller must still poll).
fn launch_app() -> io::Result<()> {
    let status = Command::new("open").arg("-a").arg("Conceptify").status()?;

    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "open command exited with status {}",
            status
        )))
    }
}

/// Ensures the app is healthy via the launch-and-wait contract:
/// 1. Probe /health at the discovered port.
/// 2. If unhealthy, attempt to launch the app.
/// 3. Poll /health up to POLL_TIMEOUT.
/// 4. Return Ok(port) if healthy within the timeout, Err otherwise.
fn ensure_app_healthy() -> Result<u16, String> {
    let port = read_port_file();

    // Try the discovered port first (may be stale).
    if let Ok(response) = probe_health(port) {
        if response.service == "conceptify" && response.status == "ok" {
            return Ok(port);
        }
    }

    // Not healthy at that port. Try launching.
    eprintln!("App not responding; attempting to launch...");
    if let Err(e) = launch_app() {
        return Err(format!("failed to launch app: {}", e));
    }

    // Poll until healthy or timeout.
    let start = Instant::now();
    loop {
        // Try the port from the file (may have been updated after launch).
        let current_port = read_port_file();
        if let Ok(response) = probe_health(current_port) {
            if response.service == "conceptify" && response.status == "ok" {
                return Ok(current_port);
            }
        }

        if start.elapsed() > POLL_TIMEOUT {
            break;
        }

        thread::sleep(POLL_INTERVAL);
    }

    Err(format!(
        "app did not become healthy within {}s",
        POLL_TIMEOUT.as_secs()
    ))
}

/// POST a JSON body to an authenticated `/api/v1/*` endpoint and deserialize
/// the response. Reads the bearer token from the token file (written by the
/// running server) and attaches it as `Authorization: Bearer <token>`.
///
/// On an HTTP error status, extracts the server's `{"error": "..."}` message
/// so the CLI can surface it verbatim on stderr.
fn authed_post<B, R>(port: u16, path: &str, body: &B) -> Result<R, String>
where
    B: Serialize,
    R: DeserializeOwned,
{
    let token = read_token()
        .map_err(|e| format!("failed to read auth token (has the app run once?): {}", e))?;
    let url = format!("http://127.0.0.1:{}{}", port, path);

    match ureq::post(&url)
        .set("Authorization", &format!("Bearer {}", token))
        .timeout(REQUEST_TIMEOUT)
        .send_json(body)
    {
        Ok(response) => response
            .into_json::<R>()
            .map_err(|e| format!("invalid JSON from {}: {}", path, e)),
        Err(ureq::Error::Status(code, response)) => {
            let msg = response
                .into_json::<serde_json::Value>()
                .ok()
                .as_ref()
                .and_then(|v| v.get("error"))
                .and_then(|e| e.as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("HTTP {}", code));
            Err(format!("{} (HTTP {})", msg, code))
        }
        Err(ureq::Error::Transport(e)) => Err(format!("failed to reach app: {}", e)),
    }
}

/// POST raw bytes to an authenticated `/api/v1/*` endpoint and deserialize
/// the JSON response. Like `authed_post` but sends raw bytes instead of JSON
/// (used for save-artifact, which takes raw HTML).
fn authed_post_bytes<R>(port: u16, path: &str, bytes: &[u8]) -> Result<R, String>
where
    R: DeserializeOwned,
{
    let token = read_token()
        .map_err(|e| format!("failed to read auth token (has the app run once?): {}", e))?;
    let url = format!("http://127.0.0.1:{}{}", port, path);

    let mut request = ureq::post(&url)
        .set("Authorization", &format!("Bearer {}", token))
        .set("Content-Type", "text/html")
        .timeout(REQUEST_TIMEOUT);
    if let Ok(run_id) = std::env::var("CONCEPTIFY_RUN_ID") {
        if !run_id.trim().is_empty() {
            request = request.set("X-Conceptify-Run-Id", &run_id);
        }
    }

    match request.send_bytes(bytes) {
        Ok(response) => response
            .into_json::<R>()
            .map_err(|e| format!("invalid JSON from {}: {}", path, e)),
        Err(ureq::Error::Status(code, response)) => {
            let msg = response
                .into_json::<serde_json::Value>()
                .ok()
                .as_ref()
                .and_then(|v| v.get("error"))
                .and_then(|e| e.as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("HTTP {}", code));
            Err(format!("{} (HTTP {})", msg, code))
        }
        Err(ureq::Error::Transport(e)) => Err(format!("failed to reach app: {}", e)),
    }
}

/// Extract the server's `{"error": "..."}` message from an HTTP error
/// response, falling back to a bare `HTTP <code>` when the body isn't the
/// documented shape. Suffixed with the status so the agent can branch on it.
fn status_error(code: u16, response: ureq::Response) -> String {
    let msg = response
        .into_json::<serde_json::Value>()
        .ok()
        .as_ref()
        .and_then(|v| v.get("error"))
        .and_then(|e| e.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("HTTP {}", code));
    format!("{} (HTTP {})", msg, code)
}

/// GET an authenticated `/api/v1/*` endpoint and deserialize the JSON response.
/// Reads the bearer token (written by the running server) and attaches it as
/// `Authorization: Bearer <token>`. A non-2xx status surfaces the server's
/// `{"error": …}` message verbatim (see `status_error`).
fn authed_get<R>(port: u16, path: &str) -> Result<R, String>
where
    R: DeserializeOwned,
{
    let token = read_token()
        .map_err(|e| format!("failed to read auth token (has the app run once?): {}", e))?;
    let url = format!("http://127.0.0.1:{}{}", port, path);

    match ureq::get(&url)
        .set("Authorization", &format!("Bearer {}", token))
        .timeout(REQUEST_TIMEOUT)
        .call()
    {
        Ok(response) => response
            .into_json::<R>()
            .map_err(|e| format!("invalid JSON from {}: {}", path, e)),
        Err(ureq::Error::Status(code, response)) => Err(status_error(code, response)),
        Err(ureq::Error::Transport(e)) => Err(format!("failed to reach app: {}", e)),
    }
}

/// PATCH an authenticated `/api/v1/*` endpoint with a JSON body and deserialize
/// the JSON response. Used by `resolve-comment`; a `404` (unknown comment) or
/// `409` (illegal status transition) surfaces the server's message verbatim.
fn authed_patch<B, R>(port: u16, path: &str, body: &B) -> Result<R, String>
where
    B: Serialize,
    R: DeserializeOwned,
{
    let token = read_token()
        .map_err(|e| format!("failed to read auth token (has the app run once?): {}", e))?;
    let url = format!("http://127.0.0.1:{}{}", port, path);

    match ureq::request("PATCH", &url)
        .set("Authorization", &format!("Bearer {}", token))
        .timeout(REQUEST_TIMEOUT)
        .send_json(body)
    {
        Ok(response) => response
            .into_json::<R>()
            .map_err(|e| format!("invalid JSON from {}: {}", path, e)),
        Err(ureq::Error::Status(code, response)) => Err(status_error(code, response)),
        Err(ureq::Error::Transport(e)) => Err(format!("failed to reach app: {}", e)),
    }
}

/// Parse `--key value` pairs from an argument slice into a map. Unknown-shaped
/// tokens (a value without a preceding `--key`, or a `--key` with no value)
/// return an error rather than being silently dropped, so a typo surfaces
/// instead of producing a wrong request.
fn parse_flags(args: &[String]) -> Result<HashMap<String, String>, String> {
    let mut flags = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let key = arg
            .strip_prefix("--")
            .ok_or_else(|| format!("unexpected argument: {}", arg))?;
        let value = args
            .get(i + 1)
            .ok_or_else(|| format!("missing value for --{}", key))?;
        flags.insert(key.to_string(), value.to_string());
        i += 2;
    }
    Ok(flags)
}

/// Print a JSON value to stdout and return success.
fn emit(value: &serde_json::Value) -> ExitCode {
    println!("{}", serde_json::to_string_pretty(value).unwrap());
    ExitCode::SUCCESS
}

/// Print an error to stderr and return failure.
fn fail(msg: impl AsRef<str>) -> ExitCode {
    eprintln!("Error: {}", msg.as_ref());
    ExitCode::FAILURE
}

/// `conceptify status` — prints app/API health, version, and the chosen
/// artifact theme as JSON. The theme (bead conceptify-89k.2) is the skill's
/// cheap one-call read of the app-level display setting at authoring time.
fn cmd_status() -> ExitCode {
    match ensure_app_healthy() {
        Ok(port) => {
            // Re-probe to get the current health info (we know it's healthy,
            // but we want the version and status fields for JSON output).
            match probe_health(port) {
                Ok(health) => {
                    // Fold in the author-time display settings via the authed
                    // endpoint. Best-effort: health already proved liveness, so
                    // a settings read hiccup must not sink `status` — it degrades
                    // to the default theme (with a stderr note) rather than
                    // failing the whole command.
                    let artifact_theme =
                        match authed_get::<DisplaySettingsResponse>(port, "/api/v1/settings/display")
                        {
                            Ok(d) => d.artifact_theme,
                            Err(e) => {
                                eprintln!(
                                    "warning: could not read artifact theme ({e}); assuming manuscript"
                                );
                                "manuscript".to_string()
                            }
                        };
                    emit(&json!({
                        "service": health.service,
                        "status": health.status,
                        "version": health.version,
                        "port": port,
                        "artifactTheme": artifact_theme,
                    }))
                }
                Err(e) => {
                    // This shouldn't happen (we just confirmed it's healthy),
                    // but handle it gracefully.
                    fail(format!("re-probing health after success: {}", e))
                }
            }
        }
        Err(e) => fail(e),
    }
}

/// Shape the API's ensure-project response into the CLI's documented output
/// contract (§5.2): `{"projectId": …, "created": bool}` (camelCase, unlike the
/// snake_case API body).
fn ensure_project_output(resp: &EnsureProjectResponse) -> serde_json::Value {
    json!({ "projectId": resp.id, "created": resp.created })
}

/// `conceptify ensure-project --dir <path> [--name <name>]`.
fn cmd_ensure_project(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args) {
        Ok(f) => f,
        Err(e) => return fail(e),
    };

    let dir = match flags.get("dir") {
        Some(d) => d,
        None => return fail("ensure-project requires --dir <path>"),
    };

    // Resolve --dir to an absolute, symlink-free path *on the CLI's side*: the
    // server canonicalizes relative to *its* cwd (wherever the app launched),
    // not the agent's, so a bare relative path would resolve against the wrong
    // directory. Canonicalizing here also gives a clean "not found" before we
    // even touch the API.
    let abs_dir = match fs::canonicalize(dir) {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(e) => return fail(format!("path not found: {} ({})", dir, e)),
    };

    let port = match ensure_app_healthy() {
        Ok(p) => p,
        Err(e) => return fail(e),
    };

    let req = EnsureProjectRequest {
        root_path: abs_dir,
        name: flags.get("name").cloned(),
    };

    match authed_post::<_, EnsureProjectResponse>(port, "/api/v1/projects/ensure", &req) {
        Ok(resp) => emit(&ensure_project_output(&resp)),
        Err(e) => fail(e),
    }
}

/// Shape the API's create-thread response into the CLI's documented output
/// contract (§5.2): `{"threadId": …}`, plus the server-derived `slug` (the
/// artifact-folder name the skill needs for save-artifact).
fn create_thread_output(resp: &CreateThreadResponse) -> serde_json::Value {
    json!({ "threadId": resp.id, "slug": resp.slug })
}

/// `conceptify create-thread --project <id> --title <t> --question <q>`.
fn cmd_create_thread(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args) {
        Ok(f) => f,
        Err(e) => return fail(e),
    };

    let (project, title, question) = match (
        flags.get("project"),
        flags.get("title"),
        flags.get("question"),
    ) {
        (Some(p), Some(t), Some(q)) => (p, t, q),
        _ => {
            return fail("create-thread requires --project <id> --title <t> --question <q>");
        }
    };

    let port = match ensure_app_healthy() {
        Ok(p) => p,
        Err(e) => return fail(e),
    };

    let req = CreateThreadRequest {
        project_id: project.clone(),
        title: title.clone(),
        initial_question: question.clone(),
    };

    match authed_post::<_, CreateThreadResponse>(port, "/api/v1/threads", &req) {
        Ok(resp) => emit(&create_thread_output(&resp)),
        Err(e) => fail(e),
    }
}

/// Shape the API's open response into the CLI's JSON output (camelCase).
fn open_output(resp: &OpenResponse) -> serde_json::Value {
    json!({
        "ok": resp.ok,
        "projectId": resp.project_id,
        "threadId": resp.thread_id,
    })
}

/// Shape the API's save-artifact response into the CLI's JSON output (camelCase).
/// Includes the version and warnings count for agent consumption.
fn save_artifact_output(resp: &SaveArtifactResponse) -> serde_json::Value {
    json!({
        "version": resp.version,
        "warningsCount": resp.warnings.len(),
    })
}

/// `conceptify save-artifact --thread <id> --file <path>` — save an artifact.
fn cmd_save_artifact(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args) {
        Ok(f) => f,
        Err(e) => return fail(e),
    };

    let (thread, file_path) = match (flags.get("thread"), flags.get("file")) {
        (Some(t), Some(f)) => (t, f),
        _ => {
            return fail("save-artifact requires --thread <id> --file <path>");
        }
    };

    // Read the file bytes on the CLI side.
    let bytes = match fs::read(file_path) {
        Ok(b) => b,
        Err(e) => return fail(format!("failed to read {}: {}", file_path, e)),
    };

    let port = match ensure_app_healthy() {
        Ok(p) => p,
        Err(e) => return fail(e),
    };

    // POST the raw HTML bytes to the artifact endpoint.
    let path = format!("/api/v1/threads/{}/artifact", thread);
    let result: Result<SaveArtifactResponse, String> = authed_post_bytes(port, &path, &bytes);

    match result {
        Ok(resp) => {
            // Print warnings to stderr (agent-visible).
            for warning in &resp.warnings {
                eprintln!("warning: {}: {}", warning.code, warning.message);
            }

            // Focus the app on the thread after successful save (the endpoint
            // doesn't handle this — checked in src-tauri/server/artifacts_routes.rs).
            let open_req = OpenRequest {
                thread_id: Some(thread.clone()),
                project_id: None,
            };
            if let Err(e) = authed_post::<_, OpenResponse>(port, "/api/v1/open", &open_req) {
                eprintln!("warning: saved artifact but failed to focus app: {}", e);
            }

            emit(&save_artifact_output(&resp))
        }
        Err(e) => {
            // For validation errors (422), try to parse the structured error
            // response and surface the specific rule violations.
            if e.contains("HTTP 422") {
                // Re-attempt the request to get the structured error body.
                // (We can't get it from the Err above because ureq consumed it.)
                // This is a bit redundant but keeps the normal path clean.
                let token = match read_token() {
                    Ok(t) => t,
                    Err(err) => return fail(format!("failed to read auth token: {}", err)),
                };
                let url = format!("http://127.0.0.1:{}{}", port, path);
                if let Err(ureq::Error::Status(422, response)) = ureq::post(&url)
                    .set("Authorization", &format!("Bearer {}", token))
                    .set("Content-Type", "text/html")
                    .timeout(REQUEST_TIMEOUT)
                    .send_bytes(&bytes)
                {
                    if let Ok(err_resp) = response.into_json::<SaveArtifactErrorResponse>() {
                        eprintln!("Error: {} ({})", err_resp.error, err_resp.code);
                        for issue in &err_resp.errors {
                            eprintln!("  {}: {}", issue.code, issue.message);
                        }
                        return ExitCode::FAILURE;
                    }
                }
            }
            fail(e)
        }
    }
}

/// `conceptify open --thread <id> | --project <id>` — focus the app on target.
fn cmd_open(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args) {
        Ok(f) => f,
        Err(e) => return fail(e),
    };

    let thread = flags.get("thread");
    let project = flags.get("project");

    // Exactly one selector: reject neither/both up front so the request is
    // unambiguous (the server would otherwise pick thread over project).
    let req = match (thread, project) {
        (Some(t), None) => OpenRequest {
            thread_id: Some(t.clone()),
            project_id: None,
        },
        (None, Some(p)) => OpenRequest {
            thread_id: None,
            project_id: Some(p.clone()),
        },
        (None, None) => return fail("open requires exactly one of --thread <id> or --project <id>"),
        (Some(_), Some(_)) => {
            return fail("open takes only one of --thread or --project, not both")
        }
    };

    let port = match ensure_app_healthy() {
        Ok(p) => p,
        Err(e) => return fail(e),
    };

    match authed_post::<_, OpenResponse>(port, "/api/v1/open", &req) {
        Ok(resp) => emit(&open_output(&resp)),
        Err(e) => fail(e),
    }
}

/// Shape one API comment into the CLI's stable camelCase form. The `anchor` is
/// the single deliberate exception: it is passed through **verbatim** (stored
/// snake_case JSON), because the anchor is a documented cross-layer contract
/// (bridge ⇄ DB ⇄ re-attachment ⇄ agent) that must round-trip byte-for-byte.
/// Shared by `get-context` (its `openComments`) and `list-comments`.
fn comment_output(c: &CommentResponse) -> serde_json::Value {
    json!({
        "id": c.id,
        "threadId": c.thread_id,
        "parentId": c.parent_id,
        "artifactVersion": c.artifact_version,
        "anchor": c.anchor,
        "body": c.body,
        "status": c.status,
        "answerHtml": c.answer_html,
        "anchorState": c.anchor_state,
        "createdAt": c.created_at,
        "resolvedAt": c.resolved_at,
    })
}

/// Shape one open ROOT comment (+ its reply chain) into the CLI's camelCase form.
/// The root's fields are the same `comment_output` shape; its ordered exchange
/// history is appended as a `replies` array (each reply also camelCase). This is
/// what lets a follow-up run read the full conversation (epic conceptify-6xi).
fn context_comment_output(tc: &conceptify_types::ThreadContextComment) -> serde_json::Value {
    let mut v = comment_output(&tc.comment);
    if let Some(obj) = v.as_object_mut() {
        obj.insert(
            "replies".to_owned(),
            serde_json::Value::Array(tc.replies.iter().map(comment_output).collect()),
        );
    }
    v
}

/// Shape the context aggregate into the CLI's camelCase output contract (§5.2):
/// the run-specific context a headless follow-up assembles into its prompt —
/// question, artifact path, project root, and the open ROOT comments to answer,
/// each with its nested reply chain. `artifactVersion`/`artifactPath` are `null`
/// when the thread has no artifact yet. Anchors inside `openComments` stay
/// verbatim (see `comment_output`).
fn get_context_output(resp: &ThreadContextResponse) -> serde_json::Value {
    json!({
        "threadId": resp.thread.id,
        "title": resp.thread.title,
        "question": resp.thread.initial_question,
        "status": resp.thread.status,
        "slug": resp.thread.slug,
        "projectId": resp.project.id,
        "projectName": resp.project.name,
        "projectRoot": resp.project.root_path,
        "artifactVersion": resp.latest_artifact.as_ref().map(|a| a.version),
        "artifactPath": resp.latest_artifact.as_ref().map(|a| a.file_path.clone()),
        "openComments": resp
            .open_comments
            .iter()
            .map(context_comment_output)
            .collect::<Vec<_>>(),
    })
}

/// `conceptify get-context --thread <id>` — the one-round-trip run context
/// (thread + question + artifact path + open comments) a headless follow-up
/// needs (PRD §5.2, §5.5; maps to `GET /api/v1/threads/:id/context`).
fn cmd_get_context(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args) {
        Ok(f) => f,
        Err(e) => return fail(e),
    };

    let thread = match flags.get("thread") {
        Some(t) => t,
        None => return fail("get-context requires --thread <id>"),
    };

    let port = match ensure_app_healthy() {
        Ok(p) => p,
        Err(e) => return fail(e),
    };

    let path = format!("/api/v1/threads/{}/context", thread);
    match authed_get::<ThreadContextResponse>(port, &path) {
        Ok(resp) => emit(&get_context_output(&resp)),
        Err(e) => fail(e),
    }
}

/// Shape the list-comments response into a bare JSON array (each comment
/// camelCase, anchors verbatim).
fn list_comments_output(resp: &ListCommentsResponse) -> serde_json::Value {
    serde_json::Value::Array(resp.comments.iter().map(comment_output).collect())
}

/// `conceptify list-comments --thread <id> [--status open|answered|applied]` —
/// list a thread's comments with anchors (PRD §5.2; maps to
/// `GET /api/v1/comments`). An invalid `--status` is rejected by the server.
fn cmd_list_comments(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args) {
        Ok(f) => f,
        Err(e) => return fail(e),
    };

    let thread = match flags.get("thread") {
        Some(t) => t,
        None => return fail("list-comments requires --thread <id>"),
    };

    // thread ids are UUIDs and status is a known enum, so neither needs URL
    // encoding; the server strictly validates the status value.
    let mut path = format!("/api/v1/comments?thread_id={}", thread);
    if let Some(status) = flags.get("status") {
        path.push_str("&status=");
        path.push_str(status);
    }

    let port = match ensure_app_healthy() {
        Ok(p) => p,
        Err(e) => return fail(e),
    };

    match authed_get::<ListCommentsResponse>(port, &path) {
        Ok(resp) => emit(&list_comments_output(&resp)),
        Err(e) => fail(e),
    }
}

/// Parse `resolve-comment`'s args: the valueless `--applied` boolean is split
/// off first (it may appear anywhere), then the remaining `--key value` pairs
/// are parsed. Returns `(id, answer_file, applied)`.
fn resolve_flags(args: &[String]) -> Result<(String, String, bool), String> {
    let applied = args.iter().any(|a| a == "--applied");
    let rest: Vec<String> = args
        .iter()
        .filter(|a| a.as_str() != "--applied")
        .cloned()
        .collect();
    let flags = parse_flags(&rest)?;

    match (flags.get("id"), flags.get("answer-file")) {
        (Some(id), Some(file)) => Ok((id.clone(), file.clone(), applied)),
        _ => Err("resolve-comment requires --id <id> --answer-file <path> [--applied]".to_string()),
    }
}

/// Shape a resolved comment into the CLI's confirmation output (§5.2).
fn resolve_comment_output(c: &CommentResponse) -> serde_json::Value {
    json!({ "ok": true, "id": c.id, "status": c.status })
}

/// `conceptify resolve-comment --id <id> --answer-file <path> [--applied]` —
/// answer (or, with `--applied`, apply) a comment (PRD §5.2, FR-4.6/4.7; maps
/// to `PATCH /api/v1/comments/:id`). Reads the answer fragment from the file on
/// the CLI side and stores it as `answer_html`, advancing the comment to
/// `answered` (default) or `applied`. The sidebar updates live via the
/// `comment-updated` event the server emits.
fn cmd_resolve_comment(args: &[String]) -> ExitCode {
    let (id, answer_file, applied) = match resolve_flags(args) {
        Ok(parts) => parts,
        Err(e) => return fail(e),
    };

    // Read the answer fragment (HTML or markdown) on the CLI side, verbatim —
    // the sidebar renders it. A missing/unreadable file fails before any HTTP.
    let answer = match fs::read_to_string(&answer_file) {
        Ok(s) => s,
        Err(e) => return fail(format!("failed to read {}: {}", answer_file, e)),
    };

    let status = if applied { "applied" } else { "answered" };

    let port = match ensure_app_healthy() {
        Ok(p) => p,
        Err(e) => return fail(e),
    };

    let req = UpdateCommentRequest {
        status: Some(status.to_string()),
        answer_html: Some(answer),
        anchor_state: None,
    };

    let path = format!("/api/v1/comments/{}", id);
    match authed_patch::<_, CommentResponse>(port, &path, &req) {
        Ok(resp) => emit(&resolve_comment_output(&resp)),
        Err(e) => fail(e),
    }
}

/// A single prerequisite check result.
#[derive(Debug, Clone)]
struct Check {
    name: String,
    ok: bool,
    detail: String,
    hint: Option<String>,
}

impl Check {
    fn pass(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ok: true,
            detail: detail.into(),
            hint: None,
        }
    }

    fn fail(name: impl Into<String>, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ok: false,
            detail: detail.into(),
            hint: Some(hint.into()),
        }
    }
}

/// Check if the app is installed (bundle exists or app is running).
fn check_app_installed() -> Check {
    // First, read the bundle identifier from tauri.conf.json to use with mdfind.
    let bundle_id = "com.chrismeehan.conceptify"; // from tauri.conf.json

    // Try mdfind first (most reliable).
    if let Ok(output) = Command::new("mdfind")
        .arg(format!("kMDItemCFBundleIdentifier == '{}'", bundle_id))
        .output()
    {
        if output.status.success() && !output.stdout.is_empty() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let paths: Vec<&str> = stdout
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            if !paths.is_empty() {
                // Take the first path (usually /Applications/Conceptify.app if installed).
                return Check::pass(
                    "app-installed",
                    format!("Conceptify.app found at {}", paths[0]),
                );
            }
        }
    }

    // Fall back to checking standard app locations.
    let app_paths = [
        "/Applications/Conceptify.app",
        &format!(
            "{}/Applications/Conceptify.app",
            std::env::var("HOME").unwrap_or_default()
        ),
    ];

    for path in &app_paths {
        if std::path::Path::new(path).exists() {
            return Check::pass("app-installed", format!("Conceptify.app found at {}", path));
        }
    }

    // Check if the app is running by probing health (may be a dev build).
    let port = read_port_file();
    if let Ok(response) = probe_health(port) {
        if response.service == "conceptify" && response.status == "ok" {
            return Check::pass(
                "app-installed",
                format!("Conceptify app is running on port {}", port),
            );
        }
    }

    Check::fail(
        "app-installed",
        "Conceptify.app not found in /Applications or ~/Applications, and not currently running",
        "Run `just build` to create Conceptify.app, then install it to /Applications",
    )
}

/// Check if `conceptify` CLI is on PATH.
fn check_cli_on_path() -> Check {
    match Command::new("which").arg("conceptify").output() {
        Ok(output) if output.status.success() => {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            // Check if it's running from target/ (dev build).
            if path.contains("/target/") {
                Check::pass(
                    "cli-on-path",
                    format!(
                        "conceptify is resolvable at {} (dev build; consider `just install-cli`)",
                        path
                    ),
                )
            } else {
                Check::pass("cli-on-path", format!("conceptify is on PATH at {}", path))
            }
        }
        _ => Check::fail(
            "cli-on-path",
            "conceptify not found on PATH",
            "Run `just install-cli` to symlink the CLI to ~/.local/bin",
        ),
    }
}

/// Check if `d2` is installed.
fn check_d2_present() -> Check {
    match Command::new("which").arg("d2").output() {
        Ok(output) if output.status.success() => {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Check::pass("d2-present", format!("d2 is available at {}", path))
        }
        _ => Check::fail(
            "d2-present",
            "d2 not found on PATH",
            "Install d2: brew install d2",
        ),
    }
}

/// Check if `dot` (graphviz) is installed.
fn check_dot_present() -> Check {
    match Command::new("which").arg("dot").output() {
        Ok(output) if output.status.success() => {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Check::pass("dot-present", format!("dot (graphviz) is available at {}", path))
        }
        _ => Check::fail(
            "dot-present",
            "dot (graphviz) not found on PATH",
            "Install graphviz: brew install graphviz",
        ),
    }
}

/// Check if `node` is installed and version >= 20.
fn check_node_present() -> Check {
    match Command::new("node").arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            // Parse version (e.g., "v20.10.0" -> 20).
            let major_version = version_str
                .strip_prefix('v')
                .and_then(|s| s.split('.').next())
                .and_then(|s| s.parse::<u32>().ok());

            match major_version {
                Some(v) if v >= 20 => Check::pass(
                    "node-present",
                    format!("node {} is available (>= v20)", version_str),
                ),
                Some(v) => Check::fail(
                    "node-present",
                    format!("node {} is installed but < v20 (found v{})", version_str, v),
                    "Install node >= v20: brew install node",
                ),
                None => Check::fail(
                    "node-present",
                    format!(
                        "node is installed ({}) but version is unparseable",
                        version_str
                    ),
                    "Install node >= v20: brew install node",
                ),
            }
        }
        _ => Check::fail(
            "node-present",
            "node not found on PATH",
            "Install node: brew install node",
        ),
    }
}

/// Check whether the skill's Remotion video project (`skill/video`, installed
/// under `~/.claude/skills/conceptify/video`) has its dependencies installed.
/// Explainer videos (bead conceptify-z9y.3) are an optional enhancement, so a
/// missing or uninstalled project is reported as information — `ok` stays
/// `true` and doctor's exit code is unaffected, matching the optional-`codex`
/// check above.
fn check_remotion_project() -> Check {
    let home = std::env::var("HOME").unwrap_or_default();
    // The skill is copied to one of these roots by `just install-skill`.
    let candidates = [
        format!("{home}/.claude/skills/conceptify/video"),
        format!("{home}/.codex/skills/conceptify/video"),
    ];
    let install_hint = |dir: &str| {
        format!("Install the render deps: cd {dir} && npm install && npm run ensure-browser")
    };
    for dir in &candidates {
        let project = std::path::Path::new(dir);
        if !project.join("package.json").exists() {
            continue;
        }
        // Project present — are its node deps installed?
        if project.join("node_modules/remotion").exists() {
            return Check::pass(
                "remotion-project",
                format!("Remotion video project is installed at {dir}"),
            );
        }
        return Check {
            name: "remotion-project".to_string(),
            ok: true, // optional: absence must never fail doctor
            detail: format!(
                "Remotion video project found at {dir} but its dependencies are not installed (explainer videos unavailable until then)"
            ),
            hint: Some(install_hint(dir)),
        };
    }
    Check {
        name: "remotion-project".to_string(),
        ok: true,
        detail:
            "Remotion video project not found (optional — only needed for explainer videos; install the conceptify skill first)"
                .to_string(),
        hint: Some(install_hint(&candidates[0])),
    }
}

/// Resolve a binary via a login shell (`zsh -lc 'which <name>'`), so the check
/// sees the user's full PATH the way the app's own lookup does (PRD §5.1).
fn login_shell_which(name: &str) -> Option<String> {
    match Command::new("zsh")
        .arg("-lc")
        .arg(format!("which {}", name))
        .output()
    {
        Ok(output) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
        }
        _ => None,
    }
}

/// Check if the default agent binary (`claude`) is resolvable via a login
/// shell. This one FAILS doctor when missing — `claude` is the default adapter
/// every zero-config run uses.
fn check_agent_binary_resolvable() -> Check {
    match login_shell_which("claude") {
        Some(path) => Check::pass(
            "agent-binary-resolvable",
            format!("claude (default agent) is resolvable at {}", path),
        ),
        None => Check::fail(
            "agent-binary-resolvable",
            "claude not found via login shell (zsh -lc 'which claude')",
            "Install Claude Code (note: settings can override the binary path later)",
        ),
    }
}

/// Check if the optional `codex` agent binary is resolvable (bead
/// conceptify-e7m.2). Codex is only needed to route runs to OpenAI models, so
/// its absence is reported as information — `ok` stays `true` either way and
/// doctor's exit code is unaffected while `claude` remains the default agent.
fn check_codex_binary_resolvable() -> Check {
    match login_shell_which("codex") {
        Some(path) => Check::pass(
            "codex-binary-resolvable",
            format!("codex (optional agent, OpenAI routes) is resolvable at {}", path),
        ),
        None => Check {
            name: "codex-binary-resolvable".to_string(),
            ok: true, // optional: absence must never fail doctor
            detail: "codex not found (optional — only needed to run OpenAI models)".to_string(),
            hint: Some("Install codex: brew install codex (or: npm install -g @openai/codex)".to_string()),
        },
    }
}

/// Shape a Check into JSON for stdout output.
fn check_to_json(check: &Check) -> serde_json::Value {
    json!({
        "name": check.name,
        "ok": check.ok,
        "detail": check.detail,
        "hint": check.hint,
    })
}

/// `conceptify doctor` — check prerequisites and report results.
fn cmd_doctor() -> ExitCode {
    // Run all checks (never fail hard mid-run).
    let checks = vec![
        check_app_installed(),
        check_cli_on_path(),
        check_d2_present(),
        check_dot_present(),
        check_node_present(),
        check_remotion_project(),
        check_agent_binary_resolvable(),
        check_codex_binary_resolvable(),
    ];

    // Print human-readable results to stderr. A hint is shown whenever one is
    // present — informational passes (e.g. the optional codex agent missing)
    // carry hints too, not just failures.
    for check in &checks {
        if check.ok {
            eprintln!("[✓] {}: {}", check.name, check.detail);
        } else {
            eprintln!("[✗] {}: {}", check.name, check.detail);
        }
        if let Some(hint) = &check.hint {
            eprintln!("    Hint: {}", hint);
        }
    }

    // Print machine-readable JSON to stdout.
    let all_ok = checks.iter().all(|c| c.ok);
    let output = json!({
        "ok": all_ok,
        "checks": checks.iter().map(check_to_json).collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&output).unwrap());

    // Exit 0 if all pass, 1 if any failed.
    if all_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> String {
        v.to_string()
    }

    #[test]
    fn parse_flags_reads_key_value_pairs() {
        let args = vec![s("--dir"), s("/tmp/x"), s("--name"), s("My Proj")];
        let flags = parse_flags(&args).unwrap();
        assert_eq!(flags.get("dir").map(String::as_str), Some("/tmp/x"));
        assert_eq!(flags.get("name").map(String::as_str), Some("My Proj"));
    }

    #[test]
    fn parse_flags_empty_is_ok() {
        let flags = parse_flags(&[]).unwrap();
        assert!(flags.is_empty());
    }

    #[test]
    fn parse_flags_rejects_dangling_key() {
        let args = vec![s("--dir")];
        let err = parse_flags(&args).unwrap_err();
        assert!(err.contains("missing value for --dir"));
    }

    #[test]
    fn parse_flags_rejects_bare_value() {
        let args = vec![s("positional")];
        let err = parse_flags(&args).unwrap_err();
        assert!(err.contains("unexpected argument"));
    }

    #[test]
    fn ensure_project_output_is_camelcase_contract() {
        let resp = EnsureProjectResponse {
            id: s("proj-123"),
            name: s("myrepo"),
            root_path: s("/Users/chris/code/myrepo"),
            created_at: s("2026-07-04T00:00:00.000Z"),
            archived: false,
            created: true,
        };
        let out = ensure_project_output(&resp);
        assert_eq!(out, json!({ "projectId": "proj-123", "created": true }));
        // Stable, parseable JSON with exactly the documented keys.
        assert!(out.get("projectId").is_some());
        assert!(out.get("created").is_some());
    }

    #[test]
    fn create_thread_output_includes_thread_id_and_slug() {
        let resp = CreateThreadResponse {
            id: s("thr-9"),
            project_id: s("proj-123"),
            title: s("How does OAuth work?"),
            slug: s("how-does-oauth-work"),
            initial_question: s("Explain it"),
            status: s("generating"),
            created_at: s("2026-07-04T00:00:00.000Z"),
            updated_at: s("2026-07-04T00:00:00.000Z"),
        };
        let out = create_thread_output(&resp);
        assert_eq!(
            out,
            json!({ "threadId": "thr-9", "slug": "how-does-oauth-work" })
        );
    }

    #[test]
    fn open_output_thread_target() {
        let resp = OpenResponse {
            ok: true,
            project_id: s("proj-123"),
            thread_id: Some(s("thr-9")),
        };
        let out = open_output(&resp);
        assert_eq!(
            out,
            json!({ "ok": true, "projectId": "proj-123", "threadId": "thr-9" })
        );
    }

    #[test]
    fn open_output_project_target_has_null_thread() {
        let resp = OpenResponse {
            ok: true,
            project_id: s("proj-123"),
            thread_id: None,
        };
        let out = open_output(&resp);
        assert_eq!(out["threadId"], serde_json::Value::Null);
        assert_eq!(out["projectId"], json!("proj-123"));
    }

    #[test]
    fn save_artifact_output_stable_camelcase_contract() {
        use conceptify_types::ArtifactIssue;
        let resp = SaveArtifactResponse {
            thread_id: s("thr-42"),
            project_id: s("proj-123"),
            version: 3,
            created_by: s("follow_up"),
            file_path: s("/Users/chris/Documents/conceptify/artifacts/proj-123/threads/slug/artifact.v3.html"),
            warnings: vec![
                ArtifactIssue {
                    code: s("W-ANCHOR-DIAGRAM"),
                    message: s("diagram has thin anchor coverage"),
                },
            ],
        };
        let out = save_artifact_output(&resp);
        assert_eq!(out, json!({ "version": 3, "warningsCount": 1 }));
        // Stable, parseable JSON with exactly the documented keys.
        assert!(out.get("version").is_some());
        assert!(out.get("warningsCount").is_some());
    }

    #[test]
    fn save_artifact_output_no_warnings() {
        let resp = SaveArtifactResponse {
            thread_id: s("thr-42"),
            project_id: s("proj-123"),
            version: 1,
            created_by: s("initial"),
            file_path: s("/path/to/artifact.v1.html"),
            warnings: vec![],
        };
        let out = save_artifact_output(&resp);
        assert_eq!(out, json!({ "version": 1, "warningsCount": 0 }));
    }

    #[test]
    fn check_to_json_pass() {
        let check = Check::pass("test-check", "everything is good");
        let json = check_to_json(&check);
        assert_eq!(json["name"], "test-check");
        assert_eq!(json["ok"], true);
        assert_eq!(json["detail"], "everything is good");
        assert_eq!(json["hint"], serde_json::Value::Null);
    }

    #[test]
    fn check_to_json_fail() {
        let check = Check::fail("test-check", "something is missing", "install it");
        let json = check_to_json(&check);
        assert_eq!(json["name"], "test-check");
        assert_eq!(json["ok"], false);
        assert_eq!(json["detail"], "something is missing");
        assert_eq!(json["hint"], "install it");
    }

    #[test]
    fn check_pass_creates_correct_state() {
        let check = Check::pass("foo", "bar");
        assert_eq!(check.name, "foo");
        assert!(check.ok);
        assert_eq!(check.detail, "bar");
        assert!(check.hint.is_none());
    }

    #[test]
    fn check_fail_creates_correct_state() {
        let check = Check::fail("foo", "bar", "baz");
        assert_eq!(check.name, "foo");
        assert!(!check.ok);
        assert_eq!(check.detail, "bar");
        assert_eq!(check.hint, Some("baz".to_string()));
    }

    fn text_anchor() -> serde_json::Value {
        json!({
            "v": 1,
            "type": "text",
            "cfy_id": "sec-walkthrough",
            "start": 142,
            "end": 210,
            "quote": { "exact": "the token is refreshed here", "prefix": "why ", "suffix": " each time" }
        })
    }

    fn open_comment() -> CommentResponse {
        CommentResponse {
            id: s("c-1"),
            thread_id: s("thr-9"),
            parent_id: None,
            artifact_version: 1,
            anchor: Some(text_anchor()),
            body: s("why refresh here?"),
            status: s("open"),
            answer_html: None,
            anchor_state: s("anchored"),
            created_at: s("2026-07-04T00:00:00.000Z"),
            resolved_at: None,
        }
    }

    /// A reply CommentResponse (no anchor, `parent_id` set).
    fn reply_comment(id: &str, parent: &str, created_at: &str) -> CommentResponse {
        CommentResponse {
            id: s(id),
            thread_id: s("thr-9"),
            parent_id: Some(s(parent)),
            artifact_version: 1,
            anchor: None,
            body: s("follow-up"),
            status: s("open"),
            answer_html: None,
            anchor_state: s("anchored"),
            created_at: s(created_at),
            resolved_at: None,
        }
    }

    #[test]
    fn comment_output_is_camelcase_with_verbatim_snakecase_anchor() {
        let out = comment_output(&open_comment());
        // Top-level keys are camelCase like every other CLI command.
        assert_eq!(out["id"], "c-1");
        assert_eq!(out["threadId"], "thr-9");
        assert_eq!(out["parentId"], serde_json::Value::Null);
        assert_eq!(out["artifactVersion"], 1);
        assert_eq!(out["anchorState"], "anchored");
        assert_eq!(out["answerHtml"], serde_json::Value::Null);
        assert_eq!(out["resolvedAt"], serde_json::Value::Null);
        // The anchor is passed through verbatim — its keys stay snake_case, so
        // the agent sees the exact stored contract.
        assert_eq!(out["anchor"], text_anchor());
        assert_eq!(out["anchor"]["cfy_id"], "sec-walkthrough");
        assert!(out.get("anchor_state").is_none(), "no snake_case top-level leakage");
    }

    #[test]
    fn comment_output_exposes_parent_id_for_replies() {
        let out = comment_output(&reply_comment("r-1", "c-1", "2026-07-04T00:00:01.000Z"));
        assert_eq!(out["parentId"], "c-1");
        assert!(out["anchor"].is_null(), "replies carry no anchor");
    }

    fn context_with_artifact() -> ThreadContextResponse {
        use conceptify_types::{
            ThreadContextArtifact, ThreadContextComment, ThreadContextProject, ThreadContextThread,
        };
        ThreadContextResponse {
            thread: ThreadContextThread {
                id: s("thr-9"),
                title: s("How does OAuth work?"),
                initial_question: s("Explain the OAuth 2.0 authorization code flow."),
                status: s("ready"),
                slug: s("how-does-oauth-work"),
            },
            project: ThreadContextProject {
                id: s("proj-1"),
                name: s("myrepo"),
                root_path: s("/Users/chris/code/myrepo"),
            },
            latest_artifact: Some(ThreadContextArtifact {
                version: 2,
                file_path: s("/Users/chris/Documents/conceptify/artifacts/proj-1/threads/how-does-oauth-work/artifact.v2.html"),
            }),
            // One open root with a two-reply exchange history.
            open_comments: vec![ThreadContextComment {
                comment: open_comment(),
                replies: vec![
                    reply_comment("r-1", "c-1", "2026-07-04T00:00:01.000Z"),
                    reply_comment("r-2", "c-1", "2026-07-04T00:00:02.000Z"),
                ],
            }],
        }
    }

    #[test]
    fn get_context_output_exposes_prompt_fields_and_nested_reply_chains() {
        let out = get_context_output(&context_with_artifact());
        assert_eq!(out["threadId"], "thr-9");
        assert_eq!(out["question"], "Explain the OAuth 2.0 authorization code flow.");
        assert_eq!(out["projectRoot"], "/Users/chris/code/myrepo");
        assert_eq!(out["artifactVersion"], 2);
        assert!(out["artifactPath"]
            .as_str()
            .unwrap()
            .ends_with("artifact.v2.html"));
        let open = out["openComments"].as_array().unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0]["id"], "c-1");
        assert!(open[0]["parentId"].is_null(), "root parentId is null");
        // Anchor round-trips verbatim through the context shaper too.
        assert_eq!(open[0]["anchor"]["cfy_id"], "sec-walkthrough");
        // The ordered reply chain nests under the root.
        let replies = open[0]["replies"].as_array().unwrap();
        let ids: Vec<&str> = replies.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["r-1", "r-2"]);
        assert_eq!(replies[0]["parentId"], "c-1");
    }

    #[test]
    fn get_context_output_null_artifact_when_thread_has_none() {
        let mut ctx = context_with_artifact();
        ctx.latest_artifact = None;
        ctx.open_comments.clear();
        let out = get_context_output(&ctx);
        assert_eq!(out["artifactVersion"], serde_json::Value::Null);
        assert_eq!(out["artifactPath"], serde_json::Value::Null);
        assert_eq!(out["openComments"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn list_comments_output_is_a_bare_array() {
        let resp = ListCommentsResponse {
            comments: vec![open_comment()],
        };
        let out = list_comments_output(&resp);
        let arr = out.as_array().expect("list-comments emits a JSON array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "c-1");
        assert_eq!(arr[0]["anchor"]["cfy_id"], "sec-walkthrough");
    }

    #[test]
    fn codex_check_is_informational_never_failing() {
        // bead conceptify-e7m.2: codex is an OPTIONAL agent — whether or not
        // it is installed on the machine running this test, the check must
        // report ok=true so doctor's exit code never depends on it.
        let check = check_codex_binary_resolvable();
        assert!(check.ok, "codex absence must not fail doctor: {:?}", check);
        assert_eq!(check.name, "codex-binary-resolvable");
    }

    #[test]
    fn resolve_comment_output_is_stable_contract() {
        let mut c = open_comment();
        c.status = s("applied");
        let out = resolve_comment_output(&c);
        assert_eq!(out, json!({ "ok": true, "id": "c-1", "status": "applied" }));
    }

    #[test]
    fn resolve_flags_defaults_to_answered_without_applied() {
        let args = vec![s("--id"), s("c-1"), s("--answer-file"), s("/tmp/a.html")];
        let (id, file, applied) = resolve_flags(&args).unwrap();
        assert_eq!(id, "c-1");
        assert_eq!(file, "/tmp/a.html");
        assert!(!applied);
    }

    #[test]
    fn resolve_flags_accepts_applied_in_any_position() {
        // Leading.
        let args = vec![s("--applied"), s("--id"), s("c-1"), s("--answer-file"), s("/tmp/a.html")];
        let (_, _, applied) = resolve_flags(&args).unwrap();
        assert!(applied);
        // Trailing.
        let args = vec![s("--id"), s("c-1"), s("--answer-file"), s("/tmp/a.html"), s("--applied")];
        let (id, file, applied) = resolve_flags(&args).unwrap();
        assert_eq!(id, "c-1");
        assert_eq!(file, "/tmp/a.html");
        assert!(applied);
    }

    #[test]
    fn resolve_flags_rejects_missing_required_flags() {
        let err = resolve_flags(&[s("--id"), s("c-1")]).unwrap_err();
        assert!(err.contains("resolve-comment requires"));
        let err = resolve_flags(&[s("--answer-file"), s("/tmp/a.html")]).unwrap_err();
        assert!(err.contains("resolve-comment requires"));
    }
}
