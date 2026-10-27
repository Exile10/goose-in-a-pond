import { createContext, useContext } from "react";
import type { Connections, SetupStatus } from "../api/types";

/** `retrying` keeps the last reading on screen while the app keeps asking. */
export type LinkHealth = "loading" | "ok" | "retrying" | "unexpected";

export interface ConnectionsValue {
  connections: Connections | null;
  setup: SetupStatus | null;
  /** The pond host's operating system, to word an OS refusal for the machine it came from. */
  platform: string | null;
  health: LinkHealth;
  recovered: boolean;
  refresh: () => Promise<boolean>;
  /** Ask again soon, not after the interval; `lost` means a request already got no answer. */
  expectDrop: (lost?: boolean) => void;
}

export const ConnectionsContext = createContext<ConnectionsValue | null>(null);

export function useConnections(): ConnectionsValue {
  const value = useContext(ConnectionsContext);
  if (!value) throw new Error("useConnections needs a ConnectionsProvider");
  return value;
}
