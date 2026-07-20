//! Tauri invoke commands for the terminal-voice child session (Architecture A).
//!
//! These drive [`crate::chat_process::VoiceChatProcess`]:
//!   * [`start_voice_session`] — stop the shell wake listener, set the
//!     `VoiceChildActive` flag, spawn the child, return its session uuid.
//!   * [`stop_voice_session`] — close child stdin, wait/kill, clear the flag,
//!     restart the wake listener if it was running before the session.
//!   * [`voice_session_active`] — read the flag.
//!
//! While the flag is set, `record_with_vad` and `run_voice_pipeline` refuse to
//! open the mic (the child exclusively owns the audio devices).

use tauri::{AppHandle, Manager, State};

use crate::audio::{self, WakeListenerState};
use crate::chat_process::VoiceChatProcess;
use crate::commands::audio_cmd::PipelineActive;
use crate::process::ServerProcess;

/// Start a terminal-voice child session.
///
/// Returns the generated session uuid. The child exclusively owns the mic and
/// speaker for its lifetime; the shell's own wake listener and mic pipeline are
/// suspended (the child does its own wake-word + barge-in in-process).
#[tauri::command]
pub async fn start_voice_session(
    app: AppHandle,
    voice: State<'_, VoiceChatProcess>,
    wake_state: State<'_, WakeListenerState>,
    server: State<'_, ServerProcess>,
) -> Result<String, String> {
    // Serialize with any concurrent start/stop.
    let _guard = voice.lifecycle_guard().await;

    // Refuse to double-spawn.
    if voice.is_active() {
        return Err("voice session already active".to_string());
    }

    // Note whether the shell wake listener was running so we can restore it,
    // then stop it — the child owns the mic while it lives.
    let wake_was_running = wake_state
        .is_running
        .load(std::sync::atomic::Ordering::SeqCst);
    voice.set_wake_was_running(wake_was_running);
    if wake_was_running {
        audio::stop_wake_listener(&wake_state);
    }

    // Resolve the resource dir the same way the setup hook does, so the child
    // binary is found via the same candidate paths (incl. POND_SERVER_BIN).
    let resource_dir = app
        .path()
        .resource_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));

    match voice.spawn(&app, &resource_dir) {
        Ok(session_id) => {
            tracing::info!("voice session started: {session_id}");
            // Keep `server` referenced so the binding is used even when the
            // resolver falls back to POND_SERVER_BIN / candidate paths — it is
            // the same instance the child will persist turns alongside.
            let _ = server.get_url();
            Ok(session_id)
        }
        Err(e) => {
            // Spawn failed — undo the mic handoff so the shell is not left
            // wedged with a suspended wake listener and no child.
            tracing::warn!("voice session spawn failed: {e}");
            if wake_was_running {
                restart_wake_listener(&app, &wake_state, &server).await;
            }
            voice.set_wake_was_running(false);
            Err(e)
        }
    }
}

/// Stop the active terminal-voice child session.
///
/// Closes the child's stdin (clean-exit signal), waits ~3s, then kills if still
/// alive. Clears the `VoiceChildActive` flag and restores the wake listener if
/// it was running before the session started.
#[tauri::command]
pub async fn stop_voice_session(
    app: AppHandle,
    voice: State<'_, VoiceChatProcess>,
    wake_state: State<'_, WakeListenerState>,
    server: State<'_, ServerProcess>,
) -> Result<(), String> {
    let _guard = voice.lifecycle_guard().await;

    if !voice.is_active() {
        // Nothing to stop, but make sure any stale wake-restore intent is clear.
        voice.set_wake_was_running(false);
        return Ok(());
    }

    // Close stdin (clean-exit signal), then wait up to the grace period for the
    // reader thread to observe stdout EOF and clear the active flag. Using
    // async sleeps yields the runtime rather than blocking a worker.
    voice.close_stdin();

    let deadline = tokio::time::Instant::now() + VoiceChatProcess::graceful_stop_timeout();
    let poll = VoiceChatProcess::graceful_stop_poll();
    while tokio::time::Instant::now() < deadline {
        if !voice.is_active() {
            tracing::info!("voice child exited cleanly after stdin close");
            break;
        }
        tokio::time::sleep(poll).await;
    }

    // If the child is still alive after the grace period, force kill and reap.
    if voice.is_active() {
        tracing::warn!("voice child did not exit within grace period; killing");
        voice.kill();
    }

    let should_restart = voice.wake_was_running();
    voice.set_wake_was_running(false);

    if should_restart {
        restart_wake_listener(&app, &wake_state, &server).await;
    }

    Ok(())
}

/// Whether a terminal-voice child session is currently active.
#[tauri::command]
pub fn voice_session_active(voice: State<'_, VoiceChatProcess>) -> bool {
    voice.is_active()
}

/// Restart the shell wake listener with the user's configured wake word.
///
/// Reads `voice_wake_word` (+ calibrated variants) from the server settings so
/// the restored listener matches what the user had before the session. Best
/// effort: on any failure it logs and returns — the shell should not wedge if
/// settings are unreachable.
async fn restart_wake_listener(
    app: &AppHandle,
    wake_state: &WakeListenerState,
    server: &ServerProcess,
) {
    let base_url = server.get_url();
    let (wake_word, variants) = match fetch_wake_settings(&base_url).await {
        Some(v) => v,
        None => {
            tracing::debug!("wake settings unavailable; not restarting wake listener");
            return;
        }
    };
    if wake_word.trim().is_empty() {
        return;
    }

    let pipeline_active = app.state::<PipelineActive>().0.clone();
    if let Err(e) = audio::start_wake_listener(
        wake_state,
        wake_word,
        variants,
        base_url,
        app.clone(),
        pipeline_active,
    ) {
        tracing::warn!("failed to restart wake listener after voice session: {e}");
    }
}

/// Fetch `voice_wake_word` and `voice_wake_word_transcriptions` from the server
/// settings endpoint. Returns `None` on any error.
async fn fetch_wake_settings(base_url: &str) -> Option<(String, Vec<String>)> {
    let resp = reqwest::Client::new()
        .get(format!("{base_url}/api/v1/settings"))
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let value: serde_json::Value = resp.json().await.ok()?;
    let wake_word = value
        .get("voice_wake_word")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let variants = value
        .get("voice_wake_word_transcriptions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some((wake_word, variants))
}
