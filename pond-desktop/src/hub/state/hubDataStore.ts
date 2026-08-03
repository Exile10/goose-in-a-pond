import { useSyncExternalStore } from "react";
import { api } from "../../api/PondApiClient";
import type {
  AgentRecipe,
  Device,
  MusicControlAction,
  NowPlayingApiResponse,
  Schedule,
  Settings,
  WeatherApiResponse,
} from "../../api/types";
import {
  HOME as MOCK_HOME,
  type CameraData,
  type CategoryData,
  type DeviceData,
  type DeviceKind,
  type HomeData,
  type NowPlayingData,
  type RoomData,
  type SceneData,
  type WeatherData,
} from "../data/mockHome";
import { ROUTINES as MOCK_ROUTINES, type RoutineDetail } from "../data/routines";
import { sunEl, filmEl, focusEl } from "../primitives/HubIco";
import { HP_PATHS } from "../primitives/icons";

// Reactive store that loads real data from PondApiClient and exposes it in the
// HomeData shape used by Hub primitives. Falls back to mock data when the API
// is offline so the UI is always renderable.

type Subscriber = () => void;

interface InternalState {
  data: HomeData;
  routines: RoutineDetail[];
  loaded: boolean;
  loading: boolean;
  subs: Set<Subscriber>;
}

const state: InternalState = {
  data: MOCK_HOME,
  routines: MOCK_ROUTINES,
  loaded: false,
  loading: false,
  subs: new Set(),
};

function emit() {
  state.subs.forEach((f) => f());
}

function subscribe(f: Subscriber): () => void {
  state.subs.add(f);
  return () => state.subs.delete(f);
}

function getSnapshot(): HomeData {
  return state.data;
}

// ─── Mappers ──────────────────────────────────────────────────

const KIND_FROM_TYPE: Record<string, DeviceKind> = {
  light: "light",
  smart_light: "light",
  bulb: "light",
  lamp: "light",
  lock: "lock",
  smart_lock: "lock",
  thermo: "thermo",
  thermostat: "thermo",
  hvac: "thermo",
  plug: "plug",
  outlet: "plug",
  smart_plug: "plug",
};

function inferKind(d: Device): DeviceKind | "camera" {
  const t = (d.device_type ?? "").toLowerCase();
  if (t === "camera") return "camera";
  if (KIND_FROM_TYPE[t]) return KIND_FROM_TYPE[t];
  // Try metadata.kind
  const meta = d.metadata ?? {};
  const mk = typeof meta.kind === "string" ? meta.kind.toLowerCase() : "";
  if (mk === "camera") return "camera";
  if (KIND_FROM_TYPE[mk]) return KIND_FROM_TYPE[mk];
  // Unrecognized types (host, sensor, gotg, smart_speaker, pond, edge, …) still
  // need a room tile — fall back to a generic kind instead of dropping the device.
  return "other";
}

function deviceFromApi(d: Device, kind: DeviceKind): DeviceData {
  const meta = d.metadata ?? {};
  const on = typeof meta.on === "boolean" ? meta.on : kind === "light" ? false : true;
  const locked = typeof meta.locked === "boolean" ? meta.locked : true;
  const target = typeof meta.target === "number" ? meta.target : 70;
  const value = typeof meta.value === "number" ? meta.value : kind === "thermo" ? 68 : undefined;
  return {
    id: d.id,
    name: d.name,
    kind,
    subtype: d.device_type,
    on,
    locked,
    target,
    value,
    room: d.room ?? "Home",
  };
}

function hueForId(id: string): number {
  let h = 0;
  for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) & 0xffff;
  return h % 360;
}

function cameraFromApi(d: Device): CameraData {
  const seen = d.last_seen ? new Date(d.last_seen) : new Date();
  const time = seen.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  return { id: d.id, name: d.name, time, hue: hueForId(d.id) };
}

function deriveRooms(devs: DeviceData[]): RoomData[] {
  // Always include "Home" first; then unique device.room values in their natural order.
  const seen = new Map<string, RoomData>();
  seen.set("home", { id: "home", name: "Home", icon: "home" });
  const ICON_FOR: Record<string, string> = {
    "Living Room": "sofa",
    "Kitchen":     "utensils",
    "Bedroom":     "bed",
    "Office":      "briefcase",
    "Outdoor":     "tree",
    "Garage":      "tree",
    "Bathroom":    "tree",
  };
  for (const d of devs) {
    const name = d.room;
    if (!name || name === "Home") continue;
    const id = name.toLowerCase().replace(/\s+/g, "");
    if (seen.has(id)) continue;
    seen.set(id, { id, name, icon: ICON_FOR[name] ?? "home" });
  }
  return Array.from(seen.values());
}

