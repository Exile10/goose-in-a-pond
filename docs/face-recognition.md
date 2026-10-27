# Face recognition

`pond-server` can enrol household members' faces and identify them from a webcam
frame or burst. Recognition runs on-device: no frame is stored, and the database
holds only 512-float embedding vectors.

It is compiled with the `face-onnx` Cargo feature, which is in `pond-server`'s default
features (as of 2026-10-27). Identification is exposed over REST, used by the desktop
**Faces** screen, and matched against person and motion events by the camera pipeline;
nothing in the chat or agent path consumes it, so the assistant does not learn who it is
talking to from it.

| Part | Where |
|---|---|
| ONNX adapters (embedder, SCRFD/UltraFace detectors, alignment, anti-spoof) | `crates/pond-adapters-face-onnx/` |
| Matcher and storage (`SqliteFaceRecognition`) | `crates/pond-infra/src/sqlite_face_recognition.rs` |
| Ports | `crates/pond-core/src/user_data/ports/face_{recognition,detector,embedding_extractor}.rs` |
| Wiring and thresholds (`build_face_recognition`, `apply_face_recognition_defaults`) | `crates/pond-server/src/main.rs` |
| Model download (`download_face_models`) | `crates/pond-server/src/model_download.rs` |
| Startup and the background download (`start_face_recognition`, `fetch_face_models_in_background`) | `crates/pond-server/src/main.rs` |
| The shared, late-filled recognition (`FaceRecognitionSlot`) | `crates/pond-core/src/user_data/services/face_recognition_slot.rs` |
| REST handlers and burst liveness | `crates/pond-api/src/routes.rs` |
| Desktop screen | `pond-desktop/src/sections/Faces.tsx` (Manage drawer → Faces) |

---

## Pipeline

```
JPEG/PNG frame
  │
  ├─ decode
  ├─ pre-detection auto-exposure      mean luminance < 0.30 → stretch/CLAHE on the 640×640 input
  ├─ detector                         SCRFD (34G if present, else 10G): bbox + 5-point landmarks
  │                                   score floor 0.50, or 0.30 on a dim frame; NMS 0.4
  │                                   else UltraFace (bbox only), else centre-square crop
  ├─ detection gates                  face ≥ 80×80 px; eye line within 25°; nose between the eyes
  ├─ Umeyama alignment                5 landmarks → canonical 112×112 ArcFace template
  ├─ crop auto-exposure               aligned crop still dim → stretch/CLAHE
  ├─ quality gates                    content variance ≥ 0.006, mean brightness 0.025–0.95,
  │                                   Laplacian variance ×1000 ≥ 4.0 (blur)
  ├─ anti-spoof                       spoof = max(Silent-Face V2, DeepPixBis); reject ≥ threshold
  └─ embedder                         Glint-R100 (or ArcFace R50): 1×3×112×112 → 512 floats, L2-normalised
        │
        ├─ ENROL     insert into face_embeddings
        └─ IDENTIFY  per-profile score = 0.6 × cosine(centroid) + 0.4 × mean(top-3 cosines)
                     ranked after S-norm (score − 0.5 × mean of other profiles)
                     identified when: raw score ≥ threshold
                                  AND margin over runner-up ≥ 0.10
                                  AND gap to mean of others ≥ 0.08
```

### Match threshold

`build_face_recognition` picks the threshold from what loaded:

| Detector | Threshold |
|---|---|
| SCRFD (landmarks, aligned crop) | 0.70 |
| UltraFace (bbox only) | 0.72 |
| none (centre-square crop) | 0.85 |

`POND_FACE_MATCH_THRESHOLD` overrides it. With only one enrolled profile the runner-up
and open-set gates cannot discriminate, so the threshold is raised by
`POND_FACE_SINGLE_PROFILE_MARGIN` (0.10, capped at 0.99) unless the operator set the
threshold explicitly. A per-profile override (below) replaces the global threshold for
that profile. A profile needs `POND_FACE_MIN_SAMPLES` enrolments (default 1) before it
can be matched.

---

## Models

