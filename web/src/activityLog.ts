export type ActivityType =
  | 'chat'
  | 'device_added'
  | 'device_removed'
  | 'device_toggled'

export interface ActivityEntry {
  id: string
  type: ActivityType
  text: string
  timestamp: number
}

const KEY = 'pond_activity'
const MAX_ENTRIES = 30

export function getActivity(): ActivityEntry[] {
  try {
    const stored = localStorage.getItem(KEY)
    return stored ? JSON.parse(stored) : []
  } catch {
    return []
  }
}

export function logActivity(type: ActivityType, text: string) {
  const entry: ActivityEntry = {
    id: crypto.randomUUID(),
    type,
    text,
    timestamp: Date.now(),
  }
  const existing = getActivity()
  const updated = [entry, ...existing].slice(0, MAX_ENTRIES)
  localStorage.setItem(KEY, JSON.stringify(updated))
}

export function clearActivity() {
  localStorage.removeItem(KEY)
}

export function timeAgo(timestamp: number): string {
  const diff = Math.floor((Date.now() - timestamp) / 1000)
  if (diff < 60) return 'just now'
  if (diff < 3600) {
    const m = Math.floor(diff / 60)
    return `${m} min ago`
  }
  if (diff < 86400) {
    const h = Math.floor(diff / 3600)
    return `${h}h ago`
  }
  const d = Math.floor(diff / 86400)
  return `${d}d ago`
}
