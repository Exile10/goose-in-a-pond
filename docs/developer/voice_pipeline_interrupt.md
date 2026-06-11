# Voice Pipeline Interrupt System

The wake word can interrupt GIAP during inference or TTS playback. Saying the wake word while the assistant is thinking or speaking immediately stops output and captures a new request.

Last updated: April 29, 2026

---

## How It Works

```
[GIAP speaking or thinking]
    |
User says wake word ("Hey Goose!")
    |
    v
tokio::select! races:
    Branch A: chat_stream_once() -- agent generating/speaking
    Branch B: wake_word_detector.wait_for_activation_with_audio()
    |
    v  (wake word wins)
1. stop_speaking() -- cuts TTS mid-sentence (<50ms)
2. stop_thinking_tone() -- silences ambient tone
3. Drop agent stream -- stops consuming LLM tokens
4. Capture new speech from wake word activation audio
5. Process new request immediately
```

---

## VoiceOutput: Interruptible Playback

**File:** `crates/pond-core/src/models/ports/voice_output.rs`

```rust
pub trait VoiceOutput: Send + Sync {
    async fn speak(&self, text: &str) -> Result<()>;
    async fn synthesize(&self, text: &str) -> Result<Option<Vec<u8>>>;
    async fn play_audio(&self, audio: Vec<u8>) -> Result<()>;
    fn stop_speaking(&self);          // Immediate interrupt
    fn start_thinking_tone(&self);
    fn stop_thinking_tone(&self);
}
```

### PiperOutput Implementation

**File:** `crates/pond-adapters-piper/src/lib.rs`

- `speech_interrupted: Arc<AtomicBool>` -- shared flag checked during playback
- `play_wav_interruptible()` -- polls the flag every 50ms via `sink.empty()` check
- When flag set: `sink.stop()` immediately halts rodio playback
- `stop_speaking()` simply sets the flag to `true`

### Pipelined TTS

Synthesis and playback are split to allow overlap:

```
synth(sentence1) -> play(sentence1) + synth(sentence2) -> play(sentence2) + synth(sentence3)
```

- `synthesize()` -- runs Piper subprocess, returns WAV bytes without playing
- `play_audio()` -- plays pre-synthesized WAV (interruptible)
- `pending_audio` buffer in `chat_stream_once()` manages the pipeline

---

## ChatService: Wake Word Racing

**File:** `crates/pond-core/src/shared/services/chat.rs`

In `run_loop()`, the agent response races against the wake word detector:

```rust
tokio::select! {
    chat_result = &mut chat_fut => {
        // Normal completion
    }
    wake_result = &mut wake_fut => {
        // Interrupted! Stop TTS, capture new speech, process new request
        self.voice_output.stop_speaking();
        self.voice_output.stop_thinking_tone();
        // ... listen for new request, process immediately
    }
}
```

The wake word detector runs **concurrently** with inference and TTS. It uses a continuous audio ring buffer (WhisperKeywordDetector) that's always listening, even during playback.

---

## Response Time

| Component | Latency |
|-----------|---------|
| Wake word detection to `stop_speaking()` call | ~400ms (detector slide interval) |
| `stop_speaking()` to audio silence | <50ms (polling interval) |
| Total: wake word to silence | ~450ms |

---

## Related Documents

- [Agent Pipeline](../architecture/agent_pipeline.md) -- overall pipeline architecture
- [Voice Pipeline Efficiency](../voice-pipeline-efficiency.md) -- TTS optimization
- [Wake Word Calibration](../wake-word-calibration.md) -- detector configuration
