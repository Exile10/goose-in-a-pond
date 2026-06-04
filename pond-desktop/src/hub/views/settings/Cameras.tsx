import { Plus } from "lucide-react";
import { HOME } from "../../data/mockHome";
import { CameraFeed } from "../../primitives/CameraFeed";
import { DetailShell } from "./DetailShell";
import { Card, Toggle } from "./controls";

interface CamerasDetailProps {
  go: (route: string) => void;
}

export function CamerasDetail({ go }: CamerasDetailProps) {
  return (
    <DetailShell
      title="Cameras"
      subtitle="3 live feeds · stored locally, auto-deleted after 7 days."
      accent="#0EA5E9"
      onBack={() => go("settings")}
      headRight={
        <button
          className="primary-btn"
          type="button"
          style={{ background: "#0EA5E9", boxShadow: "0 6px 16px rgba(14,165,233,.28)" }}
          onClick={() => { /* Phase 8: open add-camera wizard */ }}
        >
          <Plus size={15} color="#fff" strokeWidth={2.2} /> Add camera
        </button>
      }
    >
      {HOME.cameras.map((c) => (
        <Card key={c.id}>
          <div className="camset">
            <div className="camset__thumb">
              <CameraFeed cam={c} />
            </div>
            <div className="camset__body">
              <div className="camset__name">{c.name}</div>
              <div className="camset__sub">Online · last motion {c.time}</div>
              <div className="camset__rows">
                <span className="camset__opt">
                  Motion alerts <Toggle on={true} />
                </span>
                <span className="camset__opt">
                  Record 24/7 <Toggle on={c.id === "front"} />
                </span>
                <span className="camset__opt">
                  Night vision <Toggle on={true} />
                </span>
              </div>
            </div>
          </div>
        </Card>
      ))}
    </DetailShell>
  );
}
