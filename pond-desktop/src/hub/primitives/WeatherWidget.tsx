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

export function WeatherWidget({ variant = "card" }: WeatherWidgetProps) {
  const w = useHomeData().weather;

  return (
    <div className={`wx wx--${variant}`}>
      <div className="wx__main">
        <span className="wx__icon">
          <HubIco d={HP_PATHS.cloudSun} size={variant === "hero" ? 44 : 34} color="#fff" sw={1.7} />
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
