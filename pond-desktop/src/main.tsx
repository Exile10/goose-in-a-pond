import React, { useState } from "react";
import ReactDOM from "react-dom/client";

import "./styles/design-tokens.css";
import "./styles/base.css";

import { StartupScreen } from "./components/StartupScreen";
import { AppContextProvider } from "./state/AppContext";
import { App } from "./App";

function Root() {
  const [ready, setReady] = useState(true); // DEV BYPASS

  if (!ready) {
    return <StartupScreen onReady={() => setReady(true)} />;
  }

  return (
    <AppContextProvider>
      <App />
    </AppContextProvider>
  );
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <Root />
  </React.StrictMode>,
);
