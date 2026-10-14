import { useState } from "react";
import { StartupScreen } from "./components/StartupScreen";
import { AppContextProvider } from "./state/AppContext";
import { ConfirmProvider } from "./components/shared";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { App } from "./App";

/** The startup screen until the server is ready, then the app inside its providers. `main.tsx` mounts it. */
export function Root() {
  const [ready, setReady] = useState(false);

  if (!ready) {
    return <StartupScreen onReady={() => setReady(true)} />;
  }

  // A render-time exception anywhere shows a recoverable error card, not a blank window.
  return (
    <ErrorBoundary>
      <ConfirmProvider>
        <AppContextProvider>
          <App />
        </AppContextProvider>
      </ConfirmProvider>
    </ErrorBoundary>
  );
}
