// ────────────────────────────────────────────────────────────
// Home (hub surface).
//
// The screen itself now lives in `DashboardGrid`, rendered by this surface and
// by `sections/Dashboard.tsx`. The two were already deliberate twins — same
// primitives, same order, same header comment — and keeping the body in one
// place is what stops them drifting into two different Homes depending on how
// the household got in.
//
// What stays here is the part that is genuinely per-surface: how this shell
// routes, and how it reaches voice mode.
//
// The screen's own reasoning — why it is pared back, and why it is now
// arrangeable rather than fixed — is in `state/dashboardLayout.ts`.
// ────────────────────────────────────────────────────────────

import { useAppState, useAppDispatch } from "../../state/AppContext";
import { DashboardGrid } from "./DashboardGrid";

interface HomeViewProps {
  go?: (route: string) => void;
}

export function HomeView({ go }: HomeViewProps) {
  const state = useAppState();
  const dispatch = useAppDispatch();

  return (
    <DashboardGrid
      sessionId={state.sessionId}
      // The hub routes through `go` when its shell supplied one, and falls back
      // to the shared section reducer when it did not.
      onNavigate={(section) =>
        go ? go(section) : dispatch({ type: "SET_SECTION", payload: section })
      }
      onTalk={() => dispatch({ type: "SET_MODE", payload: "voice" })}
    />
  );
}
