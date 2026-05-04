//! Domain classifier — maps user messages to tool domains via keyword heuristics.
//!
//! This classifier runs at the tool-routing layer to determine which *category*
//! of MCP tools should handle a request.  It is complementary to (and runs
//! before) the per-tool `route_to_tool()` classifier in `request_classifier.rs`.
//!
//! ## Priority order
//! 1. Music (highest — prevents "play X" leaking to Knowledge/Wikipedia)
//! 2. Weather
//! 3. Home / Devices
//! 4. Schedule
//! 5. Memory
//! 6. FileSystem
//! 7. System
//! 8. Knowledge (most generic prefix patterns — checked last)
//! 9. General (fallback)

use serde::{Deserialize, Serialize};

/// Broad tool domain that a user message belongs to.
///
/// Used to select which MCP tool set to expose and what domain-specific hints
/// to inject into the system prompt before the agent processes the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolDomain {
    Music,
    Knowledge,
    Weather,
    Home,
    Schedule,
    Memory,
    System,
    FileSystem,
    General,
}

/// Common wake-word prefixes stripped before keyword matching.
const WAKE_PREFIXES: &[&str] = &[
    "goose, ",
    "goose ",
    "hey goose, ",
    "hey goose ",
    "ok computer, ",
    "ok computer ",
];