const CATEGORY_TEMPLATE: Record<Exclude<DeviceKind, "other">, Omit<CategoryData, "status">> = {
  light:  { id: "lights",  label: "Lights",   icon: "bulb",   color: "#D97706", bg: "#FEF3C7" },
  lock:   { id: "locks",   label: "Locks",    icon: "lock",   color: "#2563EB", bg: "#DBEAFE" },
  thermo: { id: "climate", label: "Climate",  icon: "thermo", color: "#EA580C", bg: "#FFEDD5" },
  plug:   { id: "plugs",   label: "Plugs",    icon: "plug",   color: "#0D9488", bg: "#CCFBF1" },
};

function deriveCategories(devs: DeviceData[], cams: CameraData[]): CategoryData[] {
  const out: CategoryData[] = [];
  // Security pinned first (no device backing yet)
  out.push({
    id: "security", label: "Security", status: "Disarmed",
    icon: "shieldCheck", color: "#16A34A", bg: "#DCFCE7",
  });

  const byKind: Record<DeviceKind, DeviceData[]> = { light: [], lock: [], thermo: [], plug: [], other: [] };
  for (const d of devs) byKind[d.kind].push(d);

  if (byKind.lock.length) {
    const locked = byKind.lock.filter((d) => d.locked).length;
    out.push({
      ...CATEGORY_TEMPLATE.lock,
      status: locked === byKind.lock.length ? "All locked" : `${locked}/${byKind.lock.length} locked`,
    });
  }
  if (byKind.thermo.length) {
    const t = byKind.thermo[0];
    out.push({ ...CATEGORY_TEMPLATE.thermo, status: `Heat to ${t.target ?? 70}°` });
  }
  if (byKind.light.length) {
    const on = byKind.light.filter((d) => d.on).length;
    out.push({ ...CATEGORY_TEMPLATE.light, status: `${on} on` });
  }
  if (cams.length) {
    out.push({
      id: "cameras", label: "Cameras", status: `${cams.length} live`,
      icon: "cctv", color: "#7C3AED", bg: "#EDE9FE",
    });
  }
  if (byKind.plug.length) {
    const on = byKind.plug.filter((d) => d.on).length;
    out.push({ ...CATEGORY_TEMPLATE.plug, status: `${on} on` });
  }
  return out;
}

const SCENE_ICONS = ["sun", "moon", "film", "away", "focus"];

function scenesFromSchedules(schedules: Schedule[]): SceneData[] {
  if (!schedules.length) return MOCK_HOME.scenes;
  return schedules.slice(0, 5).map((s, i) => ({
    id: s.id,
    name: s.label ?? s.name,
    icon: SCENE_ICONS[i] ?? "sun",
    active: i === 0,
  }));
}

// ─── Recipe → RoutineDetail mapping ───────────────────────────

const ROUTINE_TEMPLATES: Array<Omit<RoutineDetail, "id" | "name" | "does" | "time">> = [
  { iconPath: sunEl,         color: "#F59E0B", bg: "linear-gradient(150deg,#FCD34D,#F59E0B)", prompt: "" },
  { iconPath: HP_PATHS.moon, color: "#6366F1", bg: "linear-gradient(150deg,#818CF8,#4F46E5)", prompt: "" },
  { iconPath: filmEl,        color: "#7C3AED", bg: "linear-gradient(150deg,#A78BFA,#7C3AED)", prompt: "" },
  { iconPath: HP_PATHS.away, color: "#0D9488", bg: "linear-gradient(150deg,#2DD4BF,#0D9488)", prompt: "" },
  { iconPath: focusEl,       color: "#EC4899", bg: "linear-gradient(150deg,#F472B6,#DB2777)", prompt: "" },
];

function recipeIdHash(name: string): number {
  let h = 0;
  for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) & 0xffff;
  return h;
}

