import { registerMcpCard, type McpCardProps } from "../registry";

interface RouteOption {
  name: string;
  time: string;
  distance?: string;
  dist?: string;
  traffic?: string;
  best?: boolean;
}

/* ── Inline SVG icon ── */
function Ico({ d, size = 16, color = "currentColor" }: { d: string; size?: number; color?: string }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke={color}
         strokeWidth="1.75" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0 }}>
      <path d={d} />
    </svg>
  );
}

function trafficColor(t: string): string {
  const lower = (t || "").toLowerCase();
  if (lower === "light" || lower === "low") return "var(--color-success-fg)";
  if (lower === "clear" || lower === "free") return "var(--color-info-fg)";
  if (lower === "heavy" || lower === "congested") return "#DC2626";
  return "var(--color-warning-fg)"; // moderate / default
}

function MapCard({ data, variant }: McpCardProps) {
  const origin = String(data.origin ?? data.from ?? "Your location");
  const destination = String(data.destination ?? data.to ?? "Destination");
  const routes = (data.routes ?? []) as RouteOption[];
  const isCompact = variant === "compact";

  return (
    <div style={{ background: "#fff", borderRadius: 16, border: "1px solid #EBEBF0", overflow: "hidden", fontFamily: "'Quicksand', sans-serif", display: "flex", flexDirection: "column", height: "100%" }}>
      {/* SVG Map Visualization */}
      <div style={{ position: "relative", overflow: "hidden" }}>
        <svg viewBox="0 0 340 110" width="100%" height="110" preserveAspectRatio="none">
          <rect width="340" height="110" fill="#EEF2F7" />
          {/* Grid lines */}
          {[40, 80, 120, 160, 200, 240, 280, 320].map((x) => (
            <line key={`v${x}`} x1={x} y1="0" x2={x} y2="110" stroke="#DDE3EA" strokeWidth="0.8" />
          ))}
          {[22, 44, 66, 88].map((y) => (
            <line key={`h${y}`} x1="0" y1={y} x2="340" y2={y} stroke="#DDE3EA" strokeWidth="0.8" />
          ))}
          {/* Building blocks */}
          <rect x="150" y="40" width="40" height="30" rx="3" fill="#D1D8E0" opacity="0.6" />
          <rect x="230" y="55" width="30" height="20" rx="3" fill="#D1D8E0" opacity="0.5" />
          <rect x="80" y="60" width="25" height="18" rx="3" fill="#D1D8E0" opacity="0.4" />
          {/* Alt route (dashed) */}
          <path d="M28 94 Q 110 72 178 52 T 318 32" stroke="#B0B8C6" strokeWidth="2" fill="none" strokeDasharray="6 4" strokeLinecap="round" opacity="0.7" />
          {/* Primary route */}
          <path d="M28 94 Q 115 66 182 46 T 318 24" stroke="#7C3AED" strokeWidth="3.5" fill="none" strokeLinecap="round" />
          {/* Origin pin */}
          <circle cx="28" cy="94" r="6" fill="#7C3AED" stroke="white" strokeWidth="2.5" />
          {/* Destination pin */}
          <circle cx="318" cy="24" r="6" fill="#18181B" stroke="white" strokeWidth="2.5" />
          {/* Labels */}
          <text x="38" y="104" fontSize="8.5" fill="#7C3AED" fontWeight="800" fontFamily="sans-serif">{origin.slice(0, 20)}</text>
          <text x="265" y="19" fontSize="8.5" fill="#18181B" fontWeight="800" fontFamily="sans-serif">{destination.slice(0, 16)}</text>
        </svg>
      </div>

      {/* Divider */}
      <div style={{ height: 1, background: "#F1F5F9" }} />

      {/* Routes */}
      <div style={{ padding: "10px 14px", flex: 1, display: "flex", flexDirection: "column", gap: 6 }}>
        {routes.length === 0 ? (
          <div style={{ display: "flex", alignItems: "center", justifyContent: "center", padding: "16px 0", color: "var(--color-text-tertiary)", fontSize: 12 }}>
            No routes available
          </div>
        ) : (
          routes.slice(0, isCompact ? 1 : 3).map((r, i) => {
            const dist = r.distance ?? r.dist ?? "";
            return (
              <div key={i} style={{
                display: "flex", alignItems: "center", padding: "9px 12px", borderRadius: 11,
                background: r.best ? "#F5F3FF" : "#FAFAFA",
                border: `1px solid ${r.best ? "#DDD6FE" : "#F1F5F9"}`,
              }}>
                <Ico d="M3 11l19-9-9 19-2-8-8-2z" size={14} color={r.best ? "#7C3AED" : "var(--color-text-tertiary)"} />
                <div style={{ flex: 1, marginLeft: 10 }}>
                  <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
                    <span style={{ fontSize: 13, fontWeight: 700, color: "#18181B" }}>{r.name}</span>
                    {r.best && (
                      <span style={{ fontSize: 10, fontWeight: 700, background: "#7C3AED", color: "#fff", padding: "1px 7px", borderRadius: 999 }}>Fastest</span>
                    )}
                  </div>
                  {/* The purple "best route" background (#F5F3FF) drops the general
                      tertiary-text token just under 4.5:1 — darken it for that row. */}
                  <div style={{ fontSize: 11, color: r.best ? "#4B5570" : "var(--color-text-tertiary)", marginTop: 2, display: "flex", gap: 8, fontWeight: 500 }}>
                    {dist && <span>{dist}</span>}
                    {r.traffic && (
                      <span style={{ color: trafficColor(r.traffic), fontWeight: 600 }}>
                        <span style={{ fontSize: 8 }}>{"\u25CF"}</span> {r.traffic} traffic
                      </span>
                    )}
                  </div>
                </div>
                <span style={{ fontSize: 15, fontWeight: 800, color: r.best ? "#7C3AED" : "#475569" }}>{r.time}</span>
              </div>
            );
          })
        )}
      </div>

      {/* Action buttons */}
      {!isCompact && routes.length > 0 && (
        <div style={{ padding: "0 14px 14px", display: "flex", gap: 8 }}>
          <button style={{ flex: 1, padding: 9, borderRadius: 10, border: "1px solid #E4E4E7", background: "#fff", fontSize: 12, fontWeight: 700, color: "#475569", cursor: "pointer", fontFamily: "inherit" }}>
            Open in Maps
          </button>
          <button style={{
            flex: 1, padding: 9, borderRadius: 10, border: "none", background: "#7C3AED",
            fontSize: 12, fontWeight: 700, color: "#fff", cursor: "pointer", fontFamily: "inherit",
            display: "flex", alignItems: "center", justifyContent: "center", gap: 6,
          }}>
            Navigate <Ico d="M5 12h14M12 5l7 7-7 7" size={13} color="#fff" />
          </button>
        </div>
      )}
    </div>
  );
}

registerMcpCard({
  key: "map",
  label: "Maps",
  icon: "Navigation",
  toolPattern: /map|navigate|direction|route/,
  component: MapCard,
  mockTool: "giap-maps__navigate",
  mockData: {
    origin: "Your location",
    destination: "Office",
    routes: [
      { name: "Mombasa Rd", time: "28 min", distance: "14.2 km", traffic: "Light", best: true },
      { name: "Ngong Rd", time: "34 min", distance: "12.8 km", traffic: "Moderate" },
      { name: "Southern Bypass", time: "41 min", distance: "18.5 km", traffic: "Clear" },
    ],
  },
});
