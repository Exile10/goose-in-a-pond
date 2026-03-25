import { useState, useEffect } from 'react'
import { api, isPreviewMode } from '../api'

interface Props {
  token: string
}

interface Info {
  hostname: string
  version: string
  platform: string
  arch: string
  health: 'ok' | 'error' | 'loading'
}

export default function SystemStatus({ token }: Props) {
  const [info, setInfo] = useState<Info | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (isPreviewMode(token)) {
      setInfo({
        hostname: 'preview-device',
        version: '0.1.0',
        platform: 'linux',
        arch: 'x86_64',
        health: 'ok',
      })
      return
    }

    Promise.all([api.systemInfo(token), api.health()])
      .then(([sys, h]) => {
        setInfo({
          hostname: sys.hostname,
          version: sys.version,
          platform: sys.platform,
          arch: sys.arch,
          health: h.status === 'ok' ? 'ok' : 'error',
        })
      })
      .catch(err => setError(err instanceof Error ? err.message : 'Failed to load system info.'))
  }, [token])

  return (
    <div className="db-card">
      <div className="db-card-header">
        <h3>System Status</h3>
        {info && (
          <span className={`db-badge ${info.health === 'ok' ? 'db-badge-green' : 'db-badge-red'}`}>
            {info.health === 'ok' ? 'Online' : 'Degraded'}
          </span>
        )}
      </div>

      {error && <p className="db-error">{error}</p>}

      {!info && !error && <p className="db-muted">Loading...</p>}

      {info && (
        <dl className="db-info-list">
          <div className="db-info-row">
            <dt>Hostname</dt>
            <dd>{info.hostname}</dd>
          </div>
          <div className="db-info-row">
            <dt>Version</dt>
            <dd>v{info.version}</dd>
          </div>
          <div className="db-info-row">
            <dt>Platform</dt>
            <dd>{info.platform}</dd>
          </div>
          <div className="db-info-row">
            <dt>Arch</dt>
            <dd>{info.arch}</dd>
          </div>
        </dl>
      )}
    </div>
  )
}
