/**
 * SettingsContext — API-first settings store.
 *
 * Fetches settings and active role assignments from the backend on every
 * page load. Components read from this context instead of localStorage.
 *
 * localStorage is retained only for ephemeral tokens and UI cosmetics:
 *   pond_session_token, pond_client_id, pond_chat_session_id, pond_theme, pond_tts_muted
 */

import React, {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
} from 'react'
import { api, type Settings } from '../api'

// ── Public types ──────────────────────────────────────────────────────────────

export interface ActiveRoles {
  chat:        { provider: string; model: string }
  think:       { provider: string | null; model: string | null }
  task:        { provider: string | null; model: string | null }
  asr:         { model_id: string | null }
  tts:         { model_id: string | null }
  router_name: string
}

export interface AppSettings extends Settings {
  activeRoles: ActiveRoles | null
}

interface SettingsContextValue {
  /** Null until the first fetch completes. */
  settings: AppSettings | null
  /** True once at least one successful fetch has returned. */
  isLoaded: boolean
  /** Re-fetch from the API (call after saving settings). */
  refetch: () => Promise<void>
}

// ── Context ───────────────────────────────────────────────────────────────────

const SettingsContext = createContext<SettingsContextValue>({
  settings:  null,
  isLoaded:  false,
  refetch:   async () => {},
})

// ── Provider ──────────────────────────────────────────────────────────────────

interface Props {
  token: string
  children: React.ReactNode
}

export function SettingsProvider({ token, children }: Props) {
  const [settings, setSettings] = useState<AppSettings | null>(null)
  const [isLoaded, setIsLoaded] = useState(false)

  const refetch = useCallback(async () => {
    if (!token) return

    try {
      const [apiSettings, activeRoles] = await Promise.all([
        api.getSettings(token),
        api.getActiveRoles(token).catch(() => null),
      ])

      setSettings({ ...apiSettings, activeRoles })
      setIsLoaded(true)
    } catch {
      // If API is unreachable (e.g. server not yet started), leave existing
      // settings in place and mark as loaded so the UI doesn't block forever.
      setIsLoaded(true)
    }
  }, [token])

  // Fetch on every mount — this fires once per page load/navigation.
  useEffect(() => {
    void refetch()
  }, [refetch])

  return (
    <SettingsContext.Provider value={{ settings, isLoaded, refetch }}>
      {children}
    </SettingsContext.Provider>
  )
}

// ── Hook ──────────────────────────────────────────────────────────────────────

export const useSettings = () => useContext(SettingsContext)
