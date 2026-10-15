# Wake-Word Phrase Calibration

Custom wake-word phrases are calibrated by recording the user reading a few sentences that contain
the phrase. GIAP passes each recording through Whisper, finds the wake phrase *as heard* inside the
transcript (a mishearing such as "gus" for "goose"), and stores only that as a trigger variant. The
sliding-window detector then fires on the wake word itself or any stored variant, as whole words.

---

## How It Works

Whisper does not produce a deterministic transcript — the same audio may yield:

```
"hey goose"
"hey, goose."
"Hey Goose!"
"a goose"
```

Normalization removes the punctuation and case. What it cannot remove is a mishearing, so calibration
stores how Whisper hears the phrase in this user's voice (the _calibration variants_). The detector
uses `OR` matching: it fires when the transcript contains the wake word **or** any stored variant as
whole words, so "goose" never fires on "mongoose".

Only the phrase is ever stored, never the calibration sentence around it. The server aligns the
transcript with the sentence that was read out and keeps the words standing where the phrase stands,
so "Hey gus, good morning" stores "gus" (not "good", which spells closer to "goose"). Variants stored
by older builds as whole sentences are reduced to their phrase when the detector loads them.

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
Does it contain the wake word or a variant, as whole words?   ←── OR across all variants
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

Submit one recording of a calibration sentence. Call it once per take, about five times.

**Permission:** Public (accessible before onboarding is complete)

**Request** — `multipart/form-data`

| Field    | Type        | Required | Description                                                     |
|----------|-------------|----------|-----------------------------------------------------------------|
| `audio`  | binary/file | yes      | WAV audio (16-bit mono 16 kHz recommended)                      |
| `prompt` | text        | no       | The sentence that was read out; locates the phrase in the take. Without it, each sentence from `calibration_prompts` is tried |

**Response** — `200 OK`

```json
{
  "transcript":    "Hey gus, good morning.",
  "normalized":    "hey gus good morning",
  "heard":         "gus",
  "new_variant":   true,
  "all_variants":  ["gus"],
  "sample_count":  1,
  "target_count":  5,
  "complete":      false
}
```

| Field          | Type             | Description                                              |
|----------------|------------------|----------------------------------------------------------|
| `transcript`   | `string`         | Raw Whisper transcript for this take                     |
| `normalized`   | `string`         | The whole transcript, normalized (not what is stored)    |
| `heard`        | `string \| null` | The wake phrase as heard; `null` when the take missed it |
| `new_variant`  | `boolean`        | Whether `heard` was new and became a trigger             |
| `all_variants` | `string[]`       | All stored variants                                      |
| `sample_count` | `number`         | How many variants are stored, **not** how many takes      |
| `target_count` | `number`         | Takes the client should ask for (currently 5)            |
| `complete`     | `boolean`        | `sample_count >= target_count`; kept for older clients    |

Hearing the phrase correctly every time is the good outcome and stores nothing new, so a client
counts its own successful takes (a take with a non-null `heard`) rather than `sample_count`.

**Error responses**

| Status | Reason                                   |
|--------|------------------------------------------|
| 400    | Missing `audio` field or multipart error |
| 422    | No speech detected in the recording      |
| 502    | Whisper server unreachable               |
| 500    | Settings read/write failure              |

---

### `DELETE /api/v1/voice/calibrate`

Reset calibration — clears all stored variants. The detector keeps the normalized
`voice_wake_word` phrase from settings, which is always a trigger.

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

  const prompts = buildCalibrationPrompts(phrase); // src/modes/voice/wakePhrase.ts
  let takes = 0;

  while (takes < TARGET_SAMPLES) {
    // 3. Show the sentence to read
    const prompt = prompts[takes % prompts.length];
    showPrompt(`Read: "${prompt}" (${takes + 1} / ${TARGET_SAMPLES})`);

    // 4. Record a 3.5 second clip from the microphone
    const wavBlob = await recordWav(3500);

    // 5. Submit it with the sentence, so the server can find the phrase
    const form = new FormData();
    form.append('audio', wavBlob, 'sample.wav');
    form.append('prompt', prompt);

    const resp = await fetch('/api/v1/voice/calibrate', {
      method: 'POST',
      body: form,
    });
    const data = await resp.json();

    if (!resp.ok) {
      showError(data.error);
      continue; // let the user retry
    }

    if (data.heard === null) {
      showError(`That take did not catch "${phrase}". Try again.`);
      continue;
    }
    takes += 1;
    showProgress(`Heard: "${data.transcript}" (${takes} / ${TARGET_SAMPLES})`);
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
  heard:         string | null;
  new_variant:   boolean;
  all_variants:  string[];
  sample_count:  number;
  target_count:  number;
  complete:      boolean;
}
```

Show the user the raw `transcript` so they can see what Whisper heard ("Did you say: *hey goose*?"),
and a progress bar driven by successful takes over `target_count`.

---

## `PondApiClient` integration (`TypeScript`)

Add these two methods to your `PondApiClient` class:

```typescript
/** Submit one WAV recording of `prompt` as a calibration take. */
async calibrateWakeWord(wav: Blob, prompt?: string): Promise<CalibrateResponse> {
  const form = new FormData();
  form.append('audio', wav, 'sample.wav');
  if (prompt) form.append('prompt', prompt);
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
3. On each click: record 3.5 s of the shown sentence, submit it with its `prompt`, display what was heard.
4. After 5 takes with a non-null `heard`, enable **"Continue"** to advance onboarding.
5. Include a **"Start over"** button that calls `DELETE /api/v1/voice/calibrate` and resets the take count to 0.

Calibration is optional: the detector always fires on the wake word itself, and variants only add
the user's mishearings.

---

## Settings Storage

Calibration data is persisted in `pond_system.db` as a JSON array:

| Settings key                       | Type         | Default |
|------------------------------------|--------------|---------|
| `voice_wake_word`                  | `String`     | `"goose"` |
| `voice_wake_word_transcriptions`   | `Vec<String>` | `[]` (uncalibrated) |

The detector always normalises `voice_wake_word` and uses it as a trigger, then adds the stored
variants and, for "goose" and "hey goose", a few built-in mishearings. `voice_wake_word` must be one
to four words of letters (apostrophes and hyphens allowed); the server refuses anything else, since
the detector's normalization would drop it and the phrase could never match.

---

## Architecture Notes

The calibration system is intentionally thin:

- **No ML training** — "training" means storing Whisper's own transcriptions of the user's voice.
  Whisper already handles the acoustic modelling; we just teach the detector what strings Whisper
  is likely to produce for this user's pronunciation.
- **No audio stored** — only the wake phrase as heard is persisted, not the raw WAV or the sentence.
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
| `voice_kws_energy_threshold` | `0.003` | `0.0–1.0` | Min RMS to call whisper. `0.0` = disabled |
| `voice_kws_post_trigger_silence_ms` | `400` | `0–4000` | Silence to end capture early. `0` = disabled |
| `voice_kws_cooldown_ms` | `2000` | `0–10000` | Sleep before re-arming after activation |
| `voice_recording_duration_secs` | `3` | `1–300` | Recording ceiling for the browser voice path only; the voice loop ends on silence |

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
