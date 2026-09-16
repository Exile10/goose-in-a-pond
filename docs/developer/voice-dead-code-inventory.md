# Voice dead-code inventory (verified 2026-08-01)

Produced by a fan-out audit plus an independent adversarial verification pass.
**No item survived as live** — every entry below is genuinely unreachable — but
the verification found eight line ranges that break the parse or the build if
applied literally, plus several missed dependents. Those corrections are folded
in. Do not delete from memory of the original audit; use the ranges here.

Scope: ~4,700 lines. Execute in tiers; `cargo check` between them.

---

## Blocking pre-work

**`POST /api/v1/voice/calibrate` — DONE 2026-08-01.** It went straight to the
HTTP whisper server, which a default build never spawns, so onboarding's
calibration step returned 502 on every install. It now tries `state.transcribe_audio`
first, matching `POST /transcribe`. This had to land *before* the legacy
deletion, which removes the HTTP path entirely and would otherwise have frozen
the route as permanently broken.

---

## Tier 1 — `legacy-subprocess` — **EXECUTED 2026-08-02**

1,859 lines removed. The cargo feature is gone from all three manifests, so
`cargo check -p pond-server --features legacy-subprocess` now errors with
"does not contain this feature" — the escape valve cannot be re-opened by
accident. `reqwest` and `serde_json` dropped from `pond-adapters-whisper`.

**Two things bit during execution, both predicted by the verification pass:**

- A doc comment sitting *above* a `#[cfg]` attribute was orphaned when its item
  was removed, producing `expected item after doc comment`. `cargo check
  -p pond-server` did **not** catch it — only `--all-targets` did, exactly as
  the verifier warned. Always run `cargo test -p pond-server --all-targets`
  after touching gated regions; CI runs neither.
- `PIPER_GITHUB_BASE` is now annotated `KEEP` in place rather than moved. The
  first attempt localised a copy to `ensure_espeak_ng_data`, which orphaned the
  original and introduced a fresh dead-code warning.

**Still outstanding in this tier:** the ~1,000 lines of dead code in
`model_download.rs` (whisper-binary download, cmake/git bootstrap,
build-from-source, piper-binary cluster) plus their `#[cfg(test)]` tests. These
warn but compile; they are safe to leave and safe to remove, and the compiler
names every one of them. `PIPER_GITHUB_BASE` must survive that sweep.

### Original inventory (for reference)

Default features are `["goose-agent", "local-inference", "pond-agent"]`; CI never
passes `--features`. `cargo check -p pond-server` emits 25 dead-code warnings
against `model_download.rs` alone, which is the proof.

| Path | Lines | Note |
|---|---|---|
| `crates/pond-server/src/whisper_process.rs` | 165 | whole module |
| `crates/pond-server/src/piper_process.rs` | 39 | whole module |
| `crates/pond-server/src/piper_http.rs` | 179 | whole module |
| `crates/pond-server/src/ports.rs:38-39` | 3 | `PIPER_TTS`; delete *after* `piper_http.rs` |
| `crates/pond-server/src/main.rs` | ~225 | 12 gated regions + the `cfg(not(...))` scaffolding that collapses to unconditional |
| `crates/pond-server/src/model_download.rs` | ~1018 | 8 regions — binary download, cmake/git bootstrap, build-from-source |
| `crates/pond-server/tests/pipeline_integration_test.rs` | 104 | 5 gated regions; file survives at ~268 lines |
| `crates/pond-adapters-whisper/src/lib.rs` | 232 | 11 gated regions |
| `crates/pond-adapters-piper/src/lib.rs` + `tests/integration.rs` | ~390 | the whole integration file is `#![cfg(...)]` |

**Landmines:**

- **`PIPER_GITHUB_BASE` (`model_download.rs:598`) must survive** — used by
  `ensure_espeak_ng_data:869`, which is live. It sits inside a block otherwise
  being deleted. Lift it out first.
- **The 25-warning proof says nothing about `#[cfg(test)]`.** `cargo check`
  does not compile test modules, and `whisper_binary_path`, `piper_binary_path`,
  `whisper_binary_asset`, `piper_binary_asset` and `PiperBinaryAsset` are all
  referenced from tests at `model_download.rs:1702, 1721, 1743, 1759, 1769-1800,
  1859`. Those tests go with them.
- **`zip` becomes an unused dependency.** Its only other user,
  `fetch_buffalo_l_zip:1531`, is `#[cfg(feature = "face-onnx")]`, which is not
  in the default set.
