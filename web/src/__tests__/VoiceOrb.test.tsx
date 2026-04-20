import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react'
import VoiceOrb from '../components/VoiceOrb'

const TOKEN = 'test-token'

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

function mockChatStreamResponse(sessionId: string, text: string): Response {
  return mockSseResponse([
    { type: 'text', content: text, token: text },
    { done: true, session_id: sessionId, model_role: 'chat' },
  ])
}

function mockTtsResponse(): Response {
  return {
    ok: true,
    blob: () => Promise.resolve(new Blob(['wav'], { type: 'audio/wav' })),
  } as unknown as Response
}

function mockTranscribeAndChat(transcript: string, reply: string, withTts = false) {
  const fetchSpy = vi.spyOn(globalThis, 'fetch')
    .mockResolvedValueOnce({
      ok: true,
      json: () => Promise.resolve({ text: transcript }),
    } as Response)
    .mockResolvedValueOnce(mockChatStreamResponse('sess-1', reply))

  if (withTts) {
    fetchSpy.mockResolvedValueOnce(mockTtsResponse())
  }

  return fetchSpy
}

beforeEach(() => {
  vi.restoreAllMocks()
  localStorage.clear()
})

// ── Rendering ─────────────────────────────────────────────────────────────────

describe('VoiceOrb rendering', () => {
  it('renders in wait state with mic icon', () => {
    render(<VoiceOrb token={TOKEN} />)
    expect(screen.getByLabelText('Tap to speak')).toBeTruthy()
  })

  it('shows mute button', () => {
    render(<VoiceOrb token={TOKEN} />)
    expect(screen.getByLabelText('Mute voice output')).toBeTruthy()
  })
})

// ── Mute toggle ───────────────────────────────────────────────────────────────

describe('VoiceOrb mute toggle', () => {
  it('toggles muted state and persists to localStorage', () => {
    render(<VoiceOrb token={TOKEN} />)
    const muteBtn = screen.getByLabelText('Mute voice output')
    fireEvent.click(muteBtn)
    expect(localStorage.getItem('pond_tts_muted')).toBe('true')
    expect(screen.getByLabelText('Unmute voice output')).toBeTruthy()
    fireEvent.click(screen.getByLabelText('Unmute voice output'))
    expect(localStorage.getItem('pond_tts_muted')).toBe('false')
  })

  it('cancels speech if muted while speaking', async () => {
    localStorage.setItem('pond_tts_muted', 'false')
    const pauseSpy = vi.fn()
    const playSpy = vi.fn().mockResolvedValue(undefined)
    vi.stubGlobal('Audio', vi.fn(() => ({
      play: playSpy,
      pause: pauseSpy,
      onended: null,
      onerror: null,
    })) as unknown as typeof Audio)
    Object.defineProperty(URL, 'createObjectURL', {
      value: vi.fn(() => 'blob:voice'),
      configurable: true,
    })
    Object.defineProperty(URL, 'revokeObjectURL', {
      value: vi.fn(() => {}),
      configurable: true,
    })

    mockTranscribeAndChat('hello', 'hi there', true)
    render(<VoiceOrb token={TOKEN} />)

    // Start listening
    await act(async () => {
      fireEvent.click(screen.getByLabelText('Tap to speak'))
      await new Promise(r => setTimeout(r, 20))
    })

    // Stop recording → triggers transcription → chat → speak state
    await act(async () => {
      fireEvent.click(screen.getByLabelText('Listening…'))
      await new Promise(r => setTimeout(r, 100))
    })

    // Wait until we reach speak state
    await waitFor(() => {
      expect(screen.getByLabelText('Speaking…')).toBeTruthy()
    }, { timeout: 2000 })

    // Mute while in speak state — should cancel current audio playback
    fireEvent.click(screen.getByLabelText('Mute voice output'))
    expect(pauseSpy).toHaveBeenCalled()
    expect(screen.getByLabelText('Tap to speak')).toBeTruthy()
  })
})

// ── Escape key cancels ────────────────────────────────────────────────────────

describe('VoiceOrb escape key', () => {
  it('cancels from listen state on Escape', async () => {
    render(<VoiceOrb token={TOKEN} />)

    await act(async () => {
      fireEvent.click(screen.getByLabelText('Tap to speak'))
      await new Promise(r => setTimeout(r, 20))
    })

    fireEvent.keyDown(window, { key: 'Escape' })
    await waitFor(() => {
      expect(screen.getByLabelText('Tap to speak')).toBeTruthy()
    })
  })
})

// ── Error handling ────────────────────────────────────────────────────────────

describe('VoiceOrb error handling', () => {
  it('returns to wait state if transcription fails', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce({
      ok: false,
      json: () => Promise.resolve({}),
    } as Response)

    render(<VoiceOrb token={TOKEN} />)

    // Start listening
    await act(async () => {
      fireEvent.click(screen.getByLabelText('Tap to speak'))
      await new Promise(r => setTimeout(r, 20))
    })

    // Stop recording to trigger handleRecordingStop → failed fetch → back to wait
    await act(async () => {
      fireEvent.click(screen.getByLabelText('Listening…'))
      await new Promise(r => setTimeout(r, 50))
    })

    await waitFor(() => {
      expect(screen.getByLabelText('Tap to speak')).toBeTruthy()
    })
  })

  it('returns to wait state if transcription returns empty text', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce({
      ok: true,
      json: () => Promise.resolve({ text: '   ' }),
    } as Response)

    render(<VoiceOrb token={TOKEN} />)
    await act(async () => {
      fireEvent.click(screen.getByLabelText('Tap to speak'))
      await new Promise(r => setTimeout(r, 100))
    })

    await waitFor(() => {
      expect(screen.queryByText('Thinking…')).toBeNull()
    })
  })
})

// ── Full loop (muted) ─────────────────────────────────────────────────────────

describe('VoiceOrb full loop muted', () => {
  it('completes listen→think→wait without speaking when muted', async () => {
    localStorage.setItem('pond_tts_muted', 'true')
    const fetchSpy = mockTranscribeAndChat('turn on lights', 'Done, lights on.')
    render(<VoiceOrb token={TOKEN} />)

    await act(async () => {
      fireEvent.click(screen.getByLabelText('Tap to speak'))
      await new Promise(r => setTimeout(r, 20))
    })

    await act(async () => {
      fireEvent.click(screen.getByLabelText('Listening…'))
      await new Promise(r => setTimeout(r, 120))
    })

    await waitFor(() => {
      expect(screen.getByLabelText('Tap to speak')).toBeTruthy()
    }, { timeout: 2000 })

    expect(fetchSpy).toHaveBeenCalledTimes(2)
    expect(fetchSpy.mock.calls.some(([url]) => String(url).includes('/api/v1/tts'))).toBe(false)
  })
})
