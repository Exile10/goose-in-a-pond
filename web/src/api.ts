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

// ── Helpers ───────────────────────────────────────────────────────────────────

async function post<T>(path: string, body: unknown, token?: string): Promise<T> {
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

async function put<T>(path: string, body: unknown, token: string): Promise<T> {
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

async function get<T>(path: string, token: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
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
  /** Step 1: verify this device and get a session token */
  handshake: (req: HandshakeRequest) =>
    post<HandshakeResponse>('/handshake', req),

  /** Step 2: submit profile info */
  createProfile: (profile: { name: string; display_name: string }, token: string) =>
    post<{ status: string }>('/onboard', profile, token),

  /** Step 3: save personality settings */
  saveSettings: (settings: Record<string, string>, token: string) =>
    put<{ status: string }>('/settings', settings, token),

  /** Step 4: list already-registered devices */
  listDevices: (token: string) =>
    get<{ devices: { id: string; name: string }[] }>('/devices', token),

  /** Step 4: register a new device */
  registerDevice: (device: { name: string; type: string }, token: string) =>
    post<{ status: string }>('/devices', device, token),

  /** Check whether this device has completed onboarding */
  onboardingStatus: () =>
    get<OnboardingStatus>('/onboard/status', ''),

  /** System info — hostname, version, platform */
  systemInfo: (token: string) =>
    get<{ hostname: string; version: string; platform: string; arch: string }>('/system/info', token),

  /** Health check */
  health: () =>
    get<{ status: string; version: string }>('/health', ''),

  /** Send a chat message */
  chat: (message: string, token: string, sessionId?: string) =>
    post<{ session_id: string; response: string }>('/chat', { message, session_id: sessionId }, token),
}
