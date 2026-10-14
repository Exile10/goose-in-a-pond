import { createContext, useContext, type Dispatch } from "react";
import type { AppAction, AppState } from "./reducer";

// The app's state and dispatch. `AppContextProvider` in AppContext.tsx supplies both.

export const StateCtx = createContext<AppState | null>(null);
export const DispatchCtx = createContext<Dispatch<AppAction> | null>(null);

export function useAppState(): AppState {
  const ctx = useContext(StateCtx);
  if (!ctx)
    throw new Error("useAppState must be used within AppContextProvider");
  return ctx;
}

export function useAppDispatch(): Dispatch<AppAction> {
  const ctx = useContext(DispatchCtx);
  if (!ctx)
    throw new Error("useAppDispatch must be used within AppContextProvider");
  return ctx;
}
