import { HubIco } from "./HubIco";
import { HP_PATHS } from "./icons";
import { useHomeData } from "../state/hubDataStore";

interface CategoryDockProps {
  scroll?: boolean;
}

export function CategoryDock({ scroll = false }: CategoryDockProps) {
  const { categories } = useHomeData();
  return (
    <div className={`cdock${scroll ? " cdock--scroll" : ""}`}>
      {categories.map((c) => {
        const iconPath = HP_PATHS[c.icon as keyof typeof HP_PATHS];
        return (
          <button
            key={c.id}
            className="cdock__pill"
            onClick={() => window.dispatchEvent(new CustomEvent("hub:category", { detail: c.id }))}
          >
            <span
              className="cdock__icon"
              style={{ background: c.bg, color: c.color }}
            >
              {iconPath && <HubIco d={iconPath} size={16} color={c.color} />}
            </span>
            <span className="cdock__text">
              <span className="cdock__label">{c.label}</span>
              <span className="cdock__status">{c.status}</span>
            </span>
          </button>
        );
      })}
    </div>
  );
}