| Slot | Preferred (file) | Fallback (file) | Notes |
|---|---|---|---|
| Embedder | Glint-R100 from antelopev2, ~261 MB (`adaface_ir101.onnx`) | ArcFace R50 from buffalo_l (`w600k_r50.onnx`), then a legacy `arcface.onnx` | 112×112 in, 512-d out. The filename is a fixed slot name, not the model's. A path containing `mobilefacenet` loads as MobileFaceNet-128. |
| Detector | SCRFD 34G GNKPS, ~39 MB (`scrfd_34g.onnx`) | SCRFD 10G from buffalo_l (`scrfd.onnx`), then UltraFace (`ultraface.onnx`, never downloaded) | SCRFD's 5 landmarks drive the alignment. |
| Anti-spoof 1 | Silent-Face MiniFASNetV2, ~2 MB (`antispoof.onnx`) | dependency-free heuristic (saturation, highlight, gradient skew) | 3-class softmax `[fake_2D, fake_3D, live]` at 80×80. |
| Anti-spoof 2 | DeepPixBis OULU-NPU, ~13 MB (`OULU_Protocol_2_model_0_0.onnx`) | none (primary runs alone) | 224×224 RGB sigmoid. The filename must stay `OULU_*`: the variant is chosen from it. |

All files live in `<data_dir>/models/face/`. With the feature on, `setup` downloads any
missing preferred file before it finishes, and `serve` downloads them in the background
(below); buffalo_l (~281 MB zip) is fetched only when a preferred embedder or detector is
still missing afterwards.

### Startup never waits on the download

`serve` loads face recognition from whatever is on disk and binds its listeners straight
away; any missing model is fetched by a background task. On 2026-10-27 the Jetson spent about
twenty minutes fetching the 261 MB embedder over Wi-Fi while, before this change, the pond
served nothing and every phone said "Pond unreachable".

Every consumer (the REST routes and the camera pipeline) holds one `FaceRecognitionSlot`.
It starts in one of three states:

| On disk at startup | Slot | Face routes answer |
|---|---|---|
| Embedder present and it loads | ready | normally |
| Embedder present but it will not load (no ONNX Runtime, a corrupt file) | unavailable | `503 face_recognition_unavailable` |
| No embedder | downloading | `503 face_models_downloading`, with progress |

When the download ends, a slot that was waiting is filled without a restart, or marked
unavailable with the reason. The camera pipeline skips face matching while the slot is
empty, keeps the event, and logs `vision_face_matching_skipped` once (and
`vision_face_matching_resumed` when recognition arrives) rather than once per frame. Optional
models fetched after recognition was already on (an anti-spoof model, say) take effect at the
next restart.

The log, by `kind`: `face_models_download_started` (with the missing files),
`face_models_download_completed` (`elapsed_ms`, `files`, `bytes`, `still_missing`) or
`face_models_download_failed`, then `face_recognition_enabled` (`source` is `startup` or
`background_download`) or `face_recognition_unavailable`.

A download that goes 60 seconds without a byte counts as dropped
(`pond_hf_cache::DOWNLOAD_READ_TIMEOUT`, a per-read limit, not an overall one), so a stalled
connection can no longer hang anything. The Hugging Face files then resume from their
`.incomplete` blob with a Range request, up to eight attempts that make no progress, backing
off from 2 s to 60 s. The two GitHub assets (DeepPixBis and the buffalo_l zip) fail on the
stall but do not resume: the next start fetches them again.

Download sources (`model_download.rs`):

```
Glint-R100       https://huggingface.co/immich-app/antelopev2/resolve/main/recognition/model.onnx
SCRFD 34G GNKPS  https://huggingface.co/immich-app/scrfd_34g_gnkps/resolve/main/detection/model.onnx
Silent-Face V2   https://huggingface.co/hash-ash/Silent-Face-Anti-Spoofing-ONNX/resolve/main/2.7_80x80_MiniFASNetV2.onnx
                 https://huggingface.co/datasets/giap-mirror/silent-face-anti-spoofing/resolve/main/2.7_80x80_MiniFASNetV2.onnx
DeepPixBis OULU  https://github.com/ffletcherr/face-recognition-liveness/releases/download/v0.1/OULU_Protocol_2_model_0_0.onnx
buffalo_l        https://github.com/deepinsight/insightface/releases/download/v0.7/buffalo_l.zip
```

