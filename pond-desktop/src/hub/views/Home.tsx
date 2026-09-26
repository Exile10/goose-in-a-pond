// Hub Home: the screen is `DashboardGrid` (shared with `sections/Dashboard.tsx`);
// only routing and the way into voice mode are per-surface.

import { useAppState, useAppDispatch } from "../../state/AppContext";
import type { GuiSection } from "../../desktopState";
import { DashboardGrid } from "./DashboardGrid";
import { sendTurn } from "../../state/chatRunStore";

interface HomeViewProps {
  /**
   * Take the household to a section of the app.
   *
   * A GuiSection and not a hub route, because that is what the screen below
   * emits, and the two vocabularies overlap only by accident. This used to be
   * typed `(route: string) => void`, which let the hub hand a section straight
   * to its own router: "devices" matched no hub screen, fell through to the
   * fallback, and re-rendered Home. The shell that supplies this is responsible
   * for the translation -- see `Hub`'s `navigate`.
   */
  go?: (section: GuiSection) => void;
}

export function HomeView({ go }: HomeViewProps) {
  const state = useAppState();
  const dispatch = useAppDispatch();

  return (
    <DashboardGrid
      sessionId={state.sessionId}
      onNavigate={(section) =>
        go ? go(section) : dispatch({ type: "SET_SECTION", payload: section })
      }
      onTalk={() => dispatch({ type: "SET_MODE", payload: "voice" })}
      // Same two steps as the classic surface, routed the hub's way. See
      // `sections/Dashboard.tsx` for why the send comes first.
      onAsk={(prompt) => {
        sendTurn({ text: prompt });
        if (go) go("chat");
        else dispatch({ type: "SET_SECTION", payload: "chat" });
      }}
    />
  );
}
