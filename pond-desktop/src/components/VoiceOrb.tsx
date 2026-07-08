import { useEffect, useRef } from "react";
import type { VoiceState } from "../state/reducer";
import { ORB_STATE_COLORS } from "../lib/colors";

export type OrbSize = "sm" | "md" | "lg" | "xl";

interface Props {
  state: VoiceState;
  size?: OrbSize;
  audioLevel?: number; // 0–1, for listening animation
}

const SIZE_PX: Record<OrbSize, number> = {
  sm: 40,
  md: 80,
  lg: 120,
  xl: 180,
};

const STATE_LABELS: Record<VoiceState, string> = {
  idle:      "Ready",
  wait:      "Waiting…",
  recording: "Listening",
  thinking:  "Thinking",
  speaking:  "Speaking",
  error:     "Error",
};

// The canvas drawing surface is larger than the layout footprint so glow rings
// have room to fade without clipping at the canvas edge (which paints a visible square).
const GLOW_PADDING = 1.4;

export function VoiceOrb({ state, size = "md", audioLevel = 0 }: Props) {
  const px = SIZE_PX[size];
  const canvasPx = Math.round(px * GLOW_PADDING);
  const glowOffset = Math.round((canvasPx - px) / 2);
  const color = ORB_STATE_COLORS[state];
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const animRef = useRef<number>(0);
  const phaseRef = useRef(0);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const dpr = window.devicePixelRatio || 1;
    canvas.width = canvasPx * dpr;
    canvas.height = canvasPx * dpr;
    ctx.scale(dpr, dpr);

    // Drawing center is middle of the larger canvas surface
    const cx = canvasPx / 2;
    const cy = canvasPx / 2;
    // Orb visual size is still based on px (the layout footprint)
    const baseRadius = px * 0.36;

    function draw() {
      if (!ctx) return;
      ctx.clearRect(0, 0, canvasPx, canvasPx);

      // Outer glow ring (animated for active states) — solid concentric circles, no gradient
      if (state !== "idle") {
        const glowIntensity =
          state === "wait"
            ? 0.06 + Math.sin(phaseRef.current) * 0.04
            : state === "recording"
            ? 0.18 + audioLevel * 0.22
            : state === "thinking"
            ? 0.12 + Math.sin(phaseRef.current * 2) * 0.06
            : 0.15;
        const glowRadius =
          state === "recording"
            ? baseRadius * (1.35 + audioLevel * 0.25)
            : baseRadius * 1.35;

        // Three concentric circles at decreasing opacity (outer → inner)
        const glowLayers = [
          { r: glowRadius * 1.4,  alpha: Math.round(glowIntensity * 0.40 * 255) },
          { r: glowRadius * 1.15, alpha: Math.round(glowIntensity * 0.65 * 255) },
          { r: glowRadius * 0.95, alpha: Math.round(glowIntensity * 1.00 * 255) },
        ];
        for (const layer of glowLayers) {
          const alphaHex = Math.min(255, layer.alpha).toString(16).padStart(2, "0");
          ctx.fillStyle = color + alphaHex;
          ctx.beginPath();
          ctx.arc(cx, cy, layer.r, 0, Math.PI * 2);
          ctx.fill();
        }
      }

      // Ripple ring for thinking / speaking
      if (state === "thinking" || state === "speaking") {
        const rippleProgress = (phaseRef.current % (Math.PI * 2)) / (Math.PI * 2);
        const rippleRadius = baseRadius * (1 + rippleProgress * 0.8);
        const rippleAlpha = 1 - rippleProgress;
        ctx.strokeStyle = color + Math.round(rippleAlpha * 80).toString(16).padStart(2, "0");
        ctx.lineWidth = 1.5;
        ctx.beginPath();
        ctx.arc(cx, cy, rippleRadius, 0, Math.PI * 2);
        ctx.stroke();
      }

      // Main orb — solid base + offset highlight circle
      ctx.fillStyle = color;
      ctx.beginPath();
      ctx.arc(cx, cy, baseRadius, 0, Math.PI * 2);
      ctx.fill();

      // Inner highlight circle — lighter solid color, offset top-left
      ctx.fillStyle = solidLighten(color, 0.30);
      ctx.beginPath();
      ctx.arc(cx - baseRadius * 0.18, cy - baseRadius * 0.18, baseRadius * 0.45, 0, Math.PI * 2);
      ctx.fill();

      // Audio level bars for recording state
      if (state === "recording" && size !== "sm") {
        const barCount = 5;
        const maxBarH = baseRadius * 0.55;
        const barW = Math.max(2, px * 0.04);
        const gap = barW * 1.4;
        const totalW = barCount * barW + (barCount - 1) * gap;
        const startX = cx - totalW / 2;

        for (let i = 0; i < barCount; i++) {
          const t = phaseRef.current + i * 0.7;
          const h = maxBarH * (0.25 + 0.75 * Math.abs(Math.sin(t))) * (1 + audioLevel);
          const x = startX + i * (barW + gap);
          ctx.fillStyle = "rgba(255,255,255,0.88)";
          ctx.beginPath();
          ctx.roundRect(x, cy - h / 2, barW, h, barW / 2);
          ctx.fill();
        }
      }

      // Icon for sm size / idle / wait / error states
      if (size === "sm" || state === "idle" || state === "wait" || state === "error") {
        const icon = state === "error" ? "!" : state === "idle" ? "·" : state === "wait" ? "·" : "";
        if (icon) {
          ctx.fillStyle = "rgba(255,255,255,0.90)";
          ctx.font = `${Math.round(baseRadius * 0.8)}px Inter, sans-serif`;
          ctx.textAlign = "center";
          ctx.textBaseline = "middle";
          ctx.fillText(icon, cx, cy + 1);
        }
      }

      phaseRef.current +=
        state === "wait"     ? 0.015 :
        state === "thinking" ? 0.04  :
        state === "speaking" ? 0.06  : 0.025;
      animRef.current = requestAnimationFrame(draw);
    }

    draw();
    return () => cancelAnimationFrame(animRef.current);
  }, [state, size, px, canvasPx, color, audioLevel]);

  return (
    <div
      style={{
        position: "relative",
        width: px,
        height: px,
        flexShrink: 0,
        overflow: "visible",
      }}
      title={STATE_LABELS[state]}
      aria-label={STATE_LABELS[state]}
      role="img"
    >
      <canvas
        ref={canvasRef}
        style={{
          position: "absolute",
          top: -glowOffset,
          left: -glowOffset,
          width: canvasPx,
          height: canvasPx,
          display: "block",
          outline: "none",
          border: "none",
          background: "transparent",
        }}
      />
    </div>
  );
}

function solidLighten(hex: string, amount: number): string {
  const n = parseInt(hex.slice(1), 16);
  const r = Math.min(255, ((n >> 16) & 0xff) + Math.round(255 * amount));
  const g = Math.min(255, ((n >> 8) & 0xff) + Math.round(255 * amount));
  const b = Math.min(255, (n & 0xff) + Math.round(255 * amount));
  return `#${[r, g, b].map((v) => v.toString(16).padStart(2, "0")).join("")}`;
}