`POND_FACE_EMBEDDING_URL`, `POND_FACE_DETECTOR_URL`, `POND_FACE_ANTISPOOF_URL` and
`POND_FACE_ANTISPOOF_2_URL` replace them. A Hugging Face token is sent when
`HF_TOKEN`, `HUGGING_FACE_HUB_TOKEN` or `HUGGINGFACE_TOKEN` is set.

The ONNX Runtime library is found by `ensure_onnx_runtime()`: `ORT_DYLIB_PATH`, then, on a
Jetson, the GPU runtime `bash scripts/giap.sh onnxruntime build` installs under
`<data_dir>/lib/onnxruntime-gpu/<version>/`, then system locations, then a previously
downloaded copy, then an automatic download. If it still fails to load, face recognition logs
why and stays off.

With the GPU runtime, SCRFD, the embedder and the YOLOX classifier start on the CPU and each moves
to TensorRT (fp16) after the language model has loaded; `pond-onnx-accel` does the move in the
background, so no event waits for an engine. The anti-spoof model stays on the CPU, where it is as
fast. Engines and their EP-context models are cached under
`<data_dir>/cache/onnx-trt/<runtime>-trt<version>-sm_<arch>/`; the setup script builds them while
the pond is stopped, and the pond loads one only while 1 GB is available. A model whose GPU run
fails goes back to the CPU. The log says where each model runs
(`onnx: face_detector moved to tensorrt`), and so does `GET /api/v1/vision/status`
(`classifier.accelerator`, `faces.accelerators`).

### Anti-spoof ensemble

The primary model always runs when `antispoof.onnx` exists (the heuristic otherwise).
The secondary runs when `POND_FACE_ANTISPOOF_PATH_2` is set, which the server does
itself when `OULU_Protocol_2_model_0_0.onnx` exists. The frame's spoof score is the
higher of the two, so either model firing rejects it. The secondary sees a looser crop
(`POND_FACE_ANTISPOOF_SCALE_2`, default 4.0 × the face box).

The threshold (`POND_FACE_ANTISPOOF_THRESHOLD`) defaults to 0.40 for an ONNX model and
0.65 for the heuristic. `POND_FACE_ANTISPOOF=off` (or `0`, `false`) disables anti-spoof
entirely.

Each ONNX slot picks its variant (`Variant::SilentFace80` or `Variant::DeepPixBis224`)
at load: `POND_FACE_ANTISPOOF_VARIANT` / `POND_FACE_ANTISPOOF_2_VARIANT` (`silentface`
or `deeppixbis`) win; otherwise a basename containing `deeppixbis`, `oulu_protocol`,
`oulu_npu` or `pixel_supervision`, or starting `pixbis_`, means DeepPixBis; otherwise
Silent-Face.

### Low light

- **Camera.** The desktop screen requests `getUserMedia` with `exposureMode`,
  `whiteBalanceMode` and `focusMode` `continuous`, `exposureCompensation: 1.0` and
  `brightness: 128`, inside `advanced[]` so unsupported constraints are dropped.
- **Pre-detection.** `ScrfdDetector` brightens the 640×640 input when its mean
  luminance is below 0.30 (`LOW_LIGHT_TRIGGER`), and lowers its score floor to 0.30
  (`POND_FACE_SCRFD_LOW_LIGHT_THRESH`) for that frame. The matcher threshold and burst
  liveness remain the spoof safeguards.
- **Aligned crop.** The same pass runs again on the 112×112 crop if it is still dim.
- **Algorithm.** `POND_FACE_AUTO_EXPOSURE_MODE=stretch` (default: per-channel 2–98
  percentile linear stretch, about 0.2 ms) or `clahe` (8×8 tiles, clip limit 4, about
  3 ms, better for mixed lighting). `POND_FACE_AUTO_EXPOSURE=off` disables both passes.

---

## Burst liveness

The desktop captures **5 JPEG frames 400 ms apart** and posts them to
`/faces/identify-burst`. The server accepts 1 to 12 frames. For 3 or more it computes
five statistics from the landmarks and embeddings it already has, and each one that
looks photo-like counts:

