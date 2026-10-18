//! What a platform's device names say about the device: which ones are worth trying, in what
//! order, and what to call them for a person.

use pond_core::models::domain::audio_device::{AudioDirection, DeviceKind};

/// Drivers that open fine and carry nothing until something is routed into them.
const VIRTUAL_HINTS: &[&str] = &[
    "aggregate",
    "background music",
    "blackhole",
    "boom",
    "camo",
    "eqmac",
    "krisp",
    "loopback",
    "microsoft teams",
    "multi-output",
    "obs ",
    "soundflower",
    "teams audio",
    "virtual",
    "zoomaudiodevice",
];

/// How a device reaches sound, read from its name.
pub fn kind_of(name: &str) -> DeviceKind {
    let lower = name.to_ascii_lowercase();
    if matches!(lower.as_str(), "default" | "pipewire" | "pulse") {
        return DeviceKind::SoundServer;
    }
    // `plughw` converts formats but, like `hw`, holds the card for itself while open.
    if lower.starts_with("hw:") || lower.starts_with("plughw:") {
        return DeviceKind::Raw;
    }
    if VIRTUAL_HINTS.iter().any(|h| lower.contains(h)) {
        return DeviceKind::Virtual;
    }
    DeviceKind::Hardware
}

/// Whether an ALSA PCM is worth offering. ALSA lists every plugin route to every card; most are
/// the same hardware again (`front`, `dsnoop`, `dmix`), multichannel layouts, or not sound at all.
pub fn alsa_worth_offering(name: &str, direction: AudioDirection) -> bool {
    let plugin = name.split(':').next().unwrap_or(name);
    match plugin {
        "default" | "pipewire" | "pulse" | "plughw" => true,
        // `sysdefault:CARD=X` names a card; bare `sysdefault` is `default` again.
        "sysdefault" => name.contains(':'),
        "hdmi" => direction == AudioDirection::Output,
        _ => false,
    }
}

/// One line of `/proc/asound/cards`: ` 0 [Device   ]: USB-Audio - USB PnP Audio Device`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub index: u32,
    pub id: String,
    pub driver: String,
    pub name: String,
}

impl Card {
    /// A card the household plugged in, rather than one built into the board.
    pub fn plugged_in(&self) -> bool {
        self.driver.eq_ignore_ascii_case("USB-Audio")
    }
}

pub fn parse_cards(text: &str) -> Vec<Card> {
    text.lines()
        .filter_map(|line| {
            let open = line.find('[')?;
            let close = line.find(']')?;
            let index = line.get(..open)?.trim().parse().ok()?;
            let id = line.get(open + 1..close)?.trim();
            let rest = line.get(close + 1..)?.trim().trim_start_matches(':').trim();
            let (driver, name) = rest.split_once(" - ").unwrap_or(("", rest));
            (!id.is_empty()).then(|| Card {
                index,
                id: id.to_string(),
                driver: driver.trim().to_string(),
                name: name.trim().to_string(),
            })
        })
        .collect()
}

/// Which directions each card offers, from `/proc/asound/pcm`:
/// `00-00: USB Audio : USB Audio : playback 1 : capture 1`.
pub fn card_directions(pcm: &str, card: u32, direction: AudioDirection) -> bool {
    let word = match direction {
        AudioDirection::Input => "capture",
        AudioDirection::Output => "playback",
    };
    pcm.lines().any(|line| {
        line.split('-')
            .next()
            .and_then(|c| c.trim().parse::<u32>().ok())
            == Some(card)
            && line.contains(word)
    })
}

/// The card an ALSA name routes to, by its `CARD=` field.
pub fn card_of<'a>(name: &str, cards: &'a [Card]) -> Option<&'a Card> {
    let id = alsa_field(name, "CARD")?;
    cards.iter().find(|c| c.id == id)
}

fn alsa_field<'a>(name: &'a str, key: &str) -> Option<&'a str> {
    let (_, args) = name.split_once(':')?;
    args.split(',')
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
}

