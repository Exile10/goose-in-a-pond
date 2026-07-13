import { useState, useEffect } from "react";
import { MessageCircle, RefreshCw, Cpu, Wrench, Mic, Headphones } from "lucide-react";
import { useAppState, useAppDispatch } from "../state/AppContext";
import { api } from "../api/PondApiClient";
import { useHomeData } from "../hub/state/hubDataStore";
import { AskGoose } from "../hub/primitives/AskGoose";
import { RoomPills } from "../hub/primitives/RoomPills";
import { DeviceTile } from "../hub/primitives/DeviceTile";
import { CameraFeed } from "../hub/primitives/CameraFeed";
import { Scenes } from "../hub/primitives/Scenes";
import { WeatherWidget } from "../hub/primitives/WeatherWidget";
import { NowPlaying } from "../hub/primitives/NowPlaying";
import { StickyNote } from "../hub/primitives/StickyNote";
import { TodoWidget } from "../hub/primitives/TodoWidget";
import { CategoryDock } from "../hub/primitives/CategoryDock";
import { PanelHead } from "../hub/primitives/PanelHead";
import { HubClock } from "../hub/primitives/HubClock";
import { HubIco } from "../hub/primitives/HubIco";
import { HP_PATHS } from "../hub/primitives/icons";
import { RoleChip } from "../components/shared";
import type { ModelActiveRoles } from "../api/types";

const HX = {
  chat: "M21 12a8 8 0 0 1-11.5 7.2L4 21l1.8-5.4A8 8 0 1 1 21 12z",
};

function greetingForHour(h: number): string {
  if (h < 5)  return "Good night";
  if (h < 12) return "Good morning";
  if (h < 18) return "Good afternoon";
  return "Good evening";
}

export function Dashboard() {
  const state = useAppState();
  const dispatch = useAppDispatch();
  const home = useHomeData();

  const [room, setRoom] = useState("home");
  const [activeRoles, setActiveRoles] = useState<ModelActiveRoles | null>(null);
  const [rolesLoading, setRolesLoading] = useState(false);

  useEffect(() => {
    if (!state.serverOnline || !state.sessionToken) return;
    let cancelled = false;

    setRolesLoading(true);
    api.getActiveRoles()
      .then((roles) => { if (!cancelled) setActiveRoles(roles); })
      .catch(() => {})
      .finally(() => { if (!cancelled) setRolesLoading(false); });

    return () => { cancelled = true; };
  }, [state.serverOnline, state.sessionToken]);

  function refreshRoles() {
    setRolesLoading(true);
    api.getActiveRoles()
      .then((r) => setActiveRoles(r))
      .catch(() => {})
      .finally(() => setRolesLoading(false));
  }

  function go(route: string) {
    if (route === "chat") dispatch({ type: "SET_SECTION", payload: "chat" });
  }

  const roomName = home.rooms.find((r) => r.id === room)?.name ?? "Home";
  const favorites =
    room === "home"
      ? home.devices.slice(0, 6)
      : home.devices.filter((d) => d.room === roomName);

  const greeting = greetingForHour(new Date().getHours());

  const roleItems = activeRoles
    ? [
        { role: "Chat",  model: activeRoles.chat?.model ?? "—", color: "secondary", icon: <MessageCircle size={13} /> },
        { role: "Think", model: activeRoles.tool?.model ?? "—", color: "warning",   icon: <Cpu size={13} /> },
        { role: "Task",  model: activeRoles.tool?.model ?? "—", color: "primary",   icon: <Wrench size={13} /> },
        { role: "ASR",   model: activeRoles.asr?.model  ?? "—", color: "success",   icon: <Mic size={13} /> },
        { role: "TTS",   model: activeRoles.tts?.model  ?? "—", color: "danger",    icon: <Headphones size={13} /> },
      ]
    : [];

  return (
    <div className="home2">
      <header className="home2__head">
        <div>
          <div className="hub-greet">
            {greeting}, <span>{home.user}</span>
          </div>
          <div className="home2__sub">
            {home.date} · {home.weather.cond}, {home.weather.temp}°
          </div>
        </div>
        <div className="home2__head-actions">
          <button
            className="home2__iconbtn"
            onClick={() => go("chat")}
            aria-label="Open chat"
          >
            <HubIco d={HX.chat} size={20} color="var(--pp)" />
          </button>
          <HubClock />
        </div>
      </header>

      <AskGoose go={go} />
      <RoomPills value={room} onChange={setRoom} />

      <div className="home2__grid">
        <div className="home2__main">
          <section className="gpanel">
            <PanelHead
              title={room === "home" ? "Favorites" : roomName}
              action={
                <button className="ghost-btn" onClick={() => go("rooms")}>
                  <HubIco d={HP_PATHS.sliders} size={13} color="var(--pp)" /> Manage
                </button>
              }
            />
            {favorites.length > 0 ? (
              <div className="home2__tiles">
                {favorites.map((d) => (
                  <DeviceTile key={d.id} device={d} />
                ))}
              </div>
            ) : (
              <div className="home2__empty">No devices in {roomName} yet.</div>
            )}
          </section>

          <section className="gpanel">
            <PanelHead
              title="Cameras"
              action={
                <button className="ghost-btn" onClick={() => go("cameras")}>
                  All <HubIco d={HP_PATHS.chevR} size={13} color="var(--pp)" />
                </button>
              }
            />
            <div className="home2__cams">
              {home.cameras.map((c) => (
                <CameraFeed key={c.id} cam={c} h={132} big />
              ))}
            </div>
          </section>

          <section>
            <div className="home2__sectlabel">Routines</div>
            <Scenes layout="row" />
          </section>

          <section className="gpanel">
            <PanelHead
              title="System"
              action={
                <button className="ghost-btn" onClick={refreshRoles}>
                  <RefreshCw size={13} /> Refresh
                </button>
              }
            />
            <div className="dash-system">
              <div className="dash-system__status">
                <span
                  className="server-row__dot"
                  data-online={state.serverOnline}
                  data-starting={!state.serverOnline && state.serverStarting}
                />
                <span>
                  {state.serverOnline
                    ? "Server connected"
                    : state.serverStarting
                    ? "Starting…"
                    : "Server offline"}
                </span>
                <code className="server-row__url">
                  {state.serverUrl || "http://127.0.0.1:4000"}
                </code>
              </div>
              {rolesLoading ? (
                <p className="dash-role-text">Loading…</p>
              ) : roleItems.length > 0 ? (
                <div className="role-grid">
                  {roleItems.map((r) => (
                    <RoleChip
                      key={r.role}
                      role={r.role}
                      model={r.model}
                      color={r.color}
                      icon={r.icon}
                    />
                  ))}
                </div>
              ) : (
                <p className="dash-role-text">
                  {state.serverOnline
                    ? "No roles assigned yet. Configure models in Settings."
                    : "Connect to server to see active roles."}
                </p>
              )}
            </div>
          </section>
        </div>

        <aside className="home2__aside">
          <WeatherWidget />
          <div className="gpanel gpanel--padded">
            <PanelHead title="Now Playing" />
            <NowPlaying variant="tile" />
          </div>
          <StickyNote />
          <TodoWidget />
        </aside>
      </div>

      <CategoryDock />
    </div>
  );
}
