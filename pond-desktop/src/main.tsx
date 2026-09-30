// Must stay first: applies the stored theme to <html> before React renders (no flash).
import "./hub/state/themeBootstrap";
import { captureSignInLink } from "./api/hostCredential";

import React, { useState } from "react";
import ReactDOM from "react-dom/client";

import "./styles/design-tokens.css";
// Ink styles are structure only; every value reads a property `themeStore` set on <html>.
import "@jarida/ink/css";
import "./styles/base.css";
import "./styles/sections.css";
import "./hub/hub.css";
// Last: the touch layer settles equal-specificity conflicts by cascade order.
import "./styles/goose.css";
import "./styles/touch.css";

import { StartupScreen } from "./components/StartupScreen";
import { AppContextProvider } from "./state/AppContext";
import { ConfirmProvider } from "./components/shared";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { App } from "./App";
// One-time move of member preferences from localStorage to the server; never throws or blocks.
import { migrateLocalProfileToServer } from "./api/migrateLocalProfile";

// Before anything renders or logs the URL: moves a `#host=` credential out of the address bar.
try {
  captureSignInLink(window.location, window.history, window.sessionStorage);
} catch (e) {
  console.warn("[host] session storage is unavailable; this tab cannot be signed in", e);
}

void migrateLocalProfileToServer();

function Root() {
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

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <Root />
  </React.StrictMode>,
);
