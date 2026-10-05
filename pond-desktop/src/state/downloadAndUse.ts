// "Download and use": the person pressed it, so when the model arrives it becomes the
// conversation model, and only then. It lives at module scope so that leaving the page does not
// break the promise they were given. Nothing else activates a model on its own.

import { useSyncExternalStore } from "react";
import { api } from "../api/PondApiClient";
import type { DownloadEntry, DownloadStarted, ModelEntry } from "../api/types";

export interface PendingUse {
  /** The row's id, `"{category}/{name}"`. */
  id: string;
  title: string;
  /** `failed` carries the reason in `message`. */
  state: "downloading" | "activating" | "failed";
  message: string | null;
}

interface Watched extends PendingUse {
  category: string;
  name: string;
  /** Ticks spent waiting for the row to read downloaded after its file arrived. */
  settling: number;
}

const POLL_MS = 1500;
/** About 45 s for the pond to get a finished model ready before saying it did not. */
const SETTLE_TICKS = 30;

const watched = new Map<string, Watched>();
const subscribers = new Set<() => void>();
const usedListeners = new Set<(model: { id: string; title: string }) => void>();
let snapshot: readonly PendingUse[] = [];
let timer: ReturnType<typeof setTimeout> | null = null;

function publish(): void {
  snapshot = [...watched.values()].map(({ id, title, state, message }) => ({ id, title, state, message }));
  subscribers.forEach((f) => f());
}

function set(id: string, patch: Partial<Watched>): void {
  const current = watched.get(id);
  if (current) watched.set(id, { ...current, ...patch });
}

/** Called once a model has become the conversation model, because the person asked for that. */
export function onUsed(listener: (model: { id: string; title: string }) => void): () => void {
  usedListeners.add(listener);
  return () => usedListeners.delete(listener);
}

function reason(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

async function activate(w: Watched): Promise<void> {
  set(w.id, { state: "activating" });
  publish();
  try {
    await api.activateModel(w.category, w.name, "chat");
    watched.delete(w.id);
    publish();
    usedListeners.forEach((f) => f({ id: w.id, title: w.title }));
  } catch (e) {
    set(w.id, { state: "failed", message: reason(e) });
    publish();
  }
}

/** One look at one model's files: wait, give up quietly, fail with a reason, or use it. */
async function settle(w: Watched, downloads: DownloadEntry[]): Promise<void> {
  const entries = downloads.filter((d) => d.model_id === w.id);
  const file = entries.find((d) => d.part === "model") ?? entries[0];

  if (file && (file.status === "downloading" || file.status === "paused")) return;
  if (file?.status === "cancelled") {
    // They stopped it: the promise is withdrawn, not broken.
    watched.delete(w.id);
    publish();
    return;
  }
  if (file?.status === "error") {
    set(w.id, { state: "failed", message: file.error?.trim() || "The download did not finish." });
    publish();
    return;
  }

  // Arrived, or no longer tracked: use it once its row reads downloaded.
  const rows = await api.listModels().catch(() => [] as ModelEntry[]);
  if (rows.find((m) => m.id === w.id)?.downloaded) {
    await activate(w);
    return;
  }
  set(w.id, { settling: w.settling + 1 });
  if (w.settling + 1 >= SETTLE_TICKS) {
    set(w.id, {
      state: "failed",
      message: "The file arrived, but the pond has not registered it yet. Press Use in a moment.",
    });
    publish();
  }
}

async function tick(): Promise<void> {
  timer = null;
  const waiting = [...watched.values()].filter((w) => w.state === "downloading");
  if (waiting.length === 0) return;
  try {
    const { downloads } = await api.getDownloadProgress();
    for (const w of waiting) {
      if (watched.get(w.id)?.state === "downloading") await settle(w, downloads ?? []);
    }
  } catch {
    // The pond did not answer this time; the next tick asks again.
  }
  schedule();
}

function schedule(): void {
  if (timer) return;
  if ([...watched.values()].some((w) => w.state === "downloading")) {
    timer = setTimeout(() => void tick(), POLL_MS);
  }
}

/** Starts the download and promises to use the model when it arrives. Rejects, with nothing
 *  promised, when the pond refuses the download. `includePictures` is left out for a model that has
 *  no picture support to ask about. */
export async function downloadAndUse(
  model: Pick<ModelEntry, "id" | "category" | "provider" | "name" | "downloaded">,
  title: string,
  includePictures?: boolean,
): Promise<DownloadStarted | null> {
  const category = model.category ?? model.provider;
  const entry: Watched = {
    id: model.id,
    title,
    category,
    name: model.name,
    state: "downloading",
    message: null,
    settling: 0,
  };
  if (model.downloaded) {
    watched.set(entry.id, entry);
    await activate(entry);
    return null;
  }
  const started = await api.downloadModel(
    category,
    model.name,
    includePictures === undefined ? undefined : { pictures: includePictures },
  );
  watched.set(entry.id, entry);
  publish();
  schedule();
  return started;
}

/** Drops a failure the person has read. */
export function dismissPending(id: string): void {
  if (watched.get(id)?.state === "failed") {
    watched.delete(id);
    publish();
  }
}

function subscribe(f: () => void): () => void {
  subscribers.add(f);
  return () => subscribers.delete(f);
}

/** What is waiting to be used, and what failed to be. */
export function usePendingUse(): readonly PendingUse[] {
  return useSyncExternalStore(subscribe, () => snapshot, () => snapshot);
}

/** Forgets everything; for tests. */
export function __resetDownloadAndUseForTests(): void {
  watched.clear();
  usedListeners.clear();
  if (timer) clearTimeout(timer);
  timer = null;
  publish();
}
