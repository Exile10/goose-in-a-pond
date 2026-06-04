import { Download, Check } from "lucide-react";
import { HubIco } from "../../primitives/HubIco";
import { DetailShell } from "./DetailShell";
import { Card } from "./controls";

// ─── Icon path strings for this view ─────────────────────────
const SICN = {
  chat:    "M21 12a8 8 0 0 1-11.5 7.2L4 21l1.8-5.4A8 8 0 1 1 21 12z",
  spark:   "M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8z",
  bolt:    "M13 2L3 14h7l-1 8 11-12h-7z",
  ear:     "M6 8a6 6 0 0 1 12 0c0 3-1.5 4-3 5s-2 2-2 4-1 3-3 3-3-2-3-4M9 12a3 3 0 0 1 6 0",
  speaker: "M11 5L6 9H2v6h4l5 4zM19 5a10 10 0 0 1 0 14M15.5 8.5a5 5 0 0 1 0 7",
  cpu:     "M6 4h12a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2zM9 9h6v6H9zM9 1v3M15 1v3M9 20v3M15 20v3M1 9h3M1 15h3M20 9h3M20 15h3",
} as const;

// ─── Data (mock) ─────────────────────────────────────────────
const ROLES: Array<{ role: string; model: string; icon: string; c: string; bg: string }> = [
  { role: "Chat",           model: "gemma-4-E4B-it", icon: SICN.chat,    c: "#7C3AED", bg: "#EDE9FE" },
  { role: "Think",          model: "gemma-4-E4B-it", icon: SICN.spark,   c: "#D97706", bg: "#FEF3C7" },
  { role: "Task",           model: "gemma-4-E4B-it", icon: SICN.bolt,    c: "#16A34A", bg: "#DCFCE7" },
  { role: "Speech-to-text", model: "whisper / base", icon: SICN.ear,     c: "#2563EB", bg: "#DBEAFE" },
  { role: "Text-to-speech", model: "piper / lessac", icon: SICN.speaker, c: "#DB2777", bg: "#FCE7F3" },
];

const LLMS: Array<{ name: string; file: string; size: string; tags: string[]; active: boolean }> = [
  { name: "Gemma 4 E4B Instruct",       file: "gemma-4-E4B-it-Q4_K_S",   size: "2.5 GB", tags: ["chat", "think"], active: true },
  { name: "Llama 3.2 3B Instruct",      file: "llama-3.2-3b-Q4_K_M",     size: "2.3 GB", tags: ["chat"],          active: false },
  { name: "Llama 3.1 8B Instruct",      file: "llama-3.1-8b-Q4_K_M",     size: "7.1 GB", tags: ["chat"],          active: false },
  { name: "DeepSeek R1 Distill 1.5B",   file: "deepseek-r1-1.5b-Q6_K",   size: "1.1 GB", tags: ["think"],         active: false },
];

const SPEECH: Array<{ name: string; kind: string; file: string; active: boolean }> = [
  { name: "Whisper base",                 kind: "Speech-to-text", file: "whisper / base",           active: true  },
  { name: "Whisper large-v3-turbo",       kind: "Speech-to-text", file: "whisper / large-v3-turbo", active: false },
  { name: "Piper en-US Lessac (medium)", kind: "Text-to-speech",  file: "piper / en-lessac-medium", active: true  },
  { name: "Piper en-US Ryan (medium)",   kind: "Text-to-speech",  file: "piper / en-ryan-medium",   active: false },
];

// ─── Component ───────────────────────────────────────────────
interface ModelsDetailProps {
  go: (route: string) => void;
}

export function ModelsDetail({ go }: ModelsDetailProps) {
  return (
    <DetailShell
      title="Models"
      subtitle="Local language & speech models powering Goose. Everything runs on-device."
      accent="#7C3AED"
      onBack={() => go("settings")}
      headRight={
        <button className="primary-btn" type="button" onClick={() => { /* Phase 8: open model download modal */ }}>
          <Download size={15} color="#fff" strokeWidth={2.2} /> Download
        </button>
      }
    >
      {/* Active roles */}
      <Card title="Active roles">
        <div className="roles-grid">
          {ROLES.map((r) => (
            <div key={r.role} className="role2">
              <span className="role2__icon" style={{ background: r.bg }}>
                <HubIco d={r.icon} size={16} color={r.c} />
              </span>
              <div className="role2__text">
                <span className="role2__role">{r.role}</span>
                <span className="role2__model">{r.model}</span>
              </div>
            </div>
          ))}
        </div>
      </Card>

      {/* Language models */}
      <Card title="Language models">
        <div className="mlist">
          {LLMS.map((m) => (
            <div key={m.file} className={`mrow${m.active ? " mrow--active" : ""}`}>
              <span className="mrow__icon">
                <HubIco d={SICN.cpu} size={16} color={m.active ? "#7C3AED" : "#94A3B8"} />
              </span>
              <div className="mrow__text">
                <span className="mrow__name">{m.name}</span>
                <span className="mrow__file">
                  {m.file} · {m.size}
                </span>
              </div>
              <div className="mrow__tags">
                {m.tags.map((t) => (
                  <span key={t} className="mtag">{t}</span>
                ))}
              </div>
              {m.active ? (
                <span className="mrow__loaded">
                  <Check size={12} color="#16A34A" strokeWidth={3} /> Loaded
                </span>
              ) : (
                <button className="mrow__btn" type="button" onClick={() => { /* Phase 8: api.activateModel(provider, name, role) */ }}>Load</button>
              )}
            </div>
          ))}
        </div>
      </Card>

      {/* Speech models */}
      <Card title="Speech">
        <div className="mlist">
          {SPEECH.map((m) => (
            <div key={m.file} className={`mrow${m.active ? " mrow--active" : ""}`}>
              <span className="mrow__icon">
                <HubIco
                  d={m.kind === "Speech-to-text" ? SICN.ear : SICN.speaker}
                  size={16}
                  color={m.active ? "#7C3AED" : "#94A3B8"}
                />
              </span>
              <div className="mrow__text">
                <span className="mrow__name">{m.name}</span>
                <span className="mrow__file">
                  {m.kind} · {m.file}
                </span>
              </div>
              {m.active ? (
                <span className="mrow__loaded">
                  <Check size={12} color="#16A34A" strokeWidth={3} /> Active
                </span>
              ) : (
                <button className="mrow__btn" type="button" onClick={() => { /* Phase 8: api.activateModel for speech role */ }}>Use</button>
              )}
            </div>
          ))}
        </div>
      </Card>
    </DetailShell>
  );
}
