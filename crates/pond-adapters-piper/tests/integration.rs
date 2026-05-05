//! Integration tests for PiperOutput.
//!
//! These tests exercise the subprocess-based TTS adapter without requiring a
//! real piper binary or audio device.  They verify:
//!
//!   1. `speak()` surfaces a useful error when the binary path is wrong.
//!   2. `speak()` propagates a non-zero subprocess exit code as an `Err`.
//!   3. `with_espeak_data()` appends the `--espeak_data` flag to the command.
//!
//! The live smoke test at the bottom requires a real piper binary and model
//! and is ignored by default.

use pond_adapters_piper::PiperOutput;
use pond_core::ports::voice_output::VoiceOutput;
use std::path::PathBuf;

// ── Error handling ────────────────────────────────────────────────────────────

/// When the piper binary path does not exist, `speak()` must return an `Err`
/// with a message that names the missing path so the user can diagnose the
/// problem.
#[tokio::test]
async fn speak_returns_err_for_nonexistent_binary() {
    let tts = PiperOutput::new(
        PathBuf::from("/nonexistent/path/to/piper"),
        PathBuf::from("/nonexistent/model.onnx"),
    );
    let err = tts.speak("hello").await.unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("piper") || err.to_string().contains("nonexistent"),
        "expected error referencing missing binary; got: {}",
        err
    );
}

/// When the piper subprocess exits with a non-zero status, `speak()` must
/// return an `Err` that includes the exit status.
///
/// `/usr/bin/false` is a POSIX utility that always exits with status 1 and
/// produces no output — a convenient stand-in for a broken piper installation.
#[tokio::test]
#[cfg(unix)]
async fn speak_returns_err_when_subprocess_exits_nonzero() {
    let tts = PiperOutput::new(PathBuf::from("/usr/bin/false"), PathBuf::from("model.onnx"));
    let err = tts.speak("hello").await.unwrap_err();
    assert!(
        err.to_string().contains("piper exited"),
        "expected 'piper exited' in error; got: {}",
        err
    );
}

// ── Builder API ───────────────────────────────────────────────────────────────

/// `with_espeak_data()` must add `--espeak_data <path>` to the argument list.
#[test]
fn with_espeak_data_appends_flag_and_path() {
    let espeak_path = PathBuf::from("/data/espeak-ng-data");
    let tts = PiperOutput::new(PathBuf::from("piper"), PathBuf::from("model.onnx"))
        .with_espeak_data(espeak_path.clone());

    let args = tts.build_args();
    let espeak_flag_pos = args
        .iter()
        .position(|a| a == "--espeak_data")
        .expect("--espeak_data flag missing from args");

    assert_eq!(
        args[espeak_flag_pos + 1],
        espeak_path.to_string_lossy().as_ref(),
        "--espeak_data value should follow the flag"
    );
}

/// When `with_espeak_data()` is not called, the `--espeak_data` flag must be
/// absent so piper uses its built-in data path.
#[test]
fn espeak_data_absent_by_default() {
    let tts = PiperOutput::new(PathBuf::from("piper"), PathBuf::from("model.onnx"));
    assert!(
        !tts.build_args().contains(&"--espeak_data".to_string()),
        "--espeak_data should not be present when not configured"
    );
}

/// Confirm PiperOutput can be stored as a `VoiceOutput` trait object, which
/// is how `ChatService` holds it at runtime.
#[test]
fn piper_output_is_voice_output_trait_object() {
    use pond_core::ports::voice_output::VoiceOutput;
    use std::sync::Arc;
    let _: Arc<dyn VoiceOutput> = Arc::new(PiperOutput::new(
        PathBuf::from("piper"),
        PathBuf::from("model.onnx"),
    ));
}

// ── Live smoke test ───────────────────────────────────────────────────────────

/// Run with:
///   `cargo test -p pond-adapters-piper -- --ignored live_speak`
///
/// Requires:
///   - `piper` binary at `/data/bin/piper` (or update the path below)
///   - `en_US-lessac-medium.onnx` model at `/data/models/tts/`
///   - A working audio output device
#[tokio::test]
#[ignore = "requires piper binary, voice model, and audio output device"]
async fn live_speak_with_real_piper() {
    let tts = PiperOutput::new(
        PathBuf::from("/data/bin/piper"),
        PathBuf::from("/data/models/tts/en_US-lessac-medium.onnx"),
    );
    tts.speak("Goose in a pond is alive and well.")
        .await
        .expect("live piper speak failed");
}
