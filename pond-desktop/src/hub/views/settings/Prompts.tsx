import { useState } from "react";
import { HubIco } from "../../primitives/HubIco";
import { DetailShell } from "./DetailShell";
import { Card, Chip } from "./controls";

const CPU_PATH = "M6 4h12a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2zM9 9h6v6H9zM9 1v3M15 1v3M9 20v3M15 20v3M1 9h3M1 15h3M20 9h3M20 15h3";

// ─── Mock preset bodies ────────────────────────────────────────
const PRESETS: Record<string, string> = {
  Concise:
    "You are Goose, a warm on-device home assistant for {{user}}. Keep replies to one or two sentences. Control lights, locks, climate and scenes when asked. Never let data leave the home.",
  Balanced:
    "You are Goose, a friendly home copilot for {{user}}. Speak naturally and conversationally. Help with the home, the calendar, the weather and day-to-day questions. Confirm before locking or unlocking doors.",
  Warm:
    "You are Goose, a cheerful companion in {{user}}'s home. Be encouraging and personable. Offer little suggestions to make the day smoother. Always private, always on-device.",
  Technical:
    "You are Goose, a precise home automation agent for {{user}}. Favor accuracy. State device states explicitly. Require confirmation for security actions (doors, alarm).",
};

const VARS = ["{{user}}", "{{home_name}}", "{{room}}", "{{time}}", "{{weather}}", "{{devices}}"];

interface PromptsDetailProps {
  go: (route: string) => void;
}

export function PromptsDetail({ go }: PromptsDetailProps) {
  const [preset, setPreset] = useState("Concise");
  const [body, setBody] = useState(PRESETS.Concise);

  function pick(p: string) {
    setPreset(p);
    setBody(PRESETS[p]);
  }

  return (
    <DetailShell
      title="Prompts"
      subtitle="Goose's personality and system instructions."
      accent="#2563EB"
      onBack={() => go("settings")}
    >
      <Card title="Personality preset">
        <div className="preset-row">
          {Object.keys(PRESETS).map((p) => (
            <Chip key={p} active={preset === p} onClick={() => pick(p)}>
              {p}
            </Chip>
          ))}
        </div>
      </Card>

      <Card title="System prompt">
        <textarea
          className="prompt-ta"
          value={body}
          onChange={(e) => setBody(e.target.value)}
          rows={7}
        />
        <div className="prompt-foot">
          <span className="prompt-foot__tok">
            <HubIco d={CPU_PATH} size={13} color="#94A3B8" /> ~{Math.ceil(body.length / 4)} tokens
          </span>
          <div style={{ display: "flex", gap: 8 }}>
            <button className="ghost-btn" type="button" onClick={() => setBody(PRESETS[preset])}>
              Reset
            </button>
            <button
              className="primary-btn"
              type="button"
              style={{ padding: "9px 16px" }}
              onClick={() => { /* Phase 8: api.updatePrompt(preset, body) */ }}
            >
              Save
            </button>
          </div>
        </div>
      </Card>

      <Card title="Variables">
        <div className="varchips">
          {VARS.map((v) => (
            <span key={v} className="varchip">{v}</span>
          ))}
        </div>
      </Card>
    </DetailShell>
  );
}
