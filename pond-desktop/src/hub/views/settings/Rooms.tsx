import { Plus } from "lucide-react";
import { HubIco } from "../../primitives/HubIco";
import { HP_PATHS } from "../../primitives/icons";
import { HOME } from "../../data/mockHome";
import { DetailShell } from "./DetailShell";
import { Card, Row, Toggle } from "./controls";
import type { DeviceData } from "../../data/mockHome";

// ─── Build room → devices map ─────────────────────────────────
function buildByRoom(): Record<string, DeviceData[]> {
  const byRoom: Record<string, DeviceData[]> = {};
  HOME.rooms
    .filter((r) => r.id !== "home")
    .forEach((r) => { byRoom[r.name] = []; });
  HOME.devices.forEach((d) => {
    if (byRoom[d.room]) byRoom[d.room].push(d);
  });
  return byRoom;
}

function deviceLabel(d: DeviceData): string {
  if (d.kind === "thermo") return `${d.value ?? "--"}° · heat to ${d.target ?? "--"}°`;
  if (d.kind === "lock") return d.locked ? "Locked" : "Unlocked";
  return d.on ? "On" : "Off";
}

interface RoomsDetailProps {
  go: (route: string) => void;
}

export function RoomsDetail({ go }: RoomsDetailProps) {
  const byRoom = buildByRoom();
  const rooms = HOME.rooms.filter((r) => r.id !== "home");

  return (
    <DetailShell
      title="Rooms & Devices"
      subtitle="6 rooms · 18 devices connected."
      accent="#7C3AED"
      onBack={() => go("settings")}
      headRight={
        <button
          className="primary-btn"
          type="button"
          onClick={() => { /* Phase 8: open add-device wizard */ }}
        >
          <Plus size={15} color="#fff" strokeWidth={2.2} /> Add device
        </button>
      }
    >
      {rooms.map((r) => {
        const devs = byRoom[r.name] ?? [];
        const iconPath = HP_PATHS[r.icon as keyof typeof HP_PATHS];
        return (
          <Card
            key={r.id}
            title={
              <span style={{ display: "inline-flex", alignItems: "center", gap: 8 }}>
                {iconPath && <HubIco d={iconPath} size={16} color="#7C3AED" />}
                {r.name}
                <span className="room-count">
                  {devs.length} device{devs.length !== 1 ? "s" : ""}
                </span>
              </span>
            }
            right={<button className="mrow__btn" type="button" onClick={() => { /* Phase 8: open room editor */ }}>Edit room</button>}
          >
            {devs.length > 0 ? (
              <div className="devlist">
                {devs.map((d) => (
                  <Row
                    key={d.id}
                    label={d.name}
                    sub={`${d.kind} · ${deviceLabel(d)}`}
                    control={<Toggle on={d.on ?? d.locked ?? true} />}
                  />
                ))}
              </div>
            ) : (
              <div className="memempty">No devices in this room yet.</div>
            )}
          </Card>
        );
      })}
    </DetailShell>
  );
}
