//! The board's sound server (PulseAudio, or PipeWire's PulseAudio service), told through
//! `pactl`: which speaker and microphone are its defaults, and their levels. The pond itself
//! opens the ALSA card directly, and holds it while it runs; this makes the same pair the default
//! for every other program, which can play through it once the pond lets go.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use pond_core::models::domain::audio_device::{AudioDevice, LevelChange, SoundServerDefaults};

use crate::names::{self, Card};

/// The server's 100%, in its raw volume units.
const FULL: u32 = 65_536;
/// A sound server that does not answer in this long is treated as absent, not waited on.
const PACTL_LIMIT: Duration = Duration::from_secs(3);

pub trait Pactl: Send + Sync {
    fn run(&self, args: &[&str]) -> Result<String, String>;
}

/// The real `pactl`, in the C locale so its output parses on a board set to any language.
pub struct SystemPactl;

impl Pactl for SystemPactl {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        let mut child = Command::new("pactl")
            .args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("pactl: {e}"))?;
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break status;
            }
            if started.elapsed() >= PACTL_LIMIT {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the sound server did not answer".to_string());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut out = String::new();
        if let Some(mut stdout) = child.stdout.take() {
            let _ = stdout.read_to_string(&mut out);
        }
        if status.success() {
            Ok(out)
        } else {
            let mut err = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                let _ = stderr.read_to_string(&mut err);
            }
            Err(format!("pactl {}: {}", args.join(" "), err.trim()))
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Info {
    pub server: String,
    pub default_sink: Option<String>,
    pub default_source: Option<String>,
}

pub fn parse_info(text: &str) -> Info {
    let mut info = Info::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().to_string();
        match key.trim() {
            "Server Name" => info.server = value,
            "Default Sink" => info.default_sink = Some(value),
            "Default Source" => info.default_source = Some(value),
            _ => {}
        }
    }
    info
}

/// One sink or source, from `pactl list sinks` or `pactl list sources`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    pub description: String,
    pub card: Option<u32>,
    pub monitor: bool,
    pub muted: bool,
    /// The first channel's raw volume.
    pub volume: Option<u32>,
    /// The device's natural level: 100% for a speaker, the card's 0 dB for a microphone.
    pub base_volume: Option<u32>,
    /// Its volume is the card's own mixer, so it reaches a program that opens the card
    /// directly, as the pond does.
    pub hardware_volume: bool,
}

fn first_raw_volume(value: &str) -> Option<u32> {
    let after_channel = value.split_once(':').map_or(value, |(_, v)| v);
    after_channel.split_whitespace().next()?.parse().ok()
}

fn quoted(value: &str) -> &str {
    value.trim().trim_matches('"')
}

pub fn parse_nodes(text: &str) -> Vec<Node> {
    let mut nodes = Vec::new();
    let mut current: Option<Node> = None;
    for line in text.lines() {
        if line.starts_with("Sink #") || line.starts_with("Source #") {
            nodes.extend(current.take());
            current = Some(Node::default());
            continue;
        }
        let Some(node) = current.as_mut() else {
            continue;
        };
        let trimmed = line.trim();
        if let Some((key, value)) = trimmed.split_once(" = ") {
            match key {
                "alsa.card" => node.card = quoted(value).parse().ok(),
                "device.class" => node.monitor = quoted(value) == "monitor",
                _ => {}
            }
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key {
            "Name" => node.name = value.to_string(),
            "Description" => node.description = value.to_string(),
            "Mute" => node.muted = value == "yes",
            "Volume" => node.volume = first_raw_volume(value),
            "Base Volume" => {
                node.base_volume = value.split_whitespace().next().and_then(|v| v.parse().ok())
            }
            "Flags" => {
                node.hardware_volume = value.split_whitespace().any(|f| f == "HW_VOLUME_CTRL")
            }
            _ => {}
        }
    }
    nodes.extend(current);
    nodes
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Speaker,
    Mic,
}

impl Side {
    fn list(self) -> &'static str {
        match self {
            Self::Speaker => "sinks",
            Self::Mic => "sources",
        }
    }
    fn set_default(self) -> &'static str {
        match self {
            Self::Speaker => "set-default-sink",
            Self::Mic => "set-default-source",
        }
    }
    fn set_mute(self) -> &'static str {
        match self {
            Self::Speaker => "set-sink-mute",
            Self::Mic => "set-source-mute",
        }
    }
    fn set_volume(self) -> &'static str {
        match self {
            Self::Speaker => "set-sink-volume",
            Self::Mic => "set-source-volume",
        }
    }
    fn noun(self) -> &'static str {
        match self {
            Self::Speaker => "speaker",
            Self::Mic => "microphone",
        }
    }
    fn default_of(self, info: &Info) -> Option<&str> {
        match self {
            Self::Speaker => info.default_sink.as_deref(),
            Self::Mic => info.default_source.as_deref(),
        }
    }
}