function routinesFromRecipes(recipes: AgentRecipe[]): RoutineDetail[] {
  if (!recipes.length) return MOCK_ROUTINES;
  return recipes.map((r) => {
    // Re-use the mock visual template when the recipe name matches a known one
    const known = MOCK_ROUTINES.find((m) => m.name.toLowerCase() === r.name.toLowerCase());
    const template = known ?? {
      iconPath: ROUTINE_TEMPLATES[recipeIdHash(r.name) % ROUTINE_TEMPLATES.length].iconPath,
      color:    ROUTINE_TEMPLATES[recipeIdHash(r.name) % ROUTINE_TEMPLATES.length].color,
      bg:       ROUTINE_TEMPLATES[recipeIdHash(r.name) % ROUTINE_TEMPLATES.length].bg,
    };
    // Pull `does` chips from description (split on commas/semicolons), fallback to known
    const desc = (r.description ?? "").trim();
    const does = known
      ? known.does
      : desc
        ? desc.split(/[,;]/).map((s) => s.trim()).filter(Boolean).slice(0, 4)
        : ["On demand"];
    return {
      id:       r.name as RoutineDetail["id"],
      name:     known?.name ?? r.name,
      iconPath: template.iconPath,
      color:    template.color,
      bg:       template.bg,
      does:     does.length ? does : ["On demand"],
      time:     known?.time ?? "On demand",
      prompt:   known?.prompt ?? `Run routine: ${r.name}`,
    };
  });
}

function weatherFromApi(w: WeatherApiResponse | null): WeatherData {
  if (!w || !w.enabled) return MOCK_HOME.weather;
  return {
    temp: w.temp ?? MOCK_HOME.weather.temp,
    cond: w.cond ?? MOCK_HOME.weather.cond,
    icon: w.icon ?? MOCK_HOME.weather.icon,
    hi: w.hi ?? MOCK_HOME.weather.hi,
    lo: w.lo ?? MOCK_HOME.weather.lo,
    hum: w.hum ?? MOCK_HOME.weather.hum,
    wind: w.wind ?? MOCK_HOME.weather.wind,
    sunrise: w.sunrise ?? MOCK_HOME.weather.sunrise,
    sunset: w.sunset ?? MOCK_HOME.weather.sunset,
    forecast: w.forecast?.length
      ? (w.forecast as WeatherData["forecast"])
      : MOCK_HOME.weather.forecast,
  };
}

function nowPlayingFromApi(np: NowPlayingApiResponse | null): NowPlayingData {
  if (!np || !np.connected) return { ...MOCK_HOME.nowPlaying, connected: false };
  // Spotify answered but refused the request. This is NOT "nothing playing" —
  // the account is linked, so silently showing an idle player hides a problem
  // the user has to act on (and the transport controls would fail too).
  if (np.error) {
    return {
      track: np.error === "forbidden" ? "Spotify not authorised" : "Spotify unavailable",
      artist: np.message || "",
      elapsed: 0,
      hue: MOCK_HOME.nowPlaying.hue,
      connected: true,
      playing: false,
      error: np.error,
      message: np.message,
    };
  }
  // Connected but nothing actively playing (Spotify's 204 case) — show an
  // honest idle state instead of the mock/demo track, so a real connection
  // never gets mistaken for the decorative filler.
  const progress = np.progress_ms ?? 0;
  const duration = np.duration_ms ?? 0;
  return {
    track: np.track || "Nothing playing",
    artist: np.artist || "",
    elapsed: duration > 0 ? progress / duration : 0,
    hue: MOCK_HOME.nowPlaying.hue,
    connected: true,
    playing: np.playing ?? false,
  };
}

// ─── Loader ───────────────────────────────────────────────────

