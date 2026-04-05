import { useState, useEffect, useRef, useCallback } from 'react'

type LoopState = 'wait' | 'listen' | 'think' | 'speak'

interface Props {
  token: string
}

const STATE_LABEL: Record<LoopState, string> = {
  wait: 'Tap to speak',
  listen: 'Listening…',
  think: 'Thinking…',
  speak: 'Speaking…',
}

export default function VoiceOrb({ token }: Props) {
  const [loopState, setLoopState] = useState<LoopState>('wait')
  const [transcript, setTranscript] = useState('')
  const [response, setResponse] = useState('')
  const [muted, setMuted] = useState(() => localStorage.getItem('pond_tts_muted') === 'true')
  const [showOverlay, setShowOverlay] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const mediaRecorderRef = useRef<MediaRecorder | null>(null)
  const audioChunksRef = useRef<Blob[]>([])
  const streamRef = useRef<MediaStream | null>(null)
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null)

  // Cancel everything and return to Wait
  const cancel = useCallback(() => {
    if (timerRef.current) clearTimeout(timerRef.current)
    if (mediaRecorderRef.current?.state === 'recording') {
      mediaRecorderRef.current.stop()
    }
    if (streamRef.current) {
      streamRef.current.getTracks().forEach(t => t.stop())
      streamRef.current = null
    }
    window.speechSynthesis?.cancel()
    setLoopState('wait')
    setError(null)
  }, [])

  // Escape key always cancels
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === 'Escape' && loopState !== 'wait') cancel()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [loopState, cancel])

  // Cleanup on unmount
  useEffect(() => () => { cancel() }, [cancel])

  async function handleRecordingStop(chunks: Blob[]) {
    setLoopState('think')
    const blob = new Blob(chunks, { type: 'audio/webm' })

    try {
      // Transcribe via whisper proxy
      const formData = new FormData()
      formData.append('audio', blob, 'audio.webm')
      const transcribeRes = await fetch('/api/v1/transcribe', {
        method: 'POST',
        body: formData,
      })
      if (!transcribeRes.ok) throw new Error('Transcription failed')
      const { text } = await transcribeRes.json() as { text: string }

      if (!text?.trim()) {
        setLoopState('wait')
        return
      }

      setTranscript(text.trim())
      setShowOverlay(true)

      // Chat with the LLM
      const sessionId = localStorage.getItem('pond_voice_session_id') ?? undefined
      const chatRes = await fetch('/api/v1/chat', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${token}`,
        },
        body: JSON.stringify({ message: text.trim(), session_id: sessionId }),
      })
      if (!chatRes.ok) throw new Error('Chat request failed')
      const { session_id, response: reply } = await chatRes.json() as { session_id: string; response: string }

      if (session_id) localStorage.setItem('pond_voice_session_id', session_id)
      setResponse(reply)

      if (muted) {
        setLoopState('wait')
        return
      }

      setLoopState('speak')
      const utter = new SpeechSynthesisUtterance(reply)
      utter.lang = 'en-US'
      utter.onend = () => setLoopState('wait')
      utter.onerror = () => setLoopState('wait')
      window.speechSynthesis.speak(utter)

    } catch (err) {
      setError(err instanceof Error ? err.message : 'Something went wrong')
      setLoopState('wait')
    }
  }

  async function startListen() {
    setError(null)
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true })
      streamRef.current = stream

      const recorder = new MediaRecorder(stream)
      mediaRecorderRef.current = recorder
      audioChunksRef.current = []

      recorder.ondataavailable = (e) => {
        if (e.data.size > 0) audioChunksRef.current.push(e.data)
      }
      recorder.onstop = () => {
        if (streamRef.current) {
          streamRef.current.getTracks().forEach(t => t.stop())
          streamRef.current = null
        }
        handleRecordingStop(audioChunksRef.current)
      }

      recorder.start()
      setLoopState('listen')

      // Auto-stop after 5 seconds
      timerRef.current = setTimeout(() => {
        if (mediaRecorderRef.current?.state === 'recording') {
          mediaRecorderRef.current.stop()
        }
      }, 5000)

    } catch {
      setError('Microphone access denied')
      setLoopState('wait')
    }
  }

  function stopListen() {
    if (timerRef.current) clearTimeout(timerRef.current)
    if (mediaRecorderRef.current?.state === 'recording') {
      mediaRecorderRef.current.stop()
    }
  }

  function handleOrbClick() {
    if (loopState === 'wait') {
      startListen()
    } else if (loopState === 'listen') {
      stopListen()
    } else {
      cancel()
    }
  }

  function toggleMute() {
    const next = !muted
    setMuted(next)
    localStorage.setItem('pond_tts_muted', String(next))
    if (next && loopState === 'speak') {
      window.speechSynthesis?.cancel()
      setLoopState('wait')
    }
  }

  return (
    <div className="voice-orb-container" aria-label="Voice assistant">
      {/* Transcript + response overlay */}
      {showOverlay && (transcript || response) && (
        <div className="voice-orb-overlay">
          <button
            className="voice-orb-overlay-close"
            onClick={() => setShowOverlay(false)}
            aria-label="Close"
          >
            ✕
          </button>
          {transcript && (
            <div className="voice-orb-overlay-row">
              <span className="voice-orb-overlay-label">You</span>
              <span className="voice-orb-overlay-text">{transcript}</span>
            </div>
          )}
          {response && (
            <div className="voice-orb-overlay-row">
              <span className="voice-orb-overlay-label">Goose</span>
              <span className="voice-orb-overlay-text">{response}</span>
            </div>
          )}
        </div>
      )}

      {/* Error toast */}
      {error && (
        <div className="voice-orb-error" onClick={() => setError(null)}>
          {error}
        </div>
      )}

      {/* Controls row: mute + orb */}
      <div className="voice-orb-controls">
        <button
          className={`voice-orb-mute-btn ${muted ? 'muted' : ''}`}
          onClick={toggleMute}
          title={muted ? 'Unmute voice' : 'Mute voice'}
          aria-label={muted ? 'Unmute voice output' : 'Mute voice output'}
        >
          {muted ? (
            // Muted icon
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
              <line x1="1" y1="1" x2="23" y2="23" />
              <path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6" />
              <path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2a7 7 0 0 1-.11 1.23" />
              <line x1="12" y1="19" x2="12" y2="22" />
            </svg>
          ) : (
            // Unmuted icon
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
              <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
              <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
              <line x1="12" y1="19" x2="12" y2="22" />
              <line x1="8" y1="22" x2="16" y2="22" />
            </svg>
          )}
        </button>

        {/* The orb itself */}
        <button
          className={`voice-orb-btn voice-orb-${loopState}`}
          onClick={handleOrbClick}
          title={STATE_LABEL[loopState]}
          aria-label={STATE_LABEL[loopState]}
          aria-live="polite"
        >
          {loopState === 'wait' && (
            <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
              <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
              <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
              <line x1="12" y1="19" x2="12" y2="22" />
              <line x1="8" y1="22" x2="16" y2="22" />
            </svg>
          )}
          {loopState === 'listen' && (
            <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
              <rect x="4" y="4" width="16" height="16" rx="2" />
            </svg>
          )}
          {loopState === 'think' && (
            <svg className="voice-orb-spinner-icon" width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
              <path d="M21 12a9 9 0 1 1-6.219-8.56" />
            </svg>
          )}
          {loopState === 'speak' && (
            <span className="voice-orb-wave">
              <span /><span /><span /><span /><span />
            </span>
          )}
        </button>
      </div>

      <span className="voice-orb-label">{STATE_LABEL[loopState]}</span>
    </div>
  )
}
