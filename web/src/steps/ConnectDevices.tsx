import { useState, useEffect } from 'react'
import { api, isPreviewMode } from '../api'
import type { OnboardingContext } from '../pages/Onboarding'

interface Device {
  id: string
  name: string
  type: string
}

interface Props {
  ctx: OnboardingContext
  onNext: (data: Partial<OnboardingContext>) => void
  onBack: () => void
}

function saveDevices(devices: Device[]) {
  localStorage.setItem('pond_devices', JSON.stringify(devices))
}

export default function ConnectDevices({ ctx, onNext, onBack }: Props) {
  const [devices, setDevices] = useState<Device[]>(() => {
    try {
      const stored = localStorage.getItem('pond_devices')
      return stored ? JSON.parse(stored) : []
    } catch {
      return []
    }
  })
  const [deviceName, setDeviceName] = useState('')
  const [deviceType, setDeviceType] = useState('phone')
  const [adding, setAdding] = useState(false)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [addError, setAddError] = useState<string | null>(null)

  useEffect(() => {
    if (isPreviewMode(ctx.sessionToken)) return
    api.listDevices(ctx.sessionToken)
      .then(res => {
        const loaded = res.devices.map(d => ({ ...d, type: 'other' }))
        setDevices(loaded)
        saveDevices(loaded)
      })
      .catch(err => setLoadError(err instanceof Error ? err.message : 'Failed to load devices.'))
  }, [ctx.sessionToken])

  async function handleAdd(e: React.FormEvent) {
    e.preventDefault()
    if (!deviceName.trim()) return
    setAddError(null)
    setAdding(true)

    try {
      if (isPreviewMode(ctx.sessionToken)) {
        const updated = [...devices, { id: crypto.randomUUID(), name: deviceName.trim(), type: deviceType }]
        setDevices(updated)
        saveDevices(updated)
        setDeviceName('')
      } else {
        await api.registerDevice({ name: deviceName.trim(), type: deviceType }, ctx.sessionToken)
        const res = await api.listDevices(ctx.sessionToken)
        const updated = res.devices.map(d => ({ ...d, type: 'other' }))
        setDevices(updated)
        saveDevices(updated)
        setDeviceName('')
      }
    } catch (err) {
      setAddError(err instanceof Error ? err.message : 'Failed to register device.')
    } finally {
      setAdding(false)
    }
  }

  return (
    <div className="ob-form">
      <h2>Connect devices</h2>
      <p className="ob-hint">
        Register the devices that Goose will be able to control. You can add more
        later from the dashboard.
      </p>

      {loadError && <p className="ob-error">{loadError}</p>}

      {devices.length > 0 && (
        <ul className="ob-device-list">
          {devices.map(d => (
            <li key={d.id} className="ob-device-item">
              <span className="ob-device-icon">&#9654;</span>
              <span className="ob-device-name">{d.name}</span>
              <span className="ob-device-type-tag">{d.type}</span>
            </li>
          ))}
        </ul>
      )}

      {devices.length === 0 && !loadError && (
        <p className="ob-hint ob-muted">No devices added yet.</p>
      )}

      <form onSubmit={handleAdd} className="ob-add-device">
        <h3>Add a device</h3>

        <label htmlFor="device-name">Device name</label>
        <input
          id="device-name"
          type="text"
          placeholder="e.g. My Phone"
          value={deviceName}
          onChange={e => setDeviceName(e.target.value)}
        />

        <label htmlFor="device-type">Type</label>
        <select
          id="device-type"
          value={deviceType}
          onChange={e => setDeviceType(e.target.value)}
        >
          <option value="phone">Phone</option>
          <option value="tablet">Tablet</option>
          <option value="laptop">Laptop</option>
          <option value="desktop">Desktop</option>
          <option value="other">Other</option>
        </select>

        {addError && <p className="ob-error">{addError}</p>}

        <button type="submit" className="ob-btn ob-btn-secondary" disabled={adding}>
          {adding ? 'Adding...' : '+ Add device'}
        </button>
      </form>

      <div className="ob-actions">
        <button type="button" className="ob-btn ob-btn-ghost" onClick={onBack}>
          Back
        </button>
        <button type="button" className="ob-btn ob-btn-primary" onClick={() => onNext({ devices })}>
          Finish setup
        </button>
      </div>
    </div>
  )
}
