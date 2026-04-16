import React from "react";
import ReactDOM from "react-dom/client";
import { CanvasOverlay } from "./canvas/CanvasOverlay";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <CanvasOverlay />
  </React.StrictMode>,
);
