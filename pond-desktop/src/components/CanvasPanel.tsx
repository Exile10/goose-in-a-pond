import { useState, useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import VoiceOrb, { OrbState } from "../canvas/VoiceOrb";
import TranscriptFeed from "../canvas/TranscriptFeed";
import ContextCards from "../canvas/ContextCards";
import AgentTrace from "../canvas/AgentTrace";

/**
 * CanvasPanel — the right-sidebar version of the canvas, embedded in the
 * GUI mode main window. Uses the same sub-components as CanvasOverlay but
 * renders inline rather than in a floating window.
 */
export default function CanvasPanel() {
  const [orbState, setOrbState] = useState<OrbState>("idle");
  const [collapsed, setCollapsed] = useState(false);
  const [serverOnline, setServerOnline] = useState(true);

  useEffect(() => {
    const unsubs: Array<Promise<() => void>> = [];

    unsubs.push(listen("recording-started",   () => setOrbState("listening")));
    unsubs.push(listen("transcript",           () => setOrbState("thinking")));
    unsubs.push(listen("tts-start",            () => setOrbState("speaking")));
    unsubs.push(listen("tts-end",              () => setOrbState("idle")));
    unsubs.push(listen("recording-aborted",    () => setOrbState("idle")));
    unsubs.push(listen("pipeline-error",       () => {
      setOrbState("error");
      setTimeout(() => setOrbState("idle"), 3000);
    }));
    unsubs.push(listen<boolean>("server-status", (e) => setServerOnline(e.payload)));

    return () => unsubs.forEach((p) => p.then((fn) => fn()));
  }, []);

  // Toggle canvas overlay window (floating, always-on-top)
  async function toggleOverlay() {
    try {
      await invoke("toggle_canvas");
    } catch (e) {
      console.error("toggle_canvas failed:", e);
    }
  }

  return (
    <>
      {/* Offline banner */}
      {!serverOnline && (
        <div className="server-offline-banner">
          pond-server offline — voice and chat unavailable
        </div>
      )}

      {/* Right canvas panel */}
      <aside className={`canvas-panel ${collapsed ? "collapsed" : ""}`}>
        <div className="canvas-panel-header">
          <span className="canvas-panel-title">Canvas</span>

          {/* Pop-out to floating overlay */}
          {!collapsed && (
            <button
              className="canvas-panel-toggle"
              title="Open as floating overlay"
              onClick={toggleOverlay}
            >
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                <path d="M15 3h6v6M9 21H3v-6M21 3l-7 7M3 21l7-7" />
              </svg>
            </button>
          )}

          {/* Collapse / expand */}
          <button
            className="canvas-panel-toggle"
            title={collapsed ? "Expand canvas" : "Collapse canvas"}
            onClick={() => setCollapsed((c) => !c)}
          >
            <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              {collapsed
                ? <polyline points="15 18 9 12 15 6" />
                : <polyline points="9 18 15 12 9 6" />}
            </svg>
          </button>
        </div>

        {!collapsed && (
          <div className="canvas-panel-body">
            <VoiceOrb state={orbState} />
            <TranscriptFeed />
            <ContextCards />
            <AgentTrace />
          </div>
        )}
      </aside>
    </>
  );
}
