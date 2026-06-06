import { useEffect, useState } from "react";
import type { ComponentType } from "react";
import { IconRail } from "./IconRail";
import { HomeView } from "./views/Home";
import { ChatHubView } from "./views/ChatHub";
import { CanvasHubView } from "./views/CanvasHub";
import { RoutinesView } from "./views/Routines";
import { SettingsHubView } from "./views/SettingsHub";
import { NotificationsView } from "./views/Notifications";
import { ModelsDetail } from "./views/settings/Models";
import { PromptsDetail } from "./views/settings/Prompts";
import { VoiceDetail } from "./views/settings/Voice";
import { MemoryDetail } from "./views/settings/Memory";
import { ExtensionsDetail } from "./views/settings/Extensions";
import { LogsDetail } from "./views/settings/Logs";
import { PrivacyDetail } from "./views/settings/Privacy";
import { RoomsDetail } from "./views/settings/Rooms";
import { CamerasDetail } from "./views/settings/Cameras";
import { NotificationsDetail } from "./views/settings/Notifications";
// Appearance owns its own state via useTheme(); it does not navigate, so the
// wrapper below silently drops the `go` prop.
import { AppearanceView } from "./views/settings/Appearance";
import { AccountDetail } from "./views/settings/Account";
import type { SettingsRowId } from "./data/settingsConfig";

// ─── Route types ──────────────────────────────────────────────
// "notifications" is intentionally NOT in the IconRail nav list —
// it is accessed via BellShortcut in the rail foot + the Home header bell.
type HubRoute = "home" | "chat" | "canvas" | "routines" | "settings" | "notifications";
const TOP_NAV: HubRoute[] = ["home", "chat", "canvas", "routines", "settings", "notifications"];

// ─── Detail screen registry ───────────────────────────────────
// ComponentType<{ go: (r: string) => void }> — each detail screen receives go()
type DetailComponent = ComponentType<{ go: (r: string) => void }>;

const SETTINGS_VIEWS: Record<SettingsRowId, DetailComponent> = {
  models:        ModelsDetail,
  prompts:       PromptsDetail,
  voice:         VoiceDetail,
  memory:        MemoryDetail,
  extensions:    ExtensionsDetail,
  logs:          LogsDetail,
  privacy:       PrivacyDetail,
  rooms:         RoomsDetail,
  cameras:       CamerasDetail,
  notifications: NotificationsDetail,
  // AppearanceView doesn't accept a go prop — wrap it so the prop is silently dropped
  appearance:    (_props: { go: (r: string) => void }) => <AppearanceView />,
  account:       AccountDetail,
};

const SETTINGS_ROUTE_IDS = Object.keys(SETTINGS_VIEWS) as SettingsRowId[];

// ─── localStorage helpers ─────────────────────────────────────
function readStoredRoute(): string {
  try {
    const stored = localStorage.getItem("goosehub_route");
    if (stored) return stored;
  } catch {
    // localStorage not available
  }
  return "home";
}

function writeStoredRoute(route: string): void {
  try {
    localStorage.setItem("goosehub_route", route);
  } catch {
    // ignore
  }
}

// ─── Hub shell ────────────────────────────────────────────────
export function Hub() {
  const [route, setRoute] = useState<string>(readStoredRoute);

  // DeviceTile kebabs dispatch "hub:device" with the device id. Phase 8 will
  // open a HubOverlay control panel (brightness slider, thermostat dial, etc.)
  // for the dispatched device. For now we log the intent so the click isn't a
  // silent no-op for users opening devtools.
  useEffect(() => {
    function onDeviceCtrl(e: Event) {
      const id = (e as CustomEvent<string>).detail;
      // eslint-disable-next-line no-console
      console.info(`[Hub] device-control requested for "${id}" — overlay UI is Phase 8.`);
    }
    window.addEventListener("hub:device", onDeviceCtrl);
    return () => window.removeEventListener("hub:device", onDeviceCtrl);
  }, []);

  // CameraFeed expand buttons dispatch "hub:camera" with the camera id.
  // TODO Phase 8 wave 6: open a full-screen camera-feed modal overlay.
  useEffect(() => {
    function onCameraExpand(e: Event) {
      const id = (e as CustomEvent<string>).detail;
      // eslint-disable-next-line no-console
      console.info("[Hub] camera expand requested for %s — overlay UI is wave 6", id);
    }
    window.addEventListener("hub:camera", onCameraExpand);
    return () => window.removeEventListener("hub:camera", onCameraExpand);
  }, []);

  function go(r: string) {
    setRoute(r);
    writeStoredRoute(r);
  }

  // When on a sub-screen the rail's active indicator maps to "settings".
  // "notifications" keeps its own active key so BellShortcut lights up.
  const railActive: HubRoute = TOP_NAV.includes(route as HubRoute)
    ? (route as HubRoute)
    : "settings";

  function renderView() {
    // Top-level routes
    if (route === "home")          return <HomeView go={go} />;
    if (route === "chat")          return <ChatHubView />;
    if (route === "canvas")        return <CanvasHubView />;
    if (route === "routines")      return <RoutinesView />;
    if (route === "settings")      return <SettingsHubView go={go} />;
    if (route === "notifications") return <NotificationsView go={go} />;

    // Settings sub-routes
    if (SETTINGS_ROUTE_IDS.includes(route as SettingsRowId)) {
      const Detail = SETTINGS_VIEWS[route as SettingsRowId];
      return <Detail go={go} />;
    }

    // Fallback
    return <HomeView go={go} />;
  }

  return (
    <div className="ghub">
      <IconRail active={railActive} go={go} />
      <main className="ghub__main" key={route}>
        {renderView()}
      </main>
    </div>
  );
}
