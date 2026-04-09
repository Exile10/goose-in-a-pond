import { useState, useEffect, useCallback } from 'react'
import { api, isPreviewMode, type SensorReading } from '../api'
import DeviceIcon from './DeviceIcon'
import { logActivity } from '../activityLog'

interface Props {
  token: string
}

interface Device {
  id: string
  name: string
  type: string
  active: boolean
}

// Colour palette per device type
const TYPE_THEME: Record<string, { bg: string; color: string; badge: string }> = {
  phone:   { bg: '#ede9fe', color: '#7c3aed', badge: '#ddd6fe' },
  tablet:  { bg: '#e0f2fe', color: '#0369a1', badge: '#bae6fd' },
  laptop:  { bg: '#d1fae5', color: '#065f46', badge: '#a7f3d0' },
  desktop: { bg: '#fef3c7', color: '#92400e', badge: '#fde68a' },
  other:   { bg: '#f3f4f6', color: '#374151', badge: '#e5e7eb' },
}

function theme(type: string) {
  return TYPE_THEME[type] ?? TYPE_THEME.other
}

function loadStoredDevices(): Device[] {
  try {
    const stored = localStorage.getItem('pond_devices')
    if (!stored) return []
    const parsed: { id: string; name: string; type: string }[] = JSON.parse(stored)
    return parsed.map(d => ({ ...d, active: true }))
  } catch {
    return []
  }
}

function saveDevices(devices: Device[]) {
  localStorage.setItem('pond_devices', JSON.stringify(
    devices.map(({ id, name, type }) => ({ id, name, type }))
  ))
}

