//! Finding and briefly testing the machine's microphones and speakers.

use std::time::Duration;

use crate::models::domain::audio_device::{
    AudioDevice, AudioDeviceStatus, AudioDirection, ProbeOutcome,
};

/// Blocking by design: a probe opens a real device, and CoreAudio streams must be opened and
/// dropped on one thread. Call it from a dedicated thread or `spawn_blocking`.
pub trait AudioDeviceProber: Send + Sync {
    /// The system's default device, found without opening anything.
    fn default_device(&self, direction: AudioDirection) -> Option<AudioDevice>;

    /// Every device in this direction. On ALSA this opens each one to ask, so it is called only
    /// when the default alone cannot settle the choice.
    fn devices(&self, direction: AudioDirection) -> Result<Vec<AudioDevice>, String>;

    /// Devices known without opening any: the full list where that is cheap, else empty.
    fn quick_list(&self, direction: AudioDirection) -> Vec<AudioDevice>;

    /// Open the device briefly. A microphone is measured for level only, never recorded; a
    /// speaker is handed silence.
    fn probe(
        &self,
        device: &AudioDevice,
        direction: AudioDirection,
        wait: Duration,
    ) -> ProbeOutcome;

    /// Changes whenever a device appears or goes, or a default moves. Cheap enough to poll.
    fn fingerprint(&self) -> String;

    /// Play a short, soft chime on a speaker. Only on the household's say-so.
    fn chime(&self, device: &AudioDevice) -> Result<(), String>;

    /// Whether a plugged-in device goes ahead of the default: true where a sound server picks
    /// the default by priority (a board), false where a person picks it (CoreAudio).
    fn plugged_in_first(&self) -> bool {
        false
    }
}

/// What the API asks of the process that chose the devices. Blocking, like the prober.
pub trait AudioDeviceControl: Send + Sync {
    /// What is in use now. Cheap: no device is opened.
    fn status(&self) -> AudioDeviceStatus;

    /// Test every device again, apply what is chosen, and report it. `chime` also plays a short
    /// sound on the chosen speaker, which is the only time a test is audible.
    fn retest(&self, chime: bool) -> AudioDeviceStatus;

    /// Choose again the way startup does, after a setting the choice depends on changed. Returns
    /// at once; it never opens a microphone the process would not open at startup.
    fn reselect(&self);
}