/// Classify a user message into the most appropriate [`ToolDomain`].
///
/// Pure function — no I/O, no async, fully deterministic.
/// The input is the raw message as typed or transcribed.
pub fn classify_domain(message: &str) -> ToolDomain {
    let lower = message.to_lowercase();

    // Strip wake-word prefix
    let stripped = WAKE_PREFIXES
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix))
        .unwrap_or(&lower);

    let stripped = stripped.trim();

    // ── Music — check FIRST ────────────────────────────────────────
    // Must come before Knowledge to prevent "play X" matching "what is X".
    const MUSIC_PREFIXES: &[&str] = &[
        "play ",
        "pause",
        "skip",
        "next track",
        "previous track",
        "next song",
        "previous song",
        "now playing",
        "what's playing",
        "currently playing",
        "queue",
        "shuffle",
        "volume up",
        "volume down",
        "set volume",
        "add to playlist",
        "create playlist",
        "search tracks",
        "search albums",
        "search artists",
        "play music",
        "stop music",
        "resume music",
    ];
    const MUSIC_CONTAINS: &[&str] = &["spotify", "playlist", "album", "track"];

    if MUSIC_PREFIXES.iter().any(|k| stripped.starts_with(k))
        || MUSIC_CONTAINS.iter().any(|k| stripped.contains(k))
    {
        return ToolDomain::Music;
    }

    // ── Weather ────────────────────────────────────────────────────
    const WEATHER: &[&str] = &[
        "weather",
        "temperature",
        "forecast",
        "rain",
        "raining",
        "humidity",
        "wind",
        "sunny",
        "cloudy",
        "snow",
    ];
    if WEATHER.iter().any(|k| stripped.contains(k)) {
        return ToolDomain::Weather;
    }

    // ── Home / Devices ─────────────────────────────────────────────
    const HOME_PREFIXES: &[&str] = &[
        "turn on",
        "turn off",
        "switch on",
        "switch off",
        "lock ",
        "unlock ",
        "open ",
        "close ",
        "list devices",
        "show devices",
    ];
    const HOME_CONTAINS: &[&str] = &["lights", "thermostat", "door", "garage"];

    if HOME_PREFIXES.iter().any(|k| stripped.starts_with(k))
        || HOME_CONTAINS.iter().any(|k| stripped.contains(k))
    {
        return ToolDomain::Home;
    }

    // ── Schedule ───────────────────────────────────────────────────
    const SCHEDULE_PREFIXES: &[&str] = &[
        "schedule ",
        "remind me",
        "set a reminder",
        "set an alarm",
        "create schedule",
        "list schedules",
        "delete schedule",
    ];
    const SCHEDULE_CONTAINS: &[&str] = &["alarm", "timer", "cron"];

    if SCHEDULE_PREFIXES.iter().any(|k| stripped.starts_with(k))
        || SCHEDULE_CONTAINS.iter().any(|k| stripped.contains(k))
    {
        return ToolDomain::Schedule;
    }

    // ── Memory ─────────────────────────────────────────────────────
    const MEMORY_PREFIXES: &[&str] = &[
        "remember ",
        "recall ",
        "forget ",
        "my name is",
        "i prefer",
        "i like",
        "do you remember",
        "what do you know about me",
    ];
    if MEMORY_PREFIXES.iter().any(|k| stripped.starts_with(k)) {
        return ToolDomain::Memory;
    }

    // ── FileSystem ─────────────────────────────────────────────────
    const FS_PREFIXES: &[&str] = &[
        "read file",
        "write file",
        "list directory",
        "create folder",
        "delete file",
        "search files",
        "show file",
    ];
    if FS_PREFIXES.iter().any(|k| stripped.starts_with(k)) {
        return ToolDomain::FileSystem;
    }

    // ── System ─────────────────────────────────────────────────────
    const SYSTEM: &[&str] = &[
        "system info",
        "disk space",
        "uptime",
        "hostname",
        "send notification",
        "notify me",
        "clipboard",
        "run command",
        "shell command",
    ];
    if SYSTEM.iter().any(|k| stripped.contains(k)) {
        return ToolDomain::System;
    }

    // ── Knowledge — check LAST (most generic) ──────────────────────
    const KNOWLEDGE_PREFIXES: &[&str] = &[
        "who is",
        "who was",
        "what is",
        "what are",
        "what was",
        "where is",
        "when did",
        "when was",
        "how does",
        "how do",
        "explain ",
        "define ",
        "tell me about",
        "describe ",
        "history of",
        "meaning of",
    ];
    if KNOWLEDGE_PREFIXES.iter().any(|k| stripped.starts_with(k)) || stripped.contains("wikipedia")
    {
        return ToolDomain::Knowledge;
    }

    ToolDomain::General
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Music domain ───────────────────────────────────────────────

    #[test]
    fn play_command_routes_to_music() {
        assert_eq!(
            classify_domain("Play Marvin's Room by Drake"),
            ToolDomain::Music,
        );
        assert_eq!(classify_domain("Play some jazz"), ToolDomain::Music);
        assert_eq!(classify_domain("play bohemian rhapsody"), ToolDomain::Music,);
    }

    #[test]
    fn playback_controls_route_to_music() {
        assert_eq!(classify_domain("what's playing"), ToolDomain::Music);
        assert_eq!(classify_domain("pause"), ToolDomain::Music);
        assert_eq!(classify_domain("skip"), ToolDomain::Music);
        assert_eq!(classify_domain("next track"), ToolDomain::Music);
        assert_eq!(classify_domain("previous song"), ToolDomain::Music);
        assert_eq!(classify_domain("shuffle"), ToolDomain::Music);
        assert_eq!(classify_domain("volume up"), ToolDomain::Music);
        assert_eq!(classify_domain("set volume to 80"), ToolDomain::Music);
    }

    #[test]
    fn playlist_routes_to_music() {
        assert_eq!(
            classify_domain("add this to my playlist"),
            ToolDomain::Music,
        );
        assert_eq!(
            classify_domain("create playlist called chill vibes"),
            ToolDomain::Music,
        );
        assert_eq!(
            classify_domain("show my spotify playlists"),
            ToolDomain::Music,
        );
    }

    #[test]
    fn music_contains_keywords() {
        assert_eq!(classify_domain("open my spotify"), ToolDomain::Music,);
        assert_eq!(classify_domain("check this album out"), ToolDomain::Music,);
        assert_eq!(classify_domain("what track is this"), ToolDomain::Music,);
    }

    // ── Knowledge domain ───────────────────────────────────────────

    #[test]
    fn who_is_routes_to_knowledge_not_music() {
        // "who is Drake" should NOT be Music even though Drake is a musician
        assert_eq!(classify_domain("who is Drake"), ToolDomain::Knowledge);
        assert_eq!(classify_domain("who was Beethoven"), ToolDomain::Knowledge,);
    }

    #[test]
    fn knowledge_prefix_patterns() {
        assert_eq!(
            classify_domain("what is photosynthesis"),
            ToolDomain::Knowledge,
        );
        assert_eq!(
            classify_domain("where is the Eiffel Tower"),
            ToolDomain::Knowledge,
        );
        assert_eq!(
            classify_domain("when did World War 2 end"),
            ToolDomain::Knowledge,
        );
        assert_eq!(
            classify_domain("how does gravity work"),
            ToolDomain::Knowledge,
        );
        assert_eq!(
            classify_domain("explain quantum entanglement"),
            ToolDomain::Knowledge,
        );
        assert_eq!(classify_domain("define democracy"), ToolDomain::Knowledge,);
        assert_eq!(
            classify_domain("tell me about black holes"),
            ToolDomain::Knowledge,
        );
        assert_eq!(
            classify_domain("history of the Roman Empire"),
            ToolDomain::Knowledge,
        );
    }

    #[test]
    fn wikipedia_keyword_routes_to_knowledge() {
        assert_eq!(
            classify_domain("search wikipedia for cats"),
            ToolDomain::Knowledge,
        );
    }

    // ── Weather domain ─────────────────────────────────────────────

    #[test]
    fn weather_keywords() {
        assert_eq!(classify_domain("what's the weather"), ToolDomain::Weather,);
        assert_eq!(
            classify_domain("what is the temperature"),
            ToolDomain::Weather,
        );
        assert_eq!(
            classify_domain("will it rain tomorrow"),
            ToolDomain::Weather,
        );
        assert_eq!(classify_domain("is it sunny outside"), ToolDomain::Weather,);
        assert_eq!(classify_domain("how's the humidity"), ToolDomain::Weather,);
        assert_eq!(classify_domain("check the forecast"), ToolDomain::Weather,);
    }

    // ── Home domain ────────────────────────────────────────────────

    #[test]
    fn home_prefix_patterns() {
        assert_eq!(classify_domain("turn on the lights"), ToolDomain::Home,);
        assert_eq!(classify_domain("turn off the TV"), ToolDomain::Home,);
        assert_eq!(classify_domain("lock the front door"), ToolDomain::Home,);
        assert_eq!(classify_domain("list devices"), ToolDomain::Home,);
    }

    #[test]
    fn home_contains_patterns() {
        assert_eq!(classify_domain("dim the lights to 50%"), ToolDomain::Home,);
        assert_eq!(
            classify_domain("set the thermostat to 72"),
            ToolDomain::Home,
        );
        assert_eq!(classify_domain("is the garage open"), ToolDomain::Home,);
    }

    // ── Schedule domain ────────────────────────────────────────────

    #[test]
    fn schedule_patterns() {
        assert_eq!(classify_domain("remind me at 5pm"), ToolDomain::Schedule,);
        assert_eq!(classify_domain("schedule a meeting"), ToolDomain::Schedule,);
        assert_eq!(
            classify_domain("set an alarm for 7am"),
            ToolDomain::Schedule,
        );
        assert_eq!(classify_domain("what's on my timer"), ToolDomain::Schedule,);
        assert_eq!(classify_domain("delete schedule 3"), ToolDomain::Schedule,);
    }

    // ── Memory domain ──────────────────────────────────────────────

    #[test]
    fn memory_patterns() {
        assert_eq!(
            classify_domain("remember my favorite color is blue"),
            ToolDomain::Memory,
        );
        assert_eq!(
            classify_domain("do you remember my birthday"),
            ToolDomain::Memory,
        );
        assert_eq!(
            classify_domain("forget what I told you"),
            ToolDomain::Memory,
        );
        assert_eq!(classify_domain("i prefer dark mode"), ToolDomain::Memory,);
        assert_eq!(classify_domain("my name is Jerry"), ToolDomain::Memory,);
    }

    // ── FileSystem domain ──────────────────────────────────────────

    #[test]
    fn filesystem_patterns() {
        assert_eq!(
            classify_domain("read file /etc/hosts"),
            ToolDomain::FileSystem,
        );
        assert_eq!(
            classify_domain("list directory /home"),
            ToolDomain::FileSystem,
        );
        assert_eq!(
            classify_domain("create folder backups"),
            ToolDomain::FileSystem,
        );
    }

    // ── System domain ──────────────────────────────────────────────

    #[test]
    fn system_patterns() {
        assert_eq!(classify_domain("show system info"), ToolDomain::System,);
        assert_eq!(
            classify_domain("how much disk space is left"),
            ToolDomain::System,
        );
        assert_eq!(
            classify_domain("send notification test"),
            ToolDomain::System,
        );
        assert_eq!(classify_domain("run command ls -la"), ToolDomain::System,);
    }

    // ── General (fallback) ─────────────────────────────────────────

    #[test]
    fn general_fallback() {
        assert_eq!(classify_domain("what time is it"), ToolDomain::General);
        assert_eq!(classify_domain("hello"), ToolDomain::General);
        assert_eq!(classify_domain("tell me a joke"), ToolDomain::General);
        assert_eq!(classify_domain("thanks"), ToolDomain::General);
        assert_eq!(classify_domain("help me debug this"), ToolDomain::General);
    }

    // ── Priority / edge cases ──────────────────────────────────────

    #[test]
    fn music_beats_knowledge_for_play() {
        // "play X" should always be Music, even if X looks like a knowledge query
        assert_eq!(
            classify_domain("play what is love by Haddaway"),
            ToolDomain::Music,
        );
    }

    #[test]
    fn wake_word_stripped_before_classification() {
        assert_eq!(classify_domain("Goose, play some music"), ToolDomain::Music,);
        assert_eq!(
            classify_domain("Hey goose, what's the weather"),
            ToolDomain::Weather,
        );
        assert_eq!(
            classify_domain("ok computer, who is Alan Turing"),
            ToolDomain::Knowledge,
        );
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(classify_domain("PLAY SOME JAZZ"), ToolDomain::Music,);
        assert_eq!(classify_domain("WHAT'S THE WEATHER"), ToolDomain::Weather,);
        assert_eq!(classify_domain("WHO IS Einstein"), ToolDomain::Knowledge,);
    }

    #[test]
    fn weather_beats_knowledge_prefix() {
        // "what is the temperature" has both "what is" (Knowledge prefix) and
        // "temperature" (Weather keyword). Weather is checked first, so it wins.
        assert_eq!(
            classify_domain("what is the temperature"),
            ToolDomain::Weather,
        );
    }
}
