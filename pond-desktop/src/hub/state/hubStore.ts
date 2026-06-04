import { useSyncExternalStore } from "react";
import { HOME } from "../data/mockHome";

// ─── Device state shape ────────────────────────────────────────

export interface DeviceState {
  on: boolean;
  locked: boolean;
  target: number;
  cur: number;
  brightness: number;
  temp: string;
  watts: number;
  mode: string;
}

export type DevicePatch = Partial<DeviceState>;

// ─── Store implementation (mirrors HUBSTORE from design) ───────

type Subscriber = () => void;

interface HubStoreInternals {
  data: Record<string, DeviceState>;
  /** Snapshot is a new object reference each mutation so useSyncExternalStore detects the change */
  snapshot: Record<string, DeviceState>;
  subs: Set<Subscriber>;
}

const store: HubStoreInternals = {
  data: {},
  snapshot: {},
  subs: new Set(),
};

// Initialise from mock data — also set snapshot to the same initial object
HOME.devices.forEach((d) => {
  store.data[d.id] = {
    on:         d.on ?? false,
    locked:     d.locked ?? true,
    target:     d.target ?? 70,
    cur:        d.value ?? 70,
    brightness: d.kind === "light" ? (d.on ? 80 : 40) : 0,
    temp:       "warm",
    watts:      d.kind === "plug" ? 42 : 0,
    mode:       "Heat",
  };
});
// Initial snapshot is a shallow copy of data
store.snapshot = { ...store.data };

function getDevice(id: string): DeviceState {
  return store.data[id] ?? {
    on: false, locked: true, target: 70, cur: 70,
    brightness: 40, temp: "warm", watts: 0, mode: "Heat",
  };
}

function setDevice(id: string, patch: DevicePatch): void {
  store.data[id] = { ...getDevice(id), ...patch };
  // Shallow-copy data so useSyncExternalStore detects a new snapshot reference
  store.snapshot = { ...store.data };
  store.subs.forEach((f) => f());
}

function subscribe(f: Subscriber): () => void {
  store.subs.add(f);
  return () => store.subs.delete(f);
}

// Returns a new object reference on every mutation so useSyncExternalStore
// detects the change via Object.is comparison.
function getSnapshot(): Record<string, DeviceState> {
  return store.snapshot;
}

// ─── React hook ────────────────────────────────────────────────

/**
 * Returns [deviceState, setDeviceState] for a given device id.
 * Reactive: any call to setDeviceState re-renders all subscribers.
 */
export function useDeviceState(id: string): [DeviceState, (patch: DevicePatch) => void] {
  const snapshot = useSyncExternalStore(subscribe, getSnapshot);
  const state = snapshot[id] ?? getDevice(id);
  const set = (patch: DevicePatch) => setDevice(id, patch);
  return [state, set];
}

// Expose raw setDevice for non-React contexts (e.g. event handlers from
// CategoryDock that may need to toggle multiple devices at once).
export { setDevice as hubSetDevice, getDevice as hubGetDevice };
