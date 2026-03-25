import { useState } from 'react'
import { api, isPreviewMode } from '../api'
import type { OnboardingContext } from '../pages/Onboarding'

interface Props {
  ctx: Partial<OnboardingContext>
  onNext: (data: Partial<OnboardingContext>) => void
  onBack: () => void
}

export default function CreateProfile({ ctx, onNext, onBack }: Props) {
  const [name, setName] = useState(ctx.profileName ?? '')
  const [displayName, setDisplayName] = useState(ctx.displayName ?? '')
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault()
    setError(null)

    if (!name.trim()) {
      setError('Name is required.')
      return
    }

    const trimmedName = name.trim()
    const trimmedDisplay = displayName.trim() || trimmedName

    if (isPreviewMode(ctx.sessionToken!)) {
      localStorage.setItem('pond_display_name', trimmedDisplay)
      onNext({ profileName: trimmedName, displayName: trimmedDisplay })
      return
    }

    setLoading(true)
    try {
      await api.createProfile(
        { name: trimmedName, display_name: trimmedDisplay },
        ctx.sessionToken!,
      )
      localStorage.setItem('pond_display_name', trimmedDisplay)
      onNext({ profileName: trimmedName, displayName: trimmedDisplay })
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save profile.')
    } finally {
      setLoading(false)
    }
  }

  return (
    <form onSubmit={handleSubmit} className="ob-form">
      <h2>Create your profile</h2>
      <p className="ob-hint">This is how your pond will address you.</p>

      <label htmlFor="name">Full name</label>
      <input
        id="name"
        type="text"
        placeholder="e.g. Alex"
        value={name}
        onChange={e => setName(e.target.value)}
        required
      />

      <label htmlFor="display-name">
        Display name <span className="ob-optional">(optional)</span>
      </label>
      <input
        id="display-name"
        type="text"
        placeholder="Defaults to full name"
        value={displayName}
        onChange={e => setDisplayName(e.target.value)}
      />

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
