// ────────────────────────────────────────────────────────────
// Home (classic surface).
//
// The screen itself lives in `hub/views/DashboardGrid`, shared with
// `hub/views/Home.tsx`. See that file and `hub/state/dashboardLayout.ts` for
// the design reasoning; what remains here is this surface's routing.
//
// Still exported as `Dashboard` and still on the `dashboard` section id: that
// id is persisted in localStorage as `giap-section`, so renaming it would
// strand anyone whose app reopens on the screen they left. What people see is
// "Home"; what the router remembers is unchanged.
// ────────────────────────────────────────────────────────────

import { useAppState, useAppDispatch } from "../state/AppContext";
import { DashboardGrid } from "../hub/views/DashboardGrid";

export function Dashboard() {
  const state = useAppState();
  const dispatch = useAppDispatch();

  return (
    <DashboardGrid
      sessionId={state.sessionId}
      onNavigate={(section) => dispatch({ type: "SET_SECTION", payload: section })}
      onTalk={() => dispatch({ type: "SET_MODE", payload: "voice" })}
    />
  );
}
