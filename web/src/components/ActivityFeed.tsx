import { useState, useEffect } from 'react'
import { getActivity, clearActivity, timeAgo, type ActivityEntry } from '../activityLog'

const ICONS: Record<string, string> = {
  chat: '💬',
  device_added: '➕',
  device_removed: '✕',
  device_toggled: '⚡',
}

function ActivityRow({ entry }: { entry: ActivityEntry }) {
  return (
    <li className="db-activity-row">
      <span className="db-activity-icon">{ICONS[entry.type] ?? '•'}</span>
      <span className="db-activity-text">{entry.text}</span>
      <span className="db-activity-time">{timeAgo(entry.timestamp)}</span>
    </li>
  )
}

export default function ActivityFeed() {
  const [entries, setEntries] = useState<ActivityEntry[]>(() => getActivity())

  // Poll every 3 seconds so new entries from ChatWidget/DeviceList appear live
  useEffect(() => {
    const id = setInterval(() => setEntries(getActivity()), 3000)
    return () => clearInterval(id)
  }, [])

  function handleClear() {
    clearActivity()
    setEntries([])
  }

  return (
    <div className="db-card">
      <div className="db-card-header">
        <h3>Recent Activity</h3>
        {entries.length > 0 && (
          <button className="db-btn-sm" onClick={handleClear}>Clear</button>
        )}
      </div>

      {entries.length === 0 ? (
        <p className="db-muted">No activity yet. Send a message or manage a device to see events here.</p>
      ) : (
        <ul className="db-activity-list">
          {entries.map(e => <ActivityRow key={e.id} entry={e} />)}
        </ul>
      )}
    </div>
  )
}