| Signal | Photo-like when | Env override (default) |
|---|---|---|
| Mean inter-frame embedding cosine | above the ceiling (frozen) | `POND_FACE_LIVENESS_INTER_COS_MAX` (0.9994) |
| Mean landmark motion | below the floor, px | `POND_FACE_LIVENESS_MOTION_MIN` (0.5) |
| Differential (non-rigid) landmark motion | below the floor, px | `POND_FACE_LIVENESS_DIFF_MOTION_MIN` (0.60) |
| Eye-ratio spread | below the floor | `POND_FACE_LIVENESS_EYE_SPREAD_MIN` (0.003) |
| Face-size spread | below the floor | `POND_FACE_LIVENESS_SIZE_SPREAD_MIN` (0.012) |

Two or more photo-like signals is a **hard reject** (`reason: "liveness_failed"`). One
marks the burst **suspicious**, which raises the consensus floors below by
`POND_FACE_BURST_SUSPICIOUS_MARGIN` (0.05). A live person may trip one signal in a still
moment; a held-up screen typically trips three or more.

A burst that passes liveness is identified only when every face-bearing frame (at least
two) votes for the same profile, at most `POND_FACE_BURST_NOFACE_BUDGET` (20%) of frames
had no face, every winning frame scores at least threshold +
`POND_FACE_BURST_FRAME_MARGIN` (0.10), and their mean scores at least threshold +
`POND_FACE_BURST_MEAN_MARGIN` (0.12).

---

## REST API

All routes are under `/api/v1` and need a bearer token like any other route. Until
recognition is loaded every one answers `503`, except `GET /faces` and `GET /faces/models`,
which answer `200` with the same `available`, `status` and `progress` or `reason` fields:

| Why | `status` | 503 body |
|---|---|---|
| Built without `face-onnx` | `not_built` | `{"error": "Face recognition is not configured on this server", "hint": …}` |
| Models still downloading | `downloading` | `{"error": …, "code": "face_models_downloading", "progress": {"file", "downloaded_bytes", "total_bytes"} \| null}` |
| Download failed, or the model will not load | `unavailable` | `{"error": …, "code": "face_recognition_unavailable", "reason": "<sentence>"}` |

`progress` is the file in flight; `total_bytes` is the download's estimate until the server
states a length. A client waiting on `face_models_downloading` can ask again: recognition
switches on by itself. Loaded, `status` is `ready` and `available` is `true`.

| Route | Does |
|---|---|
| `GET /faces` | The household summary: `{available, status, progress?, reason?, default_threshold, people: [{profile_id, name, enrollments, threshold, last_enrolled_at}]}`. Every profile is listed, 0 enrolments included (and every count is 0 until recognition is loaded); `threshold` is the per-profile override or null. |
| `POST /faces/register` | Multipart `profile_id`, `image`, optional `bbox=x,y,w,h`. Enrols one sample. 404 for an unknown profile, 422 when a gate rejects the frame. Returns `{id, profile_id, model_dims, created_at}`. |
| `POST /faces/register-from-camera` | JSON `{profile_id, camera_id?}`. Enrols the camera's latest frame through the same `register_face`, so the sample has the angle and light recognition will see. The frame must be under 10 s old. `403 reason: "cameras_disabled"` with camera consent off, 404 for an unknown profile, `409 reason: "no_frame"` without a recent frame, `422 reason: "no_face"` when a gate rejects it. |
| `POST /faces/identify` | Multipart `image`, optional `bbox`. Single frame, no liveness: a photo can pass. Returns `{identified, profile_id, confidence, threshold}`. |
| `POST /faces/identify-burst` | Multipart `image` repeated, optional `bbox`. See below. |
| `POST /faces/enroll-quality` | Multipart `image`, optional `profile_id`. Dry run: `{ok, reason, matches_existing, matched_profile, confidence, existing_samples, recommended_min_samples, self_consistency, alignment_with_existing, consistency_warning, …}`. Nothing is stored. |
| `GET /faces/profile/{profile_id}` | `{profile_id, enrollments: [{id, profile_id, model_dims, created_at}], count}`. Never the vectors. |
| `GET` / `PUT` / `DELETE /faces/profile/{profile_id}/threshold` | Per-profile threshold override. `PUT` body `{threshold: 0.0–1.0, note?}`. |
| `GET /faces/models` | `{available, status, progress?, reason?, feature_enabled, models_dir, models: [{name, label, role, expected_mb, size_mb, downloaded, path}]}` for the six files above. |
| `GET /faces/debug/pairwise` | Cosine matrix across every enrolment, with a verdict on embedding collapse. |
| `GET /faces/debug/eval` | False-accept / false-reject rates across thresholds on the enrolled data. |
| `DELETE /users/{profile_id}/biometrics` | Deletes every embedding for the profile. Deleting the profile does the same (`ON DELETE CASCADE`). |

