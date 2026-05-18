import { registerMcpCard, type McpCardProps } from "../registry";

interface CalendarEvent {
  title: string;
  time: string;
  duration?: string;
  dur?: string;
  location?: string;
  loc?: string;
  color?: string;
}

/* ── Inline SVG icons ── */
function Ico({ d, size = 16, color = "currentColor" }: { d: string; size?: number; color?: string }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke={color}
         strokeWidth="1.75" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0 }}>
      <path d={d} />
    </svg>
  );
}

const COLORS = ["#7C3AED", "#0072F5", "#16A34A", "#F59E0B", "#EF4444", "#0EA5E9"];

function CalendarCard({ data, variant }: McpCardProps) {
  const date = String(data.date ?? "Today");
  const events = (data.events ?? []) as CalendarEvent[];
  const nextIn = data.next_in as string | undefined;
  const isCompact = variant === "compact";
  const maxEvents = isCompact ? 2 : 6;

  return (
    <div style={{ background: "#fff", borderRadius: 16, border: "1px solid #EBEBF0", overflow: "hidden", fontFamily: "'Quicksand', sans-serif", display: "flex", flexDirection: "column", height: "100%" }}>
      {/* Header */}
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", padding: "14px 16px 10px", gap: 8 }}>
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <Ico d="M8 2v3M16 2v3M3 8h18M3 6a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" size={15} color="#7C3AED" />
          <span style={{ fontSize: 13, fontWeight: 700, color: "#18181B" }}>{date}</span>
          {events.length > 0 && (
            <span style={{ fontSize: 11, fontWeight: 600, padding: "3px 9px", borderRadius: 999, background: "#EDE9FE", color: "#6D28D9" }}>
              {events.length} event{events.length !== 1 ? "s" : ""}
            </span>
          )}
        </div>
        {nextIn && (
          <span style={{ display: "flex", alignItems: "center", gap: 5, fontSize: 11, fontWeight: 600, color: "#7C3AED", background: "#EDE9FE", padding: "3px 9px", borderRadius: 999 }}>
            <Ico d="M12 7v5l3 2M12 2a9 9 0 1 0 0 18A9 9 0 0 0 12 2z" size={11} color="#7C3AED" />
            next in {nextIn}
          </span>
        )}
      </div>

      {/* Divider */}
      <div style={{ height: 1, background: "#F1F5F9" }} />

      {/* Events */}
      <div style={{ padding: "10px 16px 14px", display: "flex", flexDirection: "column", gap: 8, flex: 1 }}>
        {events.length === 0 ? (
          <div style={{ display: "flex", alignItems: "center", justifyContent: "center", padding: "24px 0", color: "#94A3B8", fontSize: 12 }}>
            No events scheduled
          </div>
        ) : (
          events.slice(0, maxEvents).map((ev, i) => (
            <div key={i} style={{ display: "flex", gap: 11, alignItems: "stretch" }}>
              <div style={{ width: 3, borderRadius: 3, background: ev.color ?? COLORS[i % COLORS.length], flexShrink: 0, minHeight: 48 }} />
              <div style={{ flex: 1, background: "#FAFAFA", borderRadius: 10, padding: "9px 12px", border: "1px solid #F1F5F9" }}>
                <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 3 }}>
                  <span style={{ fontSize: 11, fontWeight: 700, color: "#64748B" }}>{ev.time}</span>
                  {(ev.duration ?? ev.dur) && (
                    <span style={{ fontSize: 10, background: "#F1F5F9", color: "#94A3B8", padding: "1px 7px", borderRadius: 6, fontWeight: 600 }}>
                      {ev.duration ?? ev.dur}
                    </span>
                  )}
                </div>
                <div style={{ fontSize: 13, fontWeight: 700, color: "#18181B" }}>{ev.title}</div>
                {(ev.location ?? ev.loc) && (
                  <div style={{ fontSize: 11, color: "#94A3B8", marginTop: 2, display: "flex", alignItems: "center", gap: 4, fontWeight: 500 }}>
                    <Ico d="M21 10c0 7-9 13-9 13s-9-6-9-13a9 9 0 0 1 18 0z" size={10} color="#94A3B8" />
                    {ev.location ?? ev.loc}
                  </div>
                )}
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  );
}

registerMcpCard({
  key: "calendar",
  label: "Calendar",
  icon: "Calendar",
  toolPattern: "calendar",
  component: CalendarCard,
  mockTool: "giap-calendar__get_events",
  mockData: {
    date: "Today, May 16",
    next_in: "23 min",
    events: [
      { title: "Standup", time: "9:00 AM", duration: "15m", location: "Zoom", color: "#7C3AED" },
      { title: "Design Review", time: "11:00 AM", duration: "45m", location: "Room 3B", color: "#0072F5" },
      { title: "Lunch w/ Alex", time: "12:30 PM", duration: "1h", location: "Cafe", color: "#16A34A" },
      { title: "Sprint Planning", time: "3:00 PM", duration: "1h", color: "#F59E0B" },
    ],
  },
});
