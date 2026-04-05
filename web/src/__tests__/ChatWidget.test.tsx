import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import ChatWidget from '../components/ChatWidget'
import { mockSpeak } from '../test-setup'

const TOKEN = 'test-token'
const PREVIEW = 'dev-mock-token'

beforeEach(() => {
  vi.restoreAllMocks()
  localStorage.clear()
})

// ── History loading ───────────────────────────────────────────────────────────

describe('ChatWidget history loading', () => {
  it('shows greeting when no session stored', async () => {
    render(<ChatWidget token={TOKEN} />)
    await waitFor(() => expect(screen.queryByText('Loading conversation…')).toBeNull())
    expect(screen.getByText(/Good (morning|afternoon|evening)/)).toBeTruthy()
  })

  it('loads history from backend when session id is stored', async () => {
    localStorage.setItem('pond_chat_session_id', 'sess-123')
    vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce({
      ok: true,
      json: () => Promise.resolve({
        messages: [
          { id: '1', session_id: 'sess-123', role: 'user', content: 'Hello', created_at: '' },
          { id: '2', session_id: 'sess-123', role: 'assistant', content: 'Hi!', created_at: '' },
        ],
      }),
    } as Response)

    render(<ChatWidget token={TOKEN} />)
    await waitFor(() => {
      expect(screen.getByText('Hello')).toBeTruthy()
      expect(screen.getByText('Hi!')).toBeTruthy()
    })
  })

  it('falls back to greeting if history fetch fails', async () => {
    localStorage.setItem('pond_chat_session_id', 'sess-bad')
    vi.spyOn(globalThis, 'fetch').mockRejectedValueOnce(new Error('Network error'))

    render(<ChatWidget token={TOKEN} />)
    await waitFor(() => {
      expect(screen.getByText(/Good (morning|afternoon|evening)/)).toBeTruthy()
    })
  })
})

// ── Sending messages ──────────────────────────────────────────────────────────

describe('ChatWidget sending messages', () => {
  it('appends user and assistant messages on send', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce({
      ok: true,
      json: () => Promise.resolve({ session_id: 'sess-1', response: 'Hello back!' }),
    } as Response)

    render(<ChatWidget token={TOKEN} />)
    await waitFor(() => expect(screen.queryByText('Loading conversation…')).toBeNull())

    const input = screen.getByPlaceholderText(/Type a message/)
    await userEvent.type(input, 'Hello')
    fireEvent.submit(input.closest('form')!)

    await waitFor(() => {
      expect(screen.getByText('Hello')).toBeTruthy()
      expect(screen.getByText('Hello back!')).toBeTruthy()
    })
  })

  it('persists session id to localStorage after first message', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce({
      ok: true,
      json: () => Promise.resolve({ session_id: 'new-sess', response: 'Hi!' }),
    } as Response)

    render(<ChatWidget token={TOKEN} />)
    await waitFor(() => expect(screen.queryByText('Loading conversation…')).toBeNull())

    const input = screen.getByPlaceholderText(/Type a message/)
    await userEvent.type(input, 'Ping')
    fireEvent.submit(input.closest('form')!)

    await waitFor(() => {
      expect(localStorage.getItem('pond_chat_session_id')).toBe('new-sess')
    })
  })
})

// ── TTS ───────────────────────────────────────────────────────────────────────

describe('ChatWidget TTS', () => {
  it('speaks assistant response when not muted', async () => {
    localStorage.setItem('pond_tts_muted', 'false')
    vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce({
      ok: true,
      json: () => Promise.resolve({ session_id: 's1', response: 'Speaking now.' }),
    } as Response)

    render(<ChatWidget token={TOKEN} />)
    await waitFor(() => expect(screen.queryByText('Loading conversation…')).toBeNull())

    const input = screen.getByPlaceholderText(/Type a message/)
    await userEvent.type(input, 'Test')
    fireEvent.submit(input.closest('form')!)

    await waitFor(() => {
      expect(mockSpeak).toHaveBeenCalled()
    })
  })

  it('does not speak when muted', async () => {
    localStorage.setItem('pond_tts_muted', 'true')
    vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce({
      ok: true,
      json: () => Promise.resolve({ session_id: 's1', response: 'Silent response.' }),
    } as Response)

    render(<ChatWidget token={TOKEN} />)
    await waitFor(() => expect(screen.queryByText('Loading conversation…')).toBeNull())

    const input = screen.getByPlaceholderText(/Type a message/)
    await userEvent.type(input, 'Test')
    fireEvent.submit(input.closest('form')!)

    await waitFor(() => {
      expect(screen.getByText('Silent response.')).toBeTruthy()
    })
    expect(mockSpeak).not.toHaveBeenCalled()
  })
})

// ── Preview mode ──────────────────────────────────────────────────────────────

describe('ChatWidget preview mode', () => {
  it('responds with preview message without calling fetch', async () => {
    const spy = vi.spyOn(globalThis, 'fetch')
    render(<ChatWidget token={PREVIEW} />)
    await waitFor(() => expect(screen.queryByText('Loading conversation…')).toBeNull())

    const input = screen.getByPlaceholderText(/Type a message/)
    await userEvent.type(input, 'Hello preview')
    fireEvent.submit(input.closest('form')!)

    await waitFor(() => {
      expect(screen.getByText(/Preview mode/)).toBeTruthy()
    })
    expect(spy).not.toHaveBeenCalled()
  })
})

// ── Clear chat ────────────────────────────────────────────────────────────────

describe('ChatWidget clear chat', () => {
  it('resets messages and removes session id', async () => {
    localStorage.setItem('pond_chat_session_id', 'old-sess')
    render(<ChatWidget token={PREVIEW} />)
    await waitFor(() => expect(screen.queryByText('Loading conversation…')).toBeNull())

    const clearBtn = screen.getByTitle('Clear chat')
    fireEvent.click(clearBtn)

    expect(localStorage.getItem('pond_chat_session_id')).toBeNull()
  })
})

// ── Mute toggle button ────────────────────────────────────────────────────────

describe('ChatWidget mute button', () => {
  it('toggles mute label and localStorage', async () => {
    render(<ChatWidget token={PREVIEW} />)
    await waitFor(() => expect(screen.queryByText('Loading conversation…')).toBeNull())

    const muteBtn = screen.getByTitle('Mute voice')
    fireEvent.click(muteBtn)
    expect(localStorage.getItem('pond_tts_muted')).toBe('true')
    expect(screen.getByTitle('Unmute voice')).toBeTruthy()
  })
})

// ── Agent toggle ──────────────────────────────────────────────────────────────

describe('ChatWidget agent toggle', () => {
  it('disables input when agent is stopped', async () => {
    render(<ChatWidget token={PREVIEW} />)
    await waitFor(() => expect(screen.queryByText('Loading conversation…')).toBeNull())

    const stopBtn = screen.getByTitle('Stop agent')
    fireEvent.click(stopBtn)

    const input = screen.getByPlaceholderText(/Agent is stopped/)
    expect(input).toBeDisabled()
  })

  it('shows stopped message in chat', async () => {
    render(<ChatWidget token={PREVIEW} />)
    await waitFor(() => expect(screen.queryByText('Loading conversation…')).toBeNull())

    fireEvent.click(screen.getByTitle('Stop agent'))
    expect(screen.getByText(/Agent stopped/)).toBeTruthy()
  })
})
