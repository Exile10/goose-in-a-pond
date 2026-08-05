import { useEffect, useRef, useState } from "react";
import type { VoiceState } from "../state/reducer";

export type OrbSize = "sm" | "md" | "lg" | "xl";

interface Props {
  state: VoiceState;
  size?: OrbSize;
  /** Live mic RMS (0-1-ish; typical speech is well under 1) — drives ring reactivity during wait/recording. */
  audioLevel?: number;
  /** Bump this to fire a one-shot "wake word heard" wink + flash ring. */
  pulseKey?: number;
}

const SIZE_PX: Record<OrbSize, number> = {
  sm: 48,
  md: 96,
  lg: 150,
  xl: 220,
};

/** How long the one-shot wink plays before the eye returns to its ambient blink/look. */
const WINK_MS = 450;

const STATE_LABELS: Record<VoiceState, string> = {
  idle:      "Ready",
  wait:      "Listening for wake word",
  recording: "Listening",
  thinking:  "Thinking",
  speaking:  "Speaking",
  error:     "Error",
};

// Mirrors --orb-* tokens in design-tokens.css. recording gets its own cooler
// blue (--orb-receiving) distinct from wait's purple, to visually signal
// "actively receiving input" rather than "waiting."
const STATE_COLOR_VAR: Record<VoiceState, string> = {
  idle:      "var(--orb-idle)",
  wait:      "var(--orb-listening)",
  recording: "var(--orb-receiving)",
  thinking:  "var(--orb-thinking)",
  speaking:  "var(--orb-speaking)",
  error:     "var(--orb-error)",
};

