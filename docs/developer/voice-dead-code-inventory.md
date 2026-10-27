# Voice dead-code inventory

> Status (2026-10-23): every entry of the 2026-08-01 audit checked against the code. The
> `legacy-subprocess` removal, the Tauri shell and the `model_download.rs` binary cluster are gone;
> what remains is a few fake settings rows, an install-script check, and Piper residue the audit
> did not list. Each table below says removed or still present, with the evidence.

## Tier 1: `legacy-subprocess` (the HTTP whisper.cpp and Piper processes)

| Entry | Status | Evidence |
|---|---|---|
| `legacy-subprocess` cargo feature | removed | Gone from every manifest; comments in `crates/pond-server/Cargo.toml` and `crates/pond-adapters-whisper/Cargo.toml` record it |
| `crates/pond-server/src/whisper_process.rs`, `piper_process.rs`, `piper_http.rs` | removed | Files absent |
| `PIPER_TTS` in `crates/pond-server/src/ports.rs` | removed | No match |
| Gated regions in `crates/pond-server/src/main.rs` | removed | No `legacy-subprocess` cfg left |
| `model_download.rs` whisper-binary download, cmake/git bootstrap, build-from-source, piper-binary cluster and their tests | removed | No `whisper_binary_*`, `piper_binary_*` or `PiperBinaryAsset` |
| Gated regions in `crates/pond-server/tests/pipeline_integration_test.rs` | removed | No `cfg` in the file |
| Gated regions in `crates/pond-adapters-whisper/src/lib.rs` | removed | Only `#[cfg(test)]` remains |
| `crates/pond-adapters-whisper/tests/integration.rs` legacy section | removed | File is 53 lines |
| `reqwest`, `serde_json` in `pond-adapters-whisper` | removed | Not in its `[dependencies]` |
| `crates/pond-adapters-piper` | removed | Crate absent; not a workspace member |
| `POST /api/v1/voice/calibrate` going straight to the HTTP server | fixed | `calibrate_wake_word` tries `state.transcribe_audio` first ([wake-word calibration](../wake-word-calibration.md)) |

Landmines the audit named:

| Landmine | Status | Evidence |
|---|---|---|
| `PIPER_GITHUB_BASE` must survive | still present, live | `ensure_espeak_ng_data` downloads espeak-ng data from the Piper Linux tarball, which Kokoro's phonemizer needs |
| `zip` unused in the default build | still present | Unconditional dependency of `pond-server`; its only user, `fetch_buffalo_l_zip`, is `#[cfg(feature = "face-onnx")]` |
| `scripts/lib/install-models.sh` whisper-server and piper binary checks | still present | `S_WHISPER` and `S_PIPER` look for `bin/whisper-server` and `bin/piper`, which nothing installs, so a full install reports both as `failed` |
| CI cannot catch a broken gated region | resolved | `ci.yml` runs `cargo check --locked -p pond-server -p pond-adapters-goose --all-targets`. There is still no `cargo test -p pond-server` job |
| Two surviving warnings (`EventLog` import, unused `whisper_url`) | not checked | Needs a build. By inspection the `sqlite_event_log` import is used, and `serve`'s `whisper_url` feeds `AppState` |
| `docs/developer/model_architecture.md` names `whisper_process::try_start()` as the ASR path | removed | The page was rewritten; ASR is in-process whisper-rs |
| `HOWTOAI.md` port table with `PIPER_TTS = 8282` | removed | No match |

## Tier 2: desktop Rust audio (Tauri)

| Entry | Status | Evidence |
|---|---|---|
| `pond-desktop/src-tauri/` (`audio_cmd.rs`, `audio.rs`, `canvas_feed.rs`, `thought_filter.rs`, `tts_text.rs`, the rodio path, the wake listener, `SpeculativeLlmSlot`) | removed | Directory absent; the shell is Electron |

The desktop app's voice mode now runs in the `pond-server chat --voice --json-events` child, which
owns the mic; the renderer's own mic (`micRecorder.ts`, `WebVoiceBackend`) serves only the browser
path and the calibration recorder.

## Tier 3: TypeScript

| Entry | Status | Evidence |
|---|---|---|
| `pond-desktop/src/modes/webAudioUtils.ts` | removed | File absent |
| `pond-desktop/src/modes/voice/TauriVoiceBackend.ts` | removed | File absent |
| Dead tail of `VoiceBackend.ts` | removed | File is 72 lines; `createVoiceBackend` returns `WebVoiceBackend` only |
| `Voice.tsx` hands-free row | still present | `pond-desktop/src/hub/views/settings/Voice.tsx`: `handsFree = false` placeholder; the toggle persists nothing and flashes "coming in wave 4" |
| `Voice.tsx` push-to-talk row | still present | `<Toggle on={true} />` with no handler |
| `Voice.tsx` language select | still present | `defaultValue="auto"`, no backing setting |
| `Voice.tsx` Preview button | now live | `useVoicePreview`, passed to `VoicePicker` |
| `Voice.tsx` speaking rate | now live | `handlePaceChange` persists `voice_tts_speed` (debounced) |
| `hub-voice-wiring.spec.ts` slider test | still present | Tests the live pace control |
| `useVoicePipeline.ts`, `WebVoiceBackend.ts` (must survive) | present | The browser path; `VoiceMode.tsx` selects it when `isDesktopShell()` is false |
| `pond-desktop/src/modes/voice/webAudioUtils.ts` (must survive) | present | Imported by `src/sections/Canvas.tsx` and `WebVoiceBackend` |
| `tsc --noEmit` red with 35 errors | resolved | `npm run typecheck` is a CI gate |

## Piper residue (not in the audit)

Kokoro is the only TTS engine; these still name or resolve Piper.

| Entry | Status | Evidence |
|---|---|---|
| Piper voice resolution in `crates/pond-server/src/voice_models.rs` (`resolve_piper`, `any_installed_piper_voice`, `piper_download_of`, `VoiceModels::piper`) | still present, dead | `run_chat` and `serve` read only `voice_models.whisper` |
| `pond-server setup` step "[5/8] Checking Piper TTS voice model" and the "Piper TTS will fail" warnings | still present | `run_setup` in `main.rs`; it checks `models/tts/*.onnx`, not Kokoro |
| `pond_adapters_piper=debug` in the file log filter | still present | `GIAP_VERBOSE` in `crates/pond-server/src/tracing_setup.rs`; no such crate, and `pond_adapters_kokoro` is not in the list |
| `piper_http_port` in the status report | still present | Always `None` in `serve` |
| `run_chat` TTS fallback reason "did not resolve to an installed piper voice" | still present | Reached only for a `--tts` value other than `kokoro`/`none` |
| `piper_espeak_data_path`, `PIPER_ESPEAKNG_DATA_DIRECTORY` | still present, live | Names only: the espeak-rs crate reads that env var |
| `tts_piper` category handling in model routes | still present, deliberate | Ignores Piper assignments left from older installs |
| `voice_whisper_url` and the HTTP fallback in `/transcribe` and `/voice/calibrate` | still present, deliberate | Used only when no in-process Whisper model loaded |
| `pond-server calibrate --whisper-url` | still present, ignored | `run_calibrate` takes it as `_whisper_url_arg` |
