import { formatHubDate, useNow } from "../state/useNow";

interface HubClockProps {
  big?: boolean;
}

function formatTime(now: Date): { time: string; ampm: string } {
  const time = now
    .toLocaleTimeString([], { hour: "numeric", minute: "2-digit", hour12: true })
    .replace(/\s?(AM|PM)$/i, "");
  const ampm = now.getHours() >= 12 ? "PM" : "AM";
  return { time, ampm };
}

export function HubClock({ big = false }: HubClockProps) {
  const now = useNow();
  const { time, ampm } = formatTime(now);

  return (
    <div className={`hclock${big ? " hclock--big" : ""}`}>
      <div className="hclock__time">
        {time}
        <span className="hclock__ampm">{ampm}</span>
      </div>
      <div className="hclock__date">{formatHubDate(now)}</div>
    </div>
  );
}
