import { useState } from "react";
import { HubIco } from "../primitives/HubIco";
import { HP_PATHS } from "../primitives/icons";
import { DeviceTile } from "../primitives/DeviceTile";
import { CameraFeed } from "../primitives/CameraFeed";
import { RoomPills } from "../primitives/RoomPills";
import { CategoryDock } from "../primitives/CategoryDock";
import { WeatherWidget } from "../primitives/WeatherWidget";
import { StickyNote } from "../primitives/StickyNote";
import { TodoWidget } from "../primitives/TodoWidget";
import { Scenes } from "../primitives/Scenes";
import { NowPlaying } from "../primitives/NowPlaying";
import { HubClock } from "../primitives/HubClock";
import { PanelHead } from "../primitives/PanelHead";
import { AskGoose } from "../primitives/AskGoose";
import { useHomeData } from "../state/hubDataStore";

// Extra icons used only in the Home view header
const HX = {
  chat: "M21 12a8 8 0 0 1-11.5 7.2L4 21l1.8-5.4A8 8 0 1 1 21 12z",
  bell: "M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9M10.3 21a1.94 1.94 0 0 0 3.4 0",
};

interface HomeViewProps {
  go?: (route: string) => void;
}

function greetingForHour(h: number): string {
  if (h < 5)  return "Good night";
  if (h < 12) return "Good morning";
  if (h < 18) return "Good afternoon";
  return "Good evening";
}

export function HomeView({ go }: HomeViewProps) {
  const [room, setRoom] = useState("home");
  const home = useHomeData();

  const roomName = home.rooms.find((r) => r.id === room)?.name ?? "Home";
  const favorites =
    room === "home"
      ? home.devices.slice(0, 6)
      : home.devices.filter((d) => d.room === roomName);

  const greeting = greetingForHour(new Date().getHours());

  return (
    <div className="home2">
      {/* header */}
      <header className="home2__head">
        <div>
          <div className="hub-greet">
            {greeting}, <span>{home.user}</span>
          </div>
          <div className="home2__sub">
            {home.date} · {home.weather.cond}, {home.weather.temp}°
          </div>
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 18 }}>
          <button
            className="home2__iconbtn"
            onClick={() => go?.("chat")}
            aria-label="Open chat"
          >
            <HubIco d={HX.chat} size={20} color="var(--pp)" />
          </button>
          <button
            className="home2__iconbtn"
            aria-label="Notifications"
            onClick={() => go?.("notifications")}
          >
            <HubIco d={HX.bell} size={20} color="var(--pp)" />
          </button>
          <HubClock />
        </div>
      </header>

      {/* ask goose */}
      <AskGoose go={go} />

      {/* room pills */}
      <RoomPills value={room} onChange={setRoom} />

      {/* main grid: content + ambient sidebar */}
      <div className="home2__grid">
        <div className="home2__main">
          {/* favorites */}
          <section className="gpanel">
            <PanelHead
              title={room === "home" ? "Favorites" : roomName}
              action={
                <button className="ghost-btn" onClick={() => go?.("rooms")}>
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

          {/* cameras */}
          <section className="gpanel">
            <PanelHead
              title="Cameras"
              action={
                <button className="ghost-btn" onClick={() => go?.("cameras")}>
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

          {/* routines */}
          <section>
            <div className="home2__sectlabel">Routines</div>
            <Scenes layout="row" />
          </section>
        </div>

        {/* ambient sidebar */}
        <aside className="home2__aside">
          <WeatherWidget />
          <div className="gpanel" style={{ padding: 13 }}>
            <PanelHead title="Now Playing" />
            <NowPlaying variant="tile" />
          </div>
          <StickyNote />
          <TodoWidget />
        </aside>
      </div>

      {/* category dock */}
      <CategoryDock />
    </div>
  );
}
