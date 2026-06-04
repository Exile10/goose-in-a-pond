import { useState } from "react";
import { Plus } from "lucide-react";
import { HubIco } from "../../primitives/HubIco";
import { DetailShell } from "./DetailShell";
import { Card, Toggle } from "./controls";

const WRENCH_PATH =
  "M14.7 6.3a4 4 0 0 1-5.4 5.4L4 17l3 3 5.3-5.3a4 4 0 0 0 5.4-5.4l-2.5 2.5-2.7-.3-.3-2.7z";
const CHEVD_PATH = "M6 9l6 6 6-6";
const STORE_PATH =
  "M3 9l1-5h16l1 5M4 9v11a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1V9M4 9h16M9 21v-6h6v6";

type McpKind = "stdio" | "http";
type McpStatus = "ok" | "off";

interface McpExtension {
  id: string;
  name: string;
  desc: string;
  kind: McpKind;
  enabled: boolean;
  status: McpStatus;
  tools: string[];
}

const MCP: McpExtension[] = [
  { id: "weather",  name: "giap-weather",        desc: "Local & forecast weather",           kind: "stdio", enabled: true,  status: "ok",  tools: ["get_current_weather", "get_forecast"] },
  { id: "home",     name: "giap-homeassistant",   desc: "Lights, locks, climate & sensors",   kind: "http",  enabled: true,  status: "ok",  tools: ["get_status", "set_light", "lock_door", "set_thermostat"] },
  { id: "calendar", name: "giap-calendar",        desc: "Your day & upcoming events",          kind: "stdio", enabled: true,  status: "ok",  tools: ["get_events", "create_event"] },
  { id: "maps",     name: "giap-maps",            desc: "Navigation & places",                 kind: "http",  enabled: true,  status: "ok",  tools: ["navigate", "search_places"] },
  { id: "news",     name: "giap-news",            desc: "Daily headlines",                     kind: "stdio", enabled: false, status: "off", tools: ["get_headlines"] },
  { id: "finance",  name: "giap-finance",         desc: "Crypto & market prices",              kind: "http",  enabled: true,  status: "ok",  tools: ["get_crypto_prices", "get_stock"] },
  { id: "fs",       name: "filesystem",           desc: "Read & write local files",            kind: "stdio", enabled: true,  status: "ok",  tools: ["read_file", "write_file", "list_dir"] },
];

function McpRow({ ext }: { ext: McpExtension }) {
  const [open, setOpen] = useState(false);
  return (
    <div className={`ext2${ext.enabled ? "" : " ext2--off"}`}>
      <div className="ext2__head">
        <span className={`ext2__status ext2__status--${ext.status}`} />
        <span className="ext2__icon">
          <HubIco d={WRENCH_PATH} size={15} color="#7C3AED" />
        </span>
        <div className="ext2__info">
          <div className="ext2__namerow">
            <span className="ext2__name">{ext.name}</span>
            <span className={`ext2__kind ext2__kind--${ext.kind}`}>{ext.kind}</span>
            <span className="ext2__count">{ext.tools.length} tools</span>
          </div>
          <span className="ext2__desc">{ext.desc}</span>
        </div>
        <button
          className="ext2__expand"
          type="button"
          onClick={() => setOpen((o) => !o)}
          aria-label={open ? "Collapse tools" : "Expand tools"}
          style={{ transform: open ? "rotate(180deg)" : "none" }}
        >
          <HubIco d={CHEVD_PATH} size={16} color="#94A3B8" />
        </button>
        <Toggle on={ext.enabled} />
      </div>
      {open && (
        <div className="ext2__tools">
          {ext.tools.map((t) => (
            <code key={t} className="ext2__tool">{t}</code>
          ))}
        </div>
      )}
    </div>
  );
}

interface ExtensionsDetailProps {
  go: (route: string) => void;
}

export function ExtensionsDetail({ go }: ExtensionsDetailProps) {
  return (
    <DetailShell
      title="Extensions"
      subtitle="MCP servers that give Goose new abilities. All run locally."
      accent="#EA580C"
      onBack={() => go("settings")}
      headRight={
        <div style={{ display: "flex", gap: 8 }}>
          <button
            className="ghost-btn"
            type="button"
            style={{
              padding: "9px 14px",
              border: "1px solid var(--line)",
              borderRadius: 12,
              background: "var(--panel)",
              display: "inline-flex",
              alignItems: "center",
              gap: 6,
              fontSize: 13,
              fontWeight: 700,
              color: "#7C3AED",
              cursor: "pointer",
              fontFamily: "inherit",
            }}
            onClick={() => { /* Phase 8: open marketplace browser */ }}
          >
            <HubIco d={STORE_PATH} size={14} color="#7C3AED" /> Browse
          </button>
          <button
            className="primary-btn"
            type="button"
            style={{ background: "#EA580C", boxShadow: "0 6px 16px rgba(234,88,12,.28)" }}
            onClick={() => { /* Phase 8: api.addExtension(config) */ }}
          >
            <Plus size={15} color="#fff" strokeWidth={2.2} /> Add server
          </button>
        </div>
      }
    >
      <Card>
        <div className="ext2list">
          {MCP.map((e) => (
            <McpRow key={e.id} ext={e} />
          ))}
        </div>
      </Card>
    </DetailShell>
  );
}
