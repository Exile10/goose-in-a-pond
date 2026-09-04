// The settings-hub home for connected accounts. Same panel as the sections UI:
// two copies of a credential form is two places for one of them to stop
// matching the backend.
import { DetailShell } from "./DetailShell";
import { useAppState } from "../../../state/AppContext";
import { ConnectionsPanel } from "../../../connections/ConnectionsPanel";

interface ConnectionsDetailProps {
  go: (route: string) => void;
}

export function ConnectionsDetail({ go }: ConnectionsDetailProps) {
  const { sessionId } = useAppState();
  return (
    <DetailShell
      title="Accounts"
      subtitle="Calendar and mail the pond can read"
      accent="#1F6F63"
      onBack={() => go("settings")}
    >
      <ConnectionsPanel sessionId={sessionId} />
    </DetailShell>
  );
}
