import { HubIco } from "../primitives/HubIco";
import { HP_PATHS } from "../primitives/icons";
import { SETTINGS } from "../data/settingsConfig";
import type { SettingsRowId } from "../data/settingsConfig";

// ─── Chevron right ────────────────────────────────────────────
const CHEVR = HP_PATHS.chevR;

interface SettingsHubViewProps {
  go: (route: string) => void;
}

export function SettingsHubView({ go }: SettingsHubViewProps) {
  return (
    <div className="set">
      <header className="view-head">
        <div>
          <h1 className="view-title">Settings</h1>
          <p className="view-sub">
            Your home, your assistant, and the models that power it.
          </p>
        </div>
      </header>

      <div className="set__groups">
        {SETTINGS.map((g) => (
          <div key={g.group} className="set__group">
            <div className="set__glabel">{g.group}</div>
            <div className="set__list">
              {g.rows.map((r) => (
                <SettingsRow
                  key={r.id}
                  id={r.id}
                  iconPath={r.iconPath}
                  color={r.color}
                  bg={r.bg}
                  label={r.label}
                  sub={r.sub}
                  value={r.value}
                  badge={r.badge}
                  onClick={() => go(r.id)}
                />
              ))}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

// ─── Single settings row ──────────────────────────────────────
interface SettingsRowProps {
  id: SettingsRowId;
  iconPath: string;
  color: string;
  bg: string;
  label: string;
  sub: string;
  value?: string;
  badge?: string;
  onClick: () => void;
}

function SettingsRow({
  iconPath,
  color,
  bg,
  label,
  sub,
  value,
  badge,
  onClick,
}: SettingsRowProps) {
  function handleKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onClick();
    }
  }
  return (
    <div
      className="set-row"
      role="button"
      tabIndex={0}
      onClick={onClick}
      onKeyDown={handleKeyDown}
      aria-label={label}
    >
      <span className="set-row__icon" style={{ background: bg }}>
        <HubIco d={iconPath} size={18} color={color} />
      </span>
      <span className="set-row__text">
        <span className="set-row__label">{label}</span>
        <span className="set-row__sub">{sub}</span>
      </span>
      {badge && <span className="set-row__badge">{badge}</span>}
      {value && <span className="set-row__value">{value}</span>}
      <HubIco d={CHEVR} size={17} color="#C4C4CC" />
    </div>
  );
}
