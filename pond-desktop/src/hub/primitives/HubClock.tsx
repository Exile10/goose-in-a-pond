import { HOME } from "../data/mockHome";

interface HubClockProps {
  big?: boolean;
}

export function HubClock({ big = false }: HubClockProps) {
  return (
    <div className={`hclock${big ? " hclock--big" : ""}`}>
      <div className="hclock__time">
        {HOME.time}
        <span className="hclock__ampm">{HOME.ampm}</span>
      </div>
      <div className="hclock__date">{HOME.date}</div>
    </div>
  );
}
