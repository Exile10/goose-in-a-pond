import { useState, useEffect } from 'react'
import { getActivity, clearActivity, timeAgo, type ActivityEntry } from '../activityLog'
import { api, isPreviewMode, type CameraEvent } from '../api'

interface Props {
  token: string
}

// Unified display entry (local + backend camera events)
interface DisplayEntry {
  id: string
  type: string
  text: string
  timestamp: number
}

const ICONS: Record<string, string> = {
  chat:           '💬',
  device_added:   '➕',
  device_removed: '✕',
  device_toggled: '⚡',
  camera:         '📷',
}

function cameraEventToEntry(e: CameraEvent): DisplayEntry {
  return {
    id: `camera-${e.id}`,
    type: 'camera',
    text: `Camera event on ${e.camera_id}: ${e.event_type}${e.confidence != null ? ` (${Math.round(e.confidence * 100)}%)` : ''}`,
    timestamp: new Date(e.created_at).getTime(),
  }
}

function localToDisplay(e: ActivityEntry): DisplayEntry {
  return { id: e.id, type: e.type, text: e.text, timestamp: e.timestamp }
}

function DisplayRow({ entry }: { entry: DisplayEntry }) {
  return (
    <li className="db-activity-row">
      <span className="db-activity-icon">{ICONS[entry.type] ?? '•'}</span>
      <span className="db-activity-text">{entry.text}</span>
      <span className="db-activity-time">{timeAgo(entry.timestamp)}</span>
    </li>
  )
}

export default function ActivityFeed({ token }: Props) {
  const [localEntries, setLocalEntries] = useState<ActivityEntry[]>(() => getActivity())
  const [cameraEvents, setCameraEvents] = useState<CameraEvent[]>([])

  // Poll local activity every 3 seconds
  useEffect(() => {
    const id = setInterval(() => setLocalEntries(getActivity()), 3000)
    return () => clearInterval(id)
  }, [])

  // Fetch camera events from backend every 60 seconds
  useEffect(() => {
    if (isPreviewMode(token)) return

    function fetchCamera() {
      api.listCameraEvents(token)
        .then(res => setCameraEvents(res.events))
        .catch(() => { /* silently ignore if backend unavailable */ })
    }

    fetchCamera()
    const id = setInterval(fetchCamera, 60_000)
    return () => clearInterval(id)
  }, [token])

  // Merge local + camera events, sort newest first
  const all: DisplayEntry[] = [
    ...localEntries.map(localToDisplay),
    ...cameraEvents.map(cameraEventToEntry),
  ].sort((a, b) => b.timestamp - a.timestamp)

  function handleClear() {
    clearActivity()
    setLocalEntries([])
  }

  return (
    <div className="db-card">
      <div className="db-card-header">
        <h3>Recent Activity</h3>
        {localEntries.length > 0 && (
          <button className="db-btn-sm" onClick={handleClear}>Clear</button>
        )}
      </div>

      {all.length === 0 ? (
        <p className="db-muted">No activity yet. Send a message or manage a device to see events here.</p>
      ) : (
        <ul className="db-activity-list">
          {all.map(e => <DisplayRow key={e.id} entry={e} />)}
        </ul>
      )}
    </div>
  )
}