`/faces/identify-burst` answers a liveness failure with
`{identified: false, reason: "liveness_failed", liveness, frames_total, no_face_frames, per_frame}`,
and otherwise with
`{identified, profile_id, candidate_profile, mean_confidence, votes, frames_total, frames_required, no_face_frames, threshold, suspicious, liveness, per_frame}`.
`liveness` is `{hard_reject, suspicious, mean_inter_cos, landmark_motion, eye_ratio_spread, differential_motion}`
(null for fewer than 3 frames). `candidate_profile` names the near-miss when
`identified` is false.

Nothing in the desktop calls `/faces/enroll-quality` or `/faces/models`. Note that
`enroll-quality` reports `recommended_min_samples` with a default of 3, while the
matcher's `POND_FACE_MIN_SAMPLES` defaults to 1.

A development page at `http://127.0.0.1:<port>/dev/face` (loopback dashboard listener
only) drives the same routes from a browser. It sends no bearer token, so its calls are
refused unless the server runs with `POND_DEV_ALLOW_LOOPBACK=1`.

### Storage

`<data_dir>/pond_system.db`:

```
face_embeddings            (migration 0013)
  id          TEXT PRIMARY KEY
  profile_id  TEXT NOT NULL → profiles(id) ON DELETE CASCADE
  embedding   BLOB NOT NULL     packed little-endian f32, model_dims × 4 bytes
  model_dims  INTEGER NOT NULL  512 for the ArcFace family
  model_name  TEXT              "arcface-512" or "mobilefacenet-128"
  created_at  TEXT

face_profile_thresholds    (migration 0014)
  profile_id, match_threshold, note, updated_at
```

Identification compares only rows whose `model_dims` matches the loaded embedder.

---

## Desktop: the Faces screen

Manage drawer → **Faces**. A webcam preview with a crop guide, a profile selector
(from `/api/v1/profiles`), the profile's sample count, and three actions:

| Action | Calls | Shows |
|---|---|---|
| Enroll sample | `POST /faces/register` with one frame | "Sample n of 3 saved — capture …" until three, then that the profile is ready |
| Identify face | `POST /faces/identify-burst` with 5 frames | "Welcome back, <name>!", a liveness refusal ("Photos and phone screens won't work"), or that the face is not recognised |
| Delete biometrics | inline confirmation, then `DELETE /users/{id}/biometrics` | the count dropping to zero |

A 503 disables the actions and says why: `face_models_downloading` shows that the Pond is
downloading its face models, the file and megabytes so far, and that no restart is needed, and
the screen asks again every 15 seconds until recognition is on; `face_recognition_unavailable`
shows the server's `reason`; any other 503 shows "Face recognition isn't turned on yet". Other
failures show a plain sentence, never a raw status code. The strings are in
`pond-desktop/src/i18n.ts` under `faces`.

---

## Running it

```bash
cargo run -p pond-server --features face-onnx -- serve            # add --native to open the macOS app
```

`scripts/dev.sh` builds with `face-onnx` unless `NO_FACE=1`. The first start downloads
the models into `<data_dir>/models/face/` in the background while it serves; later starts
find each already present. The log shows what loaded:

```
INFO pond_adapters_face_onnx: face embedding ONNX model loaded model=ArcFace512 path=…/adaface_ir101.onnx
INFO pond_adapters_face_onnx::scrfd: SCRFD detector loaded path=…/scrfd_34g.onnx
INFO pond_adapters_face_onnx::antispoof_onnx: anti-spoof ONNX model loaded variant=SilentFace80 path=…/antispoof.onnx
INFO pond_adapters_face_onnx::antispoof_onnx: anti-spoof ONNX model loaded variant=DeepPixBis224 path=…/OULU_Protocol_2_model_0_0.onnx
INFO pond_server: SCRFD face detector loaded from … — landmark alignment enabled
INFO pond_server: face recognition enabled (model=ArcFace512, threshold=0.7, aligned=true)
```

