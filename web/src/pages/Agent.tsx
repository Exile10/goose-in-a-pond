import { useState, useEffect } from 'react'
import { api } from '../api'
import type { UserSkill, AgentRecipe, MemoryFragment } from '../api'

interface Props {
  token: string
}

type Tab = 'skills' | 'recipes' | 'memories'

// ── Skills tab ────────────────────────────────────────────────────────────────

function SkillsTab({ token }: { token: string }) {
  const [skills, setSkills] = useState<UserSkill[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [name, setName] = useState('')
  const [content, setContent] = useState('')
  const [saving, setSaving] = useState(false)

  async function load() {
    try {
      setError(null)
      const data = await api.listSkills(token)
      setSkills(data)
    } catch (e) {
      setError(String(e))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => { load() }, [])

  async function handleCreate(e: React.FormEvent) {
    e.preventDefault()
    if (!name.trim() || !content.trim()) return
    setSaving(true)
    try {
      await api.createSkill({ name: name.trim(), content: content.trim() }, token)
      setName('')
      setContent('')
      load()
    } catch (e) {
      setError(String(e))
    } finally {
      setSaving(false)
    }
  }

  async function handleToggle(skill: UserSkill) {
    try {
      await api.updateSkill(skill.id, { active: !skill.active }, token)
      load()
    } catch (e) {
      setError(String(e))
    }
  }

  async function handleDelete(id: string) {
    if (!confirm('Delete this skill?')) return
    try {
      await api.deleteSkill(id, token)
      load()
    } catch (e) {
      setError(String(e))
    }
  }

  return (
    <div className="agent-section">
      <p className="agent-section-desc">
        Skills are Markdown instructions injected into the system prompt on every turn.
        Use them to give the assistant specialized knowledge or standing orders.
      </p>

      {error && <div className="agent-error">{error}</div>}

      <form className="agent-create-form" onSubmit={handleCreate}>
        <input
          className="agent-input"
          placeholder="Skill name"
          value={name}
          onChange={e => setName(e.target.value)}
        />
        <textarea
          className="agent-textarea"
          placeholder="Skill content (Markdown instructions)"
          rows={4}
          value={content}
          onChange={e => setContent(e.target.value)}
        />
        <button className="agent-btn agent-btn-primary" type="submit" disabled={saving || !name.trim() || !content.trim()}>
          {saving ? 'Saving…' : 'Add Skill'}
        </button>
      </form>

      {loading ? (
        <p className="agent-loading">Loading…</p>
      ) : skills.length === 0 ? (
        <p className="agent-empty">No skills yet. Add one above.</p>
      ) : (
        <ul className="agent-list">
          {skills.map(s => (
            <li key={s.id} className={`agent-list-item ${s.active ? '' : 'agent-inactive'}`}>
              <div className="agent-list-item-main">
                <strong className="agent-list-item-name">{s.name}</strong>
                <span className="agent-list-item-meta">{s.content.slice(0, 80)}{s.content.length > 80 ? '…' : ''}</span>
              </div>
              <div className="agent-list-item-actions">
                <button className="agent-btn agent-btn-sm" onClick={() => handleToggle(s)}>
                  {s.active ? 'Disable' : 'Enable'}
                </button>
                <button className="agent-btn agent-btn-sm agent-btn-danger" onClick={() => handleDelete(s.id)}>
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

// ── Recipes tab ───────────────────────────────────────────────────────────────

function RecipesTab({ token }: { token: string }) {
  const [recipes, setRecipes] = useState<AgentRecipe[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [yaml, setYaml] = useState('')
  const [saving, setSaving] = useState(false)
  async function load() {
    try {
      setError(null)
      const data = await api.listRecipes(token)
      setRecipes(data)
    } catch (e) {
      setError(String(e))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => { load() }, [])

  async function handleCreate(e: React.FormEvent) {
    e.preventDefault()
    if (!name.trim() || !yaml.trim()) return
    setSaving(true)
    try {
      await api.createRecipe({ name: name.trim(), description: description.trim(), yaml: yaml.trim() }, token)
      setName('')
      setDescription('')
      setYaml('')
      load()
    } catch (e) {
      setError(String(e))
    } finally {
      setSaving(false)
    }
  }

  async function handleDelete(id: string) {
    if (!confirm('Delete this recipe?')) return
    try {
      await api.deleteRecipe(id, token)
      load()
    } catch (e) {
      setError(String(e))
    }
  }

  return (
    <div className="agent-section">
      <p className="agent-section-desc">
        Recipes are Goose YAML workflows you can trigger on-demand via the agent.
      </p>

      {error && <div className="agent-error">{error}</div>}

      <form className="agent-create-form" onSubmit={handleCreate}>
        <input
          className="agent-input"
          placeholder="Recipe name (slug, e.g. morning_brief)"
          value={name}
          onChange={e => setName(e.target.value)}
        />
        <input
          className="agent-input"
          placeholder="Description (optional)"
          value={description}
          onChange={e => setDescription(e.target.value)}
        />
        <textarea
          className="agent-textarea"
          placeholder="Goose Recipe YAML"
          rows={6}
          value={yaml}
          onChange={e => setYaml(e.target.value)}
        />
        <button className="agent-btn agent-btn-primary" type="submit" disabled={saving || !name.trim() || !yaml.trim()}>
          {saving ? 'Saving…' : 'Add Recipe'}
        </button>
      </form>

      {loading ? (
        <p className="agent-loading">Loading…</p>
      ) : recipes.length === 0 ? (
        <p className="agent-empty">No recipes yet. Add one above.</p>
      ) : (
        <ul className="agent-list">
          {recipes.map(r => (
            <li key={r.id} className={`agent-list-item ${r.active ? '' : 'agent-inactive'}`}>
              <div className="agent-list-item-main">
                <strong className="agent-list-item-name">{r.name}</strong>
                {r.description && <span className="agent-list-item-meta">{r.description}</span>}
              </div>
              <div className="agent-list-item-actions">
                <button className="agent-btn agent-btn-sm agent-btn-danger" onClick={() => handleDelete(r.id)}>
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

// ── Memories tab ──────────────────────────────────────────────────────────────

function MemoriesTab({ token }: { token: string }) {
  const [memories, setMemories] = useState<MemoryFragment[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [content, setContent] = useState('')
  const [tags, setTags] = useState('')
  const [saving, setSaving] = useState(false)

  async function load() {
    try {
      setError(null)
      const data = await api.listMemories(token)
      setMemories(data)
    } catch (e) {
      setError(String(e))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => { load() }, [])

  async function handleCreate(e: React.FormEvent) {
    e.preventDefault()
    if (!content.trim()) return
    setSaving(true)
    try {
      const tagList = tags.split(',').map(t => t.trim()).filter(Boolean)
      await api.createMemory({ content: content.trim(), tags: tagList }, token)
      setContent('')
      setTags('')
      load()
    } catch (e) {
      setError(String(e))
    } finally {
      setSaving(false)
    }
  }

  async function handleDelete(id: string) {
    if (!confirm('Delete this memory?')) return
    try {
      await api.deleteMemory(id, token)
      load()
    } catch (e) {
      setError(String(e))
    }
  }

  return (
    <div className="agent-section">
      <p className="agent-section-desc">
        Memory fragments are facts the assistant recalls across sessions.
        When memory injection is enabled in Settings, recent fragments are added to every system prompt.
      </p>

      {error && <div className="agent-error">{error}</div>}

      <form className="agent-create-form" onSubmit={handleCreate}>
        <textarea
          className="agent-textarea"
          placeholder="Memory content (a fact to remember)"
          rows={3}
          value={content}
          onChange={e => setContent(e.target.value)}
        />
        <input
          className="agent-input"
          placeholder="Tags (comma-separated, optional)"
          value={tags}
          onChange={e => setTags(e.target.value)}
        />
        <button className="agent-btn agent-btn-primary" type="submit" disabled={saving || !content.trim()}>
          {saving ? 'Saving…' : 'Save Memory'}
        </button>
      </form>

      {loading ? (
        <p className="agent-loading">Loading…</p>
      ) : memories.length === 0 ? (
        <p className="agent-empty">No memories yet. Add one above.</p>
      ) : (
        <ul className="agent-list">
          {memories.map(m => (
            <li key={m.id} className="agent-list-item">
              <div className="agent-list-item-main">
                <span className="agent-list-item-name">{m.content}</span>
                {m.tags.length > 0 && (
                  <span className="agent-list-item-tags">
                    {m.tags.map(t => <span key={t} className="agent-tag">{t}</span>)}
                  </span>
                )}
                <span className="agent-list-item-meta">{m.source} · {new Date(m.created_at).toLocaleDateString()}</span>
              </div>
              <div className="agent-list-item-actions">
                <button className="agent-btn agent-btn-sm agent-btn-danger" onClick={() => handleDelete(m.id)}>
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

export default function Agent({ token }: Props) {
  const [tab, setTab] = useState<Tab>('skills')

  return (
    <div className="db-page">
      <div className="db-page-header">
        <h1 className="db-page-title">Agent</h1>
        <p className="db-page-subtitle">Manage skills, recipes, and memories for the AI agent.</p>
      </div>
      <div className="db-page-content">
        <div className="agent-tabs">
          {(['skills', 'recipes', 'memories'] as Tab[]).map(t => (
            <button
              key={t}
              className={`agent-tab ${tab === t ? 'agent-tab-active' : ''}`}
              onClick={() => setTab(t)}
            >
              {t.charAt(0).toUpperCase() + t.slice(1)}
            </button>
          ))}
        </div>
        {tab === 'skills'   && <SkillsTab   token={token} />}
        {tab === 'recipes'  && <RecipesTab  token={token} />}
        {tab === 'memories' && <MemoriesTab token={token} />}
      </div>
    </div>
  )
}