async function load() {
  if (state.loading) return;
  state.loading = true;
  try {
    const [settings, devices, schedules, recipes, weather, nowPlaying] = await Promise.allSettled([
      api.getSettings(),
      api.listDevices(),
      api.listSchedules(),
      api.listRecipes(),
      api.getWeather(),
      api.getNowPlaying(),
    ]);

    const sOK = settings.status === "fulfilled" ? (settings.value as Settings) : null;
    const dOK = devices.status === "fulfilled" ? devices.value : [];
    const schOK = schedules.status === "fulfilled" ? schedules.value : [];
    const rcOK = recipes.status === "fulfilled" ? recipes.value : [];
    const wOK = weather.status === "fulfilled" ? weather.value : null;
    const npOK = nowPlaying.status === "fulfilled" ? nowPlaying.value : null;

    // Partition devices into controllable + cameras
    const ctlDevices: DeviceData[] = [];
    const cams: CameraData[] = [];
    for (const d of dOK) {
      const k = inferKind(d);
      if (k === "camera") cams.push(cameraFromApi(d));
      else ctlDevices.push(deviceFromApi(d, k));
    }

    // If backend has no devices at all, keep mock devices/cameras as a friendly demo.
    const useMockDevices = ctlDevices.length === 0 && cams.length === 0;
    const finalDevices = useMockDevices ? MOCK_HOME.devices : ctlDevices;
    const finalCameras = useMockDevices ? MOCK_HOME.cameras : cams;

    const rooms = useMockDevices ? MOCK_HOME.rooms : deriveRooms(finalDevices);
    const categories = useMockDevices
      ? MOCK_HOME.categories
      : deriveCategories(finalDevices, finalCameras);

    const userName = (sOK?.user_name && sOK.user_name.trim()) || MOCK_HOME.user;
    const scenes = scenesFromSchedules(schOK);

    state.data = {
      ...MOCK_HOME,
      user: userName,
      devices: finalDevices,
      cameras: finalCameras,
      rooms,
      categories,
      scenes,
      weather: weatherFromApi(wOK),
      nowPlaying: nowPlayingFromApi(npOK),
    };
    state.routines = routinesFromRecipes(rcOK);
    state.loaded = true;
    emit();
  } catch {
    // keep mock fallback
  } finally {
    state.loading = false;
  }
}

/** How often the weather slice is re-fetched. The server caches upstream
 *  responses for 15 minutes, so most of these polls are answered locally. */
const WEATHER_POLL_MS = 10 * 60_000;

// Kick off load once on first import in a browser; safe to call again.
if (typeof window !== "undefined") {
  // Fire-and-forget; UI renders mock until load resolves.
  void load();
  // Now-playing changes on its own (user starts/stops playback elsewhere),
  // unlike the rest of the dashboard — poll it so the widget catches up
  // without requiring a manual refresh action.
  setInterval(() => {
    void refreshNowPlaying();
  }, 10_000);
  // Weather changes on its own too, and a GIAP dashboard is typically left
  // open for days — without this the card keeps showing whatever the sky was
  // doing when the app started.
  setInterval(() => {
    void refreshWeather();
  }, WEATHER_POLL_MS);
  // Timers do not fire while the machine sleeps or the window is hidden, so
  // catch up as soon as the dashboard is looked at again.
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState !== "visible") return;
    void refreshWeather();
    void refreshNowPlaying();
  });
}

// ─── Public API ───────────────────────────────────────────────

export function useHomeData(): HomeData {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

export function getHomeData(): HomeData {
  return state.data;
}

function getRoutinesSnapshot(): RoutineDetail[] {
  return state.routines;
}

export function useRoutines(): RoutineDetail[] {
  return useSyncExternalStore(subscribe, getRoutinesSnapshot, getRoutinesSnapshot);
}

export function refreshHomeData(): Promise<void> {
  return load();
}

/** Re-fetches just the weather slice, without the full dashboard reload. */
export async function refreshWeather(): Promise<void> {
  try {
    const w = await api.getWeather();
    state.data = { ...state.data, weather: weatherFromApi(w) };
    emit();
  } catch {
    // keep the last known reading rather than blanking the card
  }
}

/** Re-fetches just the now-playing snapshot, without the full dashboard reload. */
export async function refreshNowPlaying(): Promise<void> {
  try {
    const np = await api.getNowPlaying();
    state.data = { ...state.data, nowPlaying: nowPlayingFromApi(np) };
    emit();
  } catch {
    // keep whatever was last known
  }
}

/** Sends a playback control action, then re-syncs from Spotify's actual state. */
export async function controlNowPlaying(action: MusicControlAction): Promise<void> {
  try {
    await api.controlMusic(action);
  } catch {
    // ignore — Spotify may report no active device etc; nothing more to do here
  }
  await refreshNowPlaying();
}

// Test hook: reset to mock data and clear subscribers — used by vitest tests.
export function __resetHubDataForTests(): void {
  state.data = MOCK_HOME;
  state.routines = MOCK_ROUTINES;
  state.loaded = false;
  state.loading = false;
}

export function __getRoutinesForTests(): RoutineDetail[] {
  return state.routines;
}