Two `anti-spoof ONNX model loaded` lines with different `variant=` values show the
ensemble is wired. Every identification logs a `face identify decision` line at INFO
with the score, margins and which gates passed.

Use `/faces/debug/pairwise` to check enrolment quality, and `/faces/debug/eval` after
changing thresholds to see the false-accept / false-reject trade-off on the household's
own data.

---

## Configuration reference

Every variable is optional. `pond-server` sets the starred ones itself when they are
unset.

| Variable | Default | Effect |
|---|---|---|
| `POND_FACE_MODEL_PATH` | Glint-R100 → R50 → `arcface.onnx` | Embedder file |
| `POND_FACE_SCRFD_PATH` | `scrfd_34g.onnx` → `scrfd.onnx` | SCRFD file |
| `POND_FACE_DETECTOR_PATH` | `ultraface.onnx` | UltraFace fallback file |
| `POND_FACE_ANTISPOOF_PATH`\* | `antispoof.onnx` when present | Primary anti-spoof file |
| `POND_FACE_ANTISPOOF_PATH_2`\* | `OULU_Protocol_2_model_0_0.onnx` when present | Secondary anti-spoof file |
| `POND_FACE_EMBEDDING_URL`, `POND_FACE_DETECTOR_URL`, `POND_FACE_ANTISPOOF_URL`, `POND_FACE_ANTISPOOF_2_URL` | sources above | Download URLs |
| `POND_FACE_EMBED_CHANNEL_ORDER` | `rgb` | `bgr` for non-canonical exports |
| `POND_FACE_AUTO_EXPOSURE` | on | `off` disables both auto-exposure passes |
| `POND_FACE_AUTO_EXPOSURE_MODE` | `stretch` | or `clahe` |
| `POND_FACE_SCRFD_LOW_LIGHT_THRESH` | 0.30 | SCRFD score floor on dim frames |
| `POND_FACE_ANTISPOOF` | on | `off`, `0` or `false` disables anti-spoof |
| `POND_FACE_ANTISPOOF_THRESHOLD` | 0.40 ONNX / 0.65 heuristic | Spoof-score reject threshold |
| `POND_FACE_ANTISPOOF_LIVE_INDEX`\* | 2 | Softmax slot of the "live" class |
| `POND_FACE_ANTISPOOF_PIXEL_SCALE`\* | `unit` | `unit` ([0,1]) or `raw` ([0,255]) model input |
| `POND_FACE_ANTISPOOF_VARIANT`, `POND_FACE_ANTISPOOF_2_VARIANT` | from filename | `silentface` or `deeppixbis` |
| `POND_FACE_ANTISPOOF_SCALE_2` | 4.0 | Secondary model's crop scale |
| `POND_FACE_MATCH_THRESHOLD` | by detector (0.70 / 0.72 / 0.85) | Cosine threshold |
| `POND_FACE_SINGLE_PROFILE_MARGIN` | 0.10 | Added to the threshold with one enrolled profile |
| `POND_FACE_RUNNER_UP_MARGIN` | 0.10 | Required S-normed gap to the runner-up |
| `POND_FACE_OPEN_SET_GAP` | 0.08 | Required S-normed gap to the mean of the others |
| `POND_FACE_MIN_SAMPLES` | 1 | Enrolments before a profile can match |
| `POND_FACE_LIVENESS_*` | see [Burst liveness](#burst-liveness) | Liveness floors and ceiling |
| `POND_FACE_BURST_FRAME_MARGIN`, `_MEAN_MARGIN`, `_SUSPICIOUS_MARGIN`, `_NOFACE_BUDGET` | 0.10, 0.12, 0.05, 0.20 | Burst consensus |

---

## Privacy

- No enrolment frame is written to disk. An enrolment is 512 × f32 (2 KB) in the local database.
  (Camera events keep their own snapshot JPEGs under `<data_dir>/snapshots`; those are not
  enrolments.)
- With camera consent (`cameras_enabled`) off, nothing is captured, and enrolment from the camera
  is refused.
- An embedding is an identification fingerprint, not an image, though it is still
  biometric data and is treated as such.
- `DELETE /users/{profile_id}/biometrics`, or deleting the profile, removes every
  embedding for it.
- Processing is on-device. The only network traffic is the model and ONNX Runtime
  downloads.
