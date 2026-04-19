import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@heroui/react";
import logoSrc from "../assets/logo.png";

interface Props {
  onReady: () => void;
}

type StartupPhase = "starting" | "connecting" | "ready" | "error";

const MAX_POLLS = 60; // 30 seconds at 500ms intervals
const POLL_INTERVAL_MS = 500;

export function StartupScreen({ onReady }: Props) {
  const [phase, setPhase] = useState<StartupPhase>("starting");
  const [error, setError] = useState<string | null>(null);
  const [dots, setDots] = useState(".");

  const tryStartup = useCallback(async () => {
    setPhase("starting");
    setError(null);

    const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

    if (isTauri) {
      try {
        await invoke("ensure_server_running");
      } catch (e) {
        // The command may fail if no binary is found — we still try polling
        // in case the user has a manually running server.
        console.warn("[GIAP] ensure_server_running error:", e);
      }
    }

    setPhase("connecting");

    if (!isTauri) {
      // Browser dev mode: poll the health endpoint directly via fetch.
      // This lets Playwright and web browser testing work without Tauri.
      const serverUrl =
        (window as { __GIAP_SERVER_URL__?: string }).__GIAP_SERVER_URL__ ??
        "http://127.0.0.1:4000";
      for (let i = 0; i < MAX_POLLS; i++) {
        await new Promise((resolve) => setTimeout(resolve, POLL_INTERVAL_MS));
        try {
          const res = await fetch(`${serverUrl}/api/v1/health`);
          if (res.ok) {
            setPhase("ready");
            await new Promise((resolve) => setTimeout(resolve, 400));
            onReady();
            return;
          }
        } catch {
          // Server not yet available — keep polling
        }
      }
      setPhase("error");
      setError("Could not reach pond-server at " + serverUrl + ". Make sure it is running.");
      return;
    }

    // Poll health endpoint via Tauri IPC
    for (let i = 0; i < MAX_POLLS; i++) {
      await new Promise((resolve) => setTimeout(resolve, POLL_INTERVAL_MS));

      try {
        const healthy = await invoke<boolean>("server_health");
        if (healthy) {
          setPhase("ready");
          // Small delay so "Ready" is visible briefly
          await new Promise((resolve) => setTimeout(resolve, 400));
          onReady();
          return;
        }
      } catch {
        // Keep polling
      }
    }

    setPhase("error");
    setError("Could not connect to pond-server within 30 seconds.");
  }, [onReady]);

  // Animated dots
  useEffect(() => {
    const id = setInterval(() => {
      setDots((d) => (d.length >= 3 ? "." : d + "."));
    }, 400);
    return () => clearInterval(id);
  }, []);

  // Start on mount
  useEffect(() => {
    tryStartup();
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const statusLabel =
    phase === "starting"   ? `Starting pond-server${dots}` :
    phase === "connecting" ? `Connecting${dots}` :
    phase === "ready"      ? "Ready" :
    "Failed to start";

  return (
    <div style={styles.root}>
      <div style={styles.card}>
        {/* Jarida logo */}
        <img src={logoSrc} alt="Goose In A Pond" style={styles.logoMark} />

        <h1 style={styles.title}>Goose In A Pond</h1>
        <p style={styles.subtitle}>by Jarida Open Source</p>

        {/* Progress bar */}
        <div style={styles.progressTrack}>
          {phase !== "error" && phase !== "ready" && (
            <div style={styles.progressBar} />
          )}
          {phase === "ready" && (
            <div style={{ ...styles.progressBar, ...styles.progressFull }} />
          )}
        </div>

        <p style={styles.statusText}>{statusLabel}</p>

        {phase === "error" && (
          <div style={styles.errorBox}>
            <p style={styles.errorText}>{error}</p>
            <p style={styles.errorHint}>
              Make sure pond-server is installed or run{" "}
              <code style={styles.code}>cargo run -p pond-server -- serve</code>{" "}
              in a terminal.
            </p>
            <Button variant="primary" onPress={tryStartup}>
              Retry
            </Button>
          </div>
        )}
      </div>
    </div>
  );
}

const styles: Record<string, React.CSSProperties> = {
  root: {
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    width: "100%",
    height: "100%",
    background: "#FFF9F9",
    fontFamily: '"Inter", "Helvetica Neue", Arial, sans-serif',
  },
  card: {
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    gap: "12px",
    width: "340px",
  },
  logoMark: {
    width: "96px",
    height: "96px",
    objectFit: "contain",
    marginBottom: "4px",
  },
  title: {
    fontFamily: '"Quicksand", "Helvetica Neue", Arial, sans-serif',
    fontWeight: 700,
    fontSize: "22px",
    color: "#171616",
    margin: 0,
  },
  subtitle: {
    fontSize: "12px",
    color: "rgba(23,22,22,0.45)",
    margin: 0,
    letterSpacing: "0.02em",
  },
  progressTrack: {
    width: "100%",
    height: "3px",
    background: "rgba(23,22,22,0.08)",
    borderRadius: "999px",
    overflow: "hidden",
    marginTop: "8px",
  },
  progressBar: {
    height: "100%",
    borderRadius: "999px",
    background: "#8C52FF",
    animation: "progressSlide 1.2s ease infinite",
    width: "60%",
  },
  progressFull: {
    width: "100%",
    animation: "none",
  },
  statusText: {
    fontSize: "12px",
    color: "rgba(23,22,22,0.55)",
    margin: 0,
    minHeight: "18px",
  },
  errorBox: {
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    gap: "10px",
    marginTop: "8px",
    padding: "16px",
    background: "rgba(255,59,48,0.06)",
    border: "1px solid rgba(255,59,48,0.20)",
    borderRadius: "10px",
    width: "100%",
  },
  errorText: {
    fontSize: "13px",
    color: "#FF3B30",
    textAlign: "center",
    margin: 0,
  },
  errorHint: {
    fontSize: "11px",
    color: "rgba(23,22,22,0.55)",
    textAlign: "center",
    margin: 0,
    lineHeight: "1.5",
  },
  code: {
    fontFamily: '"JetBrains Mono", Menlo, monospace',
    fontSize: "10px",
    background: "rgba(23,22,22,0.06)",
    padding: "1px 4px",
    borderRadius: "4px",
  },
  retryBtn: {
    background: "#8C52FF",
    color: "#FFFFFF",
    fontFamily: '"Inter", sans-serif',
    fontWeight: 600,
    fontSize: "13px",
    border: "none",
    borderRadius: "8px",
    padding: "8px 20px",
    cursor: "pointer",
    height: "36px",
    display: "inline-flex",
    alignItems: "center",
  },
};
