//! Configurable settings for GIAP, organized into four categories.
//!
//! Each field maps to a key in the `settings` SQLite table (flat key-value store).
//! Defaults are the factory values used when a key is not present in the store.

use serde::{Deserialize, Serialize};

/// All configurable settings for GIAP.
///
/// Serializes to/from JSON via serde. Each field has a default via
/// the `Default` impl and companion `serde(default)` attributes,
/// so deserializing a partial JSON object fills missing fields with defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    // ── Assistant identity ──────────────────────────────────────────────────
    /// Display name the assistant uses (default: "Goose")
    #[serde(default = "Settings::default_assistant_name")]
    pub assistant_name: String,

    /// Personality hint injected into the system prompt
    #[serde(default = "Settings::default_assistant_personality")]
    pub assistant_personality: String,

    /// Primary user's name, used to personalize responses
    #[serde(default = "Settings::default_user_name")]
    pub user_name: String,

    /// IANA timezone string, e.g. "Africa/Nairobi"
    #[serde(default = "Settings::default_timezone")]
    pub timezone: String,

    // ── LLM behaviour ──────────────────────────────────────────────────────
    /// Maximum tokens the LLM may generate per response
    #[serde(default = "Settings::default_max_tokens")]
    pub llm_max_tokens: u32,

    /// Sampling temperature (0.0 = deterministic, 1.0 = creative)
    #[serde(default = "Settings::default_temperature")]
    pub llm_temperature: f32,

    /// Active LLM provider: "llamafile", "ollama", or "mock"
    #[serde(default = "Settings::default_llm_provider")]
    pub llm_provider: String,

    // ── Voice pipeline ─────────────────────────────────────────────────────
    /// Wake word / phrase detected by WhisperKeywordDetector (case-insensitive)
    #[serde(default = "Settings::default_wake_word")]
    pub voice_wake_word: String,

    /// Piper TTS voice model filename (e.g. "en_US-lessac-medium.onnx")
    #[serde(default = "Settings::default_tts_voice")]
    pub voice_tts_voice: String,

    /// Microphone recording duration in seconds for each whisper capture
    #[serde(default = "Settings::default_recording_duration")]
    pub voice_recording_duration_secs: u32,

    /// Base URL of the whisper.cpp server
    #[serde(default = "Settings::default_whisper_url")]
    pub voice_whisper_url: String,

    // ── Active model selection ──────────────────────────────────────────────
    /// Active LLM model name from the registry (e.g. "gemma-2b", "llama-1b")
    #[serde(default = "Settings::default_active_llm_model")]
    pub active_llm_model: String,

    /// Active Whisper model name from the registry (e.g. "base", "tiny", "small")
    #[serde(default = "Settings::default_active_whisper_model")]
    pub active_whisper_model: String,

    /// Active TTS model name from the registry (e.g. "qwen-tts", "piper-lessac")
    #[serde(default = "Settings::default_active_tts_model")]
    pub active_tts_model: String,

    /// Base URL of the Qwen TTS HTTP server (OpenAI-compatible /v1/audio/speech)
    #[serde(default = "Settings::default_tts_http_url")]
    pub voice_tts_http_url: String,

    /// Voice name sent to the Qwen TTS server (e.g. "Chelsie")
    #[serde(default = "Settings::default_tts_http_voice")]
    pub voice_tts_http_voice: String,

    /// URL to fetch the latest model registry JSON
    #[serde(default = "Settings::default_model_registry_url")]
    pub model_registry_url: String,

    // ── Weather ────────────────────────────────────────────────────────────
    /// Whether to fetch live weather and inject it into the LLM system prompt.
    #[serde(default = "Settings::default_weather_enabled")]
    pub weather_enabled: bool,

    /// Latitude for the weather location (decimal degrees, e.g. -1.286 for Nairobi).
    #[serde(default = "Settings::default_weather_latitude")]
    pub weather_latitude: f64,

    /// Longitude for the weather location (decimal degrees, e.g. 36.817 for Nairobi).
    #[serde(default = "Settings::default_weather_longitude")]
    pub weather_longitude: f64,

    /// Human-readable location name shown in context and API responses.
    #[serde(default = "Settings::default_weather_location_name")]
    pub weather_location_name: String,

    // ── Data retention ─────────────────────────────────────────────────────
    /// Days to keep rows in event_log (0 = keep forever)
    #[serde(default = "Settings::default_event_log_days")]
    pub retention_event_log_days: u32,

    /// Days to keep rows in sensor_readings
    #[serde(default = "Settings::default_sensor_days")]
    pub retention_sensor_days: u32,

    /// Maximum session messages to keep per session
    #[serde(default = "Settings::default_session_messages_keep")]
    pub retention_session_messages_keep: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            assistant_name:                  Self::default_assistant_name(),
            assistant_personality:           Self::default_assistant_personality(),
            user_name:                       Self::default_user_name(),
            timezone:                        Self::default_timezone(),
            llm_max_tokens:                  Self::default_max_tokens(),
            llm_temperature:                 Self::default_temperature(),
            llm_provider:                    Self::default_llm_provider(),
            voice_wake_word:                 Self::default_wake_word(),
            voice_tts_voice:                 Self::default_tts_voice(),
            voice_recording_duration_secs:   Self::default_recording_duration(),
            voice_whisper_url:               Self::default_whisper_url(),
            active_llm_model:                Self::default_active_llm_model(),
            active_whisper_model:            Self::default_active_whisper_model(),
            active_tts_model:                Self::default_active_tts_model(),
            voice_tts_http_url:              Self::default_tts_http_url(),
            voice_tts_http_voice:            Self::default_tts_http_voice(),
            model_registry_url:              Self::default_model_registry_url(),
            weather_enabled:                 Self::default_weather_enabled(),
            weather_latitude:                Self::default_weather_latitude(),
            weather_longitude:               Self::default_weather_longitude(),
            weather_location_name:           Self::default_weather_location_name(),
            retention_event_log_days:        Self::default_event_log_days(),
            retention_sensor_days:           Self::default_sensor_days(),
            retention_session_messages_keep: Self::default_session_messages_keep(),
        }
    }
}

