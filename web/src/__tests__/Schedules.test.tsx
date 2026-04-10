import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import Schedules from '../pages/Schedules'

const TOKEN = 'test-token'
const PREVIEW = 'dev-mock-token'

function mockTask(overrides = {}) {
  return {
    id: 'task-1',
    label: 'Daily report',
    cron: '0 0 8 * * *',
    paused: false,
    currently_running: false,
    last_run: null,
    next_run: '2026-04-05T08:00:00Z',
    ...overrides,
  }
}

function mockFetch(body: unknown, status = 200) {
  return vi.spyOn(globalThis, 'fetch').mockResolvedValue({
    ok: status >= 200 && status < 300,
    status,
    json: () => Promise.resolve(body),
    text: () => Promise.resolve(String(status)),
  } as Response)
}

beforeEach(() => {
  vi.restoreAllMocks()
  localStorage.clear()
})

// ── Preview mode ──────────────────────────────────────────────────────────────

describe('Schedules preview mode', () => {
  it('renders empty state without calling API', async () => {
    render(<Schedules token={PREVIEW} />)
    await waitFor(() => {
      expect(screen.getByText(/No scheduled tasks yet/)).toBeTruthy()
    })
  })
})

// ── Loading and listing ───────────────────────────────────────────────────────

describe('Schedules listing', () => {
  it('shows task list after loading', async () => {
    mockFetch([mockTask()])
    render(<Schedules token={TOKEN} />)
    await waitFor(() => {
      expect(screen.getByText('Daily report')).toBeTruthy()
      expect(screen.getByText('0 0 8 * * *')).toBeTruthy()
    })
  })

  it('shows Active badge for active task', async () => {
    mockFetch([mockTask()])
    render(<Schedules token={TOKEN} />)
    await waitFor(() => {
      expect(screen.getByText('Active')).toBeTruthy()
    })
  })

  it('shows Paused badge for paused task', async () => {
    mockFetch([mockTask({ paused: true })])
    render(<Schedules token={TOKEN} />)
    await waitFor(() => {
      expect(screen.getByText('Paused')).toBeTruthy()
    })
  })

  it('shows Running badge for running task', async () => {
    mockFetch([mockTask({ currently_running: true })])
    render(<Schedules token={TOKEN} />)
    await waitFor(() => {
      expect(screen.getByText('Running')).toBeTruthy()
    })
  })
})

// ── Create form ───────────────────────────────────────────────────────────────

describe('Schedules create form', () => {
  it('shows form when + New Schedule is clicked', async () => {
    mockFetch([])
    render(<Schedules token={TOKEN} />)
    await waitFor(() => screen.getByText(/No scheduled tasks yet/))

    fireEvent.click(screen.getByText('+ New Schedule'))
    expect(screen.getByPlaceholderText('Task name')).toBeTruthy()
    expect(screen.getByPlaceholderText('0 0 8 * * *')).toBeTruthy()
  })

  it('creates a schedule and refreshes the list', async () => {
    const created = mockTask({ id: 'new-1', label: 'Morning standup', cron: '0 0 9 * * 1-5' })
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve([]) } as Response)          // initial load
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve(created) } as Response)     // create
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve([created]) } as Response)   // reload

    render(<Schedules token={TOKEN} />)
    await waitFor(() => screen.getByText(/No scheduled tasks yet/))

    fireEvent.click(screen.getByText('+ New Schedule'))

    await userEvent.type(screen.getByPlaceholderText('Task name'), 'Morning standup')
    await userEvent.type(screen.getByPlaceholderText('0 0 8 * * *'), '0 0 9 * * 1-5')

    await act(async () => {
      fireEvent.click(screen.getByText('Create'))
    })

    await waitFor(() => {
      expect(screen.getByText('Morning standup')).toBeTruthy()
    })
  })
})

// ── Delete ────────────────────────────────────────────────────────────────────

describe('Schedules delete', () => {
  it('removes task from list on delete', async () => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve([mockTask()]) } as Response)  // load
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve({ status: 'ok' }) } as Response) // delete

    render(<Schedules token={TOKEN} />)
    await waitFor(() => screen.getByText('Daily report'))

    fireEvent.click(screen.getByTitle('Delete'))

    await waitFor(() => {
      expect(screen.queryByText('Daily report')).toBeNull()
    })
  })
})

// ── Pause / Resume ────────────────────────────────────────────────────────────

describe('Schedules pause/resume', () => {
  it('shows Resume button for paused task', async () => {
    mockFetch([mockTask({ paused: true })])
    render(<Schedules token={TOKEN} />)
    await waitFor(() => {
      expect(screen.getByText('Resume')).toBeTruthy()
    })
  })

  it('calls pause API and reloads', async () => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve([mockTask()]) } as Response)            // load
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve({ status: 'ok' }) } as Response)        // pause
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve([mockTask({ paused: true })]) } as Response) // reload

    render(<Schedules token={TOKEN} />)
    await waitFor(() => screen.getByText('Pause'))
    fireEvent.click(screen.getByText('Pause'))
    await waitFor(() => screen.getByText('Resume'))
  })
})

// ── Run now ───────────────────────────────────────────────────────────────────

describe('Schedules run now', () => {
  it('calls run-now API on button click', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve([mockTask()]) } as Response)
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve({ status: 'ok' }) } as Response)

    render(<Schedules token={TOKEN} />)
    await waitFor(() => screen.getByText('Run'))
    fireEvent.click(screen.getByText('Run'))

    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalledWith(
        expect.stringContaining('/run-now'),
        expect.objectContaining({ method: 'POST' })
      )
    })
  })
})