/// The pond's route through the sound server: it already follows the server's default.
fn through_the_server(device: &AudioDevice) -> bool {
    matches!(device.id.as_str(), "default" | "pulse" | "pipewire")
}

/// The server's node for a device the pond chose: its current default for the server route,
/// the node on the same ALSA card otherwise. `Ok(None)` for a device that is not a card.
fn node_for(
    pactl: &dyn Pactl,
    cards: &[Card],
    info: &Info,
    device: &AudioDevice,
    side: Side,
) -> Result<Option<Node>, String> {
    let nodes: Vec<Node> = parse_nodes(&pactl.run(&["list", side.list()])?)
        .into_iter()
        .filter(|n| !n.monitor)
        .collect();
    if through_the_server(device) {
        let default = side.default_of(info);
        return Ok(nodes.into_iter().find(|n| Some(n.name.as_str()) == default));
    }
    let Some(card) = names::card_index(&device.id, cards) else {
        return Ok(None);
    };
    nodes
        .into_iter()
        .find(|n| n.card == Some(card))
        .map(Some)
        .ok_or_else(|| {
            format!(
                "{} lists no {} on '{}'",
                info.server,
                side.noun(),
                device.label
            )
        })
}

fn make_default(
    pactl: &dyn Pactl,
    cards: &[Card],
    info: &Info,
    device: &AudioDevice,
    side: Side,
) -> Result<Option<String>, String> {
    let Some(node) = node_for(pactl, cards, info, device, side)? else {
        return Ok(None);
    };
    if side.default_of(info) != Some(node.name.as_str()) {
        pactl.run(&[side.set_default(), &node.name])?;
        tracing::info!(node = %node.name, "made the pond's {} the sound server's default", side.noun());
    }
    if node.muted {
        pactl.run(&[side.set_mute(), &node.name, "0"])?;
        tracing::info!(node = %node.name, "unmuted the {}", side.noun());
    }
    Ok(Some(node.description))
}

/// Make the chosen devices the sound server's defaults and unmute them. `None` when no sound
/// server answers: then there are no defaults but the cards themselves.
pub fn set_defaults(
    pactl: &dyn Pactl,
    cards: &[Card],
    input: Option<&AudioDevice>,
    output: Option<&AudioDevice>,
) -> Option<SoundServerDefaults> {
    let info = parse_info(&pactl.run(&["info"]).ok()?);
    let mut result = SoundServerDefaults {
        server: info.server.clone(),
        ..Default::default()
    };
    for (device, side) in [(output, Side::Speaker), (input, Side::Mic)] {
        let Some(device) = device else { continue };
        match make_default(pactl, cards, &info, device, side) {
            Ok(described) => match side {
                Side::Speaker => result.speaker = described,
                Side::Mic => result.mic = described,
            },
            Err(problem) => {
                tracing::warn!(%problem, "could not make the pond's {} the board's default", side.noun());
                result.problems.push(problem);
            }
        }
    }
    Some(result)
}

fn percent(raw: u32) -> String {
    format!("{}%", (raw as u64 * 100 + FULL as u64 / 2) / FULL as u64)
}

