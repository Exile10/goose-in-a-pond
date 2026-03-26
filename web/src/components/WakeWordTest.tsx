import { useState, useRef } from 'react'

type TestStatus = 'idle' | 'recording' | 'transcribing' | 'done' | 'error'

export default function WakeWordTest() {
  const [status, setStatus] = useState<TestStatus>('idle')
  const [transcript, setTranscript] = useState('')
  const [detected, setDetected] = useState<boolean | null>(null)
  const [trigger, setTrigger] = useState('goose')
  const [countdown, setCountdown] = useState(3)
  const recorderRef = useRef<MediaRecorder | null>(null)
  const chunksRef = useRef<Blob[]>([])
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null)

  async function startTest() {
    setStatus('recording')
    setTranscript('')
    setDetected(null)
    setCountdown(3)
    chunksRef.current = []

    let stream: MediaStream
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: true })
    } catch {
      setTranscript('Microphone access denied. Allow mic access and try again.')
      setStatus('error')
      return
    }

    const mimeType = MediaRecorder.isTypeSupported('audio/webm;codecs=opus')
      ? 'audio/webm;codecs=opus'
      : MediaRecorder.isTypeSupported('audio/webm')
        ? 'audio/webm'
        : ''

    const recorder = new MediaRecorder(stream, mimeType ? { mimeType } : undefined)
    recorderRef.current = recorder

    recorder.ondataavailable = (e) => {
      if (e.data.size > 0) chunksRef.current.push(e.data)
    }

    recorder.onstop = async () => {
      stream.getTracks().forEach(t => t.stop())
      if (timerRef.current) clearInterval(timerRef.current)
      setStatus('transcribing')

      try {
        const blob = new Blob(chunksRef.current, { type: mimeType || 'audio/webm' })
        const form = new FormData()
        form.append('audio', blob, 'audio.webm')

        const res = await fetch('/api/v1/transcribe', { method: 'POST', body: form })

        if (!res.ok) {
          const body = await res.text()
          throw new Error(body || `Server error ${res.status} — is whisper.cpp running?`)
        }

        const data = await res.json()
        const text: string = data.text ?? ''
        setTranscript(text || '(no speech detected)')
        setDetected(text.toLowerCase().includes(trigger.toLowerCase()))
        setStatus('done')
      } catch (err) {
        setTranscript(err instanceof Error ? err.message : 'Transcription failed')
        setStatus('error')
      }
    }

    recorder.start(100)

    // Countdown 3 → 2 → 1 then stop
    let remaining = 3
    timerRef.current = setInterval(() => {
      remaining -= 1
      setCountdown(remaining)
      if (remaining <= 0) {
        if (timerRef.current) clearInterval(timerRef.current)
        if (recorder.state === 'recording') recorder.stop()
      }
    }, 1000)
  }

  function reset() {
    if (timerRef.current) clearInterval(timerRef.current)
    if (recorderRef.current?.state === 'recording') recorderRef.current.stop()
    setStatus('idle')
    setTranscript('')
    setDetected(null)
    setCountdown(3)
  }

  return (
    <div className="db-card" style={{ marginTop: '1.5rem' }}>
      <div className="db-card-header">
        <h3>Wake Word Test</h3>
        <span className={`db-badge ${status === 'done' && detected ? 'db-badge-green' : status === 'done' ? 'db-badge-red' : 'db-badge-gray'}`}>
          {status === 'idle' && 'Ready'}
          {status === 'recording' && `Recording ${countdown}s…`}
          {status === 'transcribing' && 'Transcribing…'}
          {status === 'done' && (detected ? 'Detected' : 'Not detected')}
          {status === 'error' && 'Error'}
        </span>
      </div>

      <p className="db-muted" style={{ marginBottom: '1rem', fontSize: '0.85rem' }}>
        Record 3 seconds of audio and check whether the whisper.cpp server hears your trigger phrase.
        Requires whisper.cpp running at <code>http://127.0.0.1:9000</code>.
      </p>

      {/* Trigger word input */}
      <div style={{ display: 'flex', gap: '0.5rem', alignItems: 'center', marginBottom: '1rem' }}>
        <label style={{ fontSize: '0.85rem', color: 'var(--text-muted)', whiteSpace: 'nowrap' }}>
          Trigger phrase:
        </label>
        <input
          type="text"
          value={trigger}
          onChange={e => setTrigger(e.target.value)}
          disabled={status === 'recording' || status === 'transcribing'}
          style={{
            flex: 1,
            padding: '0.4rem 0.75rem',
            borderRadius: '6px',
            border: '1px solid var(--glass-border)',
            background: 'var(--input-bg, rgba(255,255,255,0.06))',
            color: 'var(--text)',
            fontSize: '0.85rem',
          }}
          placeholder="e.g. goose"
        />
      </div>

      {/* Recording indicator */}
      {status === 'recording' && (
        <div style={{ display: 'flex', alignItems: 'center', gap: '0.6rem', marginBottom: '1rem' }}>
          <span style={{
            width: 10, height: 10, borderRadius: '50%',
            background: '#ef4444',
            animation: 'pulse 1s infinite',
            display: 'inline-block',
          }} />
          <span style={{ fontSize: '0.85rem', color: 'var(--text-muted)' }}>
            Speak now — say "{trigger}" — stopping in {countdown}s
          </span>
        </div>
      )}

      {status === 'transcribing' && (
        <div style={{ display: 'flex', alignItems: 'center', gap: '0.6rem', marginBottom: '1rem' }}>
          <span className="db-chat-typing" style={{ display: 'inline-flex' }}>
            <span /><span /><span />
          </span>
          <span style={{ fontSize: '0.85rem', color: 'var(--text-muted)' }}>
            Sending to whisper.cpp…
          </span>
        </div>
      )}

      {/* Result */}
      {(status === 'done' || status === 'error') && (
        <div style={{
          padding: '0.75rem 1rem',
          borderRadius: '8px',
          background: status === 'done' && detected
            ? 'rgba(34,197,94,0.1)'
            : status === 'done'
              ? 'rgba(239,68,68,0.1)'
              : 'rgba(239,68,68,0.08)',
          border: `1px solid ${status === 'done' && detected ? 'rgba(34,197,94,0.3)' : 'rgba(239,68,68,0.3)'}`,
          marginBottom: '1rem',
        }}>
          {status === 'done' && (
            <>
              <p style={{ fontSize: '0.8rem', color: 'var(--text-muted)', margin: '0 0 0.35rem' }}>Transcript:</p>
              <p style={{ fontStyle: 'italic', margin: '0 0 0.5rem', fontSize: '0.9rem' }}>"{transcript}"</p>
              <p style={{ margin: 0, fontSize: '0.85rem', fontWeight: 600 }}>
                {detected
                  ? `✅ Wake word "${trigger}" detected`
                  : `❌ Wake word "${trigger}" not found in transcript`}
              </p>
            </>
          )}
          {status === 'error' && (
            <p style={{ margin: 0, fontSize: '0.85rem', color: '#ef4444' }}>
              ⚠ {transcript}
            </p>
          )}
        </div>
      )}

      {/* Actions */}
      <div style={{ display: 'flex', gap: '0.5rem' }}>
        {status === 'idle' || status === 'done' || status === 'error' ? (
          <button
            className="db-btn-primary"
            onClick={startTest}
            disabled={!trigger.trim()}
          >
            {status === 'idle' ? '🎤 Start Recording' : '🎤 Try Again'}
          </button>
        ) : (
          <button className="db-btn-secondary" onClick={reset}>
            Cancel
          </button>
        )}
      </div>
    </div>
  )
}
