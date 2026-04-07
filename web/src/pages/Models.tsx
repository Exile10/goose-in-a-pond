import { useState, useEffect, useCallback, useRef } from 'react'
import { api, isPreviewMode, type ModelStatusEntry, type OllamaModel, type Settings, type MemoryStatus } from '../api'

interface Props {
  token: string
}

const CATEGORY_LABELS: Record<string, string> = {
  whisper:   'Whisper ASR',
  llamafile: 'Llamafile (LLM)',
  gguf:      'GGUF (Local LLM)',
  tts:       'Text-to-Speech',
}

const CATEGORY_DESC: Record<string, string> = {
  whisper:   'Speech-to-text models for voice input.',
  llamafile: 'Self-contained LLM executables for the llamafile provider.',
  gguf:      'GGUF weights for the local (Goose GGUF) provider. Requires no server.',
  tts:       'Voice synthesis models and servers.',
}

const ROLE_LABELS: Record<string, string> = { chat: 'Chat', think: 'Think', task: 'Task' }
const ROLE_ICONS:  Record<string, string> = { chat: '💬',   think: '🧠',    task: '⚙️'  }

function formatSize(mb: number): string {
  if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`
  return `${mb} MB`
}

// Map a ModelStatusEntry to the provider string GIAP uses in settings
function providerForEntry(entry: ModelStatusEntry): string {
  if (entry.category === 'llamafile') return 'llamafile'
  if (entry.category === 'gguf')      return 'local'
  return 'ollama'
}

function roleForEntry(entry: ModelStatusEntry, settings: Settings | null, ollamaName?: string): string | null {
  if (!settings) return null
  const provider = ollamaName ? 'ollama' : providerForEntry(entry)
  const name = ollamaName ?? entry.name
  if (settings.chat_provider === provider && settings.chat_model === name) return 'chat'
  if (settings.think_provider === provider && settings.think_model === name) return 'think'
  if (settings.task_provider === provider && settings.task_model === name) return 'task'
  return null
}

// ── Role Assignment Panel ──────────────────────────────────────────────────────

function MemoryBar({ status }: { status: MemoryStatus | null }) {
  if (!status || status.total_mb === 0) return null
  const usedMb   = status.total_mb - status.available_for_llm_mb
  const pct      = Math.min(100, Math.round((usedMb / status.total_mb) * 100))
  const totalGb  = (status.total_mb / 1024).toFixed(1)
  const freeGb   = (status.available_for_llm_mb / 1024).toFixed(1)
  const color    = pct > 85 ? '#e55' : pct > 60 ? '#e8a020' : '#a96ff5'

  return (
    <div style={{ marginTop: '0.75rem' }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.72rem', opacity: 0.65, marginBottom: '0.25rem' }}>
        <span>RAM available for LLMs</span>
        <span>{freeGb} / {totalGb} GB free{status.loaded_model ? ` · loaded: ${status.loaded_model}` : ''}</span>
      </div>
      <div style={{ height: '8px', borderRadius: '4px', background: 'rgba(128,128,128,0.15)', overflow: 'hidden' }}>
        <div style={{ height: '100%', width: `${pct}%`, borderRadius: '4px', background: color, transition: 'width 0.4s ease' }} />
      </div>
    </div>
  )
}

function RolePanel({
  settings,
  memory,
  onAssignRole,
}: {
  settings: Settings | null
  memory: MemoryStatus | null
  onAssignRole: (role: 'chat' | 'think' | 'task', provider: string | null, model: string | null) => void
}) {
  const roles: Array<'chat' | 'think' | 'task'> = ['chat', 'think', 'task']

  function getAssignment(role: 'chat' | 'think' | 'task'): { provider: string | null; model: string | null } {
    if (!settings) return { provider: null, model: null }
    const p = settings[`${role}_provider`] as string | null
    const m = settings[`${role}_model`]    as string | null
    return { provider: p || null, model: m || null }
  }

  return (
    <div className="db-card" style={{ marginBottom: '1.25rem' }}>
      <div className="db-card-header">
        <h3>Model Roles</h3>
        <span style={{ fontSize: '0.75rem', opacity: 0.55 }}>
          GIAP routes each request to the right model automatically.
        </span>
      </div>

      <div style={{ overflowX: 'auto' }}>
        <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: '0.82rem' }}>
          <thead>
            <tr style={{ opacity: 0.5, borderBottom: '1px solid rgba(128,128,128,0.15)' }}>
              <th style={{ textAlign: 'left', padding: '0.35rem 0.5rem', fontWeight: 600 }}>Role</th>
              <th style={{ textAlign: 'left', padding: '0.35rem 0.5rem', fontWeight: 600 }}>Provider</th>
              <th style={{ textAlign: 'left', padding: '0.35rem 0.5rem', fontWeight: 600 }}>Model</th>
              <th style={{ textAlign: 'right', padding: '0.35rem 0.5rem', fontWeight: 600 }}>Action</th>
            </tr>
          </thead>
          <tbody>
            {roles.map(role => {
              const { provider, model } = getAssignment(role)
              return (
                <tr key={role} style={{ borderBottom: '1px solid rgba(128,128,128,0.08)' }}>
                  <td style={{ padding: '0.5rem' }}>
                    <span style={{ marginRight: '0.4rem' }}>{ROLE_ICONS[role]}</span>
                    <strong>{ROLE_LABELS[role]}</strong>
                  </td>
                  <td style={{ padding: '0.5rem', opacity: 0.7 }}>{provider ?? <span style={{ opacity: 0.4 }}>—</span>}</td>
                  <td style={{ padding: '0.5rem', opacity: 0.7 }}>{model ?? <span style={{ opacity: 0.4 }}>—</span>}</td>
                  <td style={{ padding: '0.5rem', textAlign: 'right' }}>
                    {(provider || model) && (
                      <button
                        className="db-btn-sm"
                        style={{ fontSize: '0.7rem', padding: '0.15rem 0.5rem' }}
                        onClick={() => onAssignRole(role, null, null)}
                      >
                        Clear
                      </button>
                    )}
                  </td>
                </tr>
              )
            })}
          </tbody>
        </table>
      </div>

      <MemoryBar status={memory} />

      <p style={{ fontSize: '0.72rem', opacity: 0.45, marginTop: '0.6rem' }}>
        Assign models using the role badges on each model card below. Think = deep reasoning, Task = tool use, Chat = everything else.
      </p>
    </div>
  )
}

// ── Model Card ────────────────────────────────────────────────────────────────

function ModelCard({
  entry,
  onDownload,
  downloading,
  currentRole,
  onAssignRole,
}: {
  entry: ModelStatusEntry
  onDownload: (category: string, name: string) => void
  downloading: boolean
  currentRole: string | null
  onAssignRole: ((role: 'chat' | 'think' | 'task') => void) | null
}) {
  const roles: Array<'chat' | 'think' | 'task'> = ['chat', 'think', 'task']

  return (
    <div className="db-model-card" data-downloaded={entry.downloaded ? 'true' : 'false'}>
      <div className="db-model-card-info">
        <div className="db-model-card-name">
          {entry.name}
          {entry.active && <span className="db-badge db-badge-green">active</span>}
          {entry.downloaded && !entry.active && <span className="db-badge">installed</span>}
          {currentRole && (
            <span
              className="db-badge"
              style={{ background: 'rgba(169,111,245,0.18)', color: '#a96ff5', border: '1px solid rgba(169,111,245,0.3)' }}
            >
              {ROLE_ICONS[currentRole]} {ROLE_LABELS[currentRole]}
            </span>
          )}
        </div>
        {entry.hf_id && <div className="db-model-card-hf">{entry.hf_id}</div>}
        <div className="db-model-card-desc">{entry.description}</div>
        <div className="db-model-card-size" style={{ display: 'flex', gap: '0.75rem', flexWrap: 'wrap' }}>
          <span>{formatSize(entry.size_mb)}</span>
          {entry.ram_estimate_mb && <span style={{ opacity: 0.65 }}>~{formatSize(entry.ram_estimate_mb)} RAM</span>}
          {entry.recommended_role && (
            <span style={{ opacity: 0.65 }}>
              Recommended: {ROLE_ICONS[entry.recommended_role]} {ROLE_LABELS[entry.recommended_role] ?? entry.recommended_role}
            </span>
          )}
        </div>
      </div>
      <div className="db-model-card-actions" style={{ display: 'flex', flexDirection: 'column', alignItems: 'flex-end', gap: '0.35rem' }}>
        {entry.downloaded ? (
          <span className="db-model-card-installed">&#10003; Installed</span>
        ) : entry.url ? (
          <button
            className="db-btn-sm"
            disabled={downloading}
            onClick={() => onDownload(entry.category, entry.name)}
          >
            {downloading ? 'Downloading…' : 'Download'}
          </button>
        ) : (
          <span className="db-model-card-no-dl">Server-based</span>
        )}
        {onAssignRole && entry.downloaded && (
          <div style={{ display: 'flex', gap: '0.3rem', flexWrap: 'wrap', justifyContent: 'flex-end' }}>
            {roles.map(role => (
              <button
                key={role}
                className="db-btn-sm"
                title={`Assign to ${ROLE_LABELS[role]}`}
                onClick={() => onAssignRole(role)}
                style={{
                  fontSize: '0.7rem',
                  padding: '0.15rem 0.45rem',
                  background: currentRole === role ? 'rgba(169,111,245,0.25)' : undefined,
                  border: currentRole === role ? '1px solid rgba(169,111,245,0.5)' : undefined,
                }}
              >
                {ROLE_ICONS[role]}
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  )
}

// ── Page ───────────────────────────────────────────────────────────────────────

export default function Models({ token }: Props) {
  const [models, setModels] = useState<{
    whisper: ModelStatusEntry[]
    llamafile: ModelStatusEntry[]
    gguf: ModelStatusEntry[]
    tts: ModelStatusEntry[]
  } | null>(null)

  const [ollama, setOllama]             = useState<OllamaModel[] | null>(null)
  const [ollamaError, setOllamaError]   = useState<string | null>(null)
  const [loading, setLoading]           = useState(true)
  const [error, setError]               = useState<string | null>(null)
  const [downloading, setDownloading]   = useState<Set<string>>(new Set())
  const [refreshing, setRefreshing]     = useState(false)
  const [refreshMsg, setRefreshMsg]     = useState<string | null>(null)
  const [settings, setSettings]         = useState<Settings | null>(null)
  const [memory, setMemory]             = useState<MemoryStatus | null>(null)
  const memoryTimerRef                  = useRef<ReturnType<typeof setInterval> | null>(null)

  const load = useCallback(async () => {
    if (isPreviewMode(token)) {
      setModels({ whisper: [], llamafile: [], gguf: [], tts: [] })
      setLoading(false)
      return
    }
    try {
      const m = await api.listModels(token)
      setModels(m)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to load models.')
    } finally {
      setLoading(false)
    }
  }, [token])

  useEffect(() => {
    load()
    if (isPreviewMode(token)) return

    // Load settings for role assignments
    api.getSettings(token).then(setSettings).catch(() => {})

    // Probe Ollama
    api.listOllamaModels(token)
      .then(r => {
        if (r.error) setOllamaError(r.error)
        else setOllama(r.models)
      })
      .catch(() => setOllamaError('Could not reach backend'))

    // Poll memory status every 5s
    const fetchMemory = () => {
      api.getMemoryStatus(token).then(setMemory).catch(() => {})
    }
    fetchMemory()
    memoryTimerRef.current = setInterval(fetchMemory, 5000)

    return () => {
      if (memoryTimerRef.current) clearInterval(memoryTimerRef.current)
    }
  }, [load, token])

  async function handleAssignRole(
    role: 'chat' | 'think' | 'task',
    provider: string | null,
    model: string | null,
  ) {
    const update: Record<string, string | null> = {
      [`${role}_provider`]: provider,
      [`${role}_model`]:    model,
    }
    try {
      await api.saveSettings(update, token)
      setSettings(prev => prev ? { ...prev, ...update } as Settings : prev)
    } catch {
      // silently ignore; the role panel just won't update
    }
  }

  async function handleDownload(category: string, name: string) {
    const key = `${category}/${name}`
    setDownloading(prev => new Set(prev).add(key))
    try {
      await api.downloadModel(category, name, token)
      let attempts = 0
      const interval = setInterval(async () => {
        attempts++
        const refreshed = await api.listModels(token)
        const catList = (refreshed as unknown as Record<string, ModelStatusEntry[]>)[category]
        const entry = catList?.find(e => e.name === name)
        if (entry?.downloaded || attempts > 100) {
          clearInterval(interval)
          setModels(refreshed)
          setDownloading(prev => { const next = new Set(prev); next.delete(key); return next })
        }
      }, 3000)
    } catch (e) {
      setDownloading(prev => { const next = new Set(prev); next.delete(key); return next })
      setError(e instanceof Error ? e.message : 'Download failed.')
    }
  }

  async function handleRefresh() {
    setRefreshing(true)
    setRefreshMsg(null)
    try {
      await api.refreshModelRegistry(token)
      await load()
      setRefreshMsg('Registry refreshed.')
      setTimeout(() => setRefreshMsg(null), 3000)
    } catch (e) {
      setRefreshMsg(e instanceof Error ? e.message : 'Refresh failed.')
    } finally {
      setRefreshing(false)
    }
  }

  const categories = ['gguf', 'llamafile', 'whisper', 'tts'] as const

  return (
    <div className="db-page">
      <div className="db-page-header">
        <h1 className="db-page-title">Models</h1>
        <p className="db-page-subtitle">Download and manage on-device AI models.</p>
      </div>

      <div className="db-page-content">
        {/* Role assignment panel */}
        <RolePanel
          settings={settings}
          memory={memory}
          onAssignRole={handleAssignRole}
        />

        <div style={{ display: 'flex', justifyContent: 'flex-end', marginBottom: '0.75rem', gap: '0.5rem', alignItems: 'center' }}>
          {refreshMsg && <span style={{ fontSize: '0.8rem', opacity: 0.7 }}>{refreshMsg}</span>}
          <button className="db-btn-sm" onClick={handleRefresh} disabled={refreshing}>
            {refreshing ? 'Refreshing…' : 'Refresh Registry'}
          </button>
        </div>

        {error && <p className="db-error">{error}</p>}

        {loading ? (
          <p style={{ opacity: 0.6, textAlign: 'center', paddingTop: '2rem' }}>Loading models…</p>
        ) : (
          <>
            {categories.map(cat => {
              const list = models?.[cat] ?? []
              const isLlm = cat === 'llamafile' || cat === 'gguf'
              return (
                <div className="db-card" key={cat} style={{ marginBottom: '1.25rem' }}>
                  <div className="db-card-header">
                    <h3>{CATEGORY_LABELS[cat]}</h3>
                    <span style={{ fontSize: '0.75rem', opacity: 0.55 }}>{CATEGORY_DESC[cat]}</span>
                  </div>
                  {list.length === 0 ? (
                    <p style={{ fontSize: '0.8rem', opacity: 0.55, padding: '0.75rem 0' }}>No models in registry.</p>
                  ) : (
                    <div className="db-model-list">
                      {list.map(entry => {
                        const curRole = isLlm ? roleForEntry(entry, settings) : null
                        return (
                          <ModelCard
                            key={entry.name}
                            entry={entry}
                            downloading={downloading.has(`${entry.category}/${entry.name}`)}
                            onDownload={handleDownload}
                            currentRole={curRole}
                            onAssignRole={isLlm ? (role) => handleAssignRole(role, providerForEntry(entry), entry.name) : null}
                          />
                        )
                      })}
                    </div>
                  )}
                </div>
              )
            })}

            {/* Ollama section */}
            <div className="db-card">
              <div className="db-card-header">
                <h3>Ollama (running locally)</h3>
                <span style={{ fontSize: '0.75rem', opacity: 0.55 }}>
                  Models available from a running Ollama instance on this machine.
                </span>
              </div>
              {ollamaError ? (
                <p style={{ fontSize: '0.8rem', opacity: 0.55, padding: '0.75rem 0' }}>{ollamaError}</p>
              ) : ollama === null ? (
                <p style={{ fontSize: '0.8rem', opacity: 0.55, padding: '0.75rem 0' }}>Probing Ollama…</p>
              ) : ollama.length === 0 ? (
                <p style={{ fontSize: '0.8rem', opacity: 0.55, padding: '0.75rem 0' }}>No models found. Is Ollama running?</p>
              ) : (
                <div className="db-model-list">
                  {ollama.map(m => {
                    const curRole = roleForEntry({ name: m.name } as ModelStatusEntry, settings, m.name)
                    return (
                      <div className="db-model-card" key={m.name} data-downloaded="true">
                        <div className="db-model-card-info">
                          <div className="db-model-card-name">
                            {m.name}
                            <span className="db-badge db-badge-green">ready</span>
                            {curRole && (
                              <span
                                className="db-badge"
                                style={{ background: 'rgba(169,111,245,0.18)', color: '#a96ff5', border: '1px solid rgba(169,111,245,0.3)' }}
                              >
                                {ROLE_ICONS[curRole]} {ROLE_LABELS[curRole]}
                              </span>
                            )}
                          </div>
                          <div className="db-model-card-size">{formatSize(Math.round(m.size / (1024 * 1024)))}</div>
                        </div>
                        <div className="db-model-card-actions">
                          <div style={{ display: 'flex', gap: '0.3rem', flexWrap: 'wrap', justifyContent: 'flex-end' }}>
                            {(['chat', 'think', 'task'] as const).map(role => (
                              <button
                                key={role}
                                className="db-btn-sm"
                                title={`Assign to ${ROLE_LABELS[role]}`}
                                onClick={() => handleAssignRole(role, 'ollama', m.name)}
                                style={{
                                  fontSize: '0.7rem',
                                  padding: '0.15rem 0.45rem',
                                  background: curRole === role ? 'rgba(169,111,245,0.25)' : undefined,
                                  border: curRole === role ? '1px solid rgba(169,111,245,0.5)' : undefined,
                                }}
                              >
                                {ROLE_ICONS[role]}
                              </button>
                            ))}
                          </div>
                        </div>
                      </div>
                    )
                  })}
                </div>
              )}
            </div>
          </>
        )}
      </div>
    </div>
  )
}