/// What to call an ALSA PCM: the card's own name, not `plughw:CARD=Device,DEV=0`.
pub fn alsa_label(name: &str, cards: &[Card]) -> String {
    let plugin = name.split(':').next().unwrap_or(name);
    match plugin {
        "default" => return "System default".to_string(),
        "pipewire" => return "PipeWire".to_string(),
        "pulse" => return "PulseAudio".to_string(),
        _ => {}
    }
    let Some(card) = alsa_field(name, "CARD") else {
        return name.to_string();
    };
    let long = cards
        .iter()
        .find(|c| c.id == card)
        .map(|c| c.name.as_str())
        .unwrap_or(card);
    let dev = alsa_field(name, "DEV");
    match (plugin, dev) {
        ("hdmi", Some(d)) => format!("{long} (HDMI {d})"),
        ("plughw" | "hw", Some(d)) if d != "0" => format!("{long} (device {d})"),
        _ => long.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JETSON_CARDS: &str = " 0 [Device         ]: USB-Audio - USB PnP Audio Device
                      Solid State System Co.,Ltd. USB PnP Audio Device at usb-3610000.usb-2.3, full s
 1 [HDA            ]: tegra-hda - NVIDIA Jetson Orin Nano HDA
                      NVIDIA Jetson Orin Nano HDA at 0x3518000 irq 121
 2 [APE            ]: tegra-ape - NVIDIA Jetson Orin Nano APE
";

    #[test]
    fn the_cards_of_the_household_jetson_parse() {
        let cards = parse_cards(JETSON_CARDS);
        let summary: Vec<(u32, &str, &str, bool)> = cards
            .iter()
            .map(|c| (c.index, c.id.as_str(), c.name.as_str(), c.plugged_in()))
            .collect();
        assert_eq!(
            summary,
            [
                (0, "Device", "USB PnP Audio Device", true),
                (1, "HDA", "NVIDIA Jetson Orin Nano HDA", false),
                (2, "APE", "NVIDIA Jetson Orin Nano APE", false),
            ]
        );
        assert_eq!(
            card_of("sysdefault:CARD=Device", &cards).map(|c| c.index),
            Some(0)
        );
        assert_eq!(card_of("default", &cards), None);
    }

    #[test]
    fn alsa_nodes_are_named_after_their_card() {
        let cards = parse_cards(JETSON_CARDS);
        assert_eq!(
            alsa_label("plughw:CARD=Device,DEV=0", &cards),
            "USB PnP Audio Device"
        );
        assert_eq!(
            alsa_label("sysdefault:CARD=Device", &cards),
            "USB PnP Audio Device"
        );
        assert_eq!(
            alsa_label("plughw:CARD=APE,DEV=7", &cards),
            "NVIDIA Jetson Orin Nano APE (device 7)"
        );
        assert_eq!(
            alsa_label("hdmi:CARD=HDA,DEV=1", &cards),
            "NVIDIA Jetson Orin Nano HDA (HDMI 1)"
        );
        assert_eq!(alsa_label("default", &cards), "System default");
        assert_eq!(alsa_label("plughw:CARD=Gone,DEV=0", &cards), "Gone");
    }

    /// The Jetson lists 60-odd PCMs; only a handful are distinct ways to reach a card.
    #[test]
    fn only_distinct_alsa_routes_are_offered() {
        let input = AudioDirection::Input;
        for keep in [
            "default",
            "pipewire",
            "pulse",
            "sysdefault:CARD=Device",
            "plughw:CARD=Device,DEV=0",
        ] {
            assert!(alsa_worth_offering(keep, input), "{keep}");
        }
        for drop in [
            "null",
            "jack",
            "oss",
            "samplerate",
            "speexrate",
            "upmix",
            "vdownmix",
            "demixer",
            "sysdefault",
            "hw:CARD=Device,DEV=0",
            "front:CARD=Device,DEV=0",
            "dsnoop:CARD=Device,DEV=0",
            "dmix:CARD=Device,DEV=0",
            "usbstream:CARD=Device",
            "surround51:CARD=Device,DEV=0",
            "iec958:CARD=Device,DEV=0",
        ] {
            assert!(!alsa_worth_offering(drop, input), "{drop}");
        }
        assert!(alsa_worth_offering(
            "hdmi:CARD=HDA,DEV=0",
            AudioDirection::Output
        ));
        assert!(
            !alsa_worth_offering("hdmi:CARD=HDA,DEV=0", input),
            "HDMI has no microphone"
        );
    }

    #[test]
    fn kinds_follow_the_name() {
        assert_eq!(kind_of("default"), DeviceKind::SoundServer);
        assert_eq!(kind_of("pipewire"), DeviceKind::SoundServer);
        assert_eq!(kind_of("hw:CARD=Device,DEV=0"), DeviceKind::Raw);
        assert_eq!(kind_of("plughw:CARD=Device,DEV=0"), DeviceKind::Raw);
        assert_eq!(kind_of("sysdefault:CARD=Device"), DeviceKind::Hardware);
        assert_eq!(kind_of("MacBook Air Microphone"), DeviceKind::Hardware);
        assert_eq!(kind_of("BlackHole 2ch"), DeviceKind::Virtual);
        assert_eq!(kind_of("ZoomAudioDevice"), DeviceKind::Virtual);
        assert_eq!(kind_of("Microsoft Teams Audio"), DeviceKind::Virtual);
        assert_eq!(kind_of("jerr\u{2019}s Microphone"), DeviceKind::Hardware);
    }

    #[test]
    fn a_cards_directions_come_from_proc_asound_pcm() {
        let pcm = "00-00: USB Audio : USB Audio : playback 1 : capture 1\n\
                   01-03: HDMI 0 : HDMI 0 : playback 1\n\
                   02-00: tegra-dlink-0 XBAR-ADMAIF1-0 : : playback 1 : capture 1\n";
        assert!(card_directions(pcm, 0, AudioDirection::Input));
        assert!(card_directions(pcm, 0, AudioDirection::Output));
        assert!(card_directions(pcm, 1, AudioDirection::Output));
        assert!(
            !card_directions(pcm, 1, AudioDirection::Input),
            "HDMI has no microphone"
        );
        assert!(!card_directions(pcm, 7, AudioDirection::Output));
    }
}
