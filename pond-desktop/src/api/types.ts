// ────────────────────────────────────────────────────────────
// GIAP Desktop — API Type Definitions
// Mirror of pond-server REST API shapes
// ────────────────────────────────────────────────────────────

export interface HealthResponse {
  status: string;
  version?: string;
  uptime_seconds?: number;
}

// ── Settings ─────────────────────────────────────────────────
export interface Settings {
  // Identity
  primary_profile_id?: string | null;
  assistant_name: string;
  user_name: string;
  assistant_personality?: string;
  location?: string;       // legacy alias
  personality?: string;    // legacy alias
  timezone?: string;

  // Voice pipeline
  voice_wake_word?: string;
  wake_word?: string;      // legacy alias
  voice_recording_duration_secs?: number;
  voice_whisper_url?: string;
  active_whisper_model?: string;
  active_tts_model?: string;
  voice_tts_voice?: string;

  // Model roles
  chat_provider?: string;
  chat_model?: string;
  think_provider?: string | null;
  think_model?: string | null;
  task_provider?: string | null;
  task_model?: string | null;
  llm_provider?: string;
  llm_temperature?: number;
  llm_max_tokens?: number;
  active_llm_model?: string;

  // Prompts
  prompt_style: string;
  custom_system_prompt?: string | null;
  prompt_addendum?: string;

  // Location / Weather
  weather_enabled?: boolean;
  weather_location_name?: string;
  weather_latitude?: number;
  weather_longitude?: number;
  lat?: number;  // legacy alias
  lon?: number;  // legacy alias

  // Agent behaviour
  agent_goose_mode?: string;
  agent_max_turns?: number;
  agent_memory_inject: boolean;
  agent_memory_limit?: number;

  // Data retention
  retention_event_log_days?: number;
  retention_sensor_days?: number;
  retention_session_messages_keep?: number;
}

// ── Devices ───────────────────────────────────────────────────
export interface Device {
  id: string;
  name: string;
  device_type?: string;
  room?: string;
  is_online: boolean;
  last_seen?: string;
  metadata?: Record<string, unknown>;
}

// ── Schedules ─────────────────────────────────────────────────
export interface Schedule {
  id: string;
  name: string;
  cron: string;
  prompt: string;
  enabled: boolean;
  created_at?: string;
}

// ── Memory ────────────────────────────────────────────────────
export interface MemoryFragment {
  id: string;
  content: string;
  tags?: string[];
  created_at: string;
}

// ── User Skills ───────────────────────────────────────────────
export interface UserSkill {
  id: string;
  name: string;
  content: string;
  enabled: boolean;
  created_at?: string;
}

// ── Models ────────────────────────────────────────────────────
export interface ModelEntry {
  id: string;
  provider: string;
  name: string;
  display_name?: string;
  is_active: boolean;
  ram_estimate_mb?: number;
  recommended_role?: string;
}

export interface ModelMemoryStatus {
  available_mb: number;
  used_mb: number;
  models: Array<{ id: string; name: string; ram_estimate_mb: number; role?: string }>;
}

// ── Prompt Templates ──────────────────────────────────────────
export interface PromptTemplate {
  name: string;
  content: string;
  is_system: boolean;
  updated_at?: string;
}

// ── Agent ─────────────────────────────────────────────────────
export interface PromptExtra {
  key: string;
  content: string;
  enabled: boolean;
}

export interface AgentTool {
  extension: string;
  name: string;
  description?: string;
}

export interface AgentRecipe {
  name: string;
  description?: string;
  yaml: string;
}

// ── Chat / Streaming ──────────────────────────────────────────
export type ChatEventType = "text" | "tool_call" | "done" | "error";

export interface ChatEvent {
  type: ChatEventType;
  content?: string;         // for "text" events
  tool?: string;            // for "tool_call" events
  result?: unknown;         // for "tool_call" events
  error?: string;           // for "error" events
  done?: boolean;
}

// ── Transcription ─────────────────────────────────────────────
export interface TranscribeResponse {
  text: string;
}

// ── API Error ─────────────────────────────────────────────────
export class ApiError extends Error {
  constructor(
    public readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}
