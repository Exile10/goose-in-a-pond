import { Play } from "lucide-react";
import { DetailShell } from "./DetailShell";
import { Card, Row, Toggle, Segment, Slider } from "./controls";

interface VoiceDetailProps {
  go: (route: string) => void;
}

export function VoiceDetail({ go }: VoiceDetailProps) {
  return (
    <DetailShell
      title="Voice"
      subtitle="Wake word, speech recognition and Goose's voice."
      accent="#DB2777"
      onBack={() => go("settings")}
    >
      <Card title="Listening">
        <Row
          label="Hands-free mode"
          sub="Always listening for the wake word"
          control={<Toggle on={true} />}
        />
        <Row
          label="Wake word"
          sub="Say this to summon Goose"
          control={<Segment options={["Hey Goose", "Goose", "Okay Goose"]} />}
        />
        <Row
          label="Push-to-talk shortcut"
          sub="Cmd+Shift+V from anywhere"
          control={<Toggle on={true} />}
        />
      </Card>

      <Card title="Speech-to-text">
        <Row
          label="Recognition model"
          sub="Whisper — runs locally"
          control={<Segment options={["base", "small", "large-v3"]} />}
        />
        <Row
          label="Language"
          control={<Segment options={["Auto", "English"]} />}
        />
      </Card>

      <Card
        title="Goose's voice"
        right={
          <button
            className="mrow__btn"
            type="button"
            onClick={() => { /* Phase 8: api.previewTts(voice, sample) */ }}
          >
            <Play size={12} color="#7C3AED" strokeWidth={2} /> Preview
          </button>
        }
      >
        <Row
          label="Voice"
          sub="Piper — natural neural TTS"
          control={<Segment options={["Lessac", "Ryan", "Jenny"]} />}
        />
        <div className="srow" style={{ cursor: "default" }}>
          <span className="srow__text">
            <span className="srow__label">Speaking rate</span>
          </span>
          <span className="srow__control" style={{ flex: 1, maxWidth: 240 }}>
            <Slider min={50} max={150} value={100} suffix="%" />
          </span>
        </div>
      </Card>
    </DetailShell>
  );
}
