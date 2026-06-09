import { WeatherWidget } from "../../primitives/WeatherWidget";
import { DeviceTile } from "../../primitives/DeviceTile";
import { HubIco, filmEl } from "../../primitives/HubIco";
import { useHomeData } from "../../state/hubDataStore";

export type CardKind = "weather" | "lock" | "movie";

interface ResultCardProps {
  kind: CardKind;
}

export function ResultCard({ kind }: ResultCardProps) {
  const home = useHomeData();
  if (kind === "weather") {
    return (
      <div style={{ maxWidth: 320 }}>
        <WeatherWidget />
      </div>
    );
  }

  if (kind === "lock") {
    const lock = home.devices.find((d) => d.kind === "lock");
    if (!lock) return null;
    return (
      <div style={{ width: 160 }}>
        <DeviceTile device={lock} />
      </div>
    );
  }

  if (kind === "movie") {
    return (
      <div className="ch-scene-result">
        <div className="ch-scene-result__row">
          <HubIco d={filmEl} size={15} color="#7C3AED" />
          Movie Time active
        </div>
        <div className="ch-scene-result__items">
          <span>Lights 20%</span>
          <span>Blinds closed</span>
          <span>TV on</span>
        </div>
      </div>
    );
  }

  return null;
}
