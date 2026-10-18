//! [`AudioDeviceProber`] on cpal: CoreAudio on the Mac, ALSA (and through it PipeWire or
//! PulseAudio) on Linux.

use std::collections::HashSet;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait};
use pond_audio::MicHandle;
use pond_core::models::domain::audio_device::{AudioDevice, AudioDirection, ProbeOutcome};
use pond_core::models::domain::mic_gate;
use pond_core::models::ports::audio_devices::AudioDeviceProber;

use crate::names;
use crate::probe;

/// cpal's only host on Linux is ALSA, whose listing opens every node.
const ALSA: bool = cfg!(target_os = "linux");

/// Time allowed beyond a probe's own wait for the driver to open and close the device.
const OPEN_SLACK: Duration = Duration::from_secs(6);

pub struct CpalProber {
    /// The process's mic owner. Input probes run on its thread, so nothing opens a microphone
    /// beside it, and the device it has open is measured rather than reopened.
    mic: Option<MicHandle>,
}

impl CpalProber {
    pub fn new(mic: Option<MicHandle>) -> Self {
        Self { mic }
    }

    fn cards() -> Vec<(String, String)> {
        if !ALSA {
            return Vec::new();
        }
        std::fs::read_to_string("/proc/asound/cards")
            .map(|t| names::parse_cards(&t))
            .unwrap_or_default()
    }

    fn describe(name: &str, is_default: bool, cards: &[(String, String)]) -> AudioDevice {
        AudioDevice {
            id: name.to_string(),
            label: if ALSA {
                names::alsa_label(name, cards)
            } else {
                name.to_string()
            },
            is_default,
            kind: names::kind_of(name),
        }
    }

    fn default_name(direction: AudioDirection) -> Option<String> {
        let host = cpal::default_host();
        let device = match direction {
            AudioDirection::Input => host.default_input_device(),
            AudioDirection::Output => host.default_output_device(),
        }?;
        device.name().ok()
    }

    fn open_input(&self) -> Option<String> {
        let mic = self.mic.as_ref()?;
        mic.run_exclusive(Duration::from_secs(2), |view| view.open_device.clone())
            .flatten()
    }
}

impl AudioDeviceProber for CpalProber {
    fn default_device(&self, direction: AudioDirection) -> Option<AudioDevice> {
        let name = Self::default_name(direction)?;
        Some(Self::describe(&name, true, &Self::cards()))
    }

    fn devices(&self, direction: AudioDirection) -> Result<Vec<AudioDevice>, String> {
        let host = cpal::default_host();
        let default = Self::default_name(direction);
        let cards = Self::cards();
        let listed: Vec<cpal::Device> = match direction {
            AudioDirection::Input => host.input_devices().map_err(|e| e.to_string())?.collect(),
            AudioDirection::Output => host.output_devices().map_err(|e| e.to_string())?.collect(),
        };
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for name in listed.iter().filter_map(|d| d.name().ok()) {
            if ALSA && !names::alsa_worth_offering(&name, direction) {
                continue;
            }
            if seen.insert(name.clone()) {
                let is_default = default.as_deref() == Some(name.as_str());
                out.push(Self::describe(&name, is_default, &cards));
            }
        }
        // ALSA leaves out a node that is busy, and the one the mic owner holds always is.
        if direction == AudioDirection::Input {
            if let Some(open) = self.open_input().filter(|o| !seen.contains(o)) {
                let is_default = default.as_deref() == Some(open.as_str());
                out.push(Self::describe(&open, is_default, &cards));
            }
        }
        Ok(out)
    }

    fn quick_list(&self, direction: AudioDirection) -> Vec<AudioDevice> {
        if ALSA {
            return Vec::new();
        }
        self.devices(direction).unwrap_or_default()
    }

    fn probe(
        &self,
        device: &AudioDevice,
        direction: AudioDirection,
        wait: Duration,
    ) -> ProbeOutcome {
        if direction == AudioDirection::Output {
            return probe::probe_output(&device.id, wait);
        }
        if !mic_gate::mic_enabled() {
            return ProbeOutcome::Untested;
        }
        let Some(mic) = &self.mic else {
            return probe::probe_input(&device.id, wait);
        };
        let id = device.id.clone();
        // Opening can block inside the driver: an idle Continuity iPhone mic took longer than
        // three seconds on the Mac. Past this, the device is not answering, not the owner.
        mic.run_exclusive(wait + OPEN_SLACK, move |view| {
            if !view.shared.enabled() {
                ProbeOutcome::Untested
            } else if view.open_device.as_deref() == Some(id.as_str()) {
                probe::measure_open(view.shared, wait)
            } else {
                probe::probe_input(&id, wait)
            }
        })
        .unwrap_or(ProbeOutcome::Stalled)
    }

    fn fingerprint(&self) -> String {
        if ALSA {
            // A card coming or going changes these; PipeWire follows its own default behind
            // ALSA's `default`, so a Bluetooth headset there needs no reselect from us.
            let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
            return format!(
                "{}\n{}",
                read("/proc/asound/cards"),
                read("/proc/asound/pcm")
            );
        }
        let host = cpal::default_host();
        let mut all: Vec<String> = host
            .devices()
            .map(|ds| ds.filter_map(|d| d.name().ok()).collect())
            .unwrap_or_default();
        all.sort();
        format!(
            "{}|in={}|out={}",
            all.join(","),
            Self::default_name(AudioDirection::Input).unwrap_or_default(),
            Self::default_name(AudioDirection::Output).unwrap_or_default()
        )
    }

    fn chime(&self, device: &AudioDevice) -> Result<(), String> {
        pond_audio_out::route::play_chime(Some(&device.id)).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::services::voice::audio_devices::{
        AudioDeviceSelector, ProbeMode, SelectRequest,
    };
    use std::sync::Arc;

    /// Real hardware: `cargo test -p pond-adapters-audio-devices -- --ignored --nocapture`.
    #[test]
    #[ignore = "opens this machine's real microphones and speakers"]
    fn live_probe_on_this_machine() {
        let prober = Arc::new(CpalProber::new(None));
        println!("fingerprint: {}", prober.fingerprint());
        for direction in [AudioDirection::Input, AudioDirection::Output] {
            for device in prober.devices(direction).unwrap() {
                let t = std::time::Instant::now();
                let outcome = prober.probe(&device, direction, Duration::from_millis(1_200));
                println!(
                    "{direction:?} {device:?} -> {outcome:?} in {:?}",
                    t.elapsed()
                );
            }
            let selector = AudioDeviceSelector::new(prober.clone());
            let t = std::time::Instant::now();
            let choice = selector.select(&SelectRequest {
                direction,
                pinned: None,
                mode: ProbeMode::Lazy,
                exclude: &[],
                mic_enabled: true,
            });
            println!("{direction:?} lazy choice: {choice} in {:?}", t.elapsed());
            assert!(choice.device.is_some());
        }
    }
}
