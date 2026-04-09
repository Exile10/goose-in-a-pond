import '@testing-library/jest-dom'

// ── localStorage mock (jsdom doesn't always expose clear/length) ─────────────

const _store: Record<string, string> = {}
const localStorageMock: Storage = {
  getItem: (key) => _store[key] ?? null,
  setItem: (key, value) => { _store[key] = String(value) },
  removeItem: (key) => { delete _store[key] },
  clear: () => { Object.keys(_store).forEach(k => delete _store[k]) },
  get length() { return Object.keys(_store).length },
  key: (i) => Object.keys(_store)[i] ?? null,
}
Object.defineProperty(window, 'localStorage', { value: localStorageMock, writable: true })

// ── SpeechSynthesis mock ──────────────────────────────────────────────────────

const mockSpeak = vi.fn()
const mockCancel = vi.fn()

Object.defineProperty(window, 'speechSynthesis', {
  value: {
    speak: mockSpeak,
    cancel: mockCancel,
    getVoices: vi.fn(() => []),
    pause: vi.fn(),
    resume: vi.fn(),
    speaking: false,
    pending: false,
    paused: false,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  },
  writable: true,
})

Object.defineProperty(window, 'SpeechSynthesisUtterance', {
  value: class MockSpeechSynthesisUtterance {
    text: string
    lang = 'en-US'
    rate = 1
    pitch = 1
    volume = 1
    voice = null
    onstart: (() => void) | null = null
    onend: (() => void) | null = null
    onerror: (() => void) | null = null
    constructor(text: string) { this.text = text }
  },
  writable: true,
})

// ── MediaRecorder mock ────────────────────────────────────────────────────────

class MockMediaRecorder {
  state: 'inactive' | 'recording' | 'paused' = 'inactive'
  ondataavailable: ((e: { data: Blob }) => void) | null = null
  onstop: (() => void) | null = null
  onerror: (() => void) | null = null

  start() {
    this.state = 'recording'
  }

  stop() {
    this.state = 'inactive'
    // Emit a data chunk then fire onstop
    this.ondataavailable?.({ data: new Blob(['audio'], { type: 'audio/webm' }) })
    this.onstop?.()
  }

  static isTypeSupported() { return true }
}

Object.defineProperty(window, 'MediaRecorder', {
  value: MockMediaRecorder,
  writable: true,
})

// ── getUserMedia mock ─────────────────────────────────────────────────────────

Object.defineProperty(navigator, 'mediaDevices', {
  value: {
    getUserMedia: vi.fn(() =>
      Promise.resolve({
        getTracks: () => [{ stop: vi.fn() }],
      })
    ),
  },
  writable: true,
})

// ── SpeechRecognition mock ────────────────────────────────────────────────────

class MockSpeechRecognition {
  lang = 'en-US'
  continuous = false
  interimResults = false
  onstart: (() => void) | null = null
  onresult: ((e: unknown) => void) | null = null
  onerror: (() => void) | null = null
  onend: (() => void) | null = null
  start() { this.onstart?.() }
  stop() { this.onend?.() }
  abort() { this.onend?.() }
}

Object.defineProperty(window, 'SpeechRecognition', {
  value: MockSpeechRecognition,
  writable: true,
})

Object.defineProperty(window, 'webkitSpeechRecognition', {
  value: MockSpeechRecognition,
  writable: true,
})

// ── scrollIntoView mock (not implemented in jsdom) ───────────────────────────

Element.prototype.scrollIntoView = vi.fn()

export { mockSpeak, mockCancel }
