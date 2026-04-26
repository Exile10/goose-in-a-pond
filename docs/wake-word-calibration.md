# Wake-Word Phrase Calibration

Custom wake-word phrases can be trained during onboarding by recording the user saying their chosen
phrase several times. GIAP passes each recording through Whisper and stores the resulting transcription
variants in settings. The sliding-window detector then matches against any stored variant, making
detection robust to Whisper's inconsistent capitalisation and punctuation output.

---

## How It Works

Whisper does not produce a deterministic transcript — the same audio may yield:

```
"hey goose"
"hey, goose."
"Hey Goose!"
"a goose"
```

Storing **multiple observed transcriptions** (the _calibration variants_) and matching against any of
them removes this fragility. The detector uses `OR` matching: the wake word fires when the transcript
contains **any** stored variant.

```
User says "Hey Goose" (one breath)
        │
        ▼
cpal ring buffer captures audio continuously
        │
        ▼
Sliding 1500 ms window → POST /inference (whisper.cpp)
        │
        ▼
normalize_transcript("Hey, goose.") → "hey goose"
        │
        ▼
Is "hey goose" in calibration_variants?   ←── OR across all variants
        │ YES
        ▼
(optional hysteresis re-check)
        │
        ▼
Wake word confirmed — return captured command audio
```

---

## API Reference

### `POST /api/v1/voice/calibrate`

Submit one recording sample. Call this 3–5 times during the onboarding WakeWord step.

**Permission:** Public (accessible before onboarding is complete)

**Request** — `multipart/form-data`

| Field   | Type        | Required | Description                                      |
|---------|-------------|----------|--------------------------------------------------|
| `audio` | binary/file | yes      | WAV audio (16-bit mono 16 kHz recommended)       |

**Response** — `200 OK`

```json
{
  "transcript":    "hey goose",
  "normalized":    "hey goose",
  "all_variants":  ["hey goose", "hey, goose", "a goose"],
  "sample_count":  3,
  "target_count":  5,
  "complete":      false
}
```

| Field          | Type             | Description                                              |
|----------------|------------------|----------------------------------------------------------|
| `transcript`   | `string`         | Raw Whisper transcript for this sample                   |
| `normalized`   | `string`         | Normalised form stored by the detector                   |
| `all_variants` | `string[]`       | All unique normalized variants collected so far          |
| `sample_count` | `number`         | How many unique variants have been collected             |
| `target_count` | `number`         | Target samples needed for `complete: true` (currently 5)|
| `complete`     | `boolean`        | `true` when `sample_count >= target_count`               |

**Error responses**

| Status | Reason                                   |
|--------|------------------------------------------|
| 400    | Missing `audio` field or multipart error |
| 422    | No speech detected in the recording      |
| 502    | Whisper server unreachable               |
| 500    | Settings read/write failure              |

---

### `DELETE /api/v1/voice/calibrate`

Reset calibration — clears all stored variants. The detector falls back to the raw
normalized `voice_wake_word` phrase from settings.

**Permission:** Public

**Response** — `200 OK`

```json
{
  "cleared": true,
  "message": "Wake-word calibration data cleared"
}
```

---

## Frontend Implementation Guide

### Minimal flow (5-sample recording loop)

```typescript
const TARGET_SAMPLES = 5;

async function runWakeWordCalibration(phrase: string): Promise<void> {
  // 1. Let the user set their phrase (saved via PUT /api/v1/settings)
  await api.updateSettings({ voice_wake_word: phrase });

  // 2. Optionally reset any previous calibration data
  await fetch('/api/v1/voice/calibrate', { method: 'DELETE' });

  let sampleCount = 0;

  while (sampleCount < TARGET_SAMPLES) {
    // 3. Tell the user to speak the phrase
    showPrompt(`Say "${phrase}" now... (${sampleCount + 1} / ${TARGET_SAMPLES})`);

    // 4. Record a 2–3 second clip from the microphone
    const wavBlob = await recordWav(2500);

    // 5. Submit to the calibration endpoint
    const form = new FormData();
    form.append('audio', wavBlob, 'sample.wav');

    const resp = await fetch('/api/v1/voice/calibrate', {
      method: 'POST',
      body: form,
    });
    const data = await resp.json();

    if (!resp.ok) {
      showError(data.error);
      continue; // let the user retry
    }

    sampleCount = data.sample_count;
    showProgress(`Heard: "${data.transcript}" (${sampleCount} / ${TARGET_SAMPLES})`);

    if (data.complete) break;
  }

  showSuccess('Wake-word calibration complete!');
}
```

