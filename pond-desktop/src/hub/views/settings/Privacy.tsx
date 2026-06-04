import { HubIco } from "../../primitives/HubIco";
import { DetailShell } from "./DetailShell";
import { Card, Row, Toggle } from "./controls";

const SHIELD_PATH = "M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z";

interface PrivacyDetailProps {
  go: (route: string) => void;
}

export function PrivacyDetail({ go }: PrivacyDetailProps) {
  return (
    <DetailShell
      title="Privacy"
      subtitle="Goose lives entirely in your home."
      accent="#16A34A"
      onBack={() => go("settings")}
    >
      {/* Hero card */}
      <div className="privacy-hero">
        <span className="privacy-hero__icon">
          <HubIco d={SHIELD_PATH} size={30} color="#fff" />
        </span>
        <div>
          <div className="privacy-hero__title">Everything runs on-device</div>
          <div className="privacy-hero__sub">
            No audio, video or home data ever leaves this hub. No cloud account required.
          </div>
        </div>
      </div>

      <Card title="Data & sensors">
        <Row
          label="Microphone"
          sub="Used only after the wake word"
          control={<Toggle on={true} />}
        />
        <Row
          label="Cameras"
          sub="Feeds stay local; nothing is uploaded"
          control={<Toggle on={true} />}
        />
        <Row
          label="Cloud fallback"
          sub="Use a cloud model if local fails"
          control={<Toggle on={false} />}
        />
        <Row
          label="Anonymous diagnostics"
          sub="Share crash logs to improve Goose"
          control={<Toggle on={false} />}
        />
      </Card>

      <Card title="Storage">
        <Row
          label="Conversations"
          sub="Stored locally · 23 sessions"
          control={<button className="mrow__btn" type="button" onClick={() => { /* Phase 8: api.clearConversations() with confirm */ }}>Clear</button>}
        />
        <Row
          label="Camera clips"
          sub="Local · auto-deletes after 7 days"
          control={<button className="mrow__btn" type="button" onClick={() => { /* Phase 8: open camera-clips retention modal */ }}>Manage</button>}
        />
      </Card>
    </DetailShell>
  );
}
