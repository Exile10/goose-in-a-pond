import { useState, useEffect } from 'react'
import { api, isPreviewMode } from '../api'
import { clearActivity, logActivity } from '../activityLog'

interface Props {
  token: string
}

type Theme = 'system' | 'light' | 'dark'

const THEMES: { value: Theme; label: string; description: string }[] = [
  { value: 'system', label: 'System', description: 'Follows your OS or browser dark/light preference.' },
  { value: 'light',  label: 'Light',  description: 'Always use the light theme.' },
  { value: 'dark',   label: 'Dark',   description: 'Always use the dark theme.' },
]

const PROMPT_STYLES = [
  { value: 'balanced',  label: 'Balanced',  description: 'Warm and practical, full safety rules. Best for most households.' },
  { value: 'concise',   label: 'Concise',   description: 'One-sentence replies, action-first. For power users.' },
  { value: 'technical', label: 'Technical', description: 'Verbose, narrates tool use and reasoning. For developers.' },
  { value: 'warm',      label: 'Warm',      description: 'Conversational and family-friendly. No jargon.' },
]

const LLM_PROVIDERS = [
  { value: 'llamafile', label: 'Llamafile', description: 'Self-contained local LLM binary. Auto-started by GIAP.' },
  { value: 'ollama',    label: 'Ollama',    description: 'Local Ollama server. Start separately with `ollama serve`.' },
  { value: 'local',     label: 'GGUF (local)', description: 'In-process GGUF model via Goose LocalInference. No server needed.' },
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
  // ── Appearance ──
  const [theme, setTheme] = useState<Theme>(() => (localStorage.getItem('pond_theme') as Theme) ?? 'system')

  // ── Profile ──
  const [displayName, setDisplayName]     = useState(() => localStorage.getItem('pond_display_name') ?? '')
  const [assistantName, setAssistantName] = useState(() => localStorage.getItem('pond_assistant_name') ?? '')

  // ── Prompt ──
  const [personality, setPersonality]       = useState(() => localStorage.getItem('pond_personality') ?? '')
  const [promptStyle, setPromptStyle]       = useState(() => load('pond_prompt_style', 'balanced'))
  const [promptAddendum, setPromptAddendum] = useState(() => localStorage.getItem('pond_prompt_addendum') ?? '')
  const [customPrompt, setCustomPrompt]     = useState(() => localStorage.getItem('pond_custom_prompt') ?? '')
  const [showAdvanced, setShowAdvanced]     = useState(false)

  // ── LLM pipeline ──
  const [llmProvider, setLlmProvider]   = useState(() => localStorage.getItem('pond_llm_provider') ?? 'llamafile')
  const [llmModel, setLlmModel]         = useState(() => localStorage.getItem('pond_llm_model') ?? 'gemma-2b')
  const [llmMaxTokens, setLlmMaxTokens] = useState(() => load('pond_llm_max_tokens', 1024))
  const [llmTemp, setLlmTemp]           = useState(() => load('pond_llm_temperature', 0.7))
  // Model role assignments
  const [chatProvider,  setChatProvider]  = useState(() => localStorage.getItem('pond_chat_provider') ?? 'llamafile')
  const [chatModel,     setChatModel]     = useState(() => localStorage.getItem('pond_chat_model') ?? '')
  const [thinkProvider, setThinkProvider] = useState(() => localStorage.getItem('pond_think_provider') ?? '')
  const [thinkModel,    setThinkModel]    = useState(() => localStorage.getItem('pond_think_model') ?? '')
  const [taskProvider,  setTaskProvider]  = useState(() => localStorage.getItem('pond_task_provider') ?? '')
  const [taskModel,     setTaskModel]     = useState(() => localStorage.getItem('pond_task_model') ?? '')
  const [thinkSameAsChat, setThinkSameAsChat] = useState(() => !localStorage.getItem('pond_think_model'))
  const [taskSameAsChat,  setTaskSameAsChat]  = useState(() => !localStorage.getItem('pond_task_model'))

  // ── Voice ──
  const [wakeWord, setWakeWord]         = useState(() => localStorage.getItem('pond_wake_word') ?? 'goose')
  const [whisperModel, setWhisperModel] = useState(() => localStorage.getItem('pond_whisper_model') ?? 'base')
  const [ttsModel, setTtsModel]         = useState(() => localStorage.getItem('pond_tts_model') ?? 'qwen-tts')

  // ── UI state ──
  const [saving, setSaving]   = useState(false)
  const [saved, setSaved]     = useState(false)
  const [error, setError]     = useState<string | null>(null)
  const [cleared, setCleared] = useState(false)

  // ── Hydrate from backend ──
  useEffect(() => {
    if (isPreviewMode(token)) return
    api.getSettings(token)
      .then(s => {
        if (s.user_name)             { setDisplayName(s.user_name);               localStorage.setItem('pond_display_name', s.user_name) }
        if (s.assistant_name)        { setAssistantName(s.assistant_name);         localStorage.setItem('pond_assistant_name', s.assistant_name) }
        if (s.assistant_personality) { setPersonality(s.assistant_personality);    localStorage.setItem('pond_personality', s.assistant_personality) }
        if (s.prompt_style)          { setPromptStyle(s.prompt_style);             localStorage.setItem('pond_prompt_style', JSON.stringify(s.prompt_style)) }
        if (s.prompt_addendum !== undefined) { setPromptAddendum(s.prompt_addendum); localStorage.setItem('pond_prompt_addendum', s.prompt_addendum) }
        if (s.custom_system_prompt !== undefined) {
          const val = s.custom_system_prompt ?? ''
          setCustomPrompt(val)
          localStorage.setItem('pond_custom_prompt', val)
        }
        // LLM pipeline
        if (s.llm_provider)      { setLlmProvider(s.llm_provider);               localStorage.setItem('pond_llm_provider', s.llm_provider) }
        if (s.active_llm_model)  { setLlmModel(s.active_llm_model);              localStorage.setItem('pond_llm_model', s.active_llm_model) }
        if (s.llm_max_tokens)    { setLlmMaxTokens(s.llm_max_tokens);            localStorage.setItem('pond_llm_max_tokens', JSON.stringify(s.llm_max_tokens)) }
        if (s.llm_temperature !== undefined) { setLlmTemp(s.llm_temperature);    localStorage.setItem('pond_llm_temperature', JSON.stringify(s.llm_temperature)) }
        // Model role assignments
        if (s.chat_provider)  { setChatProvider(s.chat_provider);   localStorage.setItem('pond_chat_provider',  s.chat_provider) }
        if (s.chat_model)     { setChatModel(s.chat_model);         localStorage.setItem('pond_chat_model',     s.chat_model) }
        if (s.think_provider) { setThinkProvider(s.think_provider); localStorage.setItem('pond_think_provider', s.think_provider); setThinkSameAsChat(false) }
        if (s.think_model)    { setThinkModel(s.think_model);       localStorage.setItem('pond_think_model',    s.think_model);    setThinkSameAsChat(false) }
        if (s.task_provider)  { setTaskProvider(s.task_provider);   localStorage.setItem('pond_task_provider',  s.task_provider);  setTaskSameAsChat(false) }
        if (s.task_model)     { setTaskModel(s.task_model);         localStorage.setItem('pond_task_model',     s.task_model);     setTaskSameAsChat(false) }
        // Voice
        if (s.voice_wake_word)       { setWakeWord(s.voice_wake_word);           localStorage.setItem('pond_wake_word', s.voice_wake_word) }
        if (s.active_whisper_model)  { setWhisperModel(s.active_whisper_model);  localStorage.setItem('pond_whisper_model', s.active_whisper_model) }
        if (s.active_tts_model)      { setTtsModel(s.active_tts_model);          localStorage.setItem('pond_tts_model', s.active_tts_model) }
      })
      .catch(() => { /* fall back to localStorage */ })
  }, [token])

  async function handleSave(e: React.FormEvent) {
    e.preventDefault()
    setError(null)
    setSaved(false)
    setSaving(true)

    try {
      // Persist to localStorage
      localStorage.setItem('pond_display_name',    displayName.trim())
      localStorage.setItem('pond_assistant_name',  assistantName.trim())
      localStorage.setItem('pond_personality',     personality.trim())
      localStorage.setItem('pond_prompt_style',    JSON.stringify(promptStyle))
      localStorage.setItem('pond_prompt_addendum', promptAddendum.trim())
      localStorage.setItem('pond_custom_prompt',   customPrompt.trim())
      localStorage.setItem('pond_llm_provider',    llmProvider)
      localStorage.setItem('pond_llm_model',       llmModel.trim())
      localStorage.setItem('pond_llm_max_tokens',  JSON.stringify(llmMaxTokens))
      localStorage.setItem('pond_llm_temperature', JSON.stringify(llmTemp))
      localStorage.setItem('pond_wake_word',       wakeWord.trim())
      localStorage.setItem('pond_whisper_model',   whisperModel.trim())
      localStorage.setItem('pond_tts_model',       ttsModel.trim())
      localStorage.setItem('pond_chat_provider',   chatProvider.trim())
      localStorage.setItem('pond_chat_model',      chatModel.trim())
      if (!thinkSameAsChat) {
        localStorage.setItem('pond_think_provider', thinkProvider.trim())
        localStorage.setItem('pond_think_model',    thinkModel.trim())
      } else {
        localStorage.removeItem('pond_think_provider')
        localStorage.removeItem('pond_think_model')
      }
      if (!taskSameAsChat) {
        localStorage.setItem('pond_task_provider',  taskProvider.trim())
        localStorage.setItem('pond_task_model',     taskModel.trim())
      } else {
        localStorage.removeItem('pond_task_provider')
        localStorage.removeItem('pond_task_model')
      }

      if (!isPreviewMode(token)) {
        await api.saveSettings(
          {
            user_name:             displayName.trim(),
            assistant_name:        assistantName.trim(),
            assistant_personality: personality.trim(),
            prompt_style:          promptStyle,
            prompt_addendum:       promptAddendum.trim(),
            custom_system_prompt:  customPrompt.trim() || null,
            llm_provider:          llmProvider,
            active_llm_model:      llmModel.trim(),
            llm_max_tokens:        llmMaxTokens,
            llm_temperature:       llmTemp,
            voice_wake_word:       wakeWord.trim(),
            active_whisper_model:  whisperModel.trim(),
            active_tts_model:      ttsModel.trim(),
            chat_provider:         chatProvider.trim(),
            chat_model:            chatModel.trim(),
            think_provider:        thinkSameAsChat ? null : thinkProvider.trim() || null,
            think_model:           thinkSameAsChat ? null : thinkModel.trim() || null,
            task_provider:         taskSameAsChat  ? null : taskProvider.trim()  || null,
            task_model:            taskSameAsChat  ? null : taskModel.trim()     || null,
          },
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
    localStorage.removeItem('pond_assistant_name')
    localStorage.removeItem('pond_client_id')
    localStorage.removeItem('pond_devices')
    localStorage.removeItem('pond_activity')
    localStorage.removeItem('pond_personality')
    localStorage.removeItem('pond_prompt_style')
    localStorage.removeItem('pond_prompt_addendum')
    localStorage.removeItem('pond_custom_prompt')
    localStorage.removeItem('pond_theme')
    localStorage.removeItem('pond_llm_provider')
    localStorage.removeItem('pond_llm_model')
    localStorage.removeItem('pond_llm_max_tokens')
    localStorage.removeItem('pond_llm_temperature')
    localStorage.removeItem('pond_wake_word')
    localStorage.removeItem('pond_whisper_model')
    localStorage.removeItem('pond_tts_model')
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
          <div className="db-card-header"><h3>Appearance</h3></div>
          <div className="db-settings-radio-group">
            {THEMES.map(opt => (
              <label key={opt.value} className={`db-settings-radio-card ${theme === opt.value ? 'selected' : ''}`}>
                <input type="radio" name="theme" value={opt.value} checked={theme === opt.value} onChange={() => handleThemeChange(opt.value)} />
                <div><strong>{opt.label}</strong><span>{opt.description}</span></div>
              </label>
            ))}
          </div>
        </div>

        {/* ── Profile ── */}
        <div className="db-card">
          <div className="db-card-header"><h3>Profile</h3></div>
          <div className="db-settings-field">
            <label className="db-settings-label" htmlFor="displayName">Your name</label>
            <input id="displayName" className="db-settings-input" type="text" value={displayName} onChange={e => setDisplayName(e.target.value)} placeholder="Your name" maxLength={50} />
          </div>
          <div className="db-settings-field">
            <label className="db-settings-label" htmlFor="assistantName">Assistant name</label>
            <input id="assistantName" className="db-settings-input" type="text" value={assistantName} onChange={e => setAssistantName(e.target.value)} placeholder="Goose" maxLength={50} />
          </div>
        </div>

        {/* ── LLM Pipeline ── */}
        <div className="db-card">
          <div className="db-card-header"><h3>LLM Pipeline</h3></div>
          <p className="db-settings-hint">
            Assign a provider and model to each role. GIAP auto-routes requests: <strong>Chat</strong> handles everyday questions,
            <strong> Think</strong> handles deep reasoning, <strong>Task</strong> handles tool-use and scheduling.
            Use the <strong>Models</strong> page to assign roles with one click, or configure manually below.
          </p>

          {/* Role rows */}
          {([
            { role: 'chat',  icon: '💬', label: 'Chat',  provider: chatProvider,  setProvider: setChatProvider,  model: chatModel,  setModel: setChatModel,  sameAsChat: false,          setSameAsChat: null },
            { role: 'think', icon: '🧠', label: 'Think', provider: thinkProvider, setProvider: setThinkProvider, model: thinkModel, setModel: setThinkModel, sameAsChat: thinkSameAsChat, setSameAsChat: setThinkSameAsChat },
            { role: 'task',  icon: '⚙️', label: 'Task',  provider: taskProvider,  setProvider: setTaskProvider,  model: taskModel,  setModel: setTaskModel,  sameAsChat: taskSameAsChat,  setSameAsChat: setTaskSameAsChat },
          ] as const).map(row => (
            <div key={row.role} className="db-settings-field" style={{ borderTop: '1px solid rgba(128,128,128,0.1)', paddingTop: '0.85rem' }}>
              <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: '0.5rem' }}>
                <label className="db-settings-label" style={{ margin: 0 }}>
                  <span style={{ marginRight: '0.4rem' }}>{row.icon}</span>{row.label}
                </label>
                {row.setSameAsChat && (
                  <label style={{ display: 'flex', alignItems: 'center', gap: '0.4rem', fontSize: '0.78rem', opacity: 0.7, cursor: 'pointer' }}>
                    <input
                      type="checkbox"
                      checked={row.sameAsChat}
                      onChange={e => row.setSameAsChat!(e.target.checked)}
                    />
                    Same as Chat
                  </label>
                )}
              </div>
              {(!row.setSameAsChat || !row.sameAsChat) && (
                <div style={{ display: 'flex', gap: '0.5rem', flexWrap: 'wrap' }}>
                  <select
                    className="db-settings-input"
                    style={{ flex: '0 0 auto', width: 'auto', minWidth: '8rem' }}
                    value={row.provider}
                    onChange={e => row.setProvider(e.target.value)}
                  >
                    <option value="">— provider —</option>
                    {LLM_PROVIDERS.map(p => <option key={p.value} value={p.value}>{p.label}</option>)}
                  </select>
                  <input
                    className="db-settings-input"
                    style={{ flex: '1 1 10rem' }}
                    type="text"
                    value={row.model}
                    onChange={e => row.setModel(e.target.value)}
                    placeholder="model name"
                    maxLength={100}
                  />
                </div>
              )}
              {row.setSameAsChat && row.sameAsChat && (
                <p className="db-settings-hint" style={{ margin: 0 }}>Uses the Chat role model.</p>
              )}
            </div>
          ))}

          {/* Shared generation params */}
          <div className="db-settings-field" style={{ borderTop: '1px solid rgba(128,128,128,0.1)', paddingTop: '0.85rem' }}>
            <label className="db-settings-label" htmlFor="llmMaxTokens">
              Max tokens <span style={{ opacity: 0.55, fontWeight: 400 }}>({llmMaxTokens})</span>
            </label>
            <p className="db-settings-hint">Maximum tokens in each LLM response. Applies to all roles.</p>
            <input id="llmMaxTokens" className="db-settings-input" type="range" min={128} max={4096} step={128} value={llmMaxTokens} onChange={e => setLlmMaxTokens(Number(e.target.value))} style={{ padding: '0.25rem 0', cursor: 'pointer' }} />
            <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.7rem', opacity: 0.45, marginTop: '0.15rem' }}>
              <span>128</span><span>4096</span>
            </div>
          </div>
          <div className="db-settings-field">
            <label className="db-settings-label" htmlFor="llmTemp">
              Temperature <span style={{ opacity: 0.55, fontWeight: 400 }}>({llmTemp.toFixed(2)})</span>
            </label>
            <p className="db-settings-hint">Controls randomness. Lower = more predictable, higher = more creative.</p>
            <input id="llmTemp" className="db-settings-input" type="range" min={0} max={2} step={0.05} value={llmTemp} onChange={e => setLlmTemp(Number(e.target.value))} style={{ padding: '0.25rem 0', cursor: 'pointer' }} />
            <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.7rem', opacity: 0.45, marginTop: '0.15rem' }}>
              <span>0 (precise)</span><span>2 (creative)</span>
            </div>
          </div>
        </div>

        {/* ── Prompt Style ── */}
        <div className="db-card">
          <div className="db-card-header"><h3>Prompt Style</h3></div>
          <p className="db-settings-hint">
            Controls how the assistant structures its replies.
          </p>
          <div className="db-settings-radio-group">
            {PROMPT_STYLES.map(opt => (
              <label key={opt.value} className={`db-settings-radio-card ${promptStyle === opt.value ? 'selected' : ''}`}>
                <input type="radio" name="promptStyle" value={opt.value} checked={promptStyle === opt.value} onChange={() => setPromptStyle(opt.value)} />
                <div><strong>{opt.label}</strong><span>{opt.description}</span></div>
              </label>
            ))}
          </div>
        </div>

        {/* ── Personality ── */}
        <div className="db-card">
          <div className="db-card-header"><h3>Personality</h3></div>
          <div className="db-settings-field">
            <label className="db-settings-label" htmlFor="personality">Assistant personality</label>
            <p className="db-settings-hint">A short description injected into every conversation.</p>
            <input id="personality" className="db-settings-input" type="text" value={personality} onChange={e => setPersonality(e.target.value)} placeholder="warm and helpful" maxLength={200} />
          </div>
          <div className="db-settings-field">
            <label className="db-settings-label" htmlFor="promptAddendum">Extra instructions</label>
            <p className="db-settings-hint">Appended after every system prompt.</p>
            <textarea id="promptAddendum" className="db-settings-textarea" value={promptAddendum} onChange={e => setPromptAddendum(e.target.value)} placeholder="e.g. Always greet me by name." rows={3} maxLength={500} />
          </div>
        </div>

        {/* ── Voice ── */}
        <div className="db-card">
          <div className="db-card-header"><h3>Voice</h3></div>
          <div className="db-settings-field">
            <label className="db-settings-label" htmlFor="wakeWord">Wake word</label>
            <p className="db-settings-hint">Spoken phrase that activates the assistant. Default: goose</p>
            <input id="wakeWord" className="db-settings-input" type="text" value={wakeWord} onChange={e => setWakeWord(e.target.value)} placeholder="goose" maxLength={50} />
          </div>
          <div className="db-settings-field">
            <label className="db-settings-label" htmlFor="whisperModel">Whisper ASR model</label>
            <p className="db-settings-hint">Speech recognition model name, e.g. base or small.</p>
            <input id="whisperModel" className="db-settings-input" type="text" value={whisperModel} onChange={e => setWhisperModel(e.target.value)} placeholder="base" maxLength={50} />
          </div>
          <div className="db-settings-field">
            <label className="db-settings-label" htmlFor="ttsModel">TTS model</label>
            <p className="db-settings-hint">Text-to-speech model name, e.g. qwen-tts or piper-lessac.</p>
            <input id="ttsModel" className="db-settings-input" type="text" value={ttsModel} onChange={e => setTtsModel(e.target.value)} placeholder="qwen-tts" maxLength={50} />
          </div>
        </div>

        {/* ── Advanced ── */}
        <div className="db-card">
          <div className="db-card-header" onClick={() => setShowAdvanced(v => !v)} style={{ cursor: 'pointer', userSelect: 'none', display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
            <h3>Advanced</h3>
            <span style={{ fontSize: '0.75rem', opacity: 0.6 }}>{showAdvanced ? '▲' : '▼'}</span>
          </div>
          {showAdvanced && (
            <div className="db-settings-field">
              <label className="db-settings-label" htmlFor="customPrompt">Custom system prompt</label>
              <p className="db-settings-hint">
                Replaces the built-in template entirely. Leave blank to use the Prompt Style above.
                Supports <code>{'{{assistant_name}}'}</code>, <code>{'{{user_name}}'}</code>,{' '}
                <code>{'{{timezone}}'}</code>, <code>{'{{personality}}'}</code>,{' '}
                <code>{'{{location}}'}</code>, <code>{'{{prompt_addendum}}'}</code>.
              </p>
              <textarea id="customPrompt" className="db-settings-textarea" value={customPrompt} onChange={e => setCustomPrompt(e.target.value)} placeholder={`I am {{assistant_name}}, your home assistant.\nI speak only to {{user_name}}. Timezone: {{timezone}}.{{location}}\n{{prompt_addendum}}`} rows={7} maxLength={4000} />
            </div>
          )}
        </div>

        {error && <p className="db-error">{error}</p>}

        <div className="db-settings-actions">
          <button type="submit" className="db-btn-primary" disabled={saving}>
            {saving ? 'Saving…' : saved ? 'Saved!' : 'Save Settings'}
          </button>
        </div>
      </form>

      {/* ── Session ── */}
      <div className="db-card db-settings-danger-card">
        <div className="db-card-header"><h3>Session</h3></div>

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
