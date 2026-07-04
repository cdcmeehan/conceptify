//! Conceptify CLI — thin, fast client for the local HTTP API (PRD §5.2).
//!
//! Every command implements the launch-and-wait contract: probe GET /health
//! (using the port file, fallback 4477) → on failure run `open -a Conceptify`
//! → poll up to ~10s → proceed. JSON output on stdout for agent consumption;
//! human-readable errors on stderr; non-zero exit when the app can't be
//! reached.

use conceptify_types::HealthResponse;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_PORT: u16 = 4477;
const POLL_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: conceptify <command> [args...]");
        eprintln!("Commands: status");
        return ExitCode::FAILURE;
    }

    let command = &args[1];

    match command.as_str() {
        "status" => cmd_status(),
        _ => {
            eprintln!("Unknown command: {}", command);
            eprintln!("Available commands: status");
            ExitCode::FAILURE
        }
    }
}

/// Returns the path to the port file written by the server.
fn port_file_path() -> PathBuf {
    let data_dir = dirs::data_dir()
        .expect("failed to determine user data directory");
    data_dir.join("conceptify").join("port")
}

/// Returns the path to the bearer token file.
fn token_file_path() -> PathBuf {
    let data_dir = dirs::data_dir()
        .expect("failed to determine user data directory");
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
/// wrong).
#[allow(dead_code)]
fn read_token() -> io::Result<String> {
    fs::read_to_string(token_file_path())
        .map(|s| s.trim().to_string())
}

/// Probes GET /health at the given port. Returns Ok(response) if the endpoint
/// responds with a 200 and valid JSON matching the HealthResponse shape.
fn probe_health(port: u16) -> Result<HealthResponse, String> {
    let url = format!("http://127.0.0.1:{}/health", port);

    match ureq::get(&url)
        .timeout(Duration::from_secs(2))
        .call()
    {
        Ok(response) => {
            match response.into_json::<HealthResponse>() {
                Ok(health) => Ok(health),
                Err(e) => Err(format!("health endpoint returned invalid JSON: {}", e)),
            }
        }
        Err(ureq::Error::Status(code, _)) => {
            Err(format!("health endpoint returned status {}", code))
        }
        Err(ureq::Error::Transport(e)) => {
            Err(format!("failed to reach health endpoint: {}", e))
        }
    }
}

/// Attempts to launch the Conceptify app via `open -a Conceptify`. Returns
/// Ok(()) if the command was invoked successfully (not a guarantee the app
/// actually launched or will become healthy — the caller must still poll).
fn launch_app() -> io::Result<()> {
    let status = Command::new("open")
        .arg("-a")
        .arg("Conceptify")
        .status()?;

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

/// `conceptify status` — prints app/API health and version as JSON.
fn cmd_status() -> ExitCode {
    match ensure_app_healthy() {
        Ok(port) => {
            // Re-probe to get the current health info (we know it's healthy,
            // but we want the version and status fields for JSON output).
            match probe_health(port) {
                Ok(health) => {
                    let output = serde_json::json!({
                        "service": health.service,
                        "status": health.status,
                        "version": health.version,
                        "port": port,
                    });
                    println!("{}", serde_json::to_string_pretty(&output).unwrap());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    // This shouldn't happen (we just confirmed it's healthy),
                    // but handle it gracefully.
                    eprintln!("Error re-probing health after success: {}", e);
                    ExitCode::FAILURE
                }
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            ExitCode::FAILURE
        }
    }
}
