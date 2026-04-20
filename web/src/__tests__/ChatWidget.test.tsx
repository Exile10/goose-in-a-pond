import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import ChatWidget from '../components/ChatWidget'

const TOKEN = 'test-token'
const PREVIEW = 'dev-mock-token'

function mockSseResponse(events: Array<Record<string, unknown>>): Response {
  const payload = events.map((ev) => `data: ${JSON.stringify(ev)}\n`).join('')
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(new TextEncoder().encode(payload))
      controller.close()
    },
  })
  return { ok: true, body } as unknown as Response
}

function mockChatStreamResponse(sessionId: string, text: string, modelRole = 'chat'): Response {
  return mockSseResponse([
    { type: 'text', content: text, token: text },
    { done: true, session_id: sessionId, model_role: modelRole },
  ])
}

function mockTtsResponse(): Response {
  return {
    ok: true,
    blob: () => Promise.resolve(new Blob(['wav'], { type: 'audio/wav' })),
  } as unknown as Response
}

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
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(mockChatStreamResponse('sess-1', 'Hello back!'))
      .mockResolvedValueOnce(mockTtsResponse())

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
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(mockChatStreamResponse('new-sess', 'Hi!'))
      .mockResolvedValueOnce(mockTtsResponse())

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
    const fetchSpy = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(mockChatStreamResponse('s1', 'Speaking now.'))
      .mockResolvedValueOnce(mockTtsResponse())

    render(<ChatWidget token={TOKEN} />)
    await waitFor(() => expect(screen.queryByText('Loading conversation…')).toBeNull())

    const input = screen.getByPlaceholderText(/Type a message/)
    await userEvent.type(input, 'Test')
    fireEvent.submit(input.closest('form')!)

    await waitFor(() => {
      expect(screen.getByText('Speaking now.')).toBeTruthy()
    })
    expect(fetchSpy).toHaveBeenCalledTimes(2)
    expect(fetchSpy.mock.calls.some(([url]) => String(url).includes('/api/v1/tts'))).toBe(true)
  })

  it('does not speak when muted', async () => {
    localStorage.setItem('pond_tts_muted', 'true')
    const fetchSpy = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(mockChatStreamResponse('s1', 'Silent response.'))

    render(<ChatWidget token={TOKEN} />)
    await waitFor(() => expect(screen.queryByText('Loading conversation…')).toBeNull())

    const input = screen.getByPlaceholderText(/Type a message/)
    await userEvent.type(input, 'Test')
    fireEvent.submit(input.closest('form')!)

    await waitFor(() => {
      expect(screen.getByText('Silent response.')).toBeTruthy()
    })
    expect(fetchSpy).toHaveBeenCalledTimes(1)
    expect(fetchSpy.mock.calls[0]?.[0]).toContain('/api/v1/chat/stream')
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
