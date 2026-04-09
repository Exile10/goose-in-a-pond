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
    /// UUID of the primary household profile created during onboarding.
    #[serde(default)]
    pub primary_profile_id: Option<String>,

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

    /// Prompt style — selects the built-in system prompt template.
    /// Accepted values: "balanced" (default) | "concise" | "technical" | "warm"
    #[serde(default = "Settings::default_prompt_style")]
    pub prompt_style: String,

    /// Advanced: fully replace the system prompt. Supports {{assistant_name}},
    /// {{user_name}}, {{personality}}, {{timezone}}, {{location}},
    /// {{prompt_addendum}} placeholders. When Some, overrides prompt_style.
    #[serde(default)]
    pub custom_system_prompt: Option<String>,

    /// Extra instructions appended to the generated system prompt (max 500 chars).
    /// Example: "Always respond in French." or "Mention upcoming schedules proactively."
    #[serde(default = "Settings::default_prompt_addendum")]
    pub prompt_addendum: String,

    // ── Model roles ────────────────────────────────────────────────────────
    /// Provider for the Chat role (fast, conversational). Default = llm_provider.
    #[serde(default = "Settings::default_llm_provider")]
    pub chat_provider: String,

    /// Model name for the Chat role. Default = active_llm_model.
    #[serde(default = "Settings::default_active_llm_model")]
    pub chat_model: String,

    /// Provider for the Think role (deep reasoning). None = same as chat_provider.
    #[serde(default)]
    pub think_provider: Option<String>,

    /// Model name for the Think role. None = same as chat_model.
    #[serde(default)]
    pub think_model: Option<String>,

    /// Provider for the Task role (agentic tool-use). None = same as chat_provider.
    #[serde(default)]
    pub task_provider: Option<String>,

    /// Model name for the Task role. None = same as chat_model.
    #[serde(default)]
    pub task_model: Option<String>,

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

    /// Voice name sent to the Qwen TTS server (e.g. "Vivian", "Chelsie")
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

    // ── Agent behaviour ────────────────────────────────────────────────────────
    /// GooseMode for the agent loop: "auto" | "chat" | "smart"
    #[serde(default = "Settings::default_agent_goose_mode")]
    pub agent_goose_mode: String,

    /// Maximum agentic loop turns per request (safety cap)
    #[serde(default = "Settings::default_agent_max_turns")]
    pub agent_max_turns: u32,

    /// When true, recent memory fragments are injected into the system prompt each turn
    #[serde(default = "Settings::default_agent_memory_inject")]
    pub agent_memory_inject: bool,

    /// How many memory fragments to inject (most recent first)
    #[serde(default = "Settings::default_agent_memory_limit")]
    pub agent_memory_limit: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            primary_profile_id:              None,
            assistant_name:                  Self::default_assistant_name(),
            assistant_personality:           Self::default_assistant_personality(),
            user_name:                       Self::default_user_name(),
            timezone:                        Self::default_timezone(),
            prompt_style:                    Self::default_prompt_style(),
            custom_system_prompt:            None,
            prompt_addendum:                 Self::default_prompt_addendum(),
            chat_provider:                   Self::default_llm_provider(),
            chat_model:                      Self::default_active_llm_model(),
            think_provider:                  None,
            think_model:                     None,
            task_provider:                   None,
            task_model:                      None,
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
            agent_goose_mode:                Self::default_agent_goose_mode(),
            agent_max_turns:                 Self::default_agent_max_turns(),
            agent_memory_inject:             Self::default_agent_memory_inject(),
            agent_memory_limit:              Self::default_agent_memory_limit(),
        }
    }
}

impl Settings {
    fn default_prompt_style()                -> String { "balanced".to_string() }
    fn default_prompt_addendum()             -> String { "".to_string() }
    fn default_assistant_name()             -> String { "Goose".to_string() }
    fn default_assistant_personality()      -> String { "friendly and concise".to_string() }
    fn default_user_name()                  -> String { "Friend".to_string() }
    fn default_timezone()                   -> String { "UTC".to_string() }
    fn default_max_tokens()                 -> u32    { 1024 }
    fn default_temperature()                -> f32    { 0.7 }
    fn default_llm_provider()               -> String { "".to_string() }
    fn default_wake_word()                  -> String { "goose".to_string() }
    fn default_tts_voice()                  -> String { "".to_string() }
    fn default_recording_duration()         -> u32    { 5 }
    fn default_whisper_url()                -> String { "http://127.0.0.1:9000".to_string() }
    fn default_active_llm_model()           -> String { "".to_string() }
    fn default_active_whisper_model()       -> String { "".to_string() }
    fn default_active_tts_model()           -> String { "".to_string() }
    fn default_tts_http_url()               -> String { "http://127.0.0.1:8181".to_string() }
    fn default_tts_http_voice()             -> String { "".to_string() }
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
    fn default_agent_goose_mode()           -> String { "auto".to_string() }
    fn default_agent_max_turns()            -> u32    { 20 }
    fn default_agent_memory_inject()        -> bool   { false }
    fn default_agent_memory_limit()         -> u32    { 5 }
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
    fn default_prompt_style_is_balanced() {
        let s = Settings::default();
        assert_eq!(s.prompt_style, "balanced");
        assert!(s.custom_system_prompt.is_none());
        assert_eq!(s.prompt_addendum, "");
    }

    #[test]
    fn partial_json_with_prompt_fields() {
        let json = r#"{"prompt_style":"concise","prompt_addendum":"Be brief."}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.prompt_style, "concise");
        assert_eq!(s.prompt_addendum, "Be brief.");
        assert!(s.custom_system_prompt.is_none());
    }

    #[test]
    fn custom_system_prompt_roundtrips() {
        let json = r#"{"custom_system_prompt":"You are {{assistant_name}}."}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.custom_system_prompt, Some("You are {{assistant_name}}.".to_string()));
        let json2 = serde_json::to_string(&s).unwrap();
        let s2: Settings = serde_json::from_str(&json2).unwrap();
        assert_eq!(s2.custom_system_prompt, s.custom_system_prompt);
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
