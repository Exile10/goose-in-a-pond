import { useEffect, useState } from "react";

interface HubClockProps {
  big?: boolean;
}

function format(now: Date) {
  const time = now
    .toLocaleTimeString([], { hour: "numeric", minute: "2-digit", hour12: true })
    .replace(/\s?(AM|PM)$/i, "");
  const ampm = now.getHours() >= 12 ? "PM" : "AM";
  const date = now.toLocaleDateString(undefined, {
    weekday: "long",
    month: "long",
    day: "numeric",
  });
  return { time, ampm, date };
}

export function HubClock({ big = false }: HubClockProps) {
  const [now, setNow] = useState(() => new Date());

  useEffect(() => {
    const id = setInterval(() => setNow(new Date()), 30_000);
    return () => clearInterval(id);
  }, []);

  const { time, ampm, date } = format(now);

  return (
    <div className={`hclock${big ? " hclock--big" : ""}`}>
      <div className="hclock__time">
        {time}
        <span className="hclock__ampm">{ampm}</span>
      </div>
      <div className="hclock__date">{date}</div>
    </div>
  );
}
