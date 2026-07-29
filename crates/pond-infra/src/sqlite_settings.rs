//! SQLite-backed implementation of `SettingsRepository`.
//!
//! Uses the existing `settings(key TEXT PRIMARY KEY, value TEXT, updated_at TEXT)` table
//! in `pond_system.db`. Each Setting field maps to one row; missing keys fall back to
//! `Settings::default()`.
//!
//! IMPORTANT: `update()` issues one `INSERT OR REPLACE` per field — never batched into
//! a single query (sqlx only executes the first statement when multiple are batched).

use anyhow::Result;
use async_trait::async_trait;
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::ports::settings::SettingsRepository;
use serde_json;
use sqlx::{Pool, Sqlite};

pub struct SqliteSettingsRepository {
    pool: Pool<Sqlite>,
}

impl SqliteSettingsRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SettingsRepository for SqliteSettingsRepository {
    async fn get(&self) -> Result<Settings> {
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM settings")
            .fetch_all(&self.pool)
            .await?;

        let mut s = Settings::default();
        for (key, value) in rows {
            apply_key(&mut s, &key, &value);
        }
        Ok(s)
    }

    async fn update(&self, settings: &Settings) -> Result<()> {
        macro_rules! upsert {
            ($key:expr, $val:expr) => {
                sqlx::query(
                    "INSERT OR REPLACE INTO settings (key, value, updated_at) \
                     VALUES (?, ?, datetime('now'))",
                )
                .bind($key)
                .bind($val)
                .execute(&self.pool)
                .await?;
            };
        }

        upsert!(
            "primary_profile_id",
            settings.primary_profile_id.as_deref().unwrap_or("")
        );
        upsert!("chat_provider", &settings.chat_provider);
        upsert!("chat_model", &settings.chat_model);
        upsert!("tool_model", settings.tool_model.as_deref().unwrap_or(""));
        upsert!("assistant_name", &settings.assistant_name);
        upsert!("assistant_personality", &settings.assistant_personality);
        upsert!("user_name", &settings.user_name);
        upsert!("timezone", &settings.timezone);
        upsert!("home_name", &settings.home_name);
        upsert!("llm_max_tokens", settings.llm_max_tokens.to_string());
        upsert!("llm_temperature", settings.llm_temperature.to_string());
        upsert!("llm_provider", &settings.llm_provider);
        upsert!("voice_wake_word", &settings.voice_wake_word);
        upsert!(
            "voice_kws_whisper_url",
            settings.voice_kws_whisper_url.as_deref().unwrap_or("")
        );
        upsert!(
            "voice_kws_energy_threshold",
            settings.voice_kws_energy_threshold.to_string()
        );
        upsert!(
            "voice_kws_post_trigger_silence_ms",
            settings.voice_kws_post_trigger_silence_ms.to_string()
        );
        upsert!(
            "voice_kws_cooldown_ms",
            settings.voice_kws_cooldown_ms.to_string()
        );
        upsert!(
            "voice_wake_word_transcriptions",
            serde_json::to_string(&settings.voice_wake_word_transcriptions)
                .unwrap_or_else(|_| "[]".to_string())
        );
        upsert!("voice_tts_voice", &settings.voice_tts_voice);
        upsert!(
            "voice_recording_duration_secs",
            settings.voice_recording_duration_secs.to_string()
        );
        upsert!("voice_whisper_url", &settings.voice_whisper_url);
        upsert!("active_llm_model", &settings.active_llm_model);
        upsert!("active_whisper_model", &settings.active_whisper_model);
        upsert!("active_tts_model", &settings.active_tts_model);
        upsert!(
            "retention_event_log_days",
            settings.retention_event_log_days.to_string()
        );
        upsert!(
            "retention_sensor_days",
            settings.retention_sensor_days.to_string()
        );
        upsert!(
            "retention_session_messages_keep",
            settings.retention_session_messages_keep.to_string()
        );
        upsert!(
            "retention_events_days",
            settings.retention_events_days.to_string()
        );
        upsert!(
            "retention_events_by_category",
            serde_json::to_string(&settings.retention_events_by_category)
                .unwrap_or_else(|_| "{}".to_string())
        );
        upsert!(
            "retention_sensitive_days",
            settings.retention_sensitive_days.to_string()
        );
        upsert!("prompt_style", &settings.prompt_style);
        upsert!(
            "custom_system_prompt",
            settings.custom_system_prompt.as_deref().unwrap_or("")
        );
        upsert!("prompt_addendum", &settings.prompt_addendum);
        upsert!("agent_backend", &settings.agent_backend);
        upsert!("agent_goose_mode", &settings.agent_goose_mode);
        upsert!("agent_max_turns", settings.agent_max_turns.to_string());
        upsert!("voice_max_turns", settings.voice_max_turns.to_string());
        upsert!(
            "agent_memory_inject",
            if settings.agent_memory_inject {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "agent_memory_limit",
            settings.agent_memory_limit.to_string()
        );
        upsert!(
            "weather_enabled",
            if settings.weather_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!("weather_latitude", settings.weather_latitude.to_string());
        upsert!("weather_longitude", settings.weather_longitude.to_string());
        upsert!("weather_location_name", &settings.weather_location_name);
        // Vision (#130)
        upsert!(
            "vision_enabled",
            if settings.vision_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!("vision_camera_url", &settings.vision_camera_url);
        upsert!("vision_camera_id", &settings.vision_camera_id);
        upsert!("vision_fps", settings.vision_fps.to_string());
        upsert!(
            "vision_motion_threshold",
            settings.vision_motion_threshold.to_string()
        );
        upsert!("vision_classifier_model", &settings.vision_classifier_model);
        // Private mesh (#132)
        upsert!(
            "mesh_enabled",
            if settings.mesh_enabled {
                "true"
            } else {
                "false"
            }
        );
        // Privacy / sensor access
        upsert!(
            "mic_enabled",
            if settings.mic_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "cameras_enabled",
            if settings.cameras_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "cloud_fallback_enabled",
            if settings.cloud_fallback_enabled {
                "true"
            } else {
                "false"
            }
        );
        // Thinking / reasoning
        upsert!("thinking_mode", &settings.thinking_mode);
        upsert!(
            "show_thinking",
            if settings.show_thinking {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "context_window_override",
            settings.context_window_override.to_string()
        );
        // Answer review
        upsert!("review_mode", &settings.review_mode);
        upsert!("review_max_rounds", settings.review_max_rounds.to_string());
        upsert!(
            "review_pass_threshold",
            settings.review_pass_threshold.to_string()
        );
        // Memory lifecycle
        upsert!(
            "memory_extraction_enabled",
            if settings.memory_extraction_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "memory_cleanup_enabled",
            if settings.memory_cleanup_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "memory_consolidation_enabled",
            if settings.memory_consolidation_enabled {
                "true"
            } else {
                "false"
            }
        );
        // API keys
        upsert!(
            "api_key_guardian",
            settings.api_key_guardian.as_deref().unwrap_or("")
        );
        upsert!(
            "api_key_gnews",
            settings.api_key_gnews.as_deref().unwrap_or("")
        );
        upsert!(
            "api_key_finnhub",
            settings.api_key_finnhub.as_deref().unwrap_or("")
        );
        upsert!(
            "api_key_coingecko",
            settings.api_key_coingecko.as_deref().unwrap_or("")
        );
        upsert!("searxng_url", settings.searxng_url.as_deref().unwrap_or(""));
        // Embedding
        upsert!("active_embedding_model", &settings.active_embedding_model);
        upsert!("embedding_provider", &settings.embedding_provider);
        // Fast path
        upsert!(
            "fast_path_enabled",
            if settings.fast_path_enabled {
                "true"
            } else {
                "false"
            }
        );
        // Agent tuning
        upsert!(
            "agent_timeout_secs",
            settings.agent_timeout_secs.to_string()
        );
        upsert!(
            "prefix_cache_prompt",
            if settings.prefix_cache_prompt {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "tool_output_compaction",
            if settings.tool_output_compaction {
                "true"
            } else {
                "false"
            }
        );
        // Memory tuning
        upsert!(
            "memory_consolidation_mode",
            &settings.memory_consolidation_mode
        );
        upsert!(
            "memory_graph_enabled",
            if settings.memory_graph_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "memory_decay_base_half_life_days",
            settings.memory_decay_base_half_life_days.to_string()
        );
        upsert!("memory_decay_beta", settings.memory_decay_beta.to_string());
        upsert!(
            "memory_prune_threshold",
            settings.memory_prune_threshold.to_string()
        );
        upsert!(
            "memory_archive_threshold",
            settings.memory_archive_threshold.to_string()
        );
        upsert!(
            "memory_cleanup_interval_hours",
            settings.memory_cleanup_interval_hours.to_string()
        );
        upsert!(
            "memory_consolidation_interval_hours",
            settings.memory_consolidation_interval_hours.to_string()
        );
        upsert!(
            "memory_consolidation_batch_size",
            settings.memory_consolidation_batch_size.to_string()
        );
        upsert!(
            "memory_extraction_max_facts",
            settings.memory_extraction_max_facts.to_string()
        );
        upsert!(
            "memory_extraction_interval_secs",
            settings.memory_extraction_interval_secs.to_string()
        );
        // Scheduling
        upsert!(
            "schedule_result_notify",
            if settings.schedule_result_notify {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "schedule_max_concurrent",
            settings.schedule_max_concurrent.to_string()
        );
        upsert!(
            "schedule_max_runs_per_task",
            settings.schedule_max_runs_per_task.to_string()
        );
        // Monitoring & cost
        upsert!(
            "context_monitor_enabled",
            if settings.context_monitor_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "cloud_input_price_per_million",
            settings.cloud_input_price_per_million.to_string()
        );
        upsert!(
            "cloud_output_price_per_million",
            settings.cloud_output_price_per_million.to_string()
        );
        // Tool behaviour
        upsert!(
            "tool_cache_enabled",
            if settings.tool_cache_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "multi_tool_enabled",
            if settings.multi_tool_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "tool_call_validation",
            if settings.tool_call_validation {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "tool_request_detection",
            if settings.tool_request_detection {
                "true"
            } else {
                "false"
            }
        );
        // Data
        upsert!(
            "telemetry_enabled",
            if settings.telemetry_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "compact_encoding",
            if settings.compact_encoding {
                "true"
            } else {
                "false"
            }
        );
        // Extension toggles
        upsert!(
            "ext_memory_enabled",
            if settings.ext_memory_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "ext_schedule_enabled",
            if settings.ext_schedule_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "ext_weather_enabled",
            if settings.ext_weather_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "ext_knowledge_enabled",
            if settings.ext_knowledge_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "ext_system_enabled",
            if settings.ext_system_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "ext_device_enabled",
            if settings.ext_device_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "ext_news_enabled",
            if settings.ext_news_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "ext_finance_enabled",
            if settings.ext_finance_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "ext_discovery_enabled",
            if settings.ext_discovery_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "ext_audit_enabled",
            if settings.ext_audit_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "ext_vision_enabled",
            if settings.ext_vision_enabled {
                "true"
            } else {
                "false"
            }
        );

        Ok(())
    }

    async fn get_key(&self, key: &str) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|(v,)| v))
    }

    async fn set_key(&self, key: &str, value: String) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO settings (key, value, updated_at) \
             VALUES (?, ?, datetime('now'))",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// Apply a single key-value pair from the DB onto a `Settings` struct.
fn apply_key(s: &mut Settings, key: &str, value: &str) {
    match key {
        "primary_profile_id" => {
            s.primary_profile_id = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
        }
        "chat_provider" => s.chat_provider = value.to_string(),
        "chat_model" => s.chat_model = value.to_string(),
        "tool_model" => {
            s.tool_model = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
        }
        // Legacy keys — silently ignored for backward compat with old DBs
        "think_provider" | "think_model" | "task_provider" | "task_model" => {}
        "assistant_name" => s.assistant_name = value.to_string(),
        "assistant_personality" => s.assistant_personality = value.to_string(),
        "user_name" => s.user_name = value.to_string(),
        "timezone" => s.timezone = value.to_string(),
        "home_name" => s.home_name = value.to_string(),
        "llm_max_tokens" => {
            if let Ok(v) = value.parse() {
                s.llm_max_tokens = v;
            }
        }
        "llm_temperature" => {
            if let Ok(v) = value.parse() {
                s.llm_temperature = v;
            }
        }
        "llm_provider" => s.llm_provider = value.to_string(),
        "voice_wake_word" => s.voice_wake_word = value.to_string(),
        "voice_kws_whisper_url" => {
            s.voice_kws_whisper_url = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
        }
        "voice_kws_energy_threshold" => {
            if let Ok(v) = value.parse() {
                s.voice_kws_energy_threshold = v;
            }
        }
        "voice_kws_post_trigger_silence_ms" => {
            if let Ok(v) = value.parse() {
                s.voice_kws_post_trigger_silence_ms = v;
            }
        }
        "voice_kws_cooldown_ms" => {
            if let Ok(v) = value.parse() {
                s.voice_kws_cooldown_ms = v;
            }
        }
        "voice_wake_word_transcriptions" => {
            if let Ok(v) = serde_json::from_str::<Vec<String>>(value) {
                s.voice_wake_word_transcriptions = v;
            }
        }
        "voice_tts_voice" => s.voice_tts_voice = value.to_string(),
        "voice_recording_duration_secs" => {
            if let Ok(v) = value.parse() {
                s.voice_recording_duration_secs = v;
            }
        }
        "voice_whisper_url" => s.voice_whisper_url = value.to_string(),
        "active_llm_model" => s.active_llm_model = value.to_string(),
        "active_whisper_model" => s.active_whisper_model = value.to_string(),
        "active_tts_model" => s.active_tts_model = value.to_string(),
        "retention_event_log_days" => {
            if let Ok(v) = value.parse() {
                s.retention_event_log_days = v;
            }
        }
        "retention_sensor_days" => {
            if let Ok(v) = value.parse() {
                s.retention_sensor_days = v;
            }
        }
        "retention_session_messages_keep" => {
            if let Ok(v) = value.parse() {
                s.retention_session_messages_keep = v;
            }
        }
        "retention_events_days" => {
            if let Ok(v) = value.parse() {
                s.retention_events_days = v;
            }
        }
        "retention_events_by_category" => {
            if let Ok(v) = serde_json::from_str(value) {
                s.retention_events_by_category = v;
            }
        }
        "retention_sensitive_days" => {
            if let Ok(v) = value.parse() {
                s.retention_sensitive_days = v;
            }
        }
        "prompt_style" => s.prompt_style = value.to_string(),
        "custom_system_prompt" => {
            s.custom_system_prompt = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
        }
        "prompt_addendum" => s.prompt_addendum = value.to_string(),
        "agent_backend" => s.agent_backend = value.to_string(),
        "agent_goose_mode" => s.agent_goose_mode = value.to_string(),
        "agent_max_turns" => {
            if let Ok(v) = value.parse() {
                s.agent_max_turns = v;
            }
        }
        "voice_max_turns" => {
            if let Ok(v) = value.parse() {
                s.voice_max_turns = v;
            }
        }
        "agent_memory_inject" => s.agent_memory_inject = value == "true",
        "agent_memory_limit" => {
            if let Ok(v) = value.parse() {
                s.agent_memory_limit = v;
            }
        }
        "weather_enabled" => s.weather_enabled = value == "true",
        "weather_latitude" => {
            if let Ok(v) = value.parse() {
                s.weather_latitude = v;
            }
        }
        "weather_longitude" => {
            if let Ok(v) = value.parse() {
                s.weather_longitude = v;
            }
        }
        "weather_location_name" => s.weather_location_name = value.to_string(),
        // Vision (#130)
        "vision_enabled" => s.vision_enabled = value == "true",
        "vision_camera_url" => s.vision_camera_url = value.to_string(),
        "vision_camera_id" => s.vision_camera_id = value.to_string(),
        "vision_fps" => {
            if let Ok(v) = value.parse() {
                s.vision_fps = v;
            }
        }
        "vision_motion_threshold" => {
            if let Ok(v) = value.parse() {
                s.vision_motion_threshold = v;
            }
        }
        "vision_classifier_model" => s.vision_classifier_model = value.to_string(),
        // Private mesh (#132)
        "mesh_enabled" => s.mesh_enabled = value == "true",
        // Privacy / sensor access
        "mic_enabled" => s.mic_enabled = value == "true",
        "cameras_enabled" => s.cameras_enabled = value == "true",
        "cloud_fallback_enabled" => s.cloud_fallback_enabled = value == "true",
        // Thinking / reasoning
        "thinking_mode" => s.thinking_mode = value.to_string(),
        "show_thinking" => s.show_thinking = value == "true",
        "context_window_override" => {
            if let Ok(v) = value.parse() {
                s.context_window_override = v;
            }
        }
        // Answer review
        "review_mode" => s.review_mode = value.to_string(),
        "review_max_rounds" => {
            if let Ok(v) = value.parse() {
                s.review_max_rounds = v;
            }
        }
        "review_pass_threshold" => {
            if let Ok(v) = value.parse() {
                s.review_pass_threshold = v;
            }
        }
        // Embedding
        "active_embedding_model" => s.active_embedding_model = value.to_string(),
        "embedding_provider" => s.embedding_provider = value.to_string(),
        // Fast path
        "fast_path_enabled" => s.fast_path_enabled = value == "true",
        // Agent tuning
        "agent_timeout_secs" => {
            if let Ok(v) = value.parse() {
                s.agent_timeout_secs = v;
            }
        }
        "prefix_cache_prompt" => s.prefix_cache_prompt = value == "true",
        "tool_output_compaction" => s.tool_output_compaction = value == "true",
        // Memory lifecycle
        "memory_extraction_enabled" => s.memory_extraction_enabled = value == "true",
        "memory_cleanup_enabled" => s.memory_cleanup_enabled = value == "true",
        "memory_consolidation_enabled" => s.memory_consolidation_enabled = value == "true",
        // Memory tuning
        "memory_consolidation_mode" => s.memory_consolidation_mode = value.to_string(),
        "memory_graph_enabled" => s.memory_graph_enabled = value == "true",
        "memory_decay_base_half_life_days" => {
            if let Ok(v) = value.parse() {
                s.memory_decay_base_half_life_days = v;
            }
        }
        "memory_decay_beta" => {
            if let Ok(v) = value.parse() {
                s.memory_decay_beta = v;
            }
        }
        "memory_prune_threshold" => {
            if let Ok(v) = value.parse() {
                s.memory_prune_threshold = v;
            }
        }
        "memory_archive_threshold" => {
            if let Ok(v) = value.parse() {
                s.memory_archive_threshold = v;
            }
        }
        "memory_cleanup_interval_hours" => {
            if let Ok(v) = value.parse() {
                s.memory_cleanup_interval_hours = v;
            }
        }
        "memory_consolidation_interval_hours" => {
            if let Ok(v) = value.parse() {
                s.memory_consolidation_interval_hours = v;
            }
        }
        "memory_consolidation_batch_size" => {
            if let Ok(v) = value.parse() {
                s.memory_consolidation_batch_size = v;
            }
        }
        "memory_extraction_max_facts" => {
            if let Ok(v) = value.parse() {
                s.memory_extraction_max_facts = v;
            }
        }
        "memory_extraction_interval_secs" => {
            if let Ok(v) = value.parse() {
                s.memory_extraction_interval_secs = v;
            }
        }
        // Scheduling
        "schedule_result_notify" => s.schedule_result_notify = value == "true",
        "schedule_max_concurrent" => {
            if let Ok(v) = value.parse() {
                s.schedule_max_concurrent = v;
            }
        }
        "schedule_max_runs_per_task" => {
            if let Ok(v) = value.parse() {
                s.schedule_max_runs_per_task = v;
            }
        }
        // Monitoring & cost
        "context_monitor_enabled" => s.context_monitor_enabled = value == "true",
        "cloud_input_price_per_million" => {
            if let Ok(v) = value.parse() {
                s.cloud_input_price_per_million = v;
            }
        }
        "cloud_output_price_per_million" => {
            if let Ok(v) = value.parse() {
                s.cloud_output_price_per_million = v;
            }
        }
        // Tool behaviour
        "tool_cache_enabled" => s.tool_cache_enabled = value == "true",
        "multi_tool_enabled" => s.multi_tool_enabled = value == "true",
        "tool_call_validation" => s.tool_call_validation = value == "true",
        "tool_request_detection" => s.tool_request_detection = value == "true",
        // Data
        "telemetry_enabled" => s.telemetry_enabled = value == "true",
        "compact_encoding" => s.compact_encoding = value == "true",
        // API keys
        "api_key_guardian" => {
            s.api_key_guardian = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
        }
        "api_key_gnews" => {
            s.api_key_gnews = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
        }
        "api_key_finnhub" => {
            s.api_key_finnhub = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
        }
        "api_key_coingecko" => {
            s.api_key_coingecko = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
        }
        "searxng_url" => {
            s.searxng_url = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
        }
        // Extension toggles
        "ext_memory_enabled" => s.ext_memory_enabled = value == "true",
        "ext_schedule_enabled" => s.ext_schedule_enabled = value == "true",
        "ext_weather_enabled" => s.ext_weather_enabled = value == "true",
        "ext_knowledge_enabled" => s.ext_knowledge_enabled = value == "true",
        "ext_system_enabled" => s.ext_system_enabled = value == "true",
        "ext_device_enabled" => s.ext_device_enabled = value == "true",
        "ext_news_enabled" => s.ext_news_enabled = value == "true",
        "ext_finance_enabled" => s.ext_finance_enabled = value == "true",
        "ext_discovery_enabled" => s.ext_discovery_enabled = value == "true",
        "ext_audit_enabled" => s.ext_audit_enabled = value == "true",
        _ => {} // unknown key — ignore
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use tempfile::tempdir;

    async fn fresh_repo() -> SqliteSettingsRepository {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let repo = SqliteSettingsRepository::new(db.system.clone());
        std::mem::forget(tmp); // keep the sqlite file alive for the test
        repo
    }

    #[tokio::test]
    async fn privacy_and_home_settings_roundtrip() {
        let repo = fresh_repo().await;

        // Defaults before any write: mic/cameras ON, cloud fallback OFF, home empty.
        let s0 = repo.get().await.unwrap();
        assert!(s0.mic_enabled);
        assert!(s0.cameras_enabled);
        assert!(!s0.cloud_fallback_enabled);
        assert_eq!(s0.home_name, "");

        // Persist non-default privacy toggles + a home name.
        let mut s = s0;
        s.mic_enabled = false;
        s.cameras_enabled = false;
        s.cloud_fallback_enabled = true;
        s.home_name = "The Anyumba Home".to_string();
        repo.update(&s).await.unwrap();

        let got = repo.get().await.unwrap();
        assert!(!got.mic_enabled);
        assert!(!got.cameras_enabled);
        assert!(got.cloud_fallback_enabled);
        assert_eq!(got.home_name, "The Anyumba Home");
    }

    #[tokio::test]
    async fn retention_events_settings_roundtrip() {
        let repo = fresh_repo().await;

        // Defaults before any write.
        let s0 = repo.get().await.unwrap();
        assert_eq!(s0.retention_events_days, 30);
        assert_eq!(s0.retention_sensitive_days, 7);
        assert!(s0.retention_events_by_category.is_empty());

        // Persist non-default per-category + sensitivity retention.
        let mut s = s0;
        s.retention_events_days = 45;
        s.retention_sensitive_days = 3;
        s.retention_events_by_category.insert("network".into(), 14);
        s.retention_events_by_category.insert("sensor".into(), 5);
        repo.update(&s).await.unwrap();

        let got = repo.get().await.unwrap();
        assert_eq!(got.retention_events_days, 45);
        assert_eq!(got.retention_sensitive_days, 3);
        assert_eq!(got.retention_events_by_category.get("network"), Some(&14));
        assert_eq!(got.retention_events_by_category.get("sensor"), Some(&5));
    }

    #[tokio::test]
    async fn mesh_enabled_roundtrips() {
        let repo = fresh_repo().await;

        let s0 = repo.get().await.unwrap();
        assert!(!s0.mesh_enabled, "off by default");

        let mut s = s0;
        s.mesh_enabled = true;
        repo.update(&s).await.unwrap();

        let got = repo.get().await.unwrap();
        assert!(got.mesh_enabled);
    }
}
