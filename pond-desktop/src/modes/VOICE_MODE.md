# Voice Mode — Architecture & Developer Guide

Voice Mode is the primary interaction paradigm for Goose In A Pond. It provides a
local-first, always-on voice assistant experience: speak to Pond, see a live transcript,
and watch tool results surface inline as the agent works.

---

## State Machine

```
           ┌─────────────────────────────────────────────────────┐
           │                                                     │
           ▼                                                     │
  ┌──────────────┐   start_recording    ┌────────────────┐      │
  │     idle     │ ──────────────────►  │   recording    │      │
  └──────────────┘                      └────────────────┘      │
         ▲                                      │                │
         │                             stop/auto-stop            │
         │                                      ▼                │
    tts-end / error              ┌────────────────────┐         │
    auto-dismiss                 │     thinking        │         │
         │                       └────────────────────┘         │
         │                                      │                │
         │                             tts-start event           │
         │                                      ▼                │
         │                       ┌────────────────────┐         │
         └─────────────────────  │     speaking        │         │
                                 └────────────────────┘         │
                                                                 │
  ┌──────────────┐    pipeline-error / invoke failure           │
  │    error     │ ◄───────────────────────────────────────────┘
  └──────────────┘
         │
    auto-clear (4s)
         │
         ▼
       idle
```

### State Meanings

| State | Colour | What's happening |
|-------|--------|-----------------|
| `idle` | `#8E8E93` gray | Ready and waiting for activation |
| `recording` | `#8C52FF` purple | Mic open, capturing audio |
| `thinking` | `#FF9500` amber | Whisper + LLM processing |
| `speaking` | `#34C759` green | TTS playback |
| `error` | `#FF3B30` red | Pipeline failure; auto-clears after 4s |

---

## Tauri Events Contract

All events are emitted by the Rust backend (`pond-server` + Tauri commands).

| Event | Payload | When it fires |
|-------|---------|--------------|
| `audio-level` | `f32` (0–1) | Every ~30ms while recording — RMS amplitude |
| `recording-started` | — | Mic opened successfully |
| `recording-aborted` | — | Recording cancelled without sending |
| `transcript` | `string` | Whisper finished transcribing; text is the spoken query |
| `response-token` | `{ token: string, done: boolean }` | Each streamed LLM token |
| `tool-result` | `{ tool: string, data: Record<string, unknown>, timestamp_ms: number }` | Agent used a tool; `data` is the raw JSON result |
| `tts-start` | — | TTS playback begins |
| `tts-end` | — | TTS playback finished |
| `pipeline-error` | `string` | Any stage of the pipeline failed; payload is the error message |
| `desktop-summon` | — | Global hotkey pressed (also triggers `VOICE_ACTIVATE` action in AppContext) |

All events are wired in `AppContext.tsx`. `VoiceMode.tsx` only listens to `audio-level`
directly (to keep the waveform canvas update tight).

---

## AudioWaves Component

```typescript
import { AudioWaves } from "../components/AudioWaves";

<AudioWaves
  state={voiceState}   // VoiceState — drives colour + animation style
  audioLevel={level}   // 0–1 RMS from audio-level events (or 0 when not recording)
  size="lg"            // "sm" (40px, 5 bars) | "lg" (72px, 9 bars)
  style={...}          // optional React.CSSProperties passthrough
/>
```

### Animation per State

| State | Bars | Animation |
|-------|------|-----------|
| `idle` | 5/9 | Gentle breathing sine, 2.5s cycle, low amplitude |
| `recording` | 5/9 | Bars react to `audioLevel`; center bars tallest |
| `thinking` | 5/9 | Traveling sine wave — phase shifts left→right |
| `speaking` | 5/9 | Smooth pulsing sine, medium amplitude |
| `error` | 5/9 | Alternating flat bars, no animation |

The component uses:
- `ResizeObserver` for responsive canvas width
- `requestAnimationFrame` for 60fps rendering
- `devicePixelRatio` scaling for retina displays
- `roundRect()` with `fillRect()` fallback for bar drawing

### Adding a New State

1. Add the value to `VoiceState` in `reducer.ts`
2. Add a color entry to `COLORS` in `AudioWaves.tsx`
3. Add a `PHASE_STEP` entry
4. Add a `case` in the `switch (state)` block with the desired `heightFraction` formula
5. Add a label entry to `STATE_LABELS` in `VoiceMode.tsx`
6. Update `AppAction` in `reducer.ts` if the new state needs a new trigger action

---

## Auto-Stop Recording

`VoiceMode.tsx` reads `voice_recording_duration_secs` from `api.getSettings()` on mount
(default: 30s). When recording starts, a `setTimeout` is set; if the user doesn't
manually stop, `stopAndSend()` is called automatically.

A countdown ring (`CountdownRing` SVG component in `VoiceMode.tsx`) is shown in the
action bar while recording, showing seconds remaining using `stroke-dashoffset`.

---

## Transcript & Context Cards

`TranscriptFeed` accepts an optional `contextCards?: ContextCard[]` prop. After each
agent message bubble, it renders any cards whose `timestamp_ms` falls between that
message's timestamp and the next message's timestamp — placing tool results directly
below the Pond response that triggered them.

```typescript
<TranscriptFeed
  messages={state.transcript}
  contextCards={state.contextCards}
  fillHeight   // fills the parent flex container vertically
/>
```

---

## Known Limitations (Future Work)

| Feature | Status | Notes |
|---------|--------|-------|
| Wake word detection | Not implemented | Requires native binary (Porcupine / Silero VAD); OS mic permissions complex |
| Background process when window closed | Not implemented | Needs background daemon; hotkey already works when window is hidden |
| Audio device selection | Not implemented | `cpal` supports enumeration; blocked by Tauri state architecture |
| Streaming TTS output | Not implemented | Requires server-side changes to `/api/v1/tts` |
| Voice Activity Detection (VAD) | Not implemented | RMS threshold exists in `audio.rs`; deferred to avoid scope creep |
| Interrupt speaking | Partial | "Interrupt" button calls `abort_recording`; TTS abort needs Rust-side support |
