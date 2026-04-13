import React from "react";
import ReactDOM from "react-dom/client";
// Import the full GIAP web app from the shared web/ frontend
import App from "@web/App";
import CanvasPanel from "./components/CanvasPanel";
// Import all shared styles
import "@web/index.css";
import "@web/dashboard.css";
import "@web/onboarding.css";
import "./desktop.css";

// The Tauri main.rs injects window.__GIAP_SERVER_URL__ before the app loads.
// api.ts picks this up automatically.

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <div className="desktop-shell">
      {/* Full GIAP dashboard — unchanged from web/ */}
      <div className="desktop-app-area">
        <App />
      </div>
      {/* Canvas panel — right sidebar showing live context */}
      <CanvasPanel />
    </div>
  </React.StrictMode>
);
