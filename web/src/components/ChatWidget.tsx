import { useState, useRef, useEffect } from 'react'
import { api, isPreviewMode } from '../api'
import { logActivity } from '../activityLog'

// Minimal SpeechRecognition types (not in default TS lib)
interface ISpeechRecognition extends EventTarget {
  lang: string
  continuous: boolean
  interimResults: boolean
  onstart: (() => void) | null
  onresult: ((event: { results: { [i: number]: { [i: number]: { transcript: string } } } }) => void) | null
  onerror: (() => void) | null
  onend: (() => void) | null
  start(): void
  stop(): void
  abort(): void
}

declare global {
  interface Window {
    SpeechRecognition: new () => ISpeechRecognition
    webkitSpeechRecognition: new () => ISpeechRecognition
  }
}

type SpeechRecognitionCtor = new () => ISpeechRecognition

const SpeechRecognitionAPI: SpeechRecognitionCtor | null =
  typeof window !== 'undefined'
    ? (window.SpeechRecognition ?? window.webkitSpeechRecognition ?? null)
    : null

interface Props {
  token: string
}

interface Message {
  id: string
  role: 'user' | 'assistant'
  text: string
}

interface Suggestion {
  icon: string
  label: string
  prompt: string
}

function getDeviceCount(): { total: number; active: number } {
  try {
    const stored = localStorage.getItem('pond_devices')
    if (!stored) return { total: 0, active: 0 }
    const devices: { id: string; name: string; type: string }[] = JSON.parse(stored)
    return { total: devices.length, active: devices.length }
  } catch {
    return { total: 0, active: 0 }
  }
}

function greeting(): string {
  const hour = new Date().getHours()
  const name = localStorage.getItem('pond_display_name')?.trim()
  const salutation = name ? `, ${name}` : ''
  const { total, active } = getDeviceCount()
  const deviceLine = total > 0
    ? ` ${active} of your ${total} device${total !== 1 ? 's' : ''} ${active === 1 ? 'is' : 'are'} online and ready.`
    : ' No devices connected yet — add one from the Devices page.'

  if (hour < 12) return `Good morning${salutation}.${deviceLine}`
  if (hour < 17) return `Good afternoon${salutation}.${deviceLine}`
  return `Good evening${salutation}.${deviceLine}`
}

function getSuggestions(): Suggestion[] {
  const hour = new Date().getHours()
  if (hour < 12) return [
    { icon: '🌅', label: 'Morning routine',    prompt: 'Start my morning routine — turn on all devices' },
    { icon: '💡', label: 'Turn on devices',    prompt: 'Turn on all my connected devices' },
    { icon: '📋', label: 'What can you do?',   prompt: 'What tasks can you perform for me?' },
    { icon: '📱', label: 'Check devices',      prompt: 'Show me the status of all my devices' },
  ]
  if (hour < 17) return [
    { icon: '📱', label: 'Device status',      prompt: 'What devices are currently active?' },
    { icon: '⏰', label: 'Set a reminder',     prompt: 'Set a reminder for me' },
    { icon: '💡', label: 'Turn off a device',  prompt: 'Turn off a specific device for me' },
    { icon: '📋', label: 'What can you do?',   prompt: 'What tasks can you perform for me?' },
  ]
  return [
    { icon: '🌙', label: 'Bedtime routine',    prompt: 'Set a bedtime routine — turn off all devices and set an alarm for 7am' },
    { icon: '💡', label: 'Turn off all',       prompt: 'Turn off all my connected devices' },
    { icon: '⏰', label: 'Set an alarm',       prompt: 'Set an alarm for tomorrow morning' },
    { icon: '📋', label: 'What can you do?',   prompt: 'What tasks can you perform for me?' },
  ]
}

