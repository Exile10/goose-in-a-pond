import React from "react";
import ReactDOM from "react-dom/client";
import CanvasOverlay from "./canvas/CanvasOverlay";
import "./canvas/canvas.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <CanvasOverlay />
  </React.StrictMode>
);
