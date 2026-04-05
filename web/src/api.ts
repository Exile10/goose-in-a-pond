const BASE = '/api/v1'

export const DEV_MOCK_TOKEN = 'dev-mock-token'
export const isPreviewMode = (token: string) => token === DEV_MOCK_TOKEN

// ── Types matching pond-core domain ──────────────────────────────────────────

export interface HandshakeRequest {
  client_id: string
  client_type: string
  client_version: string
  pairing_code?: string
}

export interface HandshakeResponse {
  accepted: boolean
  session_token: string | null
  hostname: string
  server_version: string
  capabilities: string[]
  rejection_reason: string | null
}

export interface OnboardingStatus {
  status: string
  onboarded: boolean
  steps_completed: number
  total_steps: number
}

export interface SessionSummary {
  id: string
  title: string | null
  created_at: string
  updated_at: string
}

export interface SessionMessage {
  id: string
  session_id: string
  role: 'user' | 'assistant' | 'system'
  content: string
  created_at: string
}

export interface ScheduledTask {
  id: string
  label: string
  cron: string
  last_run: string | null
  next_run: string | null
  paused: boolean
  currently_running: boolean
}

export interface CreateScheduleRequest {
  id: string
  label: string
  cron: string
  payload: Record<string, unknown>
}

export interface SensorReading {
  device_id: string
  sensor_type: string
  value: number
  unit: string
  recorded_at: string
}

export interface CameraEvent {
  id: number
  camera_id: string
  event_type: string
  confidence: number | null
  snapshot_path: string | null
  acknowledged: boolean
  created_at: string
}

export interface Settings {
  assistant_name: string
  assistant_personality: string
  user_name: string
  timezone: string
  llm_max_tokens: number
  llm_temperature: number
  llm_provider: string
  voice_wake_word: string
  voice_tts_voice: string
  voice_recording_duration_secs: number
  voice_whisper_url: string
  active_llm_model: string
  active_whisper_model: string
  active_tts_model: string
  voice_tts_http_url: string
  voice_tts_http_voice: string
  model_registry_url: string
  weather_enabled: boolean
  weather_latitude: number
  weather_longitude: number
  weather_location_name: string
  retention_event_log_days: number
  retention_sensor_days: number
  retention_session_messages_keep: number
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async function postReq<T>(path: string, body: unknown, token?: string): Promise<T> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' }
  if (token) headers['Authorization'] = `Bearer ${token}`

  const res = await fetch(`${BASE}${path}`, {
    method: 'POST',
    headers,
    body: JSON.stringify(body),
  })

  if (!res.ok) {
    const text = await res.text()
    throw new Error(text || `HTTP ${res.status}`)
  }

  return res.json()
}

async function putReq<T>(path: string, body: unknown, token: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    method: 'PUT',
    headers: {
      'Content-Type': 'application/json',
      'Authorization': `Bearer ${token}`,
    },
    body: JSON.stringify(body),
  })

  if (!res.ok) {
    const text = await res.text()
    throw new Error(text || `HTTP ${res.status}`)
  }

  return res.json()
}

async function getReq<T>(path: string, token: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    headers: token ? { 'Authorization': `Bearer ${token}` } : {},
  })

  if (!res.ok) {
    const text = await res.text()
    throw new Error(text || `HTTP ${res.status}`)
  }

  return res.json()
}

async function deleteReq<T>(path: string, token: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    method: 'DELETE',
    headers: { 'Authorization': `Bearer ${token}` },
  })

  if (!res.ok) {
    const text = await res.text()
    throw new Error(text || `HTTP ${res.status}`)
  }

  return res.json()
}

// ── API calls ─────────────────────────────────────────────────────────────────