function timeAgoShort(iso: string): string {
  const diff = Math.floor((Date.now() - new Date(iso).getTime()) / 1000)
  if (diff < 60) return `${diff}s ago`
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`
  return `${Math.floor(diff / 3600)}h ago`
}

export default function DeviceList({ token }: Props) {
  const [devices, setDevices] = useState<Device[]>(() => loadStoredDevices())
  const [error, setError] = useState<string | null>(null)
  const [deviceName, setDeviceName] = useState('')
  const [deviceType, setDeviceType] = useState('phone')
  const [adding, setAdding] = useState(false)
  const [addError, setAddError] = useState<string | null>(null)
  const [showForm, setShowForm] = useState(false)
  const [expandedId, setExpandedId] = useState<string | null>(null)
  const [sensors, setSensors] = useState<Record<string, SensorReading[]>>({})

  useEffect(() => {
    if (isPreviewMode(token)) return
    api.listDevices(token)
      .then(res => {
        const updated = res.devices.map(d => ({ ...d, type: 'other', active: true }))
        setDevices(updated)
        saveDevices(updated)
      })
      .catch(err => setError(err instanceof Error ? err.message : 'Failed to load devices.'))
  }, [token])

  // Load sensors for the expanded device, poll every 30s
  const loadSensors = useCallback((deviceId: string) => {
    if (isPreviewMode(token)) return
    api.getSensors(deviceId, token, 5)
      .then(res => setSensors(prev => ({ ...prev, [deviceId]: res.readings })))
      .catch(() => { /* sensor data optional */ })
  }, [token])

  useEffect(() => {
    if (!expandedId) return
    loadSensors(expandedId)
    const id = setInterval(() => loadSensors(expandedId), 30_000)
    return () => clearInterval(id)
  }, [expandedId, loadSensors])

  function toggleExpand(id: string) {
    setExpandedId(prev => prev === id ? null : id)
  }

  function toggleDevice(id: string) {
    setDevices(prev => prev.map(d => {
      if (d.id !== id) return d
      const next = { ...d, active: !d.active }
      logActivity('device_toggled', `${next.name} turned ${next.active ? 'on' : 'off'}`)
      return next
    }))
  }

  function removeDevice(id: string) {
    const device = devices.find(d => d.id === id)
    if (device) logActivity('device_removed', `Removed device: ${device.name}`)
    const updated = devices.filter(d => d.id !== id)
    setDevices(updated)
    saveDevices(updated)
    if (expandedId === id) setExpandedId(null)
  }

  async function handleAdd(e: React.FormEvent) {
    e.preventDefault()
    if (!deviceName.trim()) return
    setAddError(null)
    setAdding(true)
    try {
      if (isPreviewMode(token)) {
        const updated = [...devices, {
          id: crypto.randomUUID(),
          name: deviceName.trim(),
          type: deviceType,
          active: true,
        }]
        setDevices(updated)
        saveDevices(updated)
      } else {
        await api.registerDevice({ name: deviceName.trim(), type: deviceType }, token)
        const res = await api.listDevices(token)
        const updated = res.devices.map(d => ({ ...d, type: 'other', active: true }))
        setDevices(updated)
        saveDevices(updated)
      }
      logActivity('device_added', `Added device: ${deviceName.trim()} (${deviceType})`)
      setDeviceName('')
      setShowForm(false)
    } catch (err) {
      setAddError(err instanceof Error ? err.message : 'Failed to add device.')
    } finally {
      setAdding(false)
    }
  }

  const activeCount = devices.filter(d => d.active).length

  return (
    <div className="db-card">
      <div className="db-card-header">
        <div>
          <h3>Devices</h3>
          {devices.length > 0 && (
            <span className="db-device-count">
              {devices.length} device{devices.length !== 1 ? 's' : ''} &middot; {activeCount} active
            </span>
          )}
        </div>
        <button className="db-btn-sm" onClick={() => setShowForm(f => !f)}>
          {showForm ? 'Cancel' : '+ Add Device'}
        </button>
      </div>

      {error && <p className="db-error">{error}</p>}

      {devices.length === 0 && !error && (
        <div className="db-empty-state">
          <div className="db-empty-icon">
            <DeviceIcon type="other" size={32} />
          </div>
          <p className="db-empty-title">No devices yet</p>
          <p className="db-empty-hint">Add your first device to start managing it.</p>
          <button className="db-btn-primary" onClick={() => setShowForm(true)}>
            + Add Device
          </button>
        </div>
      )}

      {devices.length > 0 && (
        <div className="db-device-grid">
          {devices.map(device => {
            const t = theme(device.type)
            const isExpanded = expandedId === device.id
            const deviceSensors = sensors[device.id] ?? []

            return (
              <div
                key={device.id}
                className={`db-device-card ${device.active ? 'active' : 'inactive'}`}
              >
                {/* Icon — click to expand sensor panel */}
                <div
                  className="db-device-card-icon"
                  style={{ background: t.bg, color: t.color, cursor: 'pointer' }}
                  onClick={() => toggleExpand(device.id)}
                  title="View sensor readings"
                >
                  <DeviceIcon type={device.type} size={32} />
                </div>

                {/* Info */}
                <div className="db-device-card-info">
                  <span className="db-device-card-name">{device.name}</span>
                  <span
                    className="db-device-card-type"
                    style={{ background: t.badge, color: t.color }}
                  >
                    {device.type}
                  </span>
                </div>

                {/* Status badge */}
                <span className={`db-device-card-status ${device.active ? 'on' : 'off'}`}>
                  <span className="db-device-card-status-dot" />
                  {device.active ? 'Online' : 'Offline'}
                </span>

                {/* Controls */}
                <div className="db-device-card-controls">
                  <button
                    className={`db-device-card-toggle ${device.active ? 'on' : 'off'}`}
                    onClick={() => toggleDevice(device.id)}
                  >
                    {device.active ? 'Turn Off' : 'Turn On'}
                  </button>
                  <button
                    className="db-btn-icon db-btn-danger"
                    onClick={() => removeDevice(device.id)}
                    title="Remove device"
                  >
                    ✕
                  </button>
                </div>

                {/* Sensor readings panel */}
                {isExpanded && (
                  <div className="db-sensor-list">
                    {deviceSensors.length === 0 ? (
                      <span className="db-sensor-chip" style={{ fontStyle: 'italic' }}>No sensor readings</span>
                    ) : (
                      deviceSensors.map((r, i) => (
                        <span key={i} className="db-sensor-chip">
                          <strong>{r.sensor_type}</strong>
                          {r.value} {r.unit}
                          <span style={{ color: 'var(--text-faint)', marginLeft: '0.25rem' }}>
                            {timeAgoShort(r.recorded_at)}
                          </span>
                        </span>
                      ))
                    )}
                  </div>
                )}
              </div>
            )
          })}
        </div>
      )}

      {showForm && (
        <form onSubmit={handleAdd} className="db-add-form">
          <input
            type="text"
            placeholder="Device name"
            value={deviceName}
            onChange={e => setDeviceName(e.target.value)}
            required
          />
          <select value={deviceType} onChange={e => setDeviceType(e.target.value)}>
            <option value="phone">Phone</option>
            <option value="tablet">Tablet</option>
            <option value="laptop">Laptop</option>
            <option value="desktop">Desktop</option>
            <option value="other">Other</option>
          </select>
          {addError && <p className="db-error">{addError}</p>}
          <button type="submit" className="db-btn-primary" disabled={adding}>
            {adding ? 'Adding…' : 'Add'}
          </button>
        </form>
      )}
    </div>
  )
}
