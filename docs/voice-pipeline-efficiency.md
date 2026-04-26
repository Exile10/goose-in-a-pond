# Voice Pipeline Efficiency Guide

This document covers performance characteristics, tuning parameters, and optimisation techniques
for GIAP's wake-word detection and voice pipeline.

---

## End-to-End Latency Map

```
User speaks "Hey Goose, what's the weather?"
       │
       │  ── WAIT state ─────────────────────────────────────────────────────────
       │
       ├─ [cpal ring buffer] ← continuous mic audio (no gaps)
       │
       ├─ [Energy gate] ─── RMS < 0.01? → skip window (~90% of windows during silence)
       │
       ├─ [Slide timer]  ──  every 500 ms
       │        │
       │        └─ Snapshot 1500 ms window
       │             Resample to 16 kHz
       │             Encode WAV
       │             POST /inference → whisper.cpp (KWS model)    ← 0.3–2s
       │             normalize_transcript()
       │             Contains trigger?
       │                 No  → continue (check hysteresis state)
       │                 Yes + ≤3 tokens → hysteresis re-check at 200ms
       │                 Yes (confirmed) → ▼
       │
       ├─ [VAD post-trigger] ── poll RMS every 50 ms
       │        "Hey Goose, [what's the weather?]" ← captured in ring buffer
       │        Exit when silence ≥ 400 ms  OR  elapsed ≥ 4000 ms ceiling     ← 0.4–1s typical
       │
       │  ── LISTEN state ────────────────────────────────────────────────────────
       │
       ├─ prime_with_captured(wav)  ← hand ring-buffer audio to WhisperInput
       │
       ├─ WhisperInput::listen() → POST /inference → whisper.cpp (ASR model)   ← 1–5s
       │
       │  ── THINK state ─────────────────────────────────────────────────────────
       │
       ├─ [Quip TTS spawned] ← "Let me think…" plays in parallel               ← 1–2s (parallel)
       │
       ├─ LLM streaming (token by token)                                         ← 1–10s
       │
       │  ── SPEAK state ─────────────────────────────────────────────────────────
       │
       └─ Sentence-chunked TTS ← speaks each sentence as it arrives             ← 0.5–1.5s/sentence

       ── WAIT (next turn) ──────────────────────────────────────────────────────
       │
       └─ Cooldown sleep (2000 ms) ← prevents TTS-echo re-trigger
```

### Typical latency targets

| Optimisation state | Wake detect | Post-trigger | ASR | Total to first LLM token |
|---|---|---|---|---|
| Before optimisations | 1–5 s | 4 s (fixed) | 1–5 s | **6–14 s** |
| After P0 (energy gate + VAD) | 0.5–2 s | 0.4–1 s | 1–5 s | **2–8 s** |
| After P1 (tiered models) | 0.3–0.5 s | 0.4–1 s | 1–5 s | **1.7–6.5 s** |

---

## `KeywordDetectorConfig` Reference

All fields are available via `KeywordDetectorConfig::default()` and the `Settings` DB.

| Field | Default | Settings key | Purpose |
|---|---|---|---|
| `window_ms` | 1500 | — | Detection window width (ms) |
| `slide_ms` | 500 | — | Window advance per cycle (ms) |
| `post_trigger_ms` | 4000 | — | Hard ceiling on post-trigger capture (ms) |
| `hysteresis_enabled` | true | — | Re-check ≤3-token matches before firing |
| `hysteresis_slide_ms` | 200 | — | Slide during hysteresis re-check (ms) |
| `energy_threshold` | 0.01 | `voice_kws_energy_threshold` | Min RMS to call whisper (0 = disable) |
| `post_trigger_silence_ms` | 400 | `voice_kws_post_trigger_silence_ms` | Silence to end capture early (0 = disable) |
| `cooldown_ms` | 2000 | `voice_kws_cooldown_ms` | Sleep before re-arming after activation (ms) |

---

## Optimisations Implemented

### 1. Energy/RMS Gate

**Problem:** ~80–90% of whisper HTTP calls happen on silence. Each call takes 0.3–5s depending
on the model.

**Solution:** Before resampling and encoding each detection window, compute the RMS of the audio
snapshot. If `rms < energy_threshold`, skip the window entirely — no HTTP call is made.

```rust
fn rms_energy(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}
```

