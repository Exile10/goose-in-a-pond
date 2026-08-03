# Voice mode

One loop: wake word, request, working tone, spoken answer. This is what the
terminal path (`pond-server chat`) and the desktop shell both run — the desktop
spawns exactly this command as a child process.

## Running it

```bash
cargo run -p pond-server -- chat --input whisper --tts piper
```

On the Jetson, `bash scripts/giap.sh` and the desktop app both take this path;
the flags above are what the desktop passes.

You should see:

```
  Goose in a Pond 0.1.0 — voice

  Listen   ggml-small.en
  Wake     "goose"
  Speak    en_US-lessac-medium
  Log      ~/Library/Application Support/goose-in-a-pond/logs/pond.log

  say "goose"
```

Then: say **"goose"**, keep talking, and the request lands in one breath —
"Goose, what's the weather in Nairobi?" is a single utterance, not a wake word
followed by a pause followed by a command.

Every line printed above is load-bearing. If a model failed to resolve, its line
says so instead of being absent, because a silent voice assistant and a broken
one look identical from the outside.

## The turn

| | What the user gets |
|---|---|
| Wake word heard | a short two-tone ping, immediately |
| Working | a soft chime every ~2.6s, mostly silence |
| Answer ready | the tone stops, the reply is spoken sentence by sentence |

There is **no spoken filler** and **no tool narration**. Both existed; both said
what the tone already says, and both had to finish synthesizing and playing
before the answer could start — delaying the answer in order to announce that
the answer was coming.

## Tuning

`KeywordDetectorConfig` (`crates/pond-adapters-whisper/src/lib.rs`). Three
settings override it at runtime: `voice_kws_energy_threshold`,
`voice_kws_cooldown_ms`, `voice_kws_post_trigger_silence_ms`.

| Knob | Default | What moving it does |
|---|---|---|
| `window_ms` | 1400 | Audio fed to whisper per cycle. Wider gives the model more to invent context from and costs more every cycle. |
| `slide_ms` | 200 | Reaction floor. The wake word cannot be noticed sooner than the next slide plus one transcription. |
| `lookback_ms` | 900 | How far back the capture reaches past the trigger. **Too low clips the first words of every request.** |
| `post_trigger_ms` | 12000 | Ceiling on capture. Silence normally ends it far sooner. |
| `energy_threshold` | 0.010 | Below this, a window never reaches whisper. Raise if a noisy room keeps waking it. |
| `silence_threshold` | 0.0025 | Below this counts as end-of-speech. **Raise it and quiet sentence endings get cut off.** |
| `post_trigger_silence_ms` | 800 | Silence that ends the capture. Raise if it cuts you off mid-thought. |
| `cooldown_ms` | 600 | Settling time before re-arming, so the reply's own tail cannot re-trigger. |

The invariants between these are asserted in tests (`the_lookback_covers_...`,
`the_ring_holds_everything_both_readers_can_ask_for`,
`ending_a_sentence_is_judged_more_leniently_than_waking_whisper`). Retune the
numbers freely; if a test fails, the relationship broke, not the number.

## Why detection changed

**Whole-word matching.** `transcript.contains("goose")` fired on "mongoose" and
"gooseberry". It now matches word sequences (`pond_voice::text::contains_trigger`).

**No more two-window re-check.** Any transcript of three tokens or fewer used to
require detection in two consecutive windows — and "goose" is always one token,
so every real activation needed whisper to independently produce the same
one-syllable word twice, 150 ms apart, from different audio. It frequently
declined. The re-check existed to suppress the substring false positives that
whole-word matching now rejects outright, in one pass.

**Lookback.** Detection lags the wake word by a slide plus a transcription.
Capture used to start *after* the trigger fired, so everything spoken in that gap
was discarded — "Goose, what's the weather" arrived as "the weather". The capture
now reaches backwards, and the wake word is removed from the *transcript*
(`strip_leading_wake_word`) rather than from the audio.

## Why ASR changed

Two profiles, because the callers want opposite things
(`TranscribeOpts`, `crates/pond-adapters-whisper/src/in_process.rs`):

- **`wake_word()`** — greedy, 2 threads, mel context capped to the clip. Runs
  several times a second forever and only has to recognise one known word.
- **`accurate()`** — beam search (width 5), all threads, full context,
  non-speech tokens suppressed, temperature fallback on low-confidence
  segments. Runs once per turn, and its output *becomes the model's prompt* — a
  word lost here is the whole request misunderstood.

Measured on `tests/blobs/jfk.wav` with `ggml-base.en.bin`:

```
cheap      "And so my fellow Americans asked not what your country can do for you, … ♪"
accurate   "And so my fellow Americans, ask not what your country can do for you, …"
```

Greedy produced "**asked** not" and a spurious music annotation. Beam search was
also *faster* on this clip. Reproduce it:

```bash
WHISPER_TEST_MODEL="$HOME/Library/Application Support/goose-in-a-pond/models/ggml-base.en.bin" \
  cargo test -p pond-adapters-whisper --features metal decode_profiles -- --ignored --nocapture
```

## Logs

The console is deliberately near-silent: warnings and errors only, plus the
curated turn lines (which are printed directly, not through tracing). Everything
else goes to `<data_dir>/logs/pond.log.<date>`, which is named on startup.

The file keeps GIAP's voice crates at `debug` — which windows matched, what the
transcript was before and after the wake word came off, which voice synthesized.
Those are the lines you need when a session misbehaves, and requiring a
reproduction with `RUST_LOG=debug` means the interesting run is always the one
that was not recorded.

Third-party native libraries are silenced by name: llama.cpp, ggml, whisper.cpp,
ONNX Runtime, goose. They warn about things a user cannot act on. Anything
genuinely wrong with GIAP is logged by GIAP.

Two traps that produced most of the old console noise, both fixed:

- **whisper.cpp bypassed tracing entirely** until `install_logging_hooks()` was
  called, so no filter could reach it. That was the
  `whisper_full_with_state: decoder 0: score = …` wall.
- **The quiet console filter replaced the carve-outs instead of adding to
  them.** A bare `warn` directive is not "warn, keeping the rules above" — every
  target silenced above came back at WARN on precisely the surface the setting
  exists to keep quiet.

`RUST_LOG` still overrides both layers for a full-verbosity session.

## Known remaining noise

`clip_model_loader: tensor[N]: …` — roughly 1400 lines when a model with a
vision/audio projector (e.g. Gemma-4 E2B) loads. This is llama.cpp's `clip.cpp`
writing **directly to stderr**: it uses `common_log`, not the `llama_log_set`
callback that `send_logs_to_tracing` installs, so no tracing filter can see it.
The C struct `mtmd_context_params` has a `verbosity` field, but
`llama-cpp-2` 0.1.146's `MtmdContextParams` wrapper does not expose it.

Two ways out, neither taken here because both reach past the voice layer:

1. A goose-fork patch constructing the raw `mtmd_context_params` with
   `verbosity = GGML_LOG_LEVEL_ERROR` (see `docs/goose-patch-management.md`).
2. Not loading the projector at all for voice sessions — it is a vision/audio
   encoder that a voice-only turn never uses, and on an 8 GB Jetson it is not
   free. This changes image-input behaviour, so it is a product decision.
