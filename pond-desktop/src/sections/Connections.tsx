// The sections-UI home for connected accounts. The panel itself is shared with
// the settings hub -- two copies of a credential form is two places for one of
// them to stop matching the backend.
import { useAppState } from "../state/AppContext";
import { ConnectionsPanel } from "../connections/ConnectionsPanel";

export function Connections() {
  const { sessionId } = useAppState();
  return (
    <div className="section-body">
      <h2>Connections</h2>
      <ConnectionsPanel sessionId={sessionId} />
    </div>
  );
}