export default function ChatWidget({ token }: Props) {
  const [messages, setMessages] = useState<Message[]>([
    { id: '0', role: 'assistant', text: greeting() },
  ])
  const [input, setInput] = useState('')
  const [loading, setLoading] = useState(false)
  const [listening, setListening] = useState(false)
  const [showSuggestions, setShowSuggestions] = useState(true)
  const [agentRunning, setAgentRunning] = useState(true)
  const suggestions = getSuggestions()
  const bottomRef = useRef<HTMLDivElement>(null)
  const recognitionRef = useRef<ISpeechRecognition | null>(null)

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [messages])

  useEffect(() => {
    return () => { recognitionRef.current?.abort() }
  }, [])

  function toggleListening() {
    if (listening) {
      recognitionRef.current?.stop()
      setListening(false)
      return
    }
    if (!SpeechRecognitionAPI) return
    const Ctor = SpeechRecognitionAPI
    const recognition = new Ctor()
    recognition.lang = 'en-US'
    recognition.continuous = false
    recognition.interimResults = false
    recognition.onstart = () => setListening(true)
    recognition.onresult = (event) => {
      const transcript = event.results[0][0].transcript
      setInput(transcript)
    }
    recognition.onerror = () => setListening(false)
    recognition.onend = () => setListening(false)
    recognitionRef.current = recognition
    recognition.start()
  }

  async function sendMessage(text: string) {
    setShowSuggestions(false)
    const userMsg: Message = { id: crypto.randomUUID(), role: 'user', text }
    setMessages(prev => [...prev, userMsg])
    logActivity('chat', text)
    setInput('')
    setLoading(true)

    try {
      if (isPreviewMode(token)) {
        await new Promise(r => setTimeout(r, 600))
        setMessages(prev => [...prev, {
          id: crypto.randomUUID(),
          role: 'assistant',
          text: '(Preview mode — connect to a live server to get real responses.)',
        }])
      } else {
        const res = await api.chat(text, token)
        setMessages(prev => [...prev, {
          id: crypto.randomUUID(),
          role: 'assistant',
          text: res.message ?? res.status,
        }])
      }
    } catch (err) {
      setMessages(prev => [...prev, {
        id: crypto.randomUUID(),
        role: 'assistant',
        text: `Error: ${err instanceof Error ? err.message : 'Failed to send message.'}`,
      }])
    } finally {
      setLoading(false)
    }
  }

  async function handleSend(e: React.FormEvent) {
    e.preventDefault()
    const text = input.trim()
    if (!text) return
    if (listening) {
      recognitionRef.current?.stop()
      setListening(false)
    }
    await sendMessage(text)
  }

  function handleSuggestion(prompt: string) {
    setInput(prompt)
  }

  function clearChat() {
    setMessages([{ id: crypto.randomUUID(), role: 'assistant', text: greeting() }])
    setShowSuggestions(true)
    setInput('')
  }

  function toggleAgent() {
    const next = !agentRunning
    setAgentRunning(next)
    setMessages(prev => [...prev, {
      id: crypto.randomUUID(),
      role: 'assistant',
      text: next
        ? 'Agent started. I\'m back online and ready to help.'
        : 'Agent stopped. I\'m paused — press Start to resume.',
    }])
    setShowSuggestions(false)
  }

  return (
    <div className="db-chat">

      {/* Control bar */}
      <div className="db-chat-controls">
        <span className={`db-chat-agent-status ${agentRunning ? 'online' : 'offline'}`}>
          <span className="db-chat-agent-dot" />
          {agentRunning ? 'Agent Online' : 'Agent Offline'}
        </span>
        <div className="db-chat-control-btns">
          <button
            className={`db-chat-control-btn ${agentRunning ? 'stop' : 'start'}`}
            onClick={toggleAgent}
            disabled={loading}
            title={agentRunning ? 'Stop agent' : 'Start agent'}
          >
            {agentRunning ? (
              <svg width="13" height="13" viewBox="0 0 24 24" fill="currentColor">
                <rect x="4" y="4" width="16" height="16" rx="2" />
              </svg>
            ) : (
              <svg width="13" height="13" viewBox="0 0 24 24" fill="currentColor">
                <polygon points="5,3 19,12 5,21" />
              </svg>
            )}
            {agentRunning ? 'Stop' : 'Start'}
          </button>
          <button
            className="db-chat-control-btn clear"
            onClick={clearChat}
            disabled={loading}
            title="Clear chat"
          >
            <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
              <polyline points="3 6 5 6 21 6" />
              <path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" />
              <path d="M10 11v6M14 11v6" />
            </svg>
            Clear
          </button>
        </div>
      </div>

      <div className="db-chat-messages">
        {messages.map(msg => (
          <div key={msg.id} className={`db-chat-msg db-chat-msg-${msg.role}`}>
            <span className="db-chat-bubble">{msg.text}</span>
          </div>
        ))}

        {/* Suggestion chips — shown after greeting, hidden once user sends a message */}
        {showSuggestions && (
          <div className="db-chat-suggestions">
            {suggestions.map(s => (
              <button
                key={s.prompt}
                className="db-chat-suggestion-chip"
                onClick={() => handleSuggestion(s.prompt)}
              >
                <span className="db-chat-suggestion-icon">{s.icon}</span>
                {s.label}
              </button>
            ))}
          </div>
        )}

        {loading && (
          <div className="db-chat-msg db-chat-msg-assistant">
            <span className="db-chat-bubble db-chat-typing">
              <span /><span /><span />
            </span>
          </div>
        )}
        <div ref={bottomRef} />
      </div>

      <form onSubmit={handleSend} className="db-chat-input-row">
        {listening && (
          <span className="db-chat-listening-badge">
            <span className="db-chat-listening-dot" />
            Listening…
          </span>
        )}
        <input
          type="text"
          placeholder={!agentRunning ? 'Agent is stopped — press Start to resume…' : listening ? 'Speak now…' : 'Type a message or use the mic…'}
          value={input}
          onChange={e => setInput(e.target.value)}
          disabled={loading || !agentRunning}
        />
        {SpeechRecognitionAPI && (
          <button
            type="button"
            className={`db-chat-mic-btn ${listening ? 'active' : ''}`}
            onClick={toggleListening}
            disabled={loading}
            title={listening ? 'Stop listening' : 'Voice input'}
          >
            <svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
              <rect x="9" y="2" width="6" height="11" rx="3" />
              <path d="M5 10a7 7 0 0 0 14 0" />
              <line x1="12" y1="19" x2="12" y2="22" />
              <line x1="8" y1="22" x2="16" y2="22" />
            </svg>
          </button>
        )}
        <button type="submit" className="db-btn-primary" disabled={loading || !input.trim() || !agentRunning}>
          Send
        </button>
      </form>
    </div>
  )
}