/// Unmute the pair and raise a volume below its natural level up to it, never beyond: louder
/// than unity distorts. Only levels the pond's own sound passes through are touched.
pub fn raise_levels(
    pactl: &dyn Pactl,
    cards: &[Card],
    speaker: &AudioDevice,
    mic: &AudioDevice,
) -> Vec<LevelChange> {
    let Ok(info) = pactl.run(&["info"]).map(|t| parse_info(&t)) else {
        return Vec::new();
    };
    let mut changes = Vec::new();
    for (device, side) in [(speaker, Side::Speaker), (mic, Side::Mic)] {
        let Ok(Some(node)) = node_for(pactl, cards, &info, device, side) else {
            continue;
        };
        if !(node.hardware_volume || through_the_server(device)) {
            continue;
        }
        if node.muted && pactl.run(&[side.set_mute(), &node.name, "0"]).is_ok() {
            changes.push(LevelChange {
                device: node.description.clone(),
                what: "mute".into(),
                from: "on".into(),
                to: "off".into(),
                undo: vec![side.set_mute().into(), node.name.clone(), "1".into()],
            });
        }
        let target = node.base_volume.unwrap_or(FULL).min(FULL);
        if let Some(volume) = node.volume.filter(|v| *v < target) {
            if pactl
                .run(&[side.set_volume(), &node.name, &target.to_string()])
                .is_ok()
            {
                changes.push(LevelChange {
                    device: node.description.clone(),
                    what: "volume".into(),
                    from: percent(volume),
                    to: percent(target),
                    undo: vec![
                        side.set_volume().into(),
                        node.name.clone(),
                        volume.to_string(),
                    ],
                });
            }
        }
    }
    if !changes.is_empty() {
        let raised: Vec<String> = changes.iter().map(ToString::to_string).collect();
        tracing::info!(?raised, "raised levels for another sound test");
    }
    changes
}