- **`scripts/lib/install-models.sh` is 64-82, not 64-81.** Line 82 is the
  closing `fi`; cutting 64-81 strands it and breaks the sourced library with a
  shell syntax error. And this one should be **fixed, not deleted** — the
  branch's false "failed" is accidentally load-bearing.
- **`crates/pond-adapters-whisper/tests/integration.rs` is 51-188**, not
  53-188; 51 is the section comment.
- **CI cannot catch a mistake here.** `ci.yml:121` runs
  `cargo check -p pond-server` with no `--all-targets`, and there is no
  `cargo test -p pond-server` job at all. Run both locally.
- Two warnings survive by design: `main.rs:2714` (unused `EventLog` import,
  pre-existing) and `main.rs:3432` (unused `whisper_url`).
- The escape valve is **not** bit-rotted —
  `cargo check -p pond-server --features legacy-subprocess --all-targets`
  exits 0 today. Deleting it is a choice, not a cleanup of something broken.

**Stale docs to update in the same commit:** `docs/voice-pipeline-efficiency.md`
(185, 202, 220, 235, 248 — presents `voice_kws_whisper_url` as a live tuning
knob), `docs/wake-word-calibration.md:319`, `docs/developer/model_architecture.md:356`
(names `whisper_process::try_start()` as the live ASR path), `HOWTOAI.md:172`
(port table incl. `PIPER_TTS = 8282`).

---

## Tier 2 — desktop dead audio (Rust) — RESOLVED

Resolved by the Electron migration: `pond-desktop/src-tauri/` was deleted
wholesale, and every item this section listed went with it — `audio_cmd.rs`,
`audio.rs`, `canvas_feed.rs`, `thought_filter.rs`, `tts_text.rs`, the rodio
playback path, the wake listener with no entry point, and the `SpeculativeLlmSlot`
scaffolding. About 4,000 lines of Rust in total, of which this section had
already identified roughly 2,900 as dead.

Two things from it survive and are worth carrying forward:

- The microphone is the renderer's now (`src/modes/voice/micRecorder.ts` and
  `WebVoiceBackend`), so the "landmine" list about which capture path owns the
  device no longer has two candidates to arbitrate between.
- `TauriVoiceBackend.ts`, the root cause named here, is also gone; `VoiceMode`
  forks on `isDesktopShell()` and the browser path is the only backend
  `createVoiceBackend` returns.

## Tier 3 — TypeScript (~845 lines)

| Item | Range | Status |
|---|---|---|
| `src/modes/webAudioUtils.ts` | whole, 548 | **DELETED 2026-08-01** — zero importers, verified twice |
| `src/modes/voice/TauriVoiceBackend.ts` | whole, 194 | DELETED — was unreachable, shipped as a dead lazy chunk |
| `src/modes/voice/VoiceBackend.ts` | **89-92**, not 89-91 | 92 is the closing brace |
| `Voice.tsx` hands-free Row | **245-254**, not 249-252 | 249-252 alone leaves `control={\n}` — a parse error |
| `Voice.tsx` push-to-talk Row | 312-316 | genuinely fake, but more evidence of being actively built |
| `Voice.tsx` language select | ~362-389 | no backing `Settings` field (`asr_language:348` is on `ModelEntry`) |
| `Voice.tsx` Preview button | **395-405**, not 393-405 | 393-394 are the surviving `<Card>`, asserted by an E2E test |
| `Voice.tsx` speaking rate | 46-50, 64, **70**, **99**, 149-161, 448-462 | 70 and 99 are the `sliderTimer` ref and its cleanup |
| `hub-voice-wiring.spec.ts` slider test | **170-189**, not 178-197 | |

**Corrections to the original plan:**

- **`useVoicePipeline.ts` and `WebVoiceBackend.ts` must SURVIVE.** The plan
  listed them for deletion; they are live at `VoiceMode.tsx:216` and are the
  browser path the E2E suite covers.
- `src/modes/voice/webAudioUtils.ts` (676) must survive — `Canvas.tsx:36`.
- `tsc --noEmit` is **already red** with 35 pre-existing errors. "Stay green" is
  not an achievable checkpoint; compare counts against that baseline.
- `playwright.config.ts` sets `retries: 0` and cold-starts Vite in CI, so a
  "cold-start only" failure is the CI default, not a local flake.