**Impact:**
- Eliminates ~90% of whisper calls in a quiet room
- Prevents ambient-noise false positives from ever reaching transcript matching
- Reduces CPU, network load, and whisper server queueing

**Tuning `energy_threshold`:**

```bash
# Run calibration to see what RMS your mic reports at idle:
pond-server chat --input whisper --wake-word goose
# Watch the trace logs (RUST_LOG=trace) for "silent window skipped (rms=…)"

# Typical values:
# Very quiet room / close mic:     0.005
# Normal room noise:               0.01  (default)
# Loud environment / far mic:      0.02–0.05
# Disable (always call whisper):   0.0
```

Set via API:
```bash
curl -X PUT http://localhost:4000/api/v1/settings \
  -H "Content-Type: application/json" \
  -d '{"voice_kws_energy_threshold": 0.02}'
```

---

### 2. VAD-Gated Post-Trigger Capture

**Problem:** After wake-word confirmation the loop slept `post_trigger_ms = 4000 ms` unconditionally.
For a 1-word command ("goose, time?") that adds 3.5 seconds of unnecessary dead time.

**Solution:** After detection fires, poll the ring buffer's RMS every 50 ms. When RMS stays below
`energy_threshold` for `post_trigger_silence_ms` consecutive milliseconds, snapshot the captured
audio immediately — no need to wait the full ceiling.

```
"Hey Goose,   what's  the  weather?"   [silence…]
  ├── wake ──┤          │               ├ 400ms ┤
             │          └── captured ───┘       └── returned
             └── ring buffer continuous
```

**Impact:** Reduces average post-trigger wait from 4000 ms to 400–1000 ms for typical commands.
**~2–3 second improvement** on every voice interaction.

**Tuning `post_trigger_silence_ms`:**

| Value | Behaviour |
|---|---|
| `0` | Disabled — always wait full `post_trigger_ms` |
| `200` | Aggressive — exits on short word gaps (may clip "um… what's the weather") |
| `400` | Default — good for most commands |
| `600` | Conservative — handles slow speakers and sentence pauses |
| `1000` | Very patient — unlikely to clip any natural speech |

---

### 3. Dead-Zone Cooldown

**Problem:** After TTS finishes speaking, residual room echo or the assistant's own voice can
re-trigger the wake-word detector on the next cycle.

**Solution:** At the start of each new detection cycle, `detection_loop` sleeps `cooldown_ms`
before opening the audio stream. This window absorbs TTS echo and lets the room settle.

**Impact:** Prevents ghost activations at zero cost to detection latency (the cooldown runs while
the assistant is still speaking or just finished).

**Tuning `cooldown_ms`:**

| Value | Situation |
|---|---|
| `0` | Disable (not recommended in rooms with echo) |
| `1000` | Anechoic / headset mic |
| `2000` | Default — covers most TTS responses |
| `3000–5000` | Large room / strong echo / far-field mic array |

---

### 4. Tiered Whisper Models

**Problem:** Wake-word detection only needs to recognise a small vocabulary. Using `base.en` (2s
inference) for keyword spotting is wasteful. Command transcription needs accuracy.

**Solution:** Point `voice_kws_whisper_url` at a separate whisper server running the `tiny` model
while `voice_whisper_url` keeps `base`/`small` for command ASR.

```bash
# Terminal 1 — tiny model for KWS (fast, ~39 MB)
./whisper-server -m models/ggml-tiny.en.bin --port 9001 --host 127.0.0.1

# Terminal 2 — base model for ASR (accurate, ~74 MB)
./whisper-server -m models/ggml-base.en.bin --port 9000 --host 127.0.0.1
```

```bash
# Configure GIAP to use tiered models
curl -X PUT http://localhost:4000/api/v1/settings \
  -H "Content-Type: application/json" \
  -d '{
    "voice_whisper_url":     "http://127.0.0.1:9000",
    "voice_kws_whisper_url": "http://127.0.0.1:9001"
  }'
```

**Impact:** Detection window processing drops from ~2s (base) to ~0.3s (tiny). ASR quality
unchanged. Both models fit in RAM simultaneously on Jetson Orin Nano (8 GB).

---

## Whisper Model Comparison

