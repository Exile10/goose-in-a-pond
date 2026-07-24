import { HubIco } from "../../primitives/HubIco";
import { DetailShell } from "./DetailShell";
import { Card, Row, Toggle } from "./controls";

const CHEVR_PATH = "M9 6l6 6-6 6";

const BOLT_PATH     = "M13 2L3 14h7l-1 8 11-12h-7z";
const ACTIVITY_PATH = "M22 12h-4l-3 9L9 3l-3 9H2";
const MEMORY_PATH   = "M8 3v2M16 3v2M6 7h12a1 1 0 0 1 1 1v8a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V8a1 1 0 0 1 1-1zM9 10h6v4H9z";

interface Debrief {
  name: string;
  icon: string;
  c: string;
  bg: string;
  when: string;
  excerpt: string;
}

const DEBRIEFS: Debrief[] = [
  {
    name: "Morning Briefing",
    icon: BOLT_PATH,
    c: "#7C3AED",
    bg: "#EDE9FE",
    when: "Today · 8:00 AM",
    excerpt: "4 meetings · 2 tasks · BTC +2.1% · focus 10–12",
  },
  {
    name: "Weekly Report",
    icon: ACTIVITY_PATH,
    c: "#16A34A",
    bg: "#DCFCE7",
    when: "Fri · 5:00 PM",
    excerpt: "14 tasks · 142k tokens · ~$0.76/day saved",
  },
  {
    name: "Memory Compaction",
    icon: MEMORY_PATH,
    c: "#0D9488",
    bg: "#CCFBF1",
    when: "Yesterday · 3:00 AM",
    excerpt: "12 → 5 entries · 58% dedupe",
  },
];

interface NotificationsDetailProps {
  go: (route: string) => void;
}

export function NotificationsDetail({ go }: NotificationsDetailProps) {
  return (
    <DetailShell
      title="Notifications"
      subtitle="Alerts and scheduled debriefs from Goose."
      accent="#D97706"
      onBack={() => go("settings")}
    >
      <Card title="Alerts">
        <Row
          label="Security alerts"
          sub="Doors, locks & alarm"
          control={<Toggle on={true} />}
        />
        <Row
          label="Camera motion"
          sub="When a camera sees movement"
          control={<Toggle on={true} />}
        />
        <Row
          label="Routine summaries"
          sub="When a scene finishes"
          control={<Toggle on={false} />}
        />
        <Row
          label="Schedule debriefs"
          sub="Recipe run results"
          control={<Toggle on={true} />}
        />
      </Card>

      <Card title="Recent debriefs">
        <div className="dbrief-list">
          {DEBRIEFS.map((d) => (
            <div
              key={d.name}
              className="dbrief"
              role="button"
              tabIndex={0}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") e.currentTarget.click();
              }}
            >
              <span className="dbrief__icon" style={{ background: d.bg }}>
                <HubIco d={d.icon} size={16} color={d.c} />
              </span>
              <span className="dbrief__body">
                <span className="dbrief__name">{d.name}</span>
                <span className="dbrief__excerpt">{d.excerpt}</span>
              </span>
              <span className="dbrief__when">{d.when}</span>
              <HubIco d={CHEVR_PATH} size={16} color="var(--color-text-tertiary,#566178)" />
            </div>
          ))}
        </div>
      </Card>
    </DetailShell>
  );
}
