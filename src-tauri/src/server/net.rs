//! Port selection (PRD §5.1, §9 S1).
//!
//! Binds `127.0.0.1:4477`. On `AddrInUse`, probes the occupant's
//! `GET /health` — if it looks like another Conceptify instance, we defer to
//! it (log and keep running without a second server; the single-instance
//! guard is expected to prevent this in practice, so this is a defensive
//! fallback, not the common path). Otherwise walks `4477..=4487` looking for
//! a free port.

use std::io;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const FIRST_PORT: u16 = 4477;
pub const LAST_PORT: u16 = 4487;

const PROBE_TIMEOUT: Duration = Duration::from_millis(750);

/// Substring present in our own `/health` response body; used to recognize
/// "the thing squatting on our port is another Conceptify instance" versus
/// some unrelated process.
const HEALTH_MARKER: &str = "\"service\":\"conceptify\"";

pub enum BindOutcome {
    Bound(TcpListener, u16),
    /// Another Conceptify instance already answers on `port`; we deferred.
    DeferToExisting(u16),
    /// Every port in the range was occupied by something else.
    NoPortAvailable,
}

pub async fn bind_with_fallback() -> BindOutcome {
    for port in FIRST_PORT..=LAST_PORT {
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => return BindOutcome::Bound(listener, port),
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
                eprintln!(
                    "[conceptify-server] port {port} is already in use; probing its /health..."
                );
                if probe_is_conceptify(port).await {
                    eprintln!(
                        "[conceptify-server] port {port} is served by another Conceptify instance; deferring to it"
                    );
                    return BindOutcome::DeferToExisting(port);
                }
                eprintln!(
                    "[conceptify-server] port {port} occupied by an unrelated process; trying next port"
                );
            }
            Err(e) => {
                eprintln!("[conceptify-server] failed to bind port {port}: {e}");
            }
        }
    }
    BindOutcome::NoPortAvailable
}

/// Best-effort check: does `GET /health` on `port` look like our own
/// server's response? Any failure (connection refused, timeout, garbage
/// response) is treated as "not us".
async fn probe_is_conceptify(port: u16) -> bool {
    let probe = async {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.ok()?;
        let request =
            b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
        stream.write_all(request).await.ok()?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.ok()?;
        Some(buf)
    };

    match tokio::time::timeout(PROBE_TIMEOUT, probe).await {
        Ok(Some(buf)) => String::from_utf8_lossy(&buf).contains(HEALTH_MARKER),
        _ => false,
    }
}
