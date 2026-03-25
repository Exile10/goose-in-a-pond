import { useState } from 'react'
import { api, isPreviewMode } from '../api'
import type { OnboardingContext } from '../pages/Onboarding'

interface Props {
  ctx: Partial<OnboardingContext>
  onNext: (data: Partial<OnboardingContext>) => void
  onBack: () => void
}

const PERSONALITIES = [
  { value: 'helpful', label: 'Helpful', description: 'Clear, thorough, and focused on getting things done.' },
  { value: 'concise', label: 'Concise', description: 'Short answers, no fluff — just the essentials.' },
  { value: 'friendly', label: 'Friendly', description: 'Warm and conversational, like talking to a friend.' },
  { value: 'technical', label: 'Technical', description: 'Precise, detailed, geared toward developers.' },
]

const STYLES = [
  { value: 'proactive', label: 'Proactive', description: 'Suggests next steps and anticipates needs.' },
  { value: 'reactive', label: 'Reactive', description: 'Only responds when asked — no unsolicited suggestions.' },
]

export default function ConfigurePersonality({ ctx, onNext, onBack }: Props) {
  const [personality, setPersonality] = useState(ctx.personality ?? 'helpful')
  const [assistantStyle, setAssistantStyle] = useState(ctx.assistantStyle ?? 'proactive')
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault()
    setError(null)

    // Always persist to localStorage so Settings page picks them up
    localStorage.setItem('pond_personality', JSON.stringify(personality))
    localStorage.setItem('pond_assistant_style', JSON.stringify(assistantStyle))

    if (isPreviewMode(ctx.sessionToken!)) {
      onNext({ personality, assistantStyle })
      return
    }

    setLoading(true)
    try {
      await api.saveSettings({ personality, assistant_style: assistantStyle }, ctx.sessionToken!)
      onNext({ personality, assistantStyle })
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save settings.')
    } finally {
      setLoading(false)
    }
  }

  return (
    <form onSubmit={handleSubmit} className="ob-form">
      <h2>Configure personality</h2>
      <p className="ob-hint">Choose how your assistant communicates with you.</p>

      <fieldset className="ob-fieldset">
        <legend>Tone</legend>
        <div className="ob-radio-group">
          {PERSONALITIES.map(opt => (
            <label
              key={opt.value}
              className={`ob-radio-card ${personality === opt.value ? 'selected' : ''}`}
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
      </fieldset>

      <fieldset className="ob-fieldset">
        <legend>Style</legend>
        <div className="ob-radio-group">
          {STYLES.map(opt => (
            <label
              key={opt.value}
              className={`ob-radio-card ${assistantStyle === opt.value ? 'selected' : ''}`}
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
      </fieldset>

      {error && <p className="ob-error">{error}</p>}

      <div className="ob-actions">
        <button type="button" className="ob-btn ob-btn-ghost" onClick={onBack}>
          Back
        </button>
        <button type="submit" className="ob-btn ob-btn-primary" disabled={loading}>
          {loading ? 'Saving...' : 'Next'}
        </button>
      </div>
    </form>
  )
}
