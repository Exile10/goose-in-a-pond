import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { api } from "../api/PondApiClient";
import { ApiError, type Connections, type SetupStatus } from "../api/types";
import { isConnections, isSetupStatus } from "./connectionsView";
import {
  ConnectionsContext,
  type ConnectionsValue,
  type LinkHealth,
} from "./useConnections";

/** A status read is cheap; this is often enough that a cable plugged in shows while the page is open. */
const FRESH_MS = 10_000;
/** After a miss: quick at first, because a join that moved the pond answers again within seconds. */
const RETRY_MS = [2_000, 3_000, 5_000, 8_000, 15_000];
const RECOVERED_NOTE_MS = 8_000;

interface ProviderProps {
  /** False keeps the provider mounted, and quiet, while some other settings page is showing. */
  enabled?: boolean;
  /** Changes when a setting that alters what the radios do was saved, so the status is read again. */
  refreshKey?: unknown;
  children: ReactNode;
}

export function ConnectionsProvider({ enabled = true, refreshKey, children }: ProviderProps) {
  const [connections, setConnections] = useState<Connections | null>(null);
  const [setup, setSetup] = useState<SetupStatus | null>(null);
  const [platform, setPlatform] = useState<string | null>(null);
  const [health, setHealth] = useState<LinkHealth>("loading");
  const [recovered, setRecovered] = useState(false);

  const alive = useRef(false);
  const seq = useRef(0);
  const failures = useRef(0);
  const wake = useRef<((ms: number) => void) | null>(null);
  const recoveredTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const load = useCallback(async (): Promise<boolean> => {
    const mine = ++seq.current;
    // Wrapped so a client without these methods fails here, not at render.
    const [read, card] = await Promise.allSettled([
      Promise.resolve().then(() => api.getConnections()),
      Promise.resolve().then(() => api.getSetupStatus()),
    ]);
    // A slower, older answer must not overwrite a newer one.
    if (!alive.current || mine !== seq.current) return read.status === "fulfilled";

    if (card.status === "fulfilled" && isSetupStatus(card.value)) setSetup(card.value);

    if (read.status === "rejected") {
      const answered = read.reason instanceof ApiError && read.reason.status !== 408;
      failures.current += 1;
      setHealth(answered ? "unexpected" : "retrying");
      return false;
    }
    if (!isConnections(read.value)) {
      failures.current += 1;
      setHealth("unexpected");
      return false;
    }

    setConnections(read.value);
    setHealth("ok");
    if (failures.current > 0) {
      setRecovered(true);
      if (recoveredTimer.current) clearTimeout(recoveredTimer.current);
      recoveredTimer.current = setTimeout(() => setRecovered(false), RECOVERED_NOTE_MS);
    }
    failures.current = 0;
    return true;
  }, []);

  useEffect(() => {
    if (!enabled) return;
    alive.current = true;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const schedule = (ms: number) => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(tick, ms);
    };
    const tick = async () => {
      // A hidden window is not looking; it reads again when it comes back.
      const ok = document.hidden ? true : await load();
      if (!alive.current) return;
      const missed = Math.max(0, Math.min(failures.current, RETRY_MS.length) - 1);
      schedule(ok ? FRESH_MS : RETRY_MS[missed]);
    };

    wake.current = schedule;
    void tick();
    Promise.resolve()
      .then(() => api.getSystemInfo())
      .then((info) => alive.current && setPlatform(info?.platform ?? null))
      .catch(() => undefined);

    return () => {
      alive.current = false;
      wake.current = null;
      if (timer) clearTimeout(timer);
      if (recoveredTimer.current) clearTimeout(recoveredTimer.current);
    };
  }, [enabled, load]);

  const first = useRef(true);
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    if (!enabled) return;
    // The radio takes a moment to follow a saved switch.
    const timer = setTimeout(() => void load(), 1_200);
    return () => clearTimeout(timer);
  }, [refreshKey, enabled, load]);

  const refresh = useCallback(() => load(), [load]);
  const expectDrop = useCallback((lost = false) => {
    if (lost) {
      failures.current = Math.max(1, failures.current);
      setHealth("retrying");
    }
    wake.current?.(RETRY_MS[0]);
  }, []);

  const value = useMemo<ConnectionsValue>(
    () => ({ connections, setup, platform, health, recovered, refresh, expectDrop }),
    [connections, setup, platform, health, recovered, refresh, expectDrop],
  );
  return <ConnectionsContext.Provider value={value}>{children}</ConnectionsContext.Provider>;
}
