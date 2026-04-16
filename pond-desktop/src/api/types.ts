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
  assistant_name: string;
  user_name: string;
  location?: string;
  timezone?: string;
  personality?: string;
  prompt_style: string;
  custom_system_prompt?: string;
  prompt_addendum?: string;
  chat_provider?: string;
  chat_model?: string;
  think_provider?: string;
  think_model?: string;
  task_provider?: string;
  task_model?: string;
  wake_word?: string;
  agent_memory_inject: boolean;
  lat?: number;
  lon?: number;
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