export const api = {
  // ── Auth & onboarding ────────────────────────────────────────────────────

  /** Step 1: verify this device and get a session token */
  handshake: (req: HandshakeRequest) =>
    postReq<HandshakeResponse>('/handshake', req),

  /** Step 2: submit profile info */
  createProfile: (profile: { name: string; display_name: string }, token: string) =>
    postReq<{ status: string }>('/onboard', profile, token),

  /** Check whether this device has completed onboarding */
  onboardingStatus: () =>
    getReq<OnboardingStatus>('/onboard/status', ''),

  /** Mark onboarding as complete on the backend, unlocking protected routes */
  completeOnboarding: () =>
    postReq<{ status: string }>('/onboard/complete', {}),

  // ── Settings ─────────────────────────────────────────────────────────────

  /** Load all settings from the backend */
  getSettings: (token: string) =>
    getReq<Settings>('/settings', token),

  /** Save settings (partial update — only provided keys are merged) */
  saveSettings: (settings: Record<string, unknown>, token: string) =>
    putReq<{ status: string }>('/settings', settings, token),

  // ── System ───────────────────────────────────────────────────────────────

  /** System info — hostname, version, platform */
  systemInfo: (token: string) =>
    getReq<{ hostname: string; version: string; platform: string; arch: string }>('/system/info', token),

  /** Health check */
  health: () =>
    getReq<{ status: string; version: string }>('/health', ''),

  // ── Chat & sessions ──────────────────────────────────────────────────────

  /** Send a chat message */
  chat: (message: string, token: string, sessionId?: string) =>
    postReq<{ session_id: string; response: string }>('/chat', { message, session_id: sessionId }, token),

  /** List all sessions */
  listSessions: (token: string) =>
    getReq<{ sessions: SessionSummary[] }>('/sessions', token),

  /** Load all messages for a session */
  getSessionMessages: (sessionId: string, token: string) =>
    getReq<{ messages: SessionMessage[] }>(`/sessions/${sessionId}/messages`, token),

  /** Rename a session */
  renameSession: (sessionId: string, title: string, token: string) =>
    fetch(`${BASE}/sessions/${sessionId}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${token}` },
      body: JSON.stringify({ title }),
    }).then(r => r.json()),

  // ── Devices ──────────────────────────────────────────────────────────────

  /** List already-registered devices */
  listDevices: (token: string) =>
    getReq<{ devices: { id: string; name: string }[] }>('/devices', token),

  /** Register a new device */
  registerDevice: (device: { name: string; type: string }, token: string) =>
    postReq<{ status: string }>('/devices', device, token),

  // ── Sensors ──────────────────────────────────────────────────────────────

  /** Get recent sensor readings for a device */
  getSensors: (deviceId: string, token: string, limit = 5) =>
    getReq<{ readings: SensorReading[] }>(`/sensors/${deviceId}?limit=${limit}`, token),

  // ── Camera ───────────────────────────────────────────────────────────────

  /** List recent camera events */
  listCameraEvents: (token: string, limit = 20) =>
    getReq<{ events: CameraEvent[] }>(`/camera/events?limit=${limit}`, token),

  // ── Scheduler ────────────────────────────────────────────────────────────

  /** List all scheduled tasks */
  listSchedules: (token: string) =>
    getReq<ScheduledTask[]>('/schedules', token),

  /** Create a new scheduled task */
  createSchedule: (req: CreateScheduleRequest, token: string) =>
    postReq<ScheduledTask>('/schedules', req, token),

  /** Delete a scheduled task */
  deleteSchedule: (id: string, token: string) =>
    deleteReq<{ status: string }>(`/schedules/${id}`, token),

  /** Pause a scheduled task */
  pauseSchedule: (id: string, token: string) =>
    postReq<{ status: string }>(`/schedules/${id}/pause`, {}, token),

  /** Resume a paused task */
  resumeSchedule: (id: string, token: string) =>
    postReq<{ status: string }>(`/schedules/${id}/resume`, {}, token),

  /** Trigger a task to run immediately */
  runScheduleNow: (id: string, token: string) =>
    postReq<{ status: string }>(`/schedules/${id}/run-now`, {}, token),
}
