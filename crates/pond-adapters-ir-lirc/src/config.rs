use std::path::Path;

/// Configuration for the IR/LIRC adapter.
///
/// Reads from environment variables; all fields are optional — the adapter
/// runs fine with defaults as long as LIRC is present on the system.
#[derive(Debug, Clone)]
pub struct IrConfig {
    /// Override the default LIRC remote name. Per-device address takes priority.
    /// Env var: `IR_LIRC_REMOTE`
    pub default_remote: Option<String>,

    /// Path to the lircd socket. Defaults to `/var/run/lirc/lircd`.
    /// Env var: `IR_LIRC_SOCKET`
    pub socket_path: String,

    /// Milliseconds to wait between repeated keypresses (e.g. volume steps).
    /// Env var: `IR_KEY_REPEAT_DELAY_MS` (default: 80)
    pub key_repeat_delay_ms: u64,
}

impl Default for IrConfig {
    fn default() -> Self {
        Self {
            default_remote: std::env::var("IR_LIRC_REMOTE").ok(),
            socket_path: std::env::var("IR_LIRC_SOCKET")
                .unwrap_or_else(|_| "/var/run/lirc/lircd".into()),
            key_repeat_delay_ms: std::env::var("IR_KEY_REPEAT_DELAY_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(80),
        }
    }
}

impl IrConfig {
    pub fn from_env() -> Self {
        Self::default()
    }

    /// Returns true if LIRC hardware appears available on this system.
    ///
    /// Checks for the lircd socket (created when lircd is running) and falls
    /// back to checking whether the `irsend` binary is in PATH.
    pub fn is_configured() -> bool {
        let socket = std::env::var("IR_LIRC_SOCKET")
            .unwrap_or_else(|_| "/var/run/lirc/lircd".into());
        if Path::new(&socket).exists() {
            return true;
        }
        // Fallback: irsend in PATH
        std::process::Command::new("which")
            .arg("irsend")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}
