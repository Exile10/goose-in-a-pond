import { describe, it, expect, vi, beforeEach } from 'vitest'
import { api } from '../api'

const TOKEN = 'test-token'

function mockFetch(body: unknown, status = 200) {
  return vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce({
    ok: status >= 200 && status < 300,
    status,
    json: () => Promise.resolve(body),
    text: () => Promise.resolve(String(status)),
  } as Response)
}

beforeEach(() => vi.restoreAllMocks())

// ── getSettings ───────────────────────────────────────────────────────────────

describe('api.getSettings', () => {
  it('calls GET /api/v1/settings with auth header', async () => {
    const spy = mockFetch({ assistant_name: 'Goose' })
    const result = await api.getSettings(TOKEN)
    expect(spy).toHaveBeenCalledWith('/api/v1/settings', expect.objectContaining({
      headers: expect.objectContaining({ 'Authorization': `Bearer ${TOKEN}` }),
    }))
    expect(result).toMatchObject({ assistant_name: 'Goose' })
  })

  it('throws on non-200', async () => {
    mockFetch({ error: 'unauthorized' }, 401)
    await expect(api.getSettings(TOKEN)).rejects.toThrow()
  })
})

// ── getSessionMessages ────────────────────────────────────────────────────────

describe('api.getSessionMessages', () => {
  it('calls GET /api/v1/sessions/:id/messages', async () => {
    const messages = [{ id: '1', session_id: 'sess', role: 'user', content: 'Hi', created_at: '' }]
    const spy = mockFetch({ messages })
    const result = await api.getSessionMessages('sess-123', TOKEN)
    expect(spy).toHaveBeenCalledWith(
      '/api/v1/sessions/sess-123/messages',
      expect.objectContaining({ headers: expect.objectContaining({ 'Authorization': `Bearer ${TOKEN}` }) })
    )
    expect(result.messages).toHaveLength(1)
  })
})

// ── listSchedules ─────────────────────────────────────────────────────────────

describe('api.listSchedules', () => {
  it('calls GET /api/v1/schedules', async () => {
    const spy = mockFetch([])
    await api.listSchedules(TOKEN)
    expect(spy).toHaveBeenCalledWith('/api/v1/schedules', expect.anything())
  })
})

// ── createSchedule ────────────────────────────────────────────────────────────

describe('api.createSchedule', () => {
  it('calls POST /api/v1/schedules with correct body', async () => {
    const spy = mockFetch({ id: 'task-1', label: 'Daily', cron: '0 0 8 * * *', paused: false, currently_running: false, last_run: null, next_run: null })
    const req = { id: 'task-1', label: 'Daily', cron: '0 0 8 * * *', payload: {} }
    await api.createSchedule(req, TOKEN)
    expect(spy).toHaveBeenCalledWith('/api/v1/schedules', expect.objectContaining({
      method: 'POST',
      body: JSON.stringify(req),
    }))
  })
})

// ── deleteSchedule ────────────────────────────────────────────────────────────

describe('api.deleteSchedule', () => {
  it('calls DELETE /api/v1/schedules/:id', async () => {
    const spy = mockFetch({ status: 'ok' })
    await api.deleteSchedule('task-1', TOKEN)
    expect(spy).toHaveBeenCalledWith('/api/v1/schedules/task-1', expect.objectContaining({ method: 'DELETE' }))
  })
})

// ── pauseSchedule / resumeSchedule ───────────────────────────────────────────

describe('api.pauseSchedule', () => {
  it('calls POST /api/v1/schedules/:id/pause', async () => {
    const spy = mockFetch({ status: 'ok' })
    await api.pauseSchedule('t1', TOKEN)
    expect(spy).toHaveBeenCalledWith('/api/v1/schedules/t1/pause', expect.objectContaining({ method: 'POST' }))
  })
})

describe('api.resumeSchedule', () => {
  it('calls POST /api/v1/schedules/:id/resume', async () => {
    const spy = mockFetch({ status: 'ok' })
    await api.resumeSchedule('t1', TOKEN)
    expect(spy).toHaveBeenCalledWith('/api/v1/schedules/t1/resume', expect.objectContaining({ method: 'POST' }))
  })
})

describe('api.runScheduleNow', () => {
  it('calls POST /api/v1/schedules/:id/run-now', async () => {
    const spy = mockFetch({ status: 'ok' })
    await api.runScheduleNow('t1', TOKEN)
    expect(spy).toHaveBeenCalledWith('/api/v1/schedules/t1/run-now', expect.objectContaining({ method: 'POST' }))
  })
})

// ── getSensors ────────────────────────────────────────────────────────────────

describe('api.getSensors', () => {
  it('calls GET /api/v1/sensors/:device_id with limit', async () => {
    const spy = mockFetch({ readings: [] })
    await api.getSensors('dev-1', TOKEN, 5)
    expect(spy).toHaveBeenCalledWith('/api/v1/sensors/dev-1?limit=5', expect.anything())
  })
})

// ── listCameraEvents ──────────────────────────────────────────────────────────

describe('api.listCameraEvents', () => {
  it('calls GET /api/v1/camera/events', async () => {
    const spy = mockFetch({ events: [] })
    await api.listCameraEvents(TOKEN)
    expect(spy).toHaveBeenCalledWith('/api/v1/camera/events?limit=20', expect.anything())
  })
})
