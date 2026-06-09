import { useState } from "react";
import { HubIco } from "./HubIco";
import { HP_PATHS } from "./icons";
import { useHomeData } from "../state/hubDataStore";

interface RoomPillsProps {
  value?: string;
  onChange?: (id: string) => void;
}

export function RoomPills({ value, onChange }: RoomPillsProps) {
  const [internal, setInternal] = useState("home");
  const active = value ?? internal;
  const { rooms } = useHomeData();

  function pick(id: string) {
    setInternal(id);
    onChange?.(id);
  }

  return (
    <div className="rpills">
      {rooms.map((r) => {
        const isActive = active === r.id;
        const iconPath = HP_PATHS[r.icon as keyof typeof HP_PATHS];
        return (
          <button
            key={r.id}
            className="rpill"
            data-active={isActive}
            onClick={() => pick(r.id)}
          >
            {iconPath && (
              <HubIco
                d={iconPath}
                size={15}
                color={isActive ? "#fff" : "var(--pp)"}
              />
            )}
            <span>{r.name}</span>
          </button>
        );
      })}
    </div>
  );
}
