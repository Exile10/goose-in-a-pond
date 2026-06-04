import { useState } from "react";
import { Plus } from "lucide-react";
import { HubIco } from "../../primitives/HubIco";
import { DetailShell } from "./DetailShell";
import { Card, Row, Toggle } from "./controls";

const TRASH_PATH =
  "M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6";

interface MemItem {
  t: string;
  d: string;
}

const INITIAL_MEMS: MemItem[] = [
  { t: "Prefers the house at 70° in the morning, 66° overnight.", d: "Jun 1" },
  { t: "Is a software engineer; works from the Office most weekdays.", d: "May 28" },
  { t: "Lactose intolerant — avoid dairy in recipe suggestions.", d: "May 24" },
  { t: "Front door is the main entrance; garage is secondary.", d: "May 20" },
  { t: "Likes ambient / lo-fi music while focusing.", d: "May 18" },
];

interface MemoryDetailProps {
  go: (route: string) => void;
}

export function MemoryDetail({ go }: MemoryDetailProps) {
  const [mems, setMems] = useState<MemItem[]>(INITIAL_MEMS);

  function del(i: number) {
    setMems((m) => m.filter((_, idx) => idx !== i));
  }

  return (
    <DetailShell
      title="Memory"
      subtitle={`${mems.length} things Goose remembers about you. Stored on-device.`}
      accent="#0D9488"
      onBack={() => go("settings")}
      headRight={
        <button
          className="primary-btn"
          type="button"
          style={{ background: "#0D9488", boxShadow: "0 6px 16px rgba(13,148,136,.28)" }}
          onClick={() => { /* Phase 8: open add-memory composer */ }}
        >
          <Plus size={15} color="#fff" strokeWidth={2.2} /> Add
        </button>
      }
    >
      <Card title="Auto-compaction">
        <Row
          label="Compact memory nightly"
          sub="Merge & dedupe at 3:00 AM · last run freed 7 entries"
          control={<Toggle on={true} />}
        />
      </Card>

      <Card title="Remembered">
        <div className="memlist">
          {mems.map((m, i) => (
            <div key={i} className="memrow">
              <span className="memrow__dot" />
              <span className="memrow__text">{m.t}</span>
              <span className="memrow__date">{m.d}</span>
              <button
                className="memrow__del"
                type="button"
                onClick={() => del(i)}
                aria-label={`Forget: ${m.t}`}
              >
                <HubIco d={TRASH_PATH} size={14} color="#CBD5E1" />
              </button>
            </div>
          ))}
          {mems.length === 0 && (
            <div className="memempty">
              No memories yet. Goose will learn as you go.
            </div>
          )}
        </div>
      </Card>
    </DetailShell>
  );
}
