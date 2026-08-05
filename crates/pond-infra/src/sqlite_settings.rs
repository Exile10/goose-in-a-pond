//! SQLite-backed implementation of `SettingsRepository`.
//!
//! Uses the existing `settings(key TEXT PRIMARY KEY, value TEXT, updated_at TEXT)` table
//! in `pond_system.db`. Each Setting field maps to one row; missing keys fall back to
//! `Settings::default()`.
//!
//! IMPORTANT: `update()` issues one upsert per field — never batched into
//! a single query (sqlx only executes the first statement when multiple are batched).
//! The whole sequence runs inside one transaction so concurrent writers cannot
//! interleave per-key and leave a torn hybrid of two snapshots.
//!
//! Every write is `INSERT … ON CONFLICT(key) DO UPDATE`, never `INSERT OR
//! REPLACE`: replace deletes the row and re-inserts it, which resets
//! `is_user_set` to its column default and would silently forget that the user
//! had chosen the key (see migration 0035).
//!
//! Every field MUST have both an upsert in `update_fields` and an arm in
//! `apply_key`; a field with only one of the two is silently unsaved or silently
//! unread. `roundtrip_persists_every_field` guards the whole class.

use anyhow::Result;
use async_trait::async_trait;
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::ports::settings::SettingsRepository;
use serde_json;
use sqlx::{Pool, Sqlite};
use std::collections::HashSet;

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
        self.update_fields(settings, None).await
    }

    async fn update_fields(
        &self,
        settings: &Settings,
        only: Option<&HashSet<String>>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;

        // `only` = write just these keys (the caller's patch), so a save of one
        // field cannot revert a field another writer changed since the caller
        // read its snapshot. `None` = write every field.
        macro_rules! upsert {
            ($key:expr, $val:expr) => {
                if only.is_none_or(|keys| keys.contains($key)) {
                    sqlx::query(
                        "INSERT INTO settings (key, value, updated_at) \
                         VALUES (?, ?, datetime('now')) \
                         ON CONFLICT(key) DO UPDATE SET \
                             value = excluded.value, \
                             updated_at = excluded.updated_at",
                    )
                    .bind($key)
                    .bind($val)
                    .execute(&mut *tx)
                    .await?;
                }
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
        // Matter (#195)
        upsert!(
            "matter_enabled",
            if settings.matter_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!("matter_ws_url", &settings.matter_ws_url);
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
        upsert!(
            "show_turn_stats",
            if settings.show_turn_stats {
                "true"
            } else {
                "false"
            }
        );
        upsert!(
            "hybrid_compaction_enabled",
            if settings.hybrid_compaction_enabled {
                "true"
            } else {
                "false"
            }
        );
        upsert!("summary_idle_secs", settings.summary_idle_secs.to_string());
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
        // API keys are NOT here: PAI-2 P2 moved them to `SecretRepository`.
        // See `crate::secret_migration` for the one-time move of any row an
        // existing pond already had. `searxng_url` is an endpoint, not a
        // credential, and stays.
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
        upsert!("tool_selection_mode", &settings.tool_selection_mode);
        upsert!("security_policy_mode", &settings.security_policy_mode);
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
        upsert!(
            "ext_sensor_enabled",
            if settings.ext_sensor_enabled {
                "true"
            } else {
                "false"
            }
        );

        tx.commit().await?;
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
            "INSERT INTO settings (key, value, updated_at) \
             VALUES (?, ?, datetime('now')) \
             ON CONFLICT(key) DO UPDATE SET \
                 value = excluded.value, \
                 updated_at = excluded.updated_at",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete_key(&self, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM settings WHERE key = ?")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn mark_user_set(&self, keys: &HashSet<String>) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        // UPDATE, not upsert: a patch may carry a key that is not a real
        // Settings field, and an unknown key must not conjure a settings row.
        // The caller writes the values first, so every real key has one.
        let mut tx = self.pool.begin().await?;
        for key in keys {
            sqlx::query("UPDATE settings SET is_user_set = 1 WHERE key = ?")
                .bind(key)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn is_user_set(&self, key: &str) -> Result<bool> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT is_user_set FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some_and(|(flag,)| flag != 0))
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
        // Matter (#195)
        "matter_enabled" => s.matter_enabled = value == "true",
        "matter_ws_url" => s.matter_ws_url = value.to_string(),
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
        "show_turn_stats" => s.show_turn_stats = value == "true",
        "hybrid_compaction_enabled" => s.hybrid_compaction_enabled = value == "true",
        "summary_idle_secs" => {
            if let Ok(v) = value.parse() {
                s.summary_idle_secs = v;
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
        "tool_selection_mode" => s.tool_selection_mode = value.to_string(),
        "security_policy_mode" => s.security_policy_mode = value.to_string(),
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
        // `api_key_*` has no arm: PAI-2 P2 moved that material to
        // `SecretRepository`. A legacy row left behind by a failed migration
        // falls through to the `_ => {}` arm below and is ignored rather than
        // re-hydrated onto a struct that `GET /settings` serialises.
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
        "ext_vision_enabled" => s.ext_vision_enabled = value == "true",
        "ext_sensor_enabled" => s.ext_sensor_enabled = value == "true",
        _ => {} // unknown key — ignore
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use pond_core::user_data::domain::settings::{DefaultAdoption, DEFAULT_ADOPTIONS};
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

    /// Perturb every scalar field of a serialised `Settings` to a value that
    /// differs from the input, so a field that fails to persist shows up as a
    /// mismatch after a round-trip.
    fn perturb(value: &serde_json::Value) -> serde_json::Value {
        use serde_json::Value;
        match value {
            Value::Bool(b) => Value::Bool(!b),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::from(i + 7)
                } else if let Some(u) = n.as_u64() {
                    Value::from(u + 7)
                } else {
                    Value::from(n.as_f64().unwrap_or(0.0) + 1.5)
                }
            }
            Value::String(s) => Value::String(format!("{s}-probe")),
            Value::Null => Value::String("probe".to_string()),
            Value::Array(_) => serde_json::json!(["probe-a", "probe-b"]),
            Value::Object(_) => serde_json::json!({ "probe-key": 11 }),
        }
    }

    /// Every field must survive `update()` -> `get()`. A field with an upsert but
    /// no `apply_key` arm (write-only), or an `apply_key` arm but no upsert
    /// (never written), fails here — the class of bug that made
    /// `ext_vision_enabled` a privacy toggle that could not be turned off and
    /// `ext_sensor_enabled` inert.
    #[tokio::test]
    async fn roundtrip_persists_every_field() {
        let repo = fresh_repo().await;
        let base = serde_json::to_value(repo.get().await.unwrap()).unwrap();

        let mut probe = base.clone();
        for (_k, v) in probe.as_object_mut().unwrap().iter_mut() {
            *v = perturb(v);
        }
        let probe_settings: Settings = serde_json::from_value(probe.clone())
            .expect("perturbed settings must still deserialize");
        // Serialize back so comparison uses the same normalisation as the read side.
        let expected = serde_json::to_value(&probe_settings).unwrap();

        repo.update(&probe_settings).await.unwrap();
        let got = serde_json::to_value(repo.get().await.unwrap()).unwrap();

        let mut unsaved = Vec::new();
        for (key, want) in expected.as_object().unwrap() {
            let have = got.get(key);
            if have != Some(want) {
                unsaved.push(format!("{key}: wrote {want}, read back {have:?}"));
            }
        }
        assert!(
            unsaved.is_empty(),
            "settings fields did not survive a write/read round-trip (missing an \
             upsert in update_fields or an arm in apply_key):\n  {}",
            unsaved.join("\n  ")
        );
    }

    /// A targeted write must not revert fields it does not name — the lost-update
    /// path when two clients each save one field.
    #[tokio::test]
    async fn update_fields_writes_only_the_named_keys() {
        let repo = fresh_repo().await;

        let mut first = repo.get().await.unwrap();
        first.mic_enabled = false;
        repo.update(&first).await.unwrap();

        // A second writer holding a STALE snapshot (mic_enabled still true)
        // saves only telemetry_enabled.
        let mut stale = repo.get().await.unwrap();
        stale.mic_enabled = true;
        stale.telemetry_enabled = false;
        let only: HashSet<String> = ["telemetry_enabled".to_string()].into_iter().collect();
        repo.update_fields(&stale, Some(&only)).await.unwrap();

        let got = repo.get().await.unwrap();
        assert!(!got.telemetry_enabled, "named key must be written");
        assert!(!got.mic_enabled, "unnamed key must not be reverted");
    }

    const MIGRATION_0035: &str = include_str!("../migrations/system/0035_settings_user_intent.sql");

    /// The on-disk system migration directory. `include_str!` needs a literal
    /// path, so it cannot reach a migration a FUTURE `DEFAULT_ADOPTIONS` entry
    /// names; reading the directory can.
    const SYSTEM_MIGRATIONS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/migrations/system");

    /// The SQL of the system migration whose filename starts with `number`.
    ///
    /// This is the FORWARD half of the tie between `DEFAULT_ADOPTIONS` and the
    /// SQL: an entry naming a migration that was never written fails here
    /// instead of registering an adoption that nothing performs.
    fn migration_sql(number: &str) -> String {
        let mut matches: Vec<std::path::PathBuf> = std::fs::read_dir(SYSTEM_MIGRATIONS_DIR)
            .unwrap_or_else(|e| panic!("cannot read {SYSTEM_MIGRATIONS_DIR}: {e}"))
            .map(|entry| entry.expect("read dir entry").path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(number) && n.ends_with(".sql"))
            })
            .collect();
        matches.sort();
        assert_eq!(
            matches.len(),
            1,
            "DEFAULT_ADOPTIONS names migration {number}, but {SYSTEM_MIGRATIONS_DIR} holds \
             {} file(s) with that prefix ({matches:?}). A registered adoption with no \
             migration file is never performed on any install.",
            matches.len()
        );
        std::fs::read_to_string(&matches[0]).expect("read migration file")
    }

    /// `migration_sql` resolves against `CARGO_MANIFEST_DIR`. If that ever
    /// stops pointing at the shipped migrations, every forward-tie assertion
    /// below would pass vacuously — so pin it to the one file that is also
    /// compiled in via `include_str!`.
    #[test]
    fn migration_lookup_resolves_to_the_shipped_files() {
        assert_eq!(migration_sql("0035"), MIGRATION_0035);
    }

    /// The adoption UPDATE statements of a migration, whitespace-normalised.
    ///
    /// Full-line `--` comments are dropped first; the shipped migrations put
    /// all prose on its own line, and a `--` inside a string literal would be
    /// a false strip.
    fn adoption_update_statements(sql: &str) -> Vec<String> {
        let stripped: String = sql
            .lines()
            .filter(|line| !line.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        stripped
            .split(';')
            .map(|stmt| stmt.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|stmt| stmt.to_ascii_uppercase().starts_with("UPDATE "))
            .collect()
    }

    /// Every migration `DEFAULT_ADOPTIONS` delegates to, oldest first.
    fn adoption_migrations() -> Vec<&'static str> {
        let mut seen: Vec<&'static str> = Vec::new();
        for entry in DEFAULT_ADOPTIONS {
            if !seen.contains(&entry.migration) {
                seen.push(entry.migration);
            }
        }
        seen.sort_unstable();
        seen
    }

    /// The adoption entries one migration is responsible for.
    fn adoptions_for(migration: &str) -> Vec<&'static DefaultAdoption> {
        DEFAULT_ADOPTIONS
            .iter()
            .filter(|a| a.migration == migration)
            .collect()
    }

    /// Replay the value-adoption half of a migration against an
    /// already-migrated schema, and report how many statements ran.
    ///
    /// `fresh_repo` runs migrations on an EMPTY settings table, so the only
    /// way to exercise an adoption is to seed the pre-migration rows and run
    /// the UPDATEs again. It executes the SHIPPED SQL (minus the one-shot
    /// `ALTER TABLE`) rather than a copy, so these assertions cannot drift
    /// from what an install actually runs.
    async fn replay_adoption_updates(pool: &Pool<Sqlite>, sql: &str) -> usize {
        let statements = adoption_update_statements(sql);
        for stmt in &statements {
            sqlx::query(stmt).execute(pool).await.unwrap();
        }
        statements.len()
    }

    /// Replay every registered adoption in migration order, as an install that
    /// upgrades across all of them does.
    async fn replay_default_adoption(pool: &Pool<Sqlite>) -> usize {
        let mut ran = 0;
        for migration in adoption_migrations() {
            ran += replay_adoption_updates(pool, &migration_sql(migration)).await;
        }
        ran
    }

    /// The tie in BOTH directions, statically:
    ///
    /// - forward — every `DEFAULT_ADOPTIONS` entry has an UPDATE that actually
    ///   moves its key from `old_default` to `new_default`, guarded on
    ///   `is_user_set`;
    /// - backward — a migration carries no adoption UPDATE that the registry
    ///   does not describe.
    ///
    /// Without this, a registry entry and its SQL can disagree (or the SQL can
    /// be missing outright) and every runtime test still passes, because they
    /// only ever replay the statements that do exist.
    #[test]
    fn every_adoption_entry_has_matching_migration_sql() {
        for migration in adoption_migrations() {
            let statements = adoption_update_statements(&migration_sql(migration));
            let entries = adoptions_for(migration);

            assert_eq!(
                statements.len(),
                entries.len(),
                "migration {migration} carries {} adoption UPDATE(s) but DEFAULT_ADOPTIONS \
                 lists {} key(s) for it: {statements:#?}",
                statements.len(),
                entries.len()
            );

            for e in entries {
                let want_set = format!("SET value = '{}'", e.new_default);
                let want_key = format!("key = '{}'", e.key);
                let want_old = format!("AND value = '{}'", e.old_default);
                // `AND is_user_set = 0`, not a bare `is_user_set = 0`: the bare
                // substring also matches a statement that ASSIGNS the column
                // (`SET is_user_set = 0`, which would unmark a key) instead of
                // guarding on it. Only the conjunction proves the guard is an
                // additional condition on the WHERE clause.
                const WANT_GUARD: &str = "AND is_user_set = 0";
                assert!(
                    statements.iter().any(|s| s.contains(&want_set)
                        && s.contains(&want_key)
                        && s.contains(&want_old)
                        && s.contains(WANT_GUARD)),
                    "migration {migration} has no UPDATE matching DEFAULT_ADOPTIONS entry \
                     `{}` ({} -> {}). Expected a statement containing `{want_key}`, \
                     `{want_old}`, `{want_set}` and `{WANT_GUARD}`; found: {statements:#?}",
                    e.key,
                    e.old_default,
                    e.new_default
                );
            }
        }
    }

    /// `new_default` must be the literal the STORE actually holds for today's
    /// default — the half of the registry check that pond-core cannot make.
    ///
    /// The domain has no way to render a field the way this adapter does, and
    /// the obvious stand-in is wrong: the adapter writes numbers with
    /// `Display`, while a `serde_json` round-trip widens every `f32` to `f64`
    /// (`0.05f32` is `0.05` here but `0.05000000074505806` through JSON). A
    /// registry checked against the JSON form would demand a literal that no
    /// `WHERE value = '...'` guard could ever match, so the migration would
    /// silently adopt nothing.
    ///
    /// So write `Settings::default()` through the real adapter and read the
    /// rows back. No rendering is inferred; whatever an install stores is what
    /// the registry must name.
    #[tokio::test]
    async fn every_adoption_entry_states_the_literal_the_adapter_writes() {
        let repo = fresh_repo().await;
        repo.update(&Settings::default()).await.unwrap();

        let mut newest: std::collections::BTreeMap<&str, &DefaultAdoption> =
            std::collections::BTreeMap::new();
        for e in DEFAULT_ADOPTIONS {
            newest.insert(e.key, e);
        }

        for (key, entry) in newest {
            let stored = raw_value(&repo.pool, key).await.unwrap_or_else(|| {
                panic!(
                    "`{key}` is in DEFAULT_ADOPTIONS but the adapter writes no row for it, \
                     so its migration UPDATEs a key nothing reads"
                )
            });
            assert_eq!(
                stored, entry.new_default,
                "`{key}` is stored as `{stored}` for today's default, but the newest \
                 DEFAULT_ADOPTIONS entry (migration {}) claims `{}`, so migration {} adopts \
                 a value no install will ever hold. APPEND a new entry \
                 {{ key: \"{key}\", old_default: \"{}\", new_default: \"{stored}\", \
                 migration: \"00NN\" }} plus a new 00NN migration — never edit an entry \
                 whose migration has already run.",
                entry.migration, entry.new_default, entry.migration, entry.new_default
            );
        }
    }

    /// Write a row as it would have looked BEFORE the migration.
    async fn seed_row(pool: &Pool<Sqlite>, key: &str, value: &str, user_set: i64) {
        sqlx::query(
            "INSERT INTO settings (key, value, is_user_set, updated_at) \
             VALUES (?, ?, ?, datetime('now'))",
        )
        .bind(key)
        .bind(value)
        .bind(user_set)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn raw_value(pool: &Pool<Sqlite>, key: &str) -> Option<String> {
        sqlx::query_as::<_, (String,)>("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(pool)
            .await
            .unwrap()
            .map(|(v,)| v)
    }

    /// An install whose row still holds the OLD default never chose it, so the
    /// new default is adopted — the whole point of 0035.
    ///
    /// Driven off `DEFAULT_ADOPTIONS` rather than a fixed key list, so a later
    /// migration is covered the day it is registered. Keys are seeded at the
    /// value they held before their FIRST adoption and the migrations replay in
    /// order, which is what an install upgrading across several of them does.
    #[tokio::test]
    async fn migration_adopts_defaults_the_user_never_chose() {
        let repo = fresh_repo().await;
        let migrations = adoption_migrations();
        assert!(
            !migrations.is_empty(),
            "DEFAULT_ADOPTIONS must adopt at least one default"
        );

        let mut seeded: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for e in DEFAULT_ADOPTIONS {
            if seeded.insert(e.key) {
                seed_row(&repo.pool, e.key, e.old_default, 0).await;
            }
        }

        for migration in &migrations {
            let entries = adoptions_for(migration);
            let ran = replay_adoption_updates(&repo.pool, &migration_sql(migration)).await;
            assert_eq!(
                ran,
                entries.len(),
                "migration {migration} carries {ran} UPDATE statement(s) but \
                 DEFAULT_ADOPTIONS lists {} key(s) for it",
                entries.len()
            );
        }

        // Each key ends at the NEWEST registered value for it — with chained
        // adoptions that is the last entry, not the first.
        for key in &seeded {
            let newest = DEFAULT_ADOPTIONS
                .iter()
                .rfind(|e| &e.key == key)
                .expect("seeded from the registry");
            assert_eq!(
                raw_value(&repo.pool, key).await.as_deref(),
                Some(newest.new_default),
                "`{key}` was not adopted through to the newest registered default"
            );
        }

        // The adopted literals must also PARSE back into what the code defaults
        // to today — a stored '50' is worthless if `apply_key` drops it.
        let got = repo.get().await.unwrap();
        let want = Settings::default();
        assert_eq!(got.agent_max_turns, want.agent_max_turns);
        assert_eq!(
            got.hybrid_compaction_enabled,
            want.hybrid_compaction_enabled
        );
    }

    /// A stored value that differs from the old default IS a choice, whether or
    /// not it was ever marked. Adoption must leave it alone.
    #[tokio::test]
    async fn migration_leaves_a_deliberately_different_value_alone() {
        let repo = fresh_repo().await;
        seed_row(&repo.pool, "agent_max_turns", "5", 0).await;
        seed_row(&repo.pool, "hybrid_compaction_enabled", "true", 0).await;

        replay_default_adoption(&repo.pool).await;

        assert_eq!(
            raw_value(&repo.pool, "agent_max_turns").await.as_deref(),
            Some("5")
        );
        assert_eq!(repo.get().await.unwrap().agent_max_turns, 5);
    }

    /// Re-running the file (a restored backup, a re-applied migration, a copy
    /// of the guard in a later migration) must change nothing the second time.
    ///
    /// EVERY registered key is marked, not one of them. Marking only
    /// `agent_max_turns` proved only that `agent_max_turns`' UPDATE carries the
    /// `is_user_set` guard; a later entry whose UPDATE omitted it would re-adopt
    /// a value the user had deliberately chosen, and this test would still pass.
    #[tokio::test]
    async fn migration_is_idempotent() {
        let repo = fresh_repo().await;
        // The value each key held before its FIRST adoption, and the value it
        // should hold after all of them.
        let mut oldest: Vec<(&str, &str)> = Vec::new();
        let mut newest: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
        for e in DEFAULT_ADOPTIONS {
            if !oldest.iter().any(|(k, _)| *k == e.key) {
                oldest.push((e.key, e.old_default));
            }
            newest.insert(e.key, e.new_default);
        }
        assert!(!oldest.is_empty(), "DEFAULT_ADOPTIONS must not be empty");

        for (key, old) in &oldest {
            seed_row(&repo.pool, key, old, 0).await;
        }

        replay_default_adoption(&repo.pool).await;
        let after_first = repo.get().await.unwrap();

        // A second pass over already-adopted rows moves nothing.
        replay_default_adoption(&repo.pool).await;
        for (key, want) in &newest {
            assert_eq!(
                raw_value(&repo.pool, key).await.as_deref(),
                Some(*want),
                "a second pass moved `{key}` off its already-adopted value"
            );
        }

        // The user then deliberately picks the OLD value back for EVERY
        // registered key and says so. Raw SQL for the values because the keys
        // are enumerated from the registry and have no common typed setter;
        // `mark_user_set` is the real adapter method.
        for (key, old) in &oldest {
            sqlx::query("UPDATE settings SET value = ? WHERE key = ?")
                .bind(old)
                .bind(key)
                .execute(&repo.pool)
                .await
                .unwrap();
        }
        let marked: HashSet<String> = oldest.iter().map(|(k, _)| k.to_string()).collect();
        repo.mark_user_set(&marked).await.unwrap();

        replay_default_adoption(&repo.pool).await;

        for (key, old) in &oldest {
            assert_eq!(
                raw_value(&repo.pool, key).await.as_deref(),
                Some(*old),
                "`{key}` was re-adopted after the user marked it — its migration UPDATE is \
                 missing the `AND is_user_set = 0` guard"
            );
        }
        // And the marked values still parse back through `apply_key`.
        let after_marked = repo.get().await.unwrap();
        assert_ne!(
            after_marked.agent_max_turns, after_first.agent_max_turns,
            "the marked pass must have restored the old value, or this test is vacuous"
        );
    }

    /// Only the keys a `PUT /api/v1/settings` patch carries count as user
    /// intent. Snapshot writers pin every key and must claim nothing.
    #[tokio::test]
    async fn only_a_patch_write_records_user_intent() {
        let repo = fresh_repo().await;

        // Server-side full snapshot (onboarding, sync_assignments_to_settings).
        repo.update(&Settings::default()).await.unwrap();
        assert!(!repo.is_user_set("agent_max_turns").await.unwrap());
        repo.set_key("chat_model", "gemma".to_string())
            .await
            .unwrap();
        assert!(!repo.is_user_set("chat_model").await.unwrap());

        // The PUT path: write the patch keys, then claim intent for exactly them.
        let mut s = repo.get().await.unwrap();
        s.agent_max_turns = 12;
        let patch: HashSet<String> = ["agent_max_turns".to_string()].into_iter().collect();
        repo.update_fields(&s, Some(&patch)).await.unwrap();
        repo.mark_user_set(&patch).await.unwrap();
        assert!(repo.is_user_set("agent_max_turns").await.unwrap());
        assert!(!repo.is_user_set("hybrid_compaction_enabled").await.unwrap());

        // A later value write must not forget the mark — `INSERT OR REPLACE`
        // dropped the row and silently reset the flag.
        repo.update(&repo.get().await.unwrap()).await.unwrap();
        repo.set_key("agent_max_turns", "12".to_string())
            .await
            .unwrap();
        assert!(repo.is_user_set("agent_max_turns").await.unwrap());

        // A patch key that is not a Settings field must not conjure a row.
        let bogus: HashSet<String> = ["not_a_setting".to_string()].into_iter().collect();
        repo.mark_user_set(&bogus).await.unwrap();
        assert!(!repo.is_user_set("not_a_setting").await.unwrap());
        assert!(raw_value(&repo.pool, "not_a_setting").await.is_none());
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
}