export function VoiceOrb({ state, size = "md", audioLevel = 0, pulseKey = 0 }: Props) {
  const px = SIZE_PX[size];
  // Raw mic RMS during normal speech is small (whisper's own onset threshold
  // is ~0.01) — amplify into a visually useful 0-1 range for the CSS scale.
  const level = Math.max(0, Math.min(1, audioLevel * 6));

  // One-shot wink when the wake word lands (pulseKey bump from VoiceMode.tsx's
  // wait->recording transition) — plays briefly, then the eye reverts to its
  // normal ambient blink/look animation on its own.
  const [isWinking, setIsWinking] = useState(false);
  useEffect(() => {
    if (pulseKey === 0) return;
    setIsWinking(true);
    const t = setTimeout(() => setIsWinking(false), WINK_MS);
    return () => clearTimeout(t);
  }, [pulseKey]);

  // While waiting for the wake word — where the orb spends almost all its
  // resting time in normal use — the eyes follow the cursor instead of
  // wandering on their own ambient cycle. cursorGaze === null means "not
  // tracking" — either not waiting, or the cursor has left the window — in
  // which case the eyes fall straight back to the normal CSS-driven random
  // blink/look.
  const rootRef = useRef<HTMLDivElement>(null);
  const [cursorGaze, setCursorGaze] = useState<{ x: number; y: number } | null>(null);

  useEffect(() => {
    if (state !== "wait") {
      setCursorGaze(null);
      return;
    }

    function handleMove(e: MouseEvent) {
      const el = rootRef.current;
      if (!el) return;
      const rect = el.getBoundingClientRect();
      const cx = rect.left + rect.width / 2;
      const cy = rect.top + rect.height / 2;
      const dx = e.clientX - cx;
      const dy = e.clientY - cy;
      const dist = Math.hypot(dx, dy) || 1;
      const maxOffset = rect.width * 0.06; // small, proportional "look toward" nudge
      setCursorGaze({ x: (dx / dist) * maxOffset, y: (dy / dist) * maxOffset });
    }

    function handleLeave() {
      setCursorGaze(null);
    }

    window.addEventListener("mousemove", handleMove);
    document.addEventListener("mouseleave", handleLeave);
    return () => {
      window.removeEventListener("mousemove", handleMove);
      document.removeEventListener("mouseleave", handleLeave);
    };
  }, [state]);

  // Overrides the ambient orb-eye-look CSS animation with a direct position
  // toward the cursor; removing the inline style (cursorGaze === null) lets
  // the CSS animation resume on its own.
  const eyeStyle: React.CSSProperties | undefined = cursorGaze
    ? { animation: "none", translate: `${cursorGaze.x}px ${cursorGaze.y}px` }
    : undefined;

  // Recording (mic input) and speaking (TTS output) both report live
  // amplitude through the same audioLevel prop, throttled Rust-side to
  // ~10x/sec — core scale and halo opacity are driven directly from it
  // rather than a fixed-timing CSS loop like the other states' ambient
  // ring-pulse. A short CSS transition (see .orb-glass__core / __halo)
  // smooths the discrete updates into a visibly voice-reactive motion.
  const isSpeaking = state === "speaking";
  const isLiveReactive = state === "recording" || isSpeaking;
  const coreStyle: React.CSSProperties | undefined = isLiveReactive
    ? { transform: `scale(${1 + level * 0.12})` }
    : undefined;
  const haloStyle: React.CSSProperties | undefined = isLiveReactive
    ? { opacity: 0.15 + level * 0.65 }
    : undefined;

  // Speaking: spawn a small particle drifting outward from the edge on each
  // amplitude spike, so the orb visibly "sparks" in time with the actual
  // speech waveform rather than on a fixed rhythm.
  const [particles, setParticles] = useState<Array<{ id: number; angle: number }>>([]);
  const particleIdRef = useRef(0);
  const lastSpikeAtRef = useRef(0);
  const SPIKE_THRESHOLD = 0.32;
  const MIN_SPIKE_INTERVAL_MS = 200;
  const PARTICLE_LIFETIME_MS = 900;
  const MAX_PARTICLES = 10;

  useEffect(() => {
    if (!isSpeaking || level < SPIKE_THRESHOLD) return;
    const now = Date.now();
    if (now - lastSpikeAtRef.current < MIN_SPIKE_INTERVAL_MS) return;
    lastSpikeAtRef.current = now;

    const id = particleIdRef.current++;
    const angle = Math.random() * 360;
    setParticles((ps) => [...ps.slice(-(MAX_PARTICLES - 1)), { id, angle }]);
    setTimeout(() => {
      setParticles((ps) => ps.filter((p) => p.id !== id));
    }, PARTICLE_LIFETIME_MS);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [level, isSpeaking]);

  return (
    <div
      ref={rootRef}
      // "orb-glass" — deliberately distinct from Canvas.tsx's unrelated
      // .voice-orb/.voice-orb__ring classes (the "Tap to speak" button),
      // which collided with these names and painted a solid legacy
      // background straight through this component.
      className={`orb-glass orb-glass--${state}`}
      style={{
        width: px,
        height: px,
        "--orb-color": STATE_COLOR_VAR[state],
        "--audio-level": level,
      } as React.CSSProperties}
      title={STATE_LABELS[state]}
      aria-label={STATE_LABELS[state]}
      role="img"
    >
      <span className="orb-glass__ring orb-glass__ring--1" />
      <span className="orb-glass__ring orb-glass__ring--2" />
      <span className="orb-glass__ring orb-glass__ring--3" />
      <span className="orb-glass__halo" style={haloStyle} />
      {pulseKey > 0 && <span key={pulseKey} className="orb-glass__flash" />}
      {particles.map((p) => (
        <span
          key={p.id}
          className="orb-glass__particle"
          style={{
            "--particle-angle": `${p.angle}deg`,
            "--particle-radius": `${px * 0.34}px`,
          } as React.CSSProperties}
        />
      ))}
      <span className="orb-glass__core" style={coreStyle}>
        <span className="orb-glass__face">
          {/* Gaze (translate) on the outer span; ambient blink on the lid,
              which always keeps running untouched. The wink is a separate
              overlay stacked on top rather than a class toggle on the lid
              itself — toggling the lid's own animation property would reset
              its blink timeline to 0% every time, drifting it out of sync
              with the other eye's uninterrupted cycle. */}
          <span className="orb-glass__eye" style={eyeStyle}>
            <span className="orb-glass__eye-lid" />
          </span>
          <span className="orb-glass__eye" style={eyeStyle}>
            <span className="orb-glass__eye-lid" />
            {isWinking && <span className="orb-glass__eye-lid orb-glass__eye-lid--wink-overlay" />}
          </span>
        </span>
        <span className="orb-glass__mouth-wrap">
          <span className="orb-glass__mouth" />
        </span>
      </span>
    </div>
  );
}
