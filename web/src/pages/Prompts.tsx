import { useState, useEffect } from 'react'
import { api } from '../api'
import type { PromptTemplate, PromptExtra } from '../api'

interface Props {
  token: string
}

type Tab = 'templates' | 'extras'

// ── Templates tab ─────────────────────────────────────────────────────────────

function TemplatesTab({ token }: { token: string }) {
  const [templates, setTemplates] = useState<PromptTemplate[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [editing, setEditing] = useState<PromptTemplate | null>(null)
  const [editContent, setEditContent] = useState('')
  const [editDescription, setEditDescription] = useState('')
  const [saving, setSaving] = useState(false)

  async function load() {
    try {
      setError(null)
      const data = await api.listPromptTemplates(token)
      setTemplates(data)
    } catch (e) {
      setError(String(e))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => { load() }, [])

  function startEdit(t: PromptTemplate) {
    setEditing(t)
    setEditContent(t.content)
    setEditDescription(t.description)
  }

  async function handleSave(e: React.FormEvent) {
    e.preventDefault()
    if (!editing) return
    setSaving(true)
    try {
      await api.updatePromptTemplate(editing.name, { content: editContent, description: editDescription }, token)
      setEditing(null)
      load()
    } catch (e) {
      setError(String(e))
    } finally {
      setSaving(false)
    }
  }

  async function handleDelete(name: string) {
    if (!confirm(`Delete template "${name}"? This cannot be undone.`)) return
    try {
      await api.deletePromptTemplate(name, token)
      load()
    } catch (e) {
      setError(String(e))
    }
  }

  if (editing) {
    return (
      <div className="agent-section">
        {error && <div className="agent-error">{error}</div>}
        <div className="agent-edit-header">
          <strong className="agent-edit-title">{editing.name}</strong>
          {editing.is_system && <span className="agent-badge">built-in</span>}
        </div>
        <form className="agent-create-form" onSubmit={handleSave}>
          <input
            className="agent-input"
            placeholder="Description"
            value={editDescription}
            onChange={e => setEditDescription(e.target.value)}
          />
          <textarea
            className="agent-textarea agent-textarea-tall"
            placeholder="Template content (supports {{assistant_name}}, {{user_name}}, {{timezone}}, {{location}}, {{personality}}, {{prompt_addendum}})"
            rows={16}
            value={editContent}
            onChange={e => setEditContent(e.target.value)}
          />
          <div className="agent-form-actions">
            <button className="agent-btn agent-btn-primary" type="submit" disabled={saving || !editContent.trim()}>
              {saving ? 'Saving…' : 'Save'}
            </button>
            <button className="agent-btn" type="button" onClick={() => setEditing(null)}>Cancel</button>
          </div>
        </form>
      </div>
    )
  }

  return (
    <div className="agent-section">
      <p className="agent-section-desc">
        Prompt templates define how the assistant introduces itself and behaves.
        Built-in templates can be edited but not deleted. The active template is selected via <em>prompt_style</em> in Settings.
      </p>

      {error && <div className="agent-error">{error}</div>}

      {loading ? (
        <p className="agent-loading">Loading…</p>
      ) : templates.length === 0 ? (
        <p className="agent-empty">No templates found.</p>
      ) : (
        <ul className="agent-list">
          {templates.map(t => (
            <li key={t.name} className="agent-list-item">
              <div className="agent-list-item-main">
                <strong className="agent-list-item-name">
                  {t.name}
                  {t.is_system && <span className="agent-badge agent-badge-sm">built-in</span>}
                </strong>
                {t.description && <span className="agent-list-item-meta">{t.description}</span>}
              </div>
              <div className="agent-list-item-actions">
                <button className="agent-btn agent-btn-sm" onClick={() => startEdit(t)}>Edit</button>
                {!t.is_system && (
                  <button className="agent-btn agent-btn-sm agent-btn-danger" onClick={() => handleDelete(t.name)}>
                    Delete
                  </button>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

// ── Extras tab ────────────────────────────────────────────────────────────────

function ExtrasTab({ token }: { token: string }) {
  const [extras, setExtras] = useState<PromptExtra[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [key, setKey] = useState('')
  const [instruction, setInstruction] = useState('')
  const [saving, setSaving] = useState(false)

  async function load() {
    try {
      setError(null)
      const data = await api.listPromptExtras(token)
      setExtras(data)
    } catch (e) {
      setError(String(e))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => { load() }, [])

  async function handleUpsert(e: React.FormEvent) {
    e.preventDefault()
    if (!key.trim() || !instruction.trim()) return
    setSaving(true)
    try {
      await api.upsertPromptExtra({ key: key.trim(), instruction: instruction.trim() }, token)
      setKey('')
      setInstruction('')
      load()
    } catch (e) {
      setError(String(e))
    } finally {
      setSaving(false)
    }
  }

  async function handleToggle(extra: PromptExtra) {
    try {
      await api.upsertPromptExtra({ key: extra.key, instruction: extra.instruction, active: !extra.active }, token)
      load()
    } catch (e) {
      setError(String(e))
    }
  }

  async function handleDelete(key: string) {
    if (!confirm(`Delete extra "${key}"?`)) return
    try {
      await api.deletePromptExtra(key, token)
      load()
    } catch (e) {
      setError(String(e))
    }
  }

  return (
    <div className="agent-section">
      <p className="agent-section-desc">
        System prompt extras are keyed instructions appended to every conversation turn.
        Use them for standing rules, language preferences, or home-specific context.
      </p>

      {error && <div className="agent-error">{error}</div>}

      <form className="agent-create-form" onSubmit={handleUpsert}>
        <input
          className="agent-input"
          placeholder="Key (e.g. safety, language, home_rules)"
          value={key}
          onChange={e => setKey(e.target.value)}
        />
        <textarea
          className="agent-textarea"
          placeholder="Instruction text injected into the system prompt"
          rows={3}
          value={instruction}
          onChange={e => setInstruction(e.target.value)}
        />
        <button className="agent-btn agent-btn-primary" type="submit" disabled={saving || !key.trim() || !instruction.trim()}>
          {saving ? 'Saving…' : 'Save Extra'}
        </button>
      </form>

      {loading ? (
        <p className="agent-loading">Loading…</p>
      ) : extras.length === 0 ? (
        <p className="agent-empty">No extras yet. Add one above.</p>
      ) : (
        <ul className="agent-list">
          {extras.map(ex => (
            <li key={ex.key} className={`agent-list-item ${ex.active ? '' : 'agent-inactive'}`}>
              <div className="agent-list-item-main">
                <strong className="agent-list-item-name">{ex.key}</strong>
                <span className="agent-list-item-meta">{ex.instruction.slice(0, 100)}{ex.instruction.length > 100 ? '…' : ''}</span>
              </div>
              <div className="agent-list-item-actions">
                <button className="agent-btn agent-btn-sm" onClick={() => handleToggle(ex)}>
                  {ex.active ? 'Disable' : 'Enable'}
                </button>
                <button className="agent-btn agent-btn-sm agent-btn-danger" onClick={() => handleDelete(ex.key)}>
                  Delete
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

// ── Page ──────────────────────────────────────────────────────────────────────

export default function Prompts({ token }: Props) {
  const [tab, setTab] = useState<Tab>('templates')

  return (
    <div className="db-page">
      <div className="db-page-header">
        <h1 className="db-page-title">Prompts</h1>
        <p className="db-page-subtitle">Manage system prompt templates and per-key instruction extras.</p>
      </div>
      <div className="db-page-content">
        <div className="agent-tabs">
          {(['templates', 'extras'] as Tab[]).map(t => (
            <button
              key={t}
              className={`agent-tab ${tab === t ? 'agent-tab-active' : ''}`}
              onClick={() => setTab(t)}
            >
              {t.charAt(0).toUpperCase() + t.slice(1)}
            </button>
          ))}
        </div>
        {tab === 'templates' && <TemplatesTab token={token} />}
        {tab === 'extras'    && <ExtrasTab    token={token} />}
      </div>
    </div>
  )
}
