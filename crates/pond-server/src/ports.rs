//! Compile-time port assignments for all GIAP services.
//!
//! **All port numbers live here and nowhere else.**  Edit these constants and
//! recompile to reassign ports across the entire server — no flags, no config
//! files at runtime, no user-facing port management.
//!
//! If a service cannot bind its configured port, GIAP automatically tries the
//! next port in arithmetic sequence (`base`, `base+1`, `base+2`, …) up to
//! [`MAX_TRIES`] attempts.  The process that eventually binds tells callers
//! what port was actually used so dependent services can connect to the right
//! address.

/// GIAP REST API + web dashboard (all interfaces, 0.0.0.0).
pub const API_SERVER: u16 = 4000;

/// whisper.cpp ASR subprocess (loopback only).
pub const WHISPER: u16 = 9000;

/// llamafile LLM subprocess (loopback only).
pub const LLAMAFILE: u16 = 8080;

/// Piper TTS in-process HTTP bridge (loopback only).
pub const PIPER_TTS: u16 = 8282;


/// How many sequential port numbers to try before giving up.
pub const MAX_TRIES: u16 = 10;

// ─────────────────────────────────────────────────────────────────────────────
// Utilities
// ─────────────────────────────────────────────────────────────────────────────

/// Bind a TCP listener on `host`, trying ports `start`, `start+1`, … up to
/// [`MAX_TRIES`] attempts.  Returns the bound listener and the actual port.
///
/// Use this for services **owned by GIAP** (API server, piper-http bridge)
/// where we hold the socket ourselves.
pub async fn bind_with_fallback(
    host: &str,
    start: u16,
) -> anyhow::Result<(tokio::net::TcpListener, u16)> {
    for offset in 0..MAX_TRIES {
        if let Some(port) = start.checked_add(offset) {
            let addr = format!("{host}:{port}");
            if let Ok(listener) = tokio::net::TcpListener::bind(&addr).await {
                if offset > 0 {
                    tracing::info!("port {} in use — bound on port {}", start, port);
                }
                return Ok((listener, port));
            }
        }
    }
    anyhow::bail!(
        "no available port in {}..{} on {}",
        start,
        start + MAX_TRIES - 1,
        host
    )
}

/// Find a free port for an **external child process** without holding the
/// socket (the child will bind it moments later).
///
/// Always probes `127.0.0.1`.  There is a small TOCTOU window, but on a
/// single-user embedded device this is acceptable.  Returns `None` if no port
/// is free within [`MAX_TRIES`] of `start`.
pub async fn find_free_port(start: u16) -> Option<u16> {
    for offset in 0..MAX_TRIES {
        if let Some(port) = start.checked_add(offset) {
            if tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
                .await
                .is_ok()
            {
                return Some(port);
            }
        }
    }
    None
}