### Recording helper (`recordWav`)

The server expects a WAV file. Browser `MediaRecorder` produces WebM/Opus by default — you need
to convert to WAV before submitting. Use `audiobuffer-to-wav` or a small encoder:

```typescript
async function recordWav(durationMs: number): Promise<Blob> {
  const stream   = await navigator.mediaDevices.getUserMedia({ audio: true });
  const ctx      = new AudioContext({ sampleRate: 16000 });
  const source   = ctx.createMediaStreamSource(stream);
  const recorder = ctx.createScriptProcessor(4096, 1, 1);
  const chunks: Float32Array[] = [];

  recorder.onaudioprocess = (e) => {
    chunks.push(new Float32Array(e.inputBuffer.getChannelData(0)));
  };
  source.connect(recorder);
  recorder.connect(ctx.destination);

  await new Promise(r => setTimeout(r, durationMs));

  stream.getTracks().forEach(t => t.stop());
  recorder.disconnect();
  source.disconnect();

  // Concatenate all chunks
  const total = chunks.reduce((n, c) => n + c.length, 0);
  const pcm   = new Float32Array(total);
  let   pos   = 0;
  for (const chunk of chunks) { pcm.set(chunk, pos); pos += chunk.length; }

  return encodeWav(pcm, 16000);
}

function encodeWav(samples: Float32Array, sampleRate: number): Blob {
  const dataLen   = samples.length * 2;
  const buffer    = new ArrayBuffer(44 + dataLen);
  const view      = new DataView(buffer);
  const writeStr  = (off: number, str: string) =>
    [...str].forEach((c, i) => view.setUint8(off + i, c.charCodeAt(0)));
  const writeU16  = (off: number, v: number) => view.setUint16(off, v, true);
  const writeU32  = (off: number, v: number) => view.setUint32(off, v, true);

  writeStr(0, 'RIFF');  writeU32(4, 36 + dataLen);  writeStr(8, 'WAVE');
  writeStr(12, 'fmt '); writeU32(16, 16);            writeU16(20, 1);
  writeU16(22, 1);      writeU32(24, sampleRate);    writeU32(28, sampleRate * 2);
  writeU16(32, 2);      writeU16(34, 16);            writeStr(36, 'data');
  writeU32(40, dataLen);

  let off = 44;
  for (const s of samples) {
    view.setInt16(off, Math.max(-32768, Math.min(32767, s * 32767)), true);
    off += 2;
  }
  return new Blob([buffer], { type: 'audio/wav' });
}
```

### Displaying calibration progress

```typescript
interface CalibrateResponse {
  transcript:    string;
  normalized:    string;
  all_variants:  string[];
  sample_count:  number;
  target_count:  number;
  complete:      boolean;
}
```

Show the user the raw `transcript` so they can see what Whisper heard ("Did you say: *hey goose*?"),
and a progress bar driven by `sample_count / target_count`.

---

## `PondApiClient` integration (`TypeScript`)

Add these two methods to your `PondApiClient` class:

```typescript
/** Submit one WAV recording as a calibration sample. */
async calibrateWakeWord(wav: Blob): Promise<CalibrateResponse> {
  const form = new FormData();
  form.append('audio', wav, 'sample.wav');
  const resp = await this.fetch('/api/v1/voice/calibrate', {
    method: 'POST',
    body:   form,
  });
  if (!resp.ok) {
    const data = await resp.json();
    throw new Error(data.error ?? `calibration failed (${resp.status})`);
  }
  return resp.json() as Promise<CalibrateResponse>;
}

/** Clear all calibration data and revert to the raw wake-word phrase. */
async resetWakeWordCalibration(): Promise<void> {
  const resp = await this.fetch('/api/v1/voice/calibrate', { method: 'DELETE' });
  if (!resp.ok) throw new Error(`reset failed (${resp.status})`);
}
```

