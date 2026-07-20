# Jetson Orin Nano — llama.cpp Setup & Optimization

Scripts to turn a **Jetson Orin Nano 8GB "Super"** (JetPack 6.2.1 / L4T R36.4.7,
CUDA 12.6) into an optimized, CUDA-accelerated **llama.cpp** inference node for
GIAP on-device intelligence.

Everything here was validated live on the target device. Each script is
idempotent, documented, and safe to re-run.

---

## TL;DR

```bash
cd ~/jetson-llama-optimization
./run-all.sh          # deps → perf mode → CUDA build → model → benchmark
```

Or step by step:

| # | Script | What it does | Root? |
|---|--------|--------------|-------|
| 00 | `00-system-info.sh` | Read-only health/inventory snapshot → `reports/` | no |
| 01 | `01-install-deps.sh` | Build toolchain + **CMake≥3.27** + ccache/ninja + `jtop` | yes |
| 02 | `02-performance-mode.sh` | `MAXN_SUPER` + `jetson_clocks` + perf governor | yes |
| 03 | `03-build-llama-cpp.sh` | Build llama.cpp with CUDA (sm_87) → `~/.local/bin` | no |
| 04 | `04-download-model.sh` | Fetch a GGUF sized for 8GB → `~/models` | no |
| 05 | `05-benchmark.sh` | `llama-bench` + tegrastats telemetry → `reports/` | no¹ |
| 06 | `06-install-services.sh` | systemd: perf-at-boot + `llama-server` daemon | yes |
| 99 | `99-install-nvidia-frameworks.sh` | **Optional** cuDNN/TensorRT/DeepStream | yes |

¹ only with `--pause-ollama`.

---

## The one thing that matters: CUDA, not the rest of the NVIDIA stack

llama.cpp's CUDA backend links **only** `cudart`, `cublas`, `cublasLt`, and the
CUDA driver — all already on the device with the CUDA 12.6 toolkit. It ships its
own GPU kernels and uses cuBLAS only for some FP16 GEMMs.

> **cuDNN, TensorRT, TensorRT-LLM, DeepStream, VPI, Triton give ZERO benefit to
> llama.cpp token generation** and are intentionally *not* installed.

This isn't incidental — single-stream LLM decode on this board is
**memory-bandwidth-bound** (~102 GB/s LPDDR5 ceiling; the 40-TOPS tensor cores
sit ~80% idle at batch 1). Graph compilers like TensorRT optimize compute, which
isn't the bottleneck. The dominant speed lever is **model size**
(tok/s ≈ 102 GB/s ÷ model_bytes), then clocks (which help *prefill* more than
*generation*). `99-install-nvidia-frameworks.sh` exists only for *other*
workloads (PyTorch/vision/video analytics).

---

## Recommended models (all **dense** — MoE GGUFs hang on decode on sm_87)

| Preset | Model | Quant | Size | Notes |
|--------|-------|-------|------|-------|
| `llama3.2-3b` *(default)* | Llama-3.2-3B-Instruct | Q4_K_M | ~2.0 GB | Best all-round; big headroom; ~28–29 tok/s gen |
| `qwen2.5-3b` | Qwen2.5-3B-Instruct | Q4_K_M | ~2.0 GB | Strong alternative |
| `llama3.2-1b` | Llama-3.2-1B-Instruct | Q8_0 | ~1.3 GB | Snappy/low-latency |
| `qwen2.5-7b` | Qwen2.5-7B-Instruct | Q4_K_M | ~4.7 GB | Ceiling; needs KV-quant + small batch |

```bash
./04-download-model.sh --list          # see all
./04-download-model.sh qwen2.5-3b
```

### Runtime flags (used by the bench + server)
`-ngl 99` (offload all layers — no PCIe cost on unified memory) · `-fa on`
(flash attention) · `-c 4096` · `-b 512 -ub 512` (drop to 256 for 7B) ·
`-t 6` · for 7B add `-ctk q8_0 -ctv q8_0` (KV-cache quant — **only valid with
`-fa on`**).

