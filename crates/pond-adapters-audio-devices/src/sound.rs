//! One sound test: a speaker plays the test sound while a microphone listens for it.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, StreamTrait};
use pond_audio::MicShared;
use pond_core::models::domain::audio_device::{AudioDirection, PairOutcome};

use crate::probe;
use crate::tone::{self, Listener};

/// What the room sounds like before the test sound, to judge the sound against.
const ROOM: Duration = Duration::from_millis(250);
/// Listening on after the sound ends: a Bluetooth speaker, or ALSA's dmix on a USB card, can lag
/// by several hundred milliseconds.
const TAIL: Duration = Duration::from_millis(900);
/// How long a microphone may take to deliver its first frames.
const FIRST_FRAMES: Duration = Duration::from_millis(1_500);
const TONE_RATE: u32 = 48_000;

fn failed(reason: impl ToString) -> PairOutcome {
    PairOutcome::Failed {
        reason: reason.to_string(),
    }
}

fn play(speaker: &str) -> Result<(), String> {
    pond_audio_out::route::play_samples_exactly(
        Some(speaker),
        tone::tone_samples(TONE_RATE),
        TONE_RATE,
        1.0,
    )
    .map_err(|e| e.to_string())
}

/// Other audio pauses for the test sound, as for every sound the pond makes.
fn with_quiet<T>(f: impl FnOnce() -> T) -> T {
    let quiet = pond_core::models::services::voice::quiet::installed();
    let _hold = quiet.as_ref().map(|q| {
        let hold = q.hold();
        q.until_quiet_blocking(Duration::from_millis(400));
        hold
    });
    f()
}

/// Test with a microphone the mic owner already has open, read from its ring.
pub fn through_open_mic(speaker: &str, shared: &MicShared) -> PairOutcome {
    with_quiet(|| {
        let start = shared.written();
        std::thread::sleep(ROOM);
        let tone_at = shared.written();
        if let Err(e) = play(speaker) {
            return failed(e);
        }
        std::thread::sleep(TAIL);
        let heard = shared.written().saturating_sub(start) as usize;
        let mut listener = Listener::new(pond_audio::CAPTURE_RATE_HZ);
        let samples = shared.recent(heard);
        let missing = heard.saturating_sub(samples.len());
        let tone_from = (tone_at.saturating_sub(start) as usize).saturating_sub(missing);
        let window = (pond_audio::CAPTURE_RATE_HZ / 50) as usize;
        listener.push(samples);
        listener.verdict(tone_from / window)
    })
}

/// Test with a microphone opened for the purpose, then closed.
pub fn through_mic(speaker: &str, mic: &str) -> PairOutcome {
    let Some(device) = probe::find(AudioDirection::Input, mic) else {
        return failed("microphone not connected");
    };
    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => return failed(e),
    };
    let channels = config.channels().max(1) as usize;
    let listener = Arc::new(Mutex::new(Listener::new(config.sample_rate().0)));
    let on_err = |e| tracing::debug!(error = %e, "sound test input stream error");
    macro_rules! build {
        ($t:ty, $to_f32:expr) => {{
            let listener = listener.clone();
            device.build_input_stream(
                &config.config(),
                move |data: &[$t], _: &_| {
                    let mono = data.iter().step_by(channels).map(|s| $to_f32(*s));
                    listener
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(mono);
                },
                on_err,
                None,
            )
        }};
    }
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build!(f32, |s: f32| s),
        cpal::SampleFormat::I16 => build!(i16, |s: i16| s as f32 / 32_768.0),
        cpal::SampleFormat::U16 => build!(u16, |s: u16| (s as f32 - 32_768.0) / 32_768.0),
        cpal::SampleFormat::I32 => build!(i32, |s: i32| s as f32 / 2_147_483_648.0),
        other => return failed(format!("unsupported sample format {other:?}")),
    };
    let stream = match stream {
        Ok(s) => s,
        Err(e) => return failed(e),
    };
    if let Err(e) = stream.play() {
        return failed(e);
    }
    let windows = || listener.lock().unwrap_or_else(|e| e.into_inner()).windows();
    let opened = Instant::now();
    while windows() == 0 {
        if opened.elapsed() >= FIRST_FRAMES {
            return failed("the microphone delivered nothing");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let outcome = with_quiet(|| {
        std::thread::sleep(ROOM);
        let tone_from = windows();
        if let Err(e) = play(speaker) {
            return failed(e);
        }
        std::thread::sleep(TAIL);
        listener
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .verdict(tone_from)
    });
    drop(stream);
    outcome
}
