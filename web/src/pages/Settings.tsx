import { useState, useEffect } from 'react'
import { api, isPreviewMode } from '../api'
import { clearActivity, logActivity } from '../activityLog'

interface Props {
  token: string
}

type Theme = 'system' | 'light' | 'dark'

const THEMES: { value: Theme; label: string; description: string }[] = [
  { value: 'system', label: 'System',  description: 'Follows your OS or browser dark/light preference.' },
  { value: 'light',  label: 'Light',   description: 'Always use the light theme.' },
  { value: 'dark',   label: 'Dark',    description: 'Always use the dark theme.' },
]

const PERSONALITIES = [
  { value: 'helpful',   label: 'Helpful',    description: 'Clear, thorough, and focused on getting things done.' },
  { value: 'concise',   label: 'Concise',    description: 'Short answers, no fluff — just the essentials.' },
  { value: 'friendly',  label: 'Friendly',   description: 'Warm and conversational, like talking to a friend.' },
  { value: 'technical', label: 'Technical',  description: 'Precise, detailed, geared toward developers.' },
]

const STYLES = [
  { value: 'proactive', label: 'Proactive', description: 'Suggests next steps and anticipates needs.' },
  { value: 'reactive',  label: 'Reactive',  description: 'Only responds when asked — no unsolicited suggestions.' },
]

function load<T>(key: string, fallback: T): T {
  try {
    const v = localStorage.getItem(key)
    return v !== null ? (JSON.parse(v) as T) : fallback
  } catch {
    return fallback
  }
}

