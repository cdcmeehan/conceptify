//! Filesystem locations for the API's runtime artifacts.
//!
//! PRD §5.1 / §9 (S1): the bearer token and the actually-bound port live in
//! `~/Library/Application Support/conceptify/`, deliberately *not* nested
//! under the reverse-DNS bundle identifier Tauri would otherwise use, so the
//! `conceptify` CLI (a separate binary with no Tauri context) can find them
//! with a plain, hardcoded relative path.

use std::io;
use std::path::PathBuf;

/// `~/Library/Application Support/conceptify` (created if missing).
pub fn app_support_dir() -> io::Result<PathBuf> {
    let base = dirs::data_dir().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "could not resolve the platform data directory",
        )
    })?;
    let dir = base.join("conceptify");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// `~/Library/Application Support/conceptify/token`
pub fn token_path() -> io::Result<PathBuf> {
    Ok(app_support_dir()?.join("token"))
}

/// `~/Library/Application Support/conceptify/port`
pub fn port_path() -> io::Result<PathBuf> {
    Ok(app_support_dir()?.join("port"))
}

/// Persist the actually-bound port for the CLI to discover. Written via
/// temp + rename so a crash mid-write never leaves a truncated file (PRD
/// N4).
pub fn write_port_file(port: u16) -> io::Result<()> {
    let path = port_path()?;
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, port.to_string())?;
    std::fs::rename(&tmp_path, &path)?;
    Ok(())
}