| Model | Size | Inference (CPU) | Inference (Jetson) | Accuracy | Best for |
|---|---|---|---|---|---|
| `tiny.en` | 39 MB | ~0.3s | ~0.1s | Good for known phrases | KWS only |
| `base.en` | 74 MB | ~2s | ~0.5s | Good | KWS + short commands |
| `small.en` | 244 MB | ~5s | ~1.5s | Better | Dictation / long commands |
| `medium.en` | 769 MB | ~15s | ~5s | Best | High-accuracy ASR |

Recommendation for `voice_kws_whisper_url`: `tiny.en`
Recommendation for `voice_whisper_url` (ASR): `base.en` or `small.en`

---

## Device Tuning Profiles

### Raspberry Pi 4 (4 GB)

```json
{
  "voice_kws_energy_threshold": 0.015,
  "voice_kws_post_trigger_silence_ms": 500,
  "voice_kws_cooldown_ms": 3000,
  "voice_whisper_url": "http://127.0.0.1:9000",
  "voice_kws_whisper_url": "http://127.0.0.1:9001"
}
```
Notes: Run `tiny.en` on 9001, `base.en` on 9000. Pi 4 is slow — tiered models make the biggest difference here. Set `slide_ms = 800` via config builder if CPU is maxed.

### Jetson Orin Nano (8 GB)

```json
{
  "voice_kws_energy_threshold": 0.01,
  "voice_kws_post_trigger_silence_ms": 400,
  "voice_kws_cooldown_ms": 2000,
  "voice_whisper_url": "http://127.0.0.1:9000",
  "voice_kws_whisper_url": "http://127.0.0.1:9001"
}
```
Notes: Build whisper.cpp with CUDA support (`make clean && WHISPER_CUDA=1 make`). Inference drops to ~0.5s for `base.en`. Both models load simultaneously with ~120 MB VRAM.

### Apple Silicon Mac (dev)

```json
{
  "voice_kws_energy_threshold": 0.008,
  "voice_kws_post_trigger_silence_ms": 300,
  "voice_kws_cooldown_ms": 1500
}
```
Notes: Same model for KWS and ASR is fine on Apple Silicon — `base.en` runs in ~0.2s with Metal. Single server on port 9000 is sufficient.

---

## Planned Optimisations (Not Yet Implemented)

### Non-Blocking Parallel Whisper Calls (P1)

Currently the detection loop is synchronous — the slide timer waits for each whisper response
before the next window. On slow hardware this means detection latency = `slide_ms + whisper_latency`
instead of just `slide_ms`.

The fix is a "latest-wins" channel:
```
[slide timer]  →  bounded(1) channel  →  [whisper worker thread]
      │                                          │
      └──── reads result_rx for transcript ──────┘
```

When a new window is ready while the worker is still processing the previous one, the old window
is discarded (latest wins). The slide timer always sleeps exactly `slide_ms`. This brings detection
latency back to ~`slide_ms` regardless of server response time.

### Incremental 16 kHz Resample Buffer (P2)

66% of each 1500 ms window overlaps the previous (500 ms slide ÷ 1500 ms window). The current
implementation resamples the full window from scratch every 500 ms — discarding 66% of the work.

The fix is to maintain a 16 kHz ring buffer alongside the raw ring buffer, filled by an online
downsampler in the `cpal` callback. The detection loop snapshots directly from the 16 kHz buffer,
eliminating per-window resampling entirely.

---

## Calibrating the Energy Gate

The energy gate threshold should be set just above the ambient noise floor of your environment.

1. Start GIAP with `RUST_LOG=trace`:
   ```bash
   RUST_LOG=trace cargo run -p pond-server -- chat --input whisper --wake-word goose
   ```

2. Stay quiet for 10 seconds. Watch for lines like:
   ```
   TRACE pond_adapters_whisper: KWS: silent window skipped (rms=0.0082)
   ```

3. Note the highest RMS value seen during silence. Set `voice_kws_energy_threshold` to
   ~20% above that value.

4. Speak normally and verify windows are no longer skipped during speech:
   ```
   DEBUG pond_adapters_whisper: KWS window: "hey goose" ...
   ```

5. Save the calibrated value:
   ```bash
   curl -X PUT http://localhost:4000/api/v1/settings \
     -H "Content-Type: application/json" \
     -d '{"voice_kws_energy_threshold": 0.012}'
   ```