export default function Settings({ token }: Props) {
  const [theme, setTheme] = useState<Theme>(() => (localStorage.getItem('pond_theme') as Theme) ?? 'system')
  const [displayName, setDisplayName]     = useState(() => localStorage.getItem('pond_display_name') ?? '')
  const [personality, setPersonality]     = useState(() => load('pond_personality', 'helpful'))
  const [assistantStyle, setAssistantStyle] = useState(() => load('pond_assistant_style', 'proactive'))
  const [systemPrompt, setSystemPrompt]   = useState(() => localStorage.getItem('pond_system_prompt') ?? '')

  // Hydrate from backend on mount
  useEffect(() => {
    if (isPreviewMode(token)) return
    api.getSettings(token)
      .then(s => {
        if (s.user_name) {
          setDisplayName(s.user_name)
          localStorage.setItem('pond_display_name', s.user_name)
        }
        if (s.assistant_personality) {
          setPersonality(s.assistant_personality)
          localStorage.setItem('pond_personality', JSON.stringify(s.assistant_personality))
        }
      })
      .catch(() => { /* fall back to localStorage */ })
  }, [token])

  const [saving, setSaving]   = useState(false)
  const [saved, setSaved]     = useState(false)
  const [error, setError]     = useState<string | null>(null)
  const [cleared, setCleared] = useState(false)

  async function handleSave(e: React.FormEvent) {
    e.preventDefault()
    setError(null)
    setSaved(false)
    setSaving(true)

    try {
      // Always persist to localStorage
      localStorage.setItem('pond_display_name', displayName.trim())
      localStorage.setItem('pond_personality', JSON.stringify(personality))
      localStorage.setItem('pond_assistant_style', JSON.stringify(assistantStyle))
      localStorage.setItem('pond_system_prompt', systemPrompt.trim())

      if (!isPreviewMode(token)) {
        await api.saveSettings(
          { personality, assistant_style: assistantStyle, system_prompt: systemPrompt.trim() },
          token,
        )
      }

      logActivity('chat', 'Settings updated')
      setSaved(true)
      setTimeout(() => setSaved(false), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save settings.')
    } finally {
      setSaving(false)
    }
  }

  function handleThemeChange(value: Theme) {
    setTheme(value)
    localStorage.setItem('pond_theme', value)
    if (value === 'system') {
      delete document.documentElement.dataset.theme
    } else {
      document.documentElement.dataset.theme = value
    }
  }

  function handleClearActivity() {
    clearActivity()
    setCleared(true)
    setTimeout(() => setCleared(false), 3000)
  }

  function handleClearSession() {
    localStorage.removeItem('pond_session_token')
    localStorage.removeItem('pond_display_name')
    localStorage.removeItem('pond_client_id')
    localStorage.removeItem('pond_devices')
    localStorage.removeItem('pond_activity')
    localStorage.removeItem('pond_personality')
    localStorage.removeItem('pond_assistant_style')
    localStorage.removeItem('pond_system_prompt')
    localStorage.removeItem('pond_theme')
    delete document.documentElement.dataset.theme
    window.location.reload()
  }

  return (
    <div className="db-page">
      <div className="db-page-header">
        <h1 className="db-page-title">Settings</h1>
        <p className="db-page-subtitle">Customise how your assistant looks and behaves.</p>
      </div>

      <form onSubmit={handleSave} className="db-page-content">

        {/* ── Appearance ── */}
        <div className="db-card">
          <div className="db-card-header">
            <h3>Appearance</h3>
          </div>
          <div className="db-settings-radio-group">
            {THEMES.map(opt => (
              <label
                key={opt.value}
                className={`db-settings-radio-card ${theme === opt.value ? 'selected' : ''}`}
              >
                <input
                  type="radio"
                  name="theme"
                  value={opt.value}
                  checked={theme === opt.value}
                  onChange={() => handleThemeChange(opt.value)}
                />
                <div>
                  <strong>{opt.label}</strong>
                  <span>{opt.description}</span>
                </div>
              </label>
            ))}
          </div>
        </div>

        {/* ── Profile ── */}
        <div className="db-card">
          <div className="db-card-header">
            <h3>Profile</h3>
          </div>
          <div className="db-settings-field">
            <label className="db-settings-label" htmlFor="displayName">Display name</label>
            <input
              id="displayName"
              className="db-settings-input"
              type="text"
              value={displayName}
              onChange={e => setDisplayName(e.target.value)}
              placeholder="Your name"
              maxLength={80}
            />
          </div>
        </div>

        {/* ── System prompt ── */}
        <div className="db-card">
          <div className="db-card-header">
            <h3>System Prompt</h3>
          </div>
          <p className="db-settings-hint">
            Custom instructions prepended to every conversation. Leave blank to use the default.
          </p>
          <textarea
            className="db-settings-textarea"
            value={systemPrompt}
            onChange={e => setSystemPrompt(e.target.value)}
            placeholder="You are a helpful home assistant called Goose. You help the user manage their smart devices and daily tasks…"
            rows={5}
          />
        </div>

        {/* ── Personality ── */}
        <div className="db-card">
          <div className="db-card-header">
            <h3>Personality</h3>
          </div>

          <div className="db-settings-section">
            <p className="db-settings-label">Tone</p>
            <div className="db-settings-radio-group">
              {PERSONALITIES.map(opt => (
                <label
                  key={opt.value}
                  className={`db-settings-radio-card ${personality === opt.value ? 'selected' : ''}`}
                >
                  <input
                    type="radio"
                    name="personality"
                    value={opt.value}
                    checked={personality === opt.value}
                    onChange={() => setPersonality(opt.value)}
                  />
                  <div>
                    <strong>{opt.label}</strong>
                    <span>{opt.description}</span>
                  </div>
                </label>
              ))}
            </div>
          </div>

          <div className="db-settings-section">
            <p className="db-settings-label">Style</p>
            <div className="db-settings-radio-group">
              {STYLES.map(opt => (
                <label
                  key={opt.value}
                  className={`db-settings-radio-card ${assistantStyle === opt.value ? 'selected' : ''}`}
                >
                  <input
                    type="radio"
                    name="assistantStyle"
                    value={opt.value}
                    checked={assistantStyle === opt.value}
                    onChange={() => setAssistantStyle(opt.value)}
                  />
                  <div>
                    <strong>{opt.label}</strong>
                    <span>{opt.description}</span>
                  </div>
                </label>
              ))}
            </div>
          </div>
        </div>

        {error && <p className="db-error">{error}</p>}

        <div className="db-settings-actions">
          <button type="submit" className="db-btn-primary" disabled={saving}>
            {saving ? 'Saving…' : saved ? 'Saved!' : 'Save Settings'}
          </button>
        </div>
      </form>

      {/* ── Danger zone ── */}
      <div className="db-card db-settings-danger-card">
        <div className="db-card-header">
          <h3>Session</h3>
        </div>

        <div className="db-settings-danger-row">
          <div>
            <p className="db-settings-danger-title">Clear activity log</p>
            <p className="db-settings-danger-desc">Remove all recent activity entries stored on this device.</p>
          </div>
          <button type="button" className="db-btn-sm" onClick={handleClearActivity}>
            {cleared ? 'Cleared!' : 'Clear Log'}
          </button>
        </div>

        <div className="db-settings-danger-row">
          <div>
            <p className="db-settings-danger-title">Sign out &amp; reset</p>
            <p className="db-settings-danger-desc">Clears all local data and returns to the onboarding screen.</p>
          </div>
          <button type="button" className="db-btn-danger-outline" onClick={handleClearSession}>
            Sign Out
          </button>
        </div>
      </div>
    </div>
  )
}