---

## Onboarding Integration

The WakeWord step (`OnboardingStep::WakeWord`) is the canonical place to run calibration.
The recommended UX sequence is:

1. Show a text input for the user's desired phrase (default: "goose"). Save it via `PUT /api/v1/settings`.
2. Show a microphone button with the label **"Say your phrase now"**.
3. On each click: record 2.5 s, submit to `POST /api/v1/voice/calibrate`, display what was heard.
4. After 5 successful samples (`complete: true`), enable **"Continue"** to advance onboarding.
5. Include a **"Start over"** button that calls `DELETE /api/v1/voice/calibrate` and resets `sample_count` to 0.

Minimum viable calibration is 3 samples (the detector is usable before 5 if you pass
`sample_count >= 3` as a threshold — adjust the UI copy accordingly).

---

## Settings Storage

Calibration data is persisted in `pond_system.db` as a JSON array:

| Settings key                       | Type         | Default |
|------------------------------------|--------------|---------|
| `voice_wake_word`                  | `String`     | `"goose"` |
| `voice_wake_word_transcriptions`   | `Vec<String>` | `[]` (uncalibrated) |

When `voice_wake_word_transcriptions` is empty the detector normalises `voice_wake_word` and uses
it as the sole trigger — no calibration required for the default "goose" phrase.

---

## Architecture Notes

The calibration system is intentionally thin:

- **No ML training** — "training" means storing Whisper's own transcriptions of the user's voice.
  Whisper already handles the acoustic modelling; we just teach the detector what strings Whisper
  is likely to produce for this user's pronunciation.
- **No audio stored** — only the normalized text variants are persisted, not the raw WAV.
- **Stateless accumulation** — each `POST /api/v1/voice/calibrate` call is idempotent with respect
  to duplicate variants (they're de-duplicated before saving).
- **Backward compatible** — `voice_wake_word_transcriptions` defaults to `[]`; existing installs
  with no calibration data continue to work exactly as before.

---

## Detector Tuning Reference

All settings below are persisted in `pond_system.db` and writable via `PUT /api/v1/settings`.

| Setting | Default | Range | Description |
|---|---|---|---|
| `voice_wake_word` | `"goose"` | any string | The activation phrase |
| `voice_kws_whisper_url` | `null` (= `voice_whisper_url`) | URL | Separate fast-model whisper for KWS |
| `voice_kws_energy_threshold` | `0.01` | `0.0–1.0` | Min RMS to call whisper. `0.0` = disabled |
| `voice_kws_post_trigger_silence_ms` | `400` | `0–4000` | Silence to end capture early. `0` = disabled |
| `voice_kws_cooldown_ms` | `2000` | `0–10000` | Sleep before re-arming after activation |
| `voice_recording_duration_secs` | `3` | `1–30` | Hard ceiling for fresh command recording |

### Calibrating `voice_kws_energy_threshold`

Run with `RUST_LOG=trace` and stay quiet — the trace log shows the RMS of each skipped window:

```
TRACE pond_adapters_whisper: KWS: silent window skipped (rms=0.0082)
```

Set the threshold ~20% above the highest idle RMS you observe. In a noisy open-plan office you
may need `0.025`; in a quiet room `0.008` is fine.

### Tuning `voice_kws_post_trigger_silence_ms`

Lower values exit the capture window sooner but risk clipping the end of long commands. Increase
if you notice commands being cut off mid-sentence. Disable (`0`) if commands are consistently
long or your room has significant reverb.

### Tuning `voice_kws_cooldown_ms`

Set to at least the length of your longest TTS response plus ~500 ms for room settling. The
startup print in `pond-server chat` shows the active value:

```
  Energy gate:   0.010 RMS  |  cooldown: 2000ms  |  VAD silence: 400ms
```
