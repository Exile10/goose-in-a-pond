import { HubIco } from "./HubIco";
import { HP_PATHS } from "./icons";
import { sunEl } from "./HubIco";
import { useHomeData } from "../state/hubDataStore";

interface WeatherWidgetProps {
  variant?: "card" | "hero";
}

function getForecastIcon(iconKey: string): string | React.ReactNode {
  if (iconKey === "sun") return sunEl;
  const path = HP_PATHS[iconKey as keyof typeof HP_PATHS];
  return path ?? "";
}

type DayPhase = "night" | "dawn" | "day" | "dusk";

/** Parses "HH:MM" into minutes-past-midnight. Returns null if malformed. */
function parseClock(t: string): number | null {
  const m = /^(\d{1,2}):(\d{2})$/.exec(t);
  if (!m) return null;
  return Number(m[1]) * 60 + Number(m[2]);
}

/**
 * Derives the visual time-of-day phase from real sunrise/sunset times
 * (already location- and season-accurate from the backend) compared
 * against the device's current local clock.
 */
export function dayPhaseFor(sunrise: string, sunset: string, now = new Date()): DayPhase {
  const sr = parseClock(sunrise);
  const ss = parseClock(sunset);
  if (sr === null || ss === null) return "day";

  const cur = now.getHours() * 60 + now.getMinutes();
  const TWILIGHT_MIN = 45;

  if (Math.abs(cur - sr) <= TWILIGHT_MIN) return "dawn";
  if (Math.abs(cur - ss) <= TWILIGHT_MIN) return "dusk";
  if (cur > sr + TWILIGHT_MIN && cur < ss - TWILIGHT_MIN) return "day";
  return "night";
}

export function WeatherWidget({ variant = "card" }: WeatherWidgetProps) {
  const w = useHomeData().weather;
  const phase = dayPhaseFor(w.sunrise, w.sunset);

  return (
    <div className={`wx wx--${variant} wx--${phase}`}>
      <div className="wx__main">
        <span className="wx__icon">
          <HubIco
            d={getForecastIcon(w.icon) || HP_PATHS.cloudSun}
            size={variant === "hero" ? 44 : 34}
            color="#fff"
            sw={1.7}
          />
        </span>
        <div className="wx__temp">{w.temp}<span>°</span></div>
        <div className="wx__meta">
          <span className="wx__cond">{w.cond}</span>
          <span className="wx__range">H:{w.hi}° L:{w.lo}°</span>
        </div>
      </div>
      <div className="wx__fc">
        {w.forecast.map((f) => (
          <div key={f.d} className="wx__fday">
            <span>{f.d}</span>
            <HubIco d={getForecastIcon(f.i)} size={16} color="rgba(255,255,255,.92)" sw={1.7} />
            <strong>{f.t}°</strong>
          </div>
        ))}
      </div>
    </div>
  );
}
