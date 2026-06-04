import { HOME } from "../../data/mockHome";
import { DetailShell } from "./DetailShell";
import { Card, Row } from "./controls";
import { HubIco } from "../../primitives/HubIco";

const CHEVR_PATH = "M9 6l6 6-6 6";

interface AccountDetailProps {
  go: (route: string) => void;
}

export function AccountDetail({ go }: AccountDetailProps) {
  return (
    <DetailShell
      title="Account"
      subtitle="Your profile and home."
      accent="#475569"
      onBack={() => go("settings")}
    >
      {/* Profile hero */}
      <div className="acct-hero">
        <span className="acct-hero__avatar">
          {HOME.user.charAt(0).toUpperCase()}
        </span>
        <div>
          <div className="acct-hero__name">{HOME.user}</div>
          <div className="acct-hero__home">Goose Pond · 6 rooms</div>
        </div>
        <span className="acct-hero__badge">On-device</span>
      </div>

      <Card title="Profile">
        <Row
          label="Name"
          sub={HOME.user}
          control={<HubIco d={CHEVR_PATH} size={16} color="#C4C4CC" />}
          onClick={() => {}}
        />
        <Row
          label="Home name"
          sub="Goose Pond"
          control={<HubIco d={CHEVR_PATH} size={16} color="#C4C4CC" />}
          onClick={() => {}}
        />
        <Row
          label="Time zone"
          sub="Africa/Nairobi"
          control={<HubIco d={CHEVR_PATH} size={16} color="#C4C4CC" />}
          onClick={() => {}}
        />
      </Card>

      <Card title="About">
        <Row
          label="Goose In A Pond"
          sub="Version 2.0 · on-device build"
          control={<span className="set-row__badge">Up to date</span>}
        />
        <Row
          label="Help & feedback"
          control={<HubIco d={CHEVR_PATH} size={16} color="#C4C4CC" />}
          onClick={() => {}}
        />
      </Card>

      <button
        className="signout-btn"
        type="button"
        onClick={() => {
          /* Phase 8: api.signOut() + clear session token */
        }}
      >
        Sign out
      </button>
    </DetailShell>
  );
}
