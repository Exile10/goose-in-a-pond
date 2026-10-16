# hey-goose.onnx: provenance

The "hey goose" wake-word detector that `pond_ear::wake::hey_goose()` embeds. The ONNX is the
shipped artifact; the crate converts it to engine weights when it loads (about 10 ms).

| | |
|---|---|
| File | `hey-goose.onnx`, 303,997 bytes |
| SHA-256 | `32ae17038863a9b9829524afc2dea0e1aa8a8e5d393934f672f402239f10f699` |
| Converted `.vmw` SHA-256 | `0f8a18c512d40ce2ce4792c2203fb5b3e0cfb542ac761fb4cb1c3a27bc584e17` (converter version 5, byte-identical to the lab's) |
| Trained on | Kokoro-82M voices only (Apache-2.0); synthetic rooms and noise. No household audio, no macOS voice |
| Source | `voice-match-lab` commit `9a059cb`, `data/wake/hey-goose/kokoro/model.onnx` |
| Trained | 2026-10-16, `train/train.py` unchanged (PyTorch on Apple MPS, 24 epochs, 392 s) |
| Network | depthwise-separable CNN, 7 blocks, batch norm folded, 75,777 parameters, 25.0 MMAC per window |
| Input | `feats [1, 120, 80]`: 1.2 s of 80-bin Kaldi fbank (WeSpeaker front end), mean-normalised per window |
| Output | one logit; the detector applies a sigmoid and fires at the threshold below |
| Threshold | **0.97** (`HEY_GOOSE_THRESHOLD`), chosen on validation voices as described below |

## Recipe

From the lab, with the pond's own Kokoro files and nothing downloaded:

1. `tools/kokoro-synth/build.sh <pond checkout>` builds a small renderer on the pond's
   `pond-adapters-kokoro` (espeak-ng phonemes, style tables, ONNX Runtime).
2. `wake build "hey goose" <dataset.bin>` (Kokoro is the default) renders through it. This path
   never reads household takes, ambient recordings or filed false wakes.
   - Voices: 17, namely the pond's 12 English voices (af_aoede, af_bella, af_heart, af_kore,
     af_sky, am_adam, am_echo, am_eric, am_onyx, am_santa, bf_emma, bf_isabella), plus ef_dora,
     ff_siwis, hf_alpha, jf_alpha and jf_gongitsune speaking English. Twelve more speakers are
     blends of two voices' style tables, each blend within one split.
   - Splits are fixed by voice name: af_kore, am_onyx, jf_alpha and 2 blends for validation;
     af_sky, am_eric, bf_emma and 2 blends for test; the rest (11 voices, 8 blends) for training.
   - Each voice says the phrase at 5 speaking rates with 3 punctuations, near misses ("the
     goose", "thank you goose", rhymes), the household's calibration prompts and random
     sentences. Total: 2,307 renders and 14,074 windows (3,480 positive).
3. `train/train.py` trains on the dataset as for every lab wake model.
4. `wake eval` picks the threshold, as described below.

## How the threshold was chosen

`wake eval` streams the detector exactly as the ear does. The model never trained on any voice
it is scored on.

- **Phrases:** each is placed after a filler sentence and a pause, then followed by a command,
  once clean and once in a synthetic room with noise.
- **False accepts:** 1.5 h per split of continuous speech that never says "goose".
- **Near misses:** "OK goose, what time is it", "my goose", "hey boose" and similar, kept as a
  row of their own.

The threshold is the highest validation recall with at most one false accept per hour. The
trainer's own window-level pick was 0.875; 0.98 sat on the edge in live use.

| at the shipped threshold | this model, 0.97 | the earlier macOS-trained model, 0.98 |
|---|---|---|
| validation voices: recall, false accepts per hour, near misses accepted | 72.0%, 0.66, 2.7% | 42.7%, 1.97, 5.9% |
| test voices: recall, false accepts per hour, near misses accepted | 54.0%, 0.68, 0.5% | 21.3%, 0.68, 3.6% |
| macOS voices (the earlier model's own domain): recall, false accepts per hour | 64.0%, 5.9 | 78.9%, 0 |
| the household's real recordings (4 clips, 16 trials, evaluation only) | 3 of 16 | 3 of 16 |

At 0.90 this model catches 85% of validation phrases and 67% of test phrases, at 5.9 and 1.4
false accepts per hour, and 8 of the 16 real trials. What still fires near the top is "OK
goose, what time is it", the "hey hoose" and "hey boose" rhymes, and a few nonsense sentences.
Real household voices are the open question: four real clips are not a measurement.

## Licence

Kokoro-82M's weights and voices are Apache-2.0 (hexgrad/Kokoro-82M). espeak-ng (GPL-3.0) only
converted text to phonemes while the dataset was built; none of it is in the model.