---

## Build flags (script 03) and why

```
cmake -B build -DGGML_CUDA=ON -DCMAKE_CUDA_ARCHITECTURES=87 \
      -DGGML_CUDA_F16=ON -DLLAMA_CURL=ON -DCMAKE_BUILD_TYPE=Release
cmake --build build -j4
```

- `GGML_CUDA=ON` — the *current* flag (`LLAMA_CUBLAS` was removed; `LLAMA_CUDA`
  is deprecated and can silently build CPU-only).
- `CMAKE_CUDA_ARCHITECTURES=87` — Orin = compute capability 8.7 (bare digits,
  **not** `sm_87`/`8.7`). Set explicitly or autodetect may yield a CPU-only binary.
- `GGML_CUDA_F16=ON` — FP16 path for Ampere.
- `LLAMA_CURL=ON` — enables `-hf user/repo:QUANT` downloads (needs
  `libcurl4-openssl-dev` at configure time).
- `-j4`, and **no `GGML_CUDA_FA_ALL_QUANTS`** — both guard against an nvcc
  compile-time OOM on the shared 7.4 GiB pool.

---

## Known gotchas baked into the scripts

1. **CMake 3.22.1 breaks the build.** Stock JetPack CMake + GCC 11 + CUDA 12.6
   dies with `arm_neon.h: '__Int8x8_t' undefined`. Script 01/03 upgrade CMake to
   ≥3.27 via a pip wheel first. *(This is the #1 blocker.)*
2. **NvMap contiguous-allocation OOM (the big one — measured on this board).**
   Loading a model with `-ngl>0` can fail with
   `NvMapMemAllocInternalTagged ... error 12` (ENOMEM) **even when `free` shows
   several GB available** — because that memory is held in (reclaimable) page
   cache and the Tegra NvMap/CMA allocator won't compact it for the large
   *contiguous* GPU buffer. Symptom: llama.cpp silently runs CPU-only and tok/s
   is ~half. **Fix (baked into 05/06): free page cache before the load** —
   `sudo sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'`. On this board that
   took a 3B from 12.5 tok/s (CPU fallback) to 22.9 tok/s (GPU). A persistent
   help is running **headless** (frees the ~1 GB the GNOME desktop holds). If it
   still OOMs, reduce `-ngl` to the highest value that loads. Stay on JetPack
   6.2.1; don't `apt upgrade` into a worse r36.4.x.
3. **IOVA accumulation.** Loading models sequentially in one process eventually
   blocks large `cudaMalloc`. **Restart the server between model swaps; load the
   largest model first.**
4. **Unified memory = one 7.4 GiB pool** shared by OS + every model. A resident
   Ollama model (held 5 min after a request) competes — `sudo systemctl stop
   ollama` before loading a large GGUF. zram swap is a crash-net only.
5. **`nvpmodel` before `jetson_clocks`** (one-way: after `jetson_clocks` you must
   reboot to change power mode). `jetson_clocks` is **not** persistent → script 06
   installs a boot service.
6. **Active cooling is effectively mandatory** under sustained MAXN_SUPER load
   (throttles silently ~80–85 °C). Keep the fan running.

---

## Monitoring

```bash
tegrastats --interval 1000     # GR3D_FREQ% (GPU busy), EMC% (bandwidth), tj@ temp, RAM
jtop                           # dashboard (after install + re-login)
journalctl -fu llama-server.service
```

## Serve on boot

```bash
./06-install-services.sh                 # serves newest model on 127.0.0.1:8080
HOST=0.0.0.0 ./06-install-services.sh     # expose on LAN (no auth — careful)
curl http://127.0.0.1:8080/v1/models
```

Ollama (port 11434) and llama-server (port 8080) **do not collide**.

---

_GIAP — marked for **device setup & optimization**. Reports land in `reports/`._
