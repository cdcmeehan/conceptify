//! Bearer token auth (PRD §9 S1).
//!
//! A random token is generated on first run and persisted with `0600`
//! permissions so only the owning user can read it. Every route except
//! `GET /health` requires `Authorization: Bearer <token>`; anything else
//! (missing header, wrong scheme, wrong token) gets a 401. This is
//! containment against other local processes / browser-page localhost
//! probing, not adversarial hardening (single-user machine, PRD §9).

use std::io;
use std::io::Write;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use rand::RngCore;

use super::paths;
use super::state::ApiState;

const TOKEN_BYTES: usize = 32;

/// Load the persisted token, generating and persisting a new one on first
/// run. Idempotent across restarts: an existing token file is reused so the
/// CLI's cached credentials keep working.
pub fn load_or_create_token() -> io::Result<String> {
    let path = paths::token_path()?;

    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            // Re-assert perms in case the file was created (or copied) with
            // looser permissions than we'd write ourselves.
            set_owner_only_perms(&path)?;
            return Ok(trimmed.to_string());
        }
    }

    let mut bytes = [0u8; TOKEN_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    let token = hex_encode(&bytes);

    // Write via temp + rename so a crash never leaves a partially-written
    // token file behind (PRD N4).
    let tmp_path = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp_path)?;
        f.write_all(token.as_bytes())?;
        f.sync_all()?;
    }
    set_owner_only_perms(&tmp_path)?;
    std::fs::rename(&tmp_path, &path)?;

    Ok(token)
}

#[cfg(unix)]
fn set_owner_only_perms(path: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_owner_only_perms(_path: &std::path::Path) -> io::Result<()> {
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// axum middleware: rejects any request whose `Authorization` header isn't
/// exactly `Bearer <token>`.
pub async fn require_bearer_token<R: tauri::Runtime>(
    State(state): State<ApiState<R>>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let presented = header.and_then(|h| h.strip_prefix("Bearer "));

    match presented {
        Some(t) if t == state.token.as_ref() => Ok(next.run(req).await),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}
