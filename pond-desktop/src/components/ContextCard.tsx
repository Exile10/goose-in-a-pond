import type { ContextCard as ContextCardType } from "../state/reducer";

interface Props {
  card: ContextCardType;
}

export function ContextCard({ card }: Props) {
  const { tool, data } = card;
  const toolName = tool.includes("__") ? tool.split("__")[1] : tool;

  return (
    <div style={styles.card} role="article" aria-label={`Tool result: ${toolName}`}>
      <div style={styles.header}>
        <span style={styles.toolName}>{formatToolName(toolName)}</span>
      </div>
      <div style={styles.body}>
        <ToolContent toolName={toolName} data={data} />
      </div>
    </div>
  );
}

function ToolContent({ toolName, data }: { toolName: string; data: Record<string, unknown> }) {
  if (toolName === "get_current_weather") return <WeatherContent data={data} />;
  if (toolName === "list_registered_devices") return <DevicesContent data={data} />;
  if (toolName === "recall_memories" || toolName === "save_memory") return <MemoryContent data={data} />;
  if (toolName === "list_schedules") return <SchedulesContent data={data} />;
  return <GenericContent data={data} />;
}

function WeatherContent({ data }: { data: Record<string, unknown> }) {
  const temp = data.temperature ?? data.temp;
  const desc = data.description ?? data.condition ?? data.weather;
  const location = data.location ?? data.city;
  return (
    <div style={styles.weatherRow}>
      <span style={styles.weatherTemp}>{temp !== undefined ? `${temp}°` : "—"}</span>
      <div>
        {desc != null && <p style={styles.value}>{String(desc)}</p>}
        {location != null && <p style={styles.hint}>{String(location)}</p>}
      </div>
    </div>
  );
}

function DevicesContent({ data }: { data: Record<string, unknown> }) {
  const devices = Array.isArray(data.devices) ? data.devices : Array.isArray(data) ? data : [];
  if (devices.length === 0) return <p style={styles.hint}>No devices found.</p>;
  return (
    <ul style={styles.list}>
      {devices.slice(0, 5).map((d: unknown, i) => {
        const dev = d as Record<string, unknown>;
        return (
          <li key={i} style={styles.listItem}>
            <span style={{ ...styles.dot, background: dev.is_online ? "#34C759" : "#8E8E93" }} />
            <span style={styles.value}>{String(dev.name ?? "Device")}</span>
            {dev.room != null && <span style={styles.hint}>{String(dev.room)}</span>}
          </li>
        );
      })}
    </ul>
  );
}

function MemoryContent({ data }: { data: Record<string, unknown> }) {
  const content = data.content ?? data.text ?? data.memory;
  return content ? (
    <p style={{ ...styles.value, userSelect: "text" as const }}>{String(content)}</p>
  ) : (
    <GenericContent data={data} />
  );
}

function SchedulesContent({ data }: { data: Record<string, unknown> }) {
  const schedules = Array.isArray(data.schedules) ? data.schedules : [];
  if (schedules.length === 0) return <p style={styles.hint}>No schedules.</p>;
  return (
    <ul style={styles.list}>
      {schedules.slice(0, 4).map((s: unknown, i) => {
        const sched = s as Record<string, unknown>;
        return (
          <li key={i} style={styles.listItem}>
            <span style={styles.value}>{String(sched.name ?? "Schedule")}</span>
            <span style={styles.hint}>{String(sched.cron ?? "")}</span>
          </li>
        );
      })}
    </ul>
  );
}

function GenericContent({ data }: { data: Record<string, unknown> }) {
  const text = typeof data === "string"
    ? data
    : JSON.stringify(data, null, 2);
  return (
    <pre style={styles.pre}>{text}</pre>
  );
}

function formatToolName(name: string): string {
  return name
    .replace(/_/g, " ")
    .replace(/\b\w/g, (c) => c.toUpperCase());
}

const styles: Record<string, React.CSSProperties> = {
  card: {
    background: "#FFFFFF",
    border: "1px solid rgba(23,22,22,0.10)",
    borderRadius: "10px",
    overflow: "hidden",
    fontSize: "12px",
  },
  header: {
    padding: "6px 10px",
    background: "rgba(140,82,255,0.06)",
    borderBottom: "1px solid rgba(23,22,22,0.07)",
  },
  toolName: {
    fontFamily: '"Quicksand", sans-serif',
    fontWeight: 600,
    fontSize: "10px",
    color: "#8C52FF",
    textTransform: "uppercase",
    letterSpacing: "0.05em",
  },
  body: {
    padding: "8px 10px",
  },
  weatherRow: {
    display: "flex",
    alignItems: "center",
    gap: "10px",
  },
  weatherTemp: {
    fontFamily: '"Quicksand", sans-serif',
    fontWeight: 700,
    fontSize: "22px",
    color: "#171616",
    lineHeight: "1",
  },
  list: {
    listStyle: "none",
    margin: 0,
    padding: 0,
    display: "flex",
    flexDirection: "column",
    gap: "4px",
  },
  listItem: {
    display: "flex",
    alignItems: "center",
    gap: "6px",
  },
  dot: {
    width: "6px",
    height: "6px",
    borderRadius: "50%",
    flexShrink: 0,
  },
  value: {
    margin: 0,
    color: "#171616",
    fontSize: "12px",
  },
  hint: {
    margin: 0,
    color: "rgba(23,22,22,0.45)",
    fontSize: "11px",
  },
  pre: {
    margin: 0,
    fontSize: "10px",
    fontFamily: '"JetBrains Mono", Menlo, monospace',
    color: "rgba(23,22,22,0.65)",
    whiteSpace: "pre-wrap",
    wordBreak: "break-all",
    userSelect: "text",
    maxHeight: "120px",
    overflowY: "auto",
  },
};
