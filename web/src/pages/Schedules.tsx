import { useState, useEffect } from 'react'
import { api, isPreviewMode, type ScheduledTask, type CreateScheduleRequest } from '../api'

interface Props {
  token: string
}

const CRON_HINT = 'Format: sec min hour day-of-month month day-of-week  (e.g. 0 0 8 * * * = 8am daily)'

function formatDate(iso: string | null): string {
  if (!iso) return '—'
  try {
    return new Date(iso).toLocaleString(undefined, { dateStyle: 'short', timeStyle: 'short' })
  } catch {
    return iso
  }
}

export default function Schedules({ token }: Props) {
  const [tasks, setTasks] = useState<ScheduledTask[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [showForm, setShowForm] = useState(false)

  // New schedule form state
  const [label, setLabel] = useState('')
  const [cron, setCron] = useState('')
  const [webhookUrl, setWebhookUrl] = useState('')
  const [submitting, setSubmitting] = useState(false)
  const [formError, setFormError] = useState<string | null>(null)

  async function loadTasks() {
    if (isPreviewMode(token)) {
      setTasks([])
      setLoading(false)
      return
    }
    try {
      const data = await api.listSchedules(token)
      setTasks(Array.isArray(data) ? data : [])
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load schedules.')
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    loadTasks()
    const id = setInterval(loadTasks, 30_000)
    return () => clearInterval(id)
  }, [token])

  async function handleCreate(e: React.FormEvent) {
    e.preventDefault()
    if (!label.trim() || !cron.trim()) return
    setFormError(null)
    setSubmitting(true)
    try {
      const req: CreateScheduleRequest = {
        id: crypto.randomUUID(),
        label: label.trim(),
        cron: cron.trim(),
        payload: webhookUrl.trim() ? { webhook_url: webhookUrl.trim() } : {},
      }
      await api.createSchedule(req, token)
      setLabel('')
      setCron('')
      setWebhookUrl('')
      setShowForm(false)
      await loadTasks()
    } catch (err) {
      setFormError(err instanceof Error ? err.message : 'Failed to create schedule.')
    } finally {
      setSubmitting(false)
    }
  }

  async function handleDelete(id: string) {
    try {
      await api.deleteSchedule(id, token)
      setTasks(prev => prev.filter(t => t.id !== id))
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to delete schedule.')
    }
  }

  async function handlePauseResume(task: ScheduledTask) {
    try {
      if (task.paused) {
        await api.resumeSchedule(task.id, token)
      } else {
        await api.pauseSchedule(task.id, token)
      }
      await loadTasks()
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to update schedule.')
    }
  }

  async function handleRunNow(id: string) {
    try {
      await api.runScheduleNow(id, token)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to trigger schedule.')
    }
  }

  return (
    <div className="db-page">
      <div className="db-page-header">
        <h1 className="db-page-title">Schedules</h1>
        <p className="db-page-subtitle">Automate recurring tasks with cron-based triggers.</p>
      </div>

      <div className="db-page-content">
        <div className="db-card">
          <div className="db-card-header">
            <div>
              <h3>Scheduled Tasks</h3>
              {tasks.length > 0 && (
                <span className="db-device-count">{tasks.length} task{tasks.length !== 1 ? 's' : ''}</span>
              )}
            </div>
            <button className="db-btn-sm" onClick={() => setShowForm(f => !f)}>
              {showForm ? 'Cancel' : '+ New Schedule'}
            </button>
          </div>

          {error && <p className="db-error" onClick={() => setError(null)}>{error}</p>}

          {/* Create form */}
          {showForm && (
            <form onSubmit={handleCreate} className="db-add-form">
              <input
                type="text"
                placeholder="Task name"
                value={label}
                onChange={e => setLabel(e.target.value)}
                required
              />
              <input
                className="db-cron-input"
                type="text"
                placeholder="0 0 8 * * *"
                value={cron}
                onChange={e => setCron(e.target.value)}
                required
                title={CRON_HINT}
              />
              <input
                type="url"
                placeholder="Webhook URL (optional)"
                value={webhookUrl}
                onChange={e => setWebhookUrl(e.target.value)}
              />
              <p className="db-muted" style={{ fontSize: '0.72rem', margin: '0' }}>{CRON_HINT}</p>
              {formError && <p className="db-error">{formError}</p>}
              <button type="submit" className="db-btn-primary" disabled={submitting}>
                {submitting ? 'Creating…' : 'Create'}
              </button>
            </form>
          )}

          {/* Table */}
          {loading ? (
            <p className="db-muted">Loading…</p>
          ) : tasks.length === 0 ? (
            <p className="db-schedules-empty">No scheduled tasks yet. Create one to automate actions.</p>
          ) : (
            <div className="db-table-wrapper">
              <table className="db-table">
                <thead>
                  <tr>
                    <th>Name</th>
                    <th>Cron</th>
                    <th>Status</th>
                    <th>Next Run</th>
                    <th>Last Run</th>
                    <th>Actions</th>
                  </tr>
                </thead>
                <tbody>
                  {tasks.map(task => (
                    <tr key={task.id}>
                      <td>{task.label}</td>
                      <td><code style={{ fontSize: '0.78rem' }}>{task.cron}</code></td>
                      <td>
                        {task.currently_running ? (
                          <span className="db-badge db-badge-running">Running</span>
                        ) : task.paused ? (
                          <span className="db-badge db-badge-paused">Paused</span>
                        ) : (
                          <span className="db-badge db-badge-active">Active</span>
                        )}
                      </td>
                      <td style={{ fontSize: '0.78rem', color: 'var(--text-secondary)' }}>
                        {formatDate(task.next_run)}
                      </td>
                      <td style={{ fontSize: '0.78rem', color: 'var(--text-secondary)' }}>
                        {formatDate(task.last_run)}
                      </td>
                      <td>
                        <div className="db-table-actions">
                          <button
                            className="db-btn-sm"
                            onClick={() => handleRunNow(task.id)}
                            title="Run now"
                            disabled={task.currently_running}
                          >
                            Run
                          </button>
                          <button
                            className="db-btn-sm"
                            onClick={() => handlePauseResume(task)}
                            title={task.paused ? 'Resume' : 'Pause'}
                          >
                            {task.paused ? 'Resume' : 'Pause'}
                          </button>
                          <button
                            className="db-btn-icon db-btn-danger"
                            onClick={() => handleDelete(task.id)}
                            title="Delete"
                          >
                            ✕
                          </button>
                        </div>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
