import { useState } from "react";
import { Download } from "lucide-react";
import { DetailShell } from "./DetailShell";
import { Card } from "./controls";

type LogLevel = "INFO" | "WARN" | "ERROR";
type LogFilter = "All" | "Info" | "Warn" | "Error";

interface LogEntry {
  ts: string;
  lvl: LogLevel;
  src: string;
  msg: string;
}

const LOGS: LogEntry[] = [
  { ts: "08:50:02", lvl: "INFO",  src: "pond",    msg: "Server bound to 127.0.0.1:4000" },
  { ts: "08:50:02", lvl: "INFO",  src: "models",  msg: "Loaded gemma-4-E4B-it (2.5 GB)" },
  { ts: "08:50:04", lvl: "INFO",  src: "whisper", msg: "Speech-to-text ready: whisper/base" },
  { ts: "08:50:07", lvl: "INFO",  src: "mcp",     msg: "giap-homeassistant connected · 4 tools" },
  { ts: "08:50:08", lvl: "WARN",  src: "memory",  msg: "Available memory below 25% (1.8 GB)" },
  { ts: "08:50:12", lvl: "INFO",  src: "mcp",     msg: "giap-weather connected · 2 tools" },
  { ts: "08:50:18", lvl: "ERROR", src: "mcp",     msg: "giap-news handshake failed — disabled" },
  { ts: "08:50:21", lvl: "INFO",  src: "agent",   msg: "Good Morning routine executed (4 actions)" },
  { ts: "08:50:23", lvl: "INFO",  src: "tts",     msg: "Spoke briefing · 142 chars · 3.1s" },
];

const FILTERS: LogFilter[] = ["All", "Info", "Warn", "Error"];

interface LogsDetailProps {
  go: (route: string) => void;
}

export function LogsDetail({ go }: LogsDetailProps) {
  const [filter, setFilter] = useState<LogFilter>("All");
  const rows = LOGS.filter(
    (l) => filter === "All" || l.lvl === filter.toUpperCase()
  );

  return (
    <DetailShell
      title="Logs"
      subtitle="Live activity from the Goose server."
      accent="#475569"
      onBack={() => go("settings")}
    >
      <div className="logs2__bar">
        <div className="logs2__tabs">
          {FILTERS.map((t) => (
            <button
              key={t}
              className="logs2__tab"
              data-active={filter === t}
              onClick={() => setFilter(t)}
              type="button"
            >
              {t}
            </button>
          ))}
        </div>
        <button
          type="button"
          onClick={() => { /* Phase 8: api.exportLogs(filter, range) → download */ }}
          style={{
            padding: "8px 14px",
            border: "1px solid var(--line)",
            borderRadius: 10,
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
        >
          <Download size={13} color="#7C3AED" strokeWidth={2} /> Export
        </button>
      </div>
      <Card>
        <div className="logs2">
          {rows.map((l, i) => (
            <div key={i} className="logrow">
              <code className="logrow__ts">{l.ts}</code>
              <span className={`logrow__lvl logrow__lvl--${l.lvl.toLowerCase()}`}>{l.lvl}</span>
              <code className="logrow__src">{l.src}</code>
              <span className="logrow__msg">{l.msg}</span>
            </div>
          ))}
        </div>
      </Card>
    </DetailShell>
  );
}
