//! The real capture device: opening a stream and pushing frames. Everything decidable — the
//! privacy gate, the state machine, buffering, format normalisation — lives in [`crate::owner`]
//! and [`crate::ring`], where it is testable without a sound card.

use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::owner::{CaptureDevice, MicShared};
use crate::ring::{i16_to_f32, to_mono_f32, u16_to_f32};

/// `cpal::Stream` is `!Send` — CoreAudio attaches property listeners to the creating thread.
/// It is created on, used by, and dropped on the owner thread and never touched elsewhere, so
/// the transfer is sound. The field is never read: holding the stream alive is what keeps the
/// device open, and dropping it is what closes it.
struct SendableStream(#[allow(dead_code)] cpal::Stream);
unsafe impl Send for SendableStream {}

/// The default system input device.
#[derive(Default)]
pub struct CpalCapture {
    stream: Option<SendableStream>,
    /// Preferred device name; `None` uses the host default.
    preferred: Option<String>,
}

impl CpalCapture {
    pub fn new() -> Self {
        Self::default()
    }

    /// Capture from a named device instead of the host default.
    pub fn with_device(name: impl Into<String>) -> Self {
        Self {
            stream: None,
            preferred: Some(name.into()),
        }
    }

    fn pick(&self) -> Result<cpal::Device, String> {
        let host = cpal::default_host();
        if let Some(want) = &self.preferred {
            let mut found = host
                .input_devices()
                .map_err(|e| format!("cannot enumerate input devices: {e}"))?;
            if let Some(d) = found.find(|d| d.name().map(|n| &n == want).unwrap_or(false)) {
                return Ok(d);
            }
            tracing::warn!(
                device = %want,
                "configured input device not found; falling back to the system default"
            );
        }
        host.default_input_device()
            .ok_or_else(|| "no audio input device found".to_string())
    }
}

/// Every input device this host exposes, for a settings picker.
///
/// There was no device enumeration anywhere before — every call site took
/// `default_input_device()` and a user with the wrong default had no recourse.
pub fn input_device_names() -> Vec<String> {
    cpal::default_host()
        .input_devices()
        .map(|ds| ds.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

impl CaptureDevice for CpalCapture {
    fn start(&mut self, shared: Arc<MicShared>) -> Result<(), String> {
        self.stop();

        let device = self.pick()?;
        let supported = device
            .default_input_config()
            .map_err(|e| format!("no usable input config: {e}"))?;
        let sample_rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();

        // Resample once, here, so subscribers only ever see 16 kHz mono f32.
        // Previously each consumer resampled its own window on every pass —
        // the wake-word detector did it every 300 ms over 2.5 s of audio.
        let push = move |mono: Vec<f32>| {
            let at_16k = pond_voice::dsp::resample_to_16k(&mono, sample_rate);
            let mut ring = shared.ring.lock().unwrap_or_else(|e| e.into_inner());
            ring.push(&at_16k);
            drop(ring);
            shared.bump();
        };

        let on_err = |e| tracing::warn!(error = %e, "input stream error");

        let stream = match format {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config,
                move |d: &[f32], _: &_| push(to_mono_f32(d, channels, |s| s)),
                on_err,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config,
                move |d: &[i16], _: &_| push(to_mono_f32(d, channels, i16_to_f32)),
                on_err,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_input_stream(
                &config,
                move |d: &[u16], _: &_| push(to_mono_f32(d, channels, u16_to_f32)),
                on_err,
                None,
            ),
            other => return Err(format!("unsupported sample format {other:?}")),
        }
        .map_err(|e| format!("could not open input stream: {e}"))?;

        stream
            .play()
            .map_err(|e| format!("could not start input stream: {e}"))?;

        tracing::debug!(
            sample_rate,
            channels,
            ?format,
            "microphone open (normalised to 16 kHz mono)"
        );
        self.stream = Some(SendableStream(stream));
        Ok(())
    }

    fn stop(&mut self) {
        // Dropping the stream closes the device — that is what makes
        // `mic_enabled = false` actually turn the OS indicator off.
        self.stream = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Enumeration must never panic on a headless box or one with no input.
    #[test]
    fn enumeration_is_safe_with_or_without_devices() {
        let _ = input_device_names();
    }

    /// An unknown name falls back to the default rather than failing outright —
    /// a stale device setting should not make the assistant deaf.
    #[test]
    fn an_unknown_preferred_device_falls_back_to_the_default() {
        let cap = CpalCapture::with_device("no-such-device-8f3a2b");
        match cap.pick() {
            // Fell back to a real default.
            Ok(_) => {}
            // Or this machine genuinely has no input at all.
            Err(e) => assert!(e.contains("no audio input device"), "{e}"),
        }
    }

    #[test]
    fn stop_is_idempotent() {
        let mut cap = CpalCapture::new();
        cap.stop();
        cap.stop();
    }
}
