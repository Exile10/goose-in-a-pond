// The Models data every surface reads: the catalogue, who holds each job, the memory budget and
// what is coming down. Polls the downloads only while one is moving, and when it settles re-reads
// the rest, since a finished transfer changes the list, the budget and the disk.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api/PondApiClient";
import type {
  DiskUsage,
  DownloadEntry,
  ModelActiveRoles,
  ModelEntry,
  ModelMemoryStatus,
} from "../api/types";
import { transferOf, type ModelTransfer } from "../sections/models/modelDownloads";

const POLL_MS = 1500;
/** Ticks to keep asking after every part has arrived, while the pond gets the model ready. */
const ARRIVAL_TICKS = 40;

export interface UseModels {
  models: ModelEntry[];
  roles: ModelActiveRoles | null;
  memory: ModelMemoryStatus | null;
  downloads: DownloadEntry[];
  disk: DiskUsage | null;
  /** The list has not loaded once yet. */
  loading: boolean;
  /** Why the list could not be read. */
  error: string | null;
  /** Re-reads everything. */
  reload: () => Promise<void>;
  reloadModels: () => Promise<void>;
  reloadRoles: () => Promise<void>;
  reloadMemory: () => Promise<void>;
  reloadDownloads: () => Promise<void>;
  /** Starts watching: something was just set coming down. */
  watchDownloads: () => void;
  /** What is coming down for this row, read together. */
  transferFor: (m: Pick<ModelEntry, "id" | "downloaded">) => ModelTransfer | null;
}

/** A partial `api` (a test double) may lack a method; a sync throw reads like a failed request. */
async function attempt<T>(call: () => Promise<T>): Promise<T | null> {
  try {
    return await call();
  } catch {
    return null;
  }
}

export function useModels(opts: { disk?: boolean } = {}): UseModels {
  const wantDisk = opts.disk ?? false;
  const [models, setModels] = useState<ModelEntry[]>([]);
  const [roles, setRoles] = useState<ModelActiveRoles | null>(null);
  const [memory, setMemory] = useState<ModelMemoryStatus | null>(null);
  const [downloads, setDownloads] = useState<DownloadEntry[]>([]);
  const [disk, setDisk] = useState<DiskUsage | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const alive = useRef(true);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  /** Models this page watched come down, until their row reads downloaded. */
  const arriving = useRef(new Set<string>());
  const arrivalTicks = useRef(0);

  const reloadModels = useCallback(async () => {
    try {
      const list = await api.listModels();
      if (!alive.current) return;
      setModels(list);
      setError(null);
      for (const m of list) if (m.downloaded) arriving.current.delete(m.id);
    } catch (e) {
      if (alive.current) setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (alive.current) setLoading(false);
    }
  }, []);

  const reloadRoles = useCallback(async () => {
    const r = await attempt(() => api.getActiveRoles());
    if (alive.current && r) setRoles(r);
  }, []);

  const reloadMemory = useCallback(async () => {
    const m = await attempt(() => api.getMemoryStatus());
    if (alive.current && m) setMemory(m);
  }, []);

  const reloadDisk = useCallback(async () => {
    if (!wantDisk) return;
    const d = await attempt(() => api.getDiskUsage());
    if (alive.current && d) setDisk(d);
  }, [wantDisk]);

  const readDownloads = useCallback(async (): Promise<DownloadEntry[]> => {
    const r = await attempt(() => api.getDownloadProgress());
    const list = r?.downloads ?? [];
    if (alive.current) {
      for (const d of list) {
        if ((d.status === "downloading" || d.status === "paused") && d.model_id) {
          arriving.current.add(d.model_id);
        }
      }
      setDownloads(list);
    }
    return list;
  }, []);

  const tick = useCallback(async () => {
    timer.current = null;
    if (!alive.current) return;
    if (typeof document !== "undefined" && document.visibilityState === "hidden") {
      timer.current = setTimeout(() => void tick(), POLL_MS);
      return;
    }
    const list = await readDownloads();
    if (!alive.current) return;
    if (list.some((d) => d.status === "downloading")) {
      arrivalTicks.current = 0;
      timer.current = setTimeout(() => void tick(), POLL_MS);
      return;
    }
    // Something ended or paused: the list, the budget and the disk may have moved.
    await Promise.all([reloadModels(), reloadMemory(), reloadRoles(), reloadDisk()]);
    if (arriving.current.size > 0 && arrivalTicks.current < ARRIVAL_TICKS) {
      arrivalTicks.current += 1;
      timer.current = setTimeout(() => void tick(), POLL_MS);
    } else {
      arrivalTicks.current = 0;
      arriving.current.clear();
    }
  }, [readDownloads, reloadModels, reloadMemory, reloadRoles, reloadDisk]);

  const watchDownloads = useCallback(() => {
    if (timer.current) return;
    timer.current = setTimeout(() => void tick(), POLL_MS);
  }, [tick]);

  const reloadDownloads = useCallback(async () => {
    const list = await readDownloads();
    if (list.some((d) => d.status === "downloading")) watchDownloads();
  }, [readDownloads, watchDownloads]);

  const reload = useCallback(async () => {
    await Promise.all([
      reloadModels(),
      reloadRoles(),
      reloadMemory(),
      reloadDisk(),
      reloadDownloads(),
    ]);
  }, [reloadModels, reloadRoles, reloadMemory, reloadDisk, reloadDownloads]);

  useEffect(() => {
    alive.current = true;
    void reload();
    return () => {
      alive.current = false;
      if (timer.current) clearTimeout(timer.current);
      timer.current = null;
    };
    // Once, on mount: the callbacks are stable for a given `wantDisk`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const transferFor = useCallback(
    (m: Pick<ModelEntry, "id" | "downloaded">) =>
      transferOf(m.id, downloads, {
        downloaded: m.downloaded,
        arriving: arriving.current.has(m.id),
      }),
    [downloads],
  );

  return useMemo(
    () => ({
      models,
      roles,
      memory,
      downloads,
      disk,
      loading,
      error,
      reload,
      reloadModels,
      reloadRoles,
      reloadMemory,
      reloadDownloads,
      watchDownloads,
      transferFor,
    }),
    [
      models,
      roles,
      memory,
      downloads,
      disk,
      loading,
      error,
      reload,
      reloadModels,
      reloadRoles,
      reloadMemory,
      reloadDownloads,
      watchDownloads,
      transferFor,
    ],
  );
}
