import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react'
import VoiceOrb from '../components/VoiceOrb'
import { mockSpeak, mockCancel } from '../test-setup'

const TOKEN = 'test-token'

function mockTranscribeAndChat(transcript: string, reply: string) {
  vi.spyOn(globalThis, 'fetch')
    .mockResolvedValueOnce({
      ok: true,
      json: () => Promise.resolve({ text: transcript }),
    } as Response)
    .mockResolvedValueOnce({
      ok: true,
      json: () => Promise.resolve({ session_id: 'sess-1', response: reply }),
    } as Response)
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
    mockTranscribeAndChat('hello', 'hi there')
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

    // Mute while in speak state — should cancel speech synthesis
    fireEvent.click(screen.getByLabelText('Mute voice output'))
    expect(mockCancel).toHaveBeenCalled()
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
    mockTranscribeAndChat('turn on lights', 'Done, lights on.')
    render(<VoiceOrb token={TOKEN} />)

    await act(async () => {
      fireEvent.click(screen.getByLabelText('Tap to speak'))
      await new Promise(r => setTimeout(r, 200))
    })

    await waitFor(() => {
      expect(mockSpeak).not.toHaveBeenCalled()
    }, { timeout: 2000 })
  })
})
