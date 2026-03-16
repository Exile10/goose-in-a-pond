import { useState } from 'react'
import { api } from '../api'
import type { OnboardingContext } from '../pages/Onboarding'

interface Props {
  onNext: (data: Partial<OnboardingContext>) => void
}

function getOrCreateClientId(): string {
  const key = 'pond_client_id'
  let id = localStorage.getItem(key)
  if (!id) {
    id = crypto.randomUUID()
    localStorage.setItem(key, id)
  }
  return id
}

export default function VerifyDevice({ onNext }: Props) {
  const [pairingCode, setPairingCode] = useState('')
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault()
    setError(null)
    setLoading(true)

    const clientId = getOrCreateClientId()

    try {
      const res = await api.handshake({
        client_id: clientId,
        client_type: 'web',
        client_version: '0.1.0',
        pairing_code: pairingCode || undefined,
      })

      if (!res.accepted || !res.session_token) {
        setError(res.rejection_reason ?? 'Device was not accepted by the server.')
        return
      }

      localStorage.setItem('pond_session_token', res.session_token)
      onNext({ sessionToken: res.session_token, clientId })
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Connection failed.')
    } finally {
      setLoading(false)
    }
  }

  return (
    <form onSubmit={handleSubmit} className="ob-form">
      <h2>Verify this device</h2>
      <p className="ob-hint">
        Connect this browser to your Goose In A Pond instance. If your pond
        requires a pairing code, enter it below.
      </p>

      <label htmlFor="pairing-code">Pairing code <span className="ob-optional">(optional)</span></label>
      <input
        id="pairing-code"
        type="text"
        placeholder="e.g. 1234-ABCD"
        value={pairingCode}
        onChange={e => setPairingCode(e.target.value)}
        autoComplete="off"
      />

      {error && (
        <>
          <p className="ob-error">{error}</p>
          <p className="ob-hint" style={{ marginTop: '0.5rem' }}>
            No server running?{' '}
            <button
              type="button"
              className="ob-btn-inline"
              onClick={() => {
                const clientId = getOrCreateClientId()
                localStorage.setItem('pond_session_token', 'dev-mock-token')
                onNext({ sessionToken: 'dev-mock-token', clientId })
              }}
            >
              Continue in preview mode
            </button>
          </p>
        </>
      )}

      <div className="ob-actions">
        <button type="submit" className="ob-btn ob-btn-primary" disabled={loading}>
          {loading ? 'Connecting...' : 'Connect'}
        </button>
      </div>
    </form>
  )
}
