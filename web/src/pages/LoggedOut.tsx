import { useState } from 'react'
import logo from '../assets/logo.png'

interface Props {
  onSignIn: () => Promise<void>
}

export default function LoggedOut({ onSignIn }: Props) {
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState('')

  async function handleSignIn() {
    setLoading(true)
    setError('')
    try {
      await onSignIn()
    } catch {
      setError('Could not reconnect. Make sure the pond server is running.')
      setLoading(false)
    }
  }

  return (
    <div style={{
      display: 'flex',
      flexDirection: 'column',
      alignItems: 'center',
      justifyContent: 'center',
      height: '100vh',
      gap: '1.5rem',
      background: 'var(--bg-base, #0f1117)',
      color: 'var(--text-primary, #e8eaf0)',
    }}>
      <img src={logo} alt="Goose In A Pond" style={{ width: 64, height: 64, borderRadius: 16, opacity: 0.9 }} />

      <div style={{ textAlign: 'center' }}>
        <h2 style={{ margin: 0, fontSize: '1.25rem', fontWeight: 600 }}>You've been signed out</h2>
        <p style={{ margin: '0.5rem 0 0', fontSize: '0.875rem', color: 'var(--text-secondary, rgba(255,255,255,0.45))' }}>
          Sign back in to continue using Goose In A Pond.
        </p>
      </div>

      {error && (
        <p style={{ fontSize: '0.8rem', color: 'var(--color-error, #f87171)', margin: 0 }}>{error}</p>
      )}

      <button
        onClick={handleSignIn}
        disabled={loading}
        style={{
          padding: '0.6rem 1.75rem',
          borderRadius: 8,
          border: 'none',
          background: 'var(--accent, #6c63ff)',
          color: '#fff',
          fontSize: '0.9rem',
          fontWeight: 600,
          cursor: loading ? 'not-allowed' : 'pointer',
          opacity: loading ? 0.6 : 1,
        }}
      >
        {loading ? 'Signing in…' : 'Sign back in'}
      </button>
    </div>
  )
}
