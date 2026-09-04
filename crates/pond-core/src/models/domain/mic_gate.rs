//! Process-wide microphone permission: the enforcement of `Settings::mic_enabled`. It is a
//! global because two adapter crates need it and the capture entry points are free functions.
//! Enforcement means refusing to OPEN the device — capturing and discarding still lights the OS
//! mic indicator — so every call site checks this gate before `default_input_device()`.

use std::sync::atomic::{AtomicBool, Ordering};

/// Default open: an install that has never touched the setting still has a
/// working microphone, and `Settings::mic_enabled` itself defaults to true.
static MIC_ENABLED: AtomicBool = AtomicBool::new(true);

/// Microphone capture was refused because the user turned the mic off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MicDisabled;

impl std::fmt::Display for MicDisabled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "microphone is disabled in Settings (mic_enabled = false)"
        )
    }
}

impl std::error::Error for MicDisabled {}

/// Apply the current `Settings::mic_enabled`.
///
/// Called at startup and whenever settings change, so revoking permission takes
/// effect on the next capture attempt rather than at the next restart.
pub fn set_mic_enabled(enabled: bool) {
    let previous = MIC_ENABLED.swap(enabled, Ordering::SeqCst);
    if previous != enabled {
        tracing::info!(
            mic_enabled = enabled,
            "microphone permission changed; capture will {} on the next attempt",
            if enabled { "resume" } else { "be refused" }
        );
    }
}

/// Whether the microphone may be opened.
pub fn mic_enabled() -> bool {
    MIC_ENABLED.load(Ordering::SeqCst)
}

/// Guard to place immediately before opening an input device.
pub fn ensure_mic_enabled() -> Result<(), MicDisabled> {
    if mic_enabled() {
        Ok(())
    } else {
        Err(MicDisabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These mutate a process global, so they share one lock rather than
    /// racing each other under the test harness's thread pool.
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_gate(f: impl FnOnce()) {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = mic_enabled();
        f();
        set_mic_enabled(saved);
    }

    #[test]
    fn open_by_default() {
        with_gate(|| {
            set_mic_enabled(true);
            assert!(mic_enabled());
            assert!(ensure_mic_enabled().is_ok());
        });
    }

    #[test]
    fn disabling_refuses_capture() {
        with_gate(|| {
            set_mic_enabled(false);
            assert!(!mic_enabled());
            assert_eq!(ensure_mic_enabled(), Err(MicDisabled));
        });
    }

    #[test]
    fn the_setting_can_be_toggled_back_on_without_a_restart() {
        with_gate(|| {
            set_mic_enabled(false);
            assert!(ensure_mic_enabled().is_err());
            set_mic_enabled(true);
            assert!(ensure_mic_enabled().is_ok());
        });
    }

    /// The message reaches the user through capture errors, so it has to name
    /// the setting rather than read like a hardware fault.
    #[test]
    fn the_refusal_names_the_setting() {
        let msg = MicDisabled.to_string();
        assert!(msg.contains("mic_enabled"), "{msg}");
        assert!(msg.contains("Settings"), "{msg}");
    }
}