pub fn restore_levels(pactl: &dyn Pactl, changes: &[LevelChange]) {
    for change in changes.iter().rev() {
        let args: Vec<&str> = change.undo.iter().map(String::as_str).collect();
        if let Err(e) = pactl.run(&args) {
            tracing::warn!(error = %e, "could not put a raised level back");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::domain::audio_device::DeviceKind;
    use std::sync::Mutex;

    const INFO: &str = "Server String: /run/user/1000/pulse/native
Server Name: pulseaudio
Server Version: 15.99.1
Default Sink: alsa_output.platform-sound.analog-stereo
Default Source: alsa_input.platform-sound.analog-stereo
";

    /// The household nano's own `pactl list sinks`, trimmed to the lines that matter.
    const SINKS: &str = "Sink #0
\tState: SUSPENDED
\tName: alsa_output.usb-Solid_State_System_Co._Ltd._USB_PnP_Audio_Device_000000000000-00.analog-stereo
\tDescription: USB PnP Audio Device Analog Stereo
\tMute: no
\tVolume: front-left: 33595 /  51% / -17.41 dB,   front-right: 33595 /  51% / -17.41 dB
\t        balance 0.00
\tBase Volume: 65536 / 100% / 0.00 dB
\tFlags: HARDWARE HW_MUTE_CTRL HW_VOLUME_CTRL DECIBEL_VOLUME LATENCY
\tProperties:
\t\tdevice.class = \"sound\"
\t\talsa.card = \"0\"
\t\talsa.card_name = \"USB PnP Audio Device\"
Sink #1
\tState: SUSPENDED
\tName: alsa_output.platform-sound.analog-stereo
\tDescription: Built-in Audio Analog Stereo
\tMute: no
\tVolume: front-left: 62914 /  96% / -1.06 dB,   front-right: 62914 /  96% / -1.06 dB
\tBase Volume: 65536 / 100% / 0.00 dB
\tFlags: HARDWARE DECIBEL_VOLUME LATENCY
\tProperties:
\t\tdevice.class = \"sound\"
\t\talsa.card = \"2\"
";

    const SOURCES: &str = "Source #0
\tName: alsa_output.usb-Solid_State_System_Co._Ltd._USB_PnP_Audio_Device_000000000000-00.analog-stereo.monitor
\tDescription: Monitor of USB PnP Audio Device Analog Stereo
\tMute: no
\tVolume: front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100% / 0.00 dB
\tBase Volume: 65536 / 100% / 0.00 dB
\tFlags: DECIBEL_VOLUME LATENCY
\tProperties:
\t\tdevice.class = \"monitor\"
\t\talsa.card = \"0\"
Source #1
\tName: alsa_input.usb-Solid_State_System_Co._Ltd._USB_PnP_Audio_Device_000000000000-00.analog-stereo
\tDescription: USB PnP Audio Device Analog Stereo
\tMute: yes
\tVolume: front-left: 6553 /  10% / -60.00 dB,   front-right: 6553 /  10% / -60.00 dB
\tBase Volume: 19944 /  30% / -31.00 dB
\tFlags: HARDWARE HW_MUTE_CTRL HW_VOLUME_CTRL DECIBEL_VOLUME LATENCY
\tProperties:
\t\tdevice.class = \"sound\"
\t\talsa.card = \"0\"
Source #3
\tName: alsa_input.platform-sound.analog-stereo
\tDescription: Built-in Audio Analog Stereo
\tMute: no
\tVolume: front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100% / 0.00 dB
\tBase Volume: 65536 / 100% / 0.00 dB
\tFlags: HARDWARE DECIBEL_VOLUME LATENCY
\tProperties:
\t\tdevice.class = \"sound\"
\t\talsa.card = \"2\"
";

    const USB_SINK: &str =
        "alsa_output.usb-Solid_State_System_Co._Ltd._USB_PnP_Audio_Device_000000000000-00.analog-stereo";
    const USB_SOURCE: &str =
        "alsa_input.usb-Solid_State_System_Co._Ltd._USB_PnP_Audio_Device_000000000000-00.analog-stereo";

    #[derive(Default)]
    struct FakePactl {
        absent: bool,
        calls: Mutex<Vec<String>>,
    }

    impl FakePactl {
        fn changes(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.starts_with("set-"))
                .cloned()
                .collect()
        }
    }

    impl Pactl for FakePactl {
        fn run(&self, args: &[&str]) -> Result<String, String> {
            if self.absent {
                return Err("Connection failure: Connection refused".into());
            }
            self.calls.lock().unwrap().push(args.join(" "));
            Ok(match args {
                ["info"] => INFO,
                ["list", "sinks"] => SINKS,
                ["list", "sources"] => SOURCES,
                _ => "",
            }
            .to_string())
        }
    }

    fn cards() -> Vec<Card> {
        names::parse_cards(
            " 0 [Device         ]: USB-Audio - USB PnP Audio Device\n 2 [APE            ]: tegra-ape - NVIDIA Jetson Orin Nano APE\n",
        )
    }

    fn usb() -> AudioDevice {
        AudioDevice::new("sysdefault:CARD=Device", false, DeviceKind::Hardware).plugged_in()
    }

    #[test]
    fn the_nanos_sinks_and_sources_parse() {
        let sinks = parse_nodes(SINKS);
        assert_eq!(sinks.len(), 2);
        assert_eq!(sinks[0].name, USB_SINK);
        assert_eq!(sinks[0].card, Some(0));
        assert_eq!(sinks[0].volume, Some(33_595));
        assert_eq!(sinks[0].base_volume, Some(FULL));
        assert!(sinks[0].hardware_volume && !sinks[0].muted && !sinks[0].monitor);
        assert!(!sinks[1].hardware_volume);
        let sources = parse_nodes(SOURCES);
        assert!(sources[0].monitor, "a sink's monitor is not a microphone");
        assert_eq!(sources[1].base_volume, Some(19_944));
        assert!(sources[1].muted);
        let info = parse_info(INFO);
        assert_eq!(info.server, "pulseaudio");
        assert_eq!(
            info.default_sink.as_deref(),
            Some("alsa_output.platform-sound.analog-stereo")
        );
    }

    /// The nano's PulseAudio defaulted to the onboard path, which nothing is plugged into.
    #[test]
    fn the_usb_pair_becomes_the_boards_default_and_is_unmuted() {
        let pactl = FakePactl::default();
        let usb = usb();
        let done = set_defaults(&pactl, &cards(), Some(&usb), Some(&usb)).expect("server");
        assert_eq!(done.server, "pulseaudio");
        assert_eq!(
            done.speaker.as_deref(),
            Some("USB PnP Audio Device Analog Stereo")
        );
        assert_eq!(
            done.mic.as_deref(),
            Some("USB PnP Audio Device Analog Stereo")
        );
        assert!(done.problems.is_empty());
        assert_eq!(
            pactl.changes(),
            [
                format!("set-default-sink {USB_SINK}"),
                format!("set-default-source {USB_SOURCE}"),
                format!("set-source-mute {USB_SOURCE} 0"),
            ]
        );
    }

    #[test]
    fn a_device_that_already_follows_the_server_changes_nothing() {
        let pactl = FakePactl::default();
        let default = AudioDevice::new("default", true, DeviceKind::SoundServer);
        let done = set_defaults(&pactl, &cards(), None, Some(&default)).expect("server");
        assert_eq!(
            done.speaker.as_deref(),
            Some("Built-in Audio Analog Stereo")
        );
        assert!(pactl.changes().is_empty());
    }

    #[test]
    fn a_card_the_server_does_not_list_is_reported_not_guessed() {
        let pactl = FakePactl::default();
        let hdmi = AudioDevice::new("hdmi:CARD=HDA,DEV=0", false, DeviceKind::Hardware);
        let cards =
            names::parse_cards(" 1 [HDA            ]: tegra-hda - NVIDIA Jetson Orin Nano HDA\n");
        let done = set_defaults(&pactl, &cards, None, Some(&hdmi)).expect("server");
        assert_eq!(done.speaker, None);
        assert_eq!(done.problems.len(), 1, "{:?}", done.problems);
        assert!(pactl.changes().is_empty());
    }

    #[test]
    fn without_a_sound_server_there_is_nothing_to_tell() {
        let pactl = FakePactl {
            absent: true,
            ..Default::default()
        };
        assert_eq!(
            set_defaults(&pactl, &cards(), Some(&usb()), Some(&usb())),
            None
        );
        assert!(raise_levels(&pactl, &cards(), &usb(), &usb()).is_empty());
    }

    /// The speaker goes to 100%; the microphone only to its card's 0 dB, not to the +31 dB its
    /// 100% means, which would clip speech.
    #[test]
    fn levels_rise_to_their_natural_level_and_go_back_exactly() {
        let pactl = FakePactl::default();
        let raised = raise_levels(&pactl, &cards(), &usb(), &usb());
        let shown: Vec<String> = raised.iter().map(ToString::to_string).collect();
        assert_eq!(
            shown,
            [
                "USB PnP Audio Device Analog Stereo volume from 51% to 100%",
                "USB PnP Audio Device Analog Stereo mute from on to off",
                "USB PnP Audio Device Analog Stereo volume from 10% to 30%",
            ]
        );
        assert_eq!(
            pactl.changes(),
            [
                format!("set-sink-volume {USB_SINK} 65536"),
                format!("set-source-mute {USB_SOURCE} 0"),
                format!("set-source-volume {USB_SOURCE} 19944"),
            ]
        );
        pactl.calls.lock().unwrap().clear();
        restore_levels(&pactl, &raised);
        assert_eq!(
            pactl.changes(),
            [
                format!("set-source-volume {USB_SOURCE} 6553"),
                format!("set-source-mute {USB_SOURCE} 1"),
                format!("set-sink-volume {USB_SINK} 33595"),
            ]
        );
    }

    /// The onboard sink's volume lives only in the server, so the pond's direct playback would
    /// not get louder for raising it.
    #[test]
    fn a_level_the_ponds_own_sound_bypasses_is_left_alone() {
        let pactl = FakePactl::default();
        let ape = AudioDevice::new("sysdefault:CARD=APE", false, DeviceKind::Hardware);
        let raised = raise_levels(&pactl, &cards(), &ape, &ape);
        assert!(raised.is_empty(), "{raised:?}");
    }
}