impl Settings {
    fn default_assistant_name()             -> String { "Goose".to_string() }
    fn default_assistant_personality()      -> String { "friendly and concise".to_string() }
    fn default_user_name()                  -> String { "Friend".to_string() }
    fn default_timezone()                   -> String { "UTC".to_string() }
    fn default_max_tokens()                 -> u32    { 1024 }
    fn default_temperature()                -> f32    { 0.7 }
    fn default_llm_provider()               -> String { "llamafile".to_string() }
    fn default_wake_word()                  -> String { "goose".to_string() }
    fn default_tts_voice()                  -> String { "en_US-lessac-medium.onnx".to_string() }
    fn default_recording_duration()         -> u32    { 5 }
    fn default_whisper_url()                -> String { "http://127.0.0.1:9000".to_string() }
    fn default_active_llm_model()           -> String { "gemma-2b".to_string() }
    fn default_active_whisper_model()       -> String { "base".to_string() }
    fn default_active_tts_model()           -> String { "qwen-tts".to_string() }
    fn default_tts_http_url()               -> String { "http://127.0.0.1:8181".to_string() }
    fn default_tts_http_voice()             -> String { "Chelsie".to_string() }
    fn default_model_registry_url()         -> String {
        "https://raw.githubusercontent.com/jarida-io/goose-in-a-pond/main/crates/pond-server/registry.json".to_string()
    }
    fn default_weather_enabled()             -> bool   { false }
    fn default_weather_latitude()            -> f64    { 0.0 }
    fn default_weather_longitude()           -> f64    { 0.0 }
    fn default_weather_location_name()       -> String { "".to_string() }
    fn default_event_log_days()              -> u32    { 30 }
    fn default_sensor_days()                -> u32    { 7 }
    fn default_session_messages_keep()      -> u32    { 500 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_have_expected_values() {
        let s = Settings::default();
        assert_eq!(s.assistant_name, "Goose");
        assert_eq!(s.llm_max_tokens, 1024);
        assert_eq!(s.llm_temperature, 0.7);
        assert_eq!(s.voice_wake_word, "goose");
        assert_eq!(s.retention_event_log_days, 30);
    }

    #[test]
    fn settings_roundtrip_via_json() {
        let mut s = Settings::default();
        s.assistant_name = "Duck".to_string();
        s.llm_max_tokens = 2048;
        let json = serde_json::to_string(&s).unwrap();
        let s2: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(s2.assistant_name, "Duck");
        assert_eq!(s2.llm_max_tokens, 2048);
        // Unchanged fields keep defaults
        assert_eq!(s2.llm_temperature, 0.7);
    }

    #[test]
    fn partial_json_fills_missing_with_defaults() {
        let json = r#"{"assistant_name": "Pond"}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.assistant_name, "Pond");
        assert_eq!(s.llm_max_tokens, 1024); // default
        assert_eq!(s.timezone, "UTC");       // default
    }
}
