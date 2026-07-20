import { Logo } from "../components/Logo";
import { HubIco } from "./primitives/HubIco";
import { HP_PATHS } from "./primitives/icons";
import { BellShortcut } from "./primitives/BellShortcut";
import "./views/notifications.css";

type HubRoute = "home" | "chat" | "canvas" | "routines" | "settings" | "notifications";

interface NavItem {
  id: HubRoute;
  label: string;
  iconKey: keyof typeof HP_PATHS;
}

const NAV: NavItem[] = [
  { id: "home",     label: "Home",     iconKey: "railHome" },
  { id: "chat",     label: "Goose",    iconKey: "railChat" },
  { id: "canvas",   label: "Canvas",   iconKey: "railCanvas" },
  { id: "routines", label: "Routines", iconKey: "railRoutines" },
  { id: "settings", label: "Settings", iconKey: "railSettings" },
];

interface IconRailProps {
  active: HubRoute;
  go: (route: string) => void;
}

export function IconRail({ active, go }: IconRailProps) {
  return (
    <nav className="irail">
      <div className="irail__brand">
        <span className="irail__logo">
          <Logo size={32} />
        </span>
      </div>
      <div className="irail__nav">
        {NAV.map((n) => {
          const isActive = active === n.id;
          return (
            <button
              key={n.id}
              className="irail__item"
              data-active={isActive}
              onClick={() => go(n.id)}
              aria-label={n.label}
              aria-current={isActive ? "page" : undefined}
            >
              <span className="irail__icobox">
                <HubIco
                  d={HP_PATHS[n.iconKey]}
                  size={22}
                  color={isActive ? "var(--pp)" : "var(--color-text-tertiary)"}
                />
              </span>
              <span className="irail__label">{n.label}</span>
            </button>
          );
        })}
      </div>
      <div className="irail__foot">
        <BellShortcut
          count={3}
          active={active === "notifications"}
          onClick={() => go("notifications")}
        />
        <button className="irail__avatar" aria-label="User profile">J</button>
      </div>
    </nav>
  );
}
