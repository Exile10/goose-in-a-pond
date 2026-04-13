import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

interface ToolResult {
  tool: string;
  data: Record<string, unknown>;
  timestamp_ms: number;
}

interface ContextCard {
  id: number;
  tool: string;
  data: Record<string, unknown>;
  timestamp_ms: number;
}

export default function ContextCards() {
  const [cards, setCards] = useState<ContextCard[]>([]);
  let idCounter = 0;

  useEffect(() => {
    const unlisten = listen<ToolResult>("tool-result", (event) => {
      const card: ContextCard = {
        id: ++idCounter,
        ...event.payload,
      };
      // Keep at most 5 cards visible
      setCards((prev) => [...prev.slice(-4), card]);
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  if (cards.length === 0) return null;

  return (
    <div className="context-cards">
      {cards.map((card) => (
        <ContextCardItem key={card.id} card={card} />
      ))}
    </div>
  );
}

function ContextCardItem({ card }: { card: ContextCard }) {
  const timeLabel = formatRelativeTime(card.timestamp_ms);

  switch (card.tool) {
    case "giap__get_current_weather":
      return <WeatherCard data={card.data} time={timeLabel} />;
    case "giap__list_registered_devices":
      return <DeviceCard data={card.data} time={timeLabel} />;
    case "giap__recall_memories":
    case "giap__save_memory":
      return <MemoryCard data={card.data} tool={card.tool} time={timeLabel} />;
    case "giap__list_schedules":
      return <ScheduleCard data={card.data} time={timeLabel} />;
    default:
      return <GenericCard tool={card.tool} data={card.data} time={timeLabel} />;
  }
}

function WeatherCard({ data, time }: { data: Record<string, unknown>; time: string }) {
  const temp = data.temperature ?? data.temp ?? data.current_temperature ?? "–";
  const desc = (data.description ?? data.condition ?? data.weather_description ?? "") as string;
  const unit = (data.unit ?? "°C") as string;
  const location = (data.location ?? data.location_name ?? "") as string;

  return (
    <div className="context-card">
      <div className="context-card-header">
        <span className="context-card-icon">🌤</span>
        <span className="context-card-title">Weather</span>
        {location && <span className="context-card-title" style={{ opacity: 0.6 }}> · {location}</span>}
        <span className="context-card-time">{time}</span>
      </div>
      <div className="context-card-body">
        <div className="weather-card-temp">{String(temp)}{unit}</div>
        {desc && <div className="weather-card-desc">{desc}</div>}
      </div>
    </div>
  );
}

function DeviceCard({ data, time }: { data: Record<string, unknown>; time: string }) {
  const devices = Array.isArray(data.devices) ? data.devices as Record<string, unknown>[] : [];
  const count = (data.count ?? devices.length) as number;

  return (
    <div className="context-card">
      <div className="context-card-header">
        <span className="context-card-icon">🖥</span>
        <span className="context-card-title">Devices ({count})</span>
        <span className="context-card-time">{time}</span>
      </div>
      <div className="context-card-body">
        {devices.slice(0, 4).map((d, i) => (
          <div key={i} style={{ fontSize: "0.77rem", opacity: 0.85 }}>
            {(d.name ?? d.device_name ?? d.id) as string}
            {d.online !== undefined && (
              <span style={{ color: d.online ? "#34d399" : "#f87171", marginLeft: 6 }}>
                {d.online ? "●" : "○"}
              </span>
            )}
          </div>
        ))}
        {devices.length > 4 && (
          <div style={{ fontSize: "0.72rem", opacity: 0.45 }}>+{devices.length - 4} more</div>
        )}
      </div>
    </div>
  );
}

function MemoryCard({
  data,
  tool,
  time,
}: {
  data: Record<string, unknown>;
  tool: string;
  time: string;
}) {
  const isSave = tool === "giap__save_memory";
  const memories = Array.isArray(data.memories)
    ? (data.memories as Record<string, unknown>[])
    : [];
  const content = (data.content ?? data.text ?? "") as string;

  return (
    <div className="context-card">
      <div className="context-card-header">
        <span className="context-card-icon">{isSave ? "💾" : "🧠"}</span>
        <span className="context-card-title">{isSave ? "Memory Saved" : "Memories"}</span>
        <span className="context-card-time">{time}</span>
      </div>
      <div className="context-card-body">
        {isSave ? (
          <div style={{ fontSize: "0.78rem" }}>{content || "Memory persisted"}</div>
        ) : (
          memories.slice(0, 3).map((m, i) => (
            <div key={i} style={{ fontSize: "0.77rem", opacity: 0.85, marginBottom: 3 }}>
              {(m.content ?? m.text ?? m.fragment) as string}
            </div>
          ))
        )}
      </div>
    </div>
  );
}

function ScheduleCard({ data, time }: { data: Record<string, unknown>; time: string }) {
  const schedules = Array.isArray(data.schedules)
    ? (data.schedules as Record<string, unknown>[])
    : [];

  return (
    <div className="context-card">
      <div className="context-card-header">
        <span className="context-card-icon">📅</span>
        <span className="context-card-title">Schedules ({schedules.length})</span>
        <span className="context-card-time">{time}</span>
      </div>
      <div className="context-card-body">
        {schedules.slice(0, 3).map((s, i) => (
          <div key={i} style={{ fontSize: "0.77rem", opacity: 0.85 }}>
            {(s.name ?? s.id) as string}
            {s.cron && <span style={{ opacity: 0.5, marginLeft: 6, fontSize: "0.7rem" }}>{s.cron as string}</span>}
          </div>
        ))}
      </div>
    </div>
  );
}

function GenericCard({
  tool,
  data,
  time,
}: {
  tool: string;
  data: Record<string, unknown>;
  time: string;
}) {
  // Strip the giap__ prefix for display
  const displayName = tool.replace("giap__", "").replace(/_/g, " ");
  const preview = JSON.stringify(data, null, 0).slice(0, 200);

  return (
    <div className="context-card">
      <div className="context-card-header">
        <span className="context-card-icon">⚡</span>
        <span className="context-card-title">{displayName}</span>
        <span className="context-card-time">{time}</span>
      </div>
      <div className="context-card-body" style={{ fontSize: "0.72rem", opacity: 0.7, fontFamily: "monospace", wordBreak: "break-all" }}>
        {preview}
      </div>
    </div>
  );
}

function formatRelativeTime(ts_ms: number): string {
  const delta = Date.now() - ts_ms;
  if (delta < 60_000) return "just now";
  if (delta < 3_600_000) return `${Math.floor(delta / 60_000)}m ago`;
  return `${Math.floor(delta / 3_600_000)}h ago`;
}
