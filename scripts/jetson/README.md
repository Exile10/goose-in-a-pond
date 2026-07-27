# Jetson (Orin Nano) build & deploy

Everything Jetson lives here, driven by the single entry point at the repo root:

```bash
bash scripts/jetson.sh <command>
```

| Command | Where it runs | What it does |
|---|---|---|
| `deploy` | dev machine | One-command deploy: reset the nano's checkout to `origin/<branch>`, build the web UI locally (nano's Node is too old for Vite), rsync `dist/`, release-build with CUDA on the device, restart the user service, health-check. |
| `build [--cuda] [--desktop]` | ON the Jetson | Native build. `--cuda` enables GPU inference (sm_87) — GPU builds MUST be on-device (nvcc + JetPack must match). `--desktop` adds the Tauri app. |
| `docker-build [--full]` | dev machine (arm64) | CPU-only aarch64 binary via a native `linux/arm64` container (no cross/qemu on Apple Silicon). No CUDA possible here. |
| `optimize` | ON the Jetson | The llama.cpp optimization suite (`llama-optimization/`): deps, perf mode (max clocks), CUDA sm_87 llama.cpp build, model download, benchmark, services. |
| `probe-mlc [--keep]` | ON the Jetson | Experiment: MLC-LLM serve + tool-call quality probe. |

## Daily loop (dev machine)

```bash
# 1. Push to BOTH remotes — the nano pulls `origin` (JERRYFROMKENYA fork).
#    Pushing only jarida-io means the deploy silently builds stale code.
git push origin main && git push jarida-io main

# 2. Deploy (stop the device service first for big builds — the release link
#    can OOM if a model is resident: ssh nano 'systemctl --user stop goose-in-a-pond')
bash scripts/jetson.sh deploy
```

Prereqs already true on `nano.local`: ssh alias `nano` (`~/.ssh/config`, key
`nano_jetson`), `loginctl enable-linger nano`, user systemd unit
`goose-in-a-pond.service` on :8080.

## First-time device setup

```bash
ssh nano
git clone --recurse-submodules https://github.com/JERRYFROMKENYA/goose-in-a-pond
cd goose-in-a-pond
bash scripts/jetson.sh build --cuda
bash scripts/jetson.sh optimize        # perf mode + benchmark (optional but recommended)
```

## Gotchas (all learned the hard way)

- **`-C target-cpu=native` landmine**: `.cargo/config.toml` sets it for on-device
  builds. Any cross/containerized build must override it (`target-cpu=cortex-a78`
  for rustc) or the binary SIGILLs on the Orin. C/C++ needs a valid `-march` ISA
  string (`-march=armv8.2-a+fp16+dotprod`) — a CPU *name* is rustc-only.
  `ggml-toolchain.cmake` pins `GGML_NATIVE=OFF` for the same reason.
- **NvMap OOM (error 12)**: `sync && echo 3 > /proc/sys/vm/drop_caches` before
  loading a model with `-ngl` — the GPU carveout hits a ~586 MiB wall otherwise.
  Baked into the `llama-optimization` scripts.
- **Link OOM**: never run a chat/model-load while a release build is linking on
  the 8 GB device; stop the service first.
- **Memory bandwidth is the ceiling**: decode tok/s ≈ 102 / model_GB on the 8GB
  Super. Keep models ≤ GPU budget with ~1 GB headroom for KV cache.
- Hand-edits to the model registry's Jetson tuning block are reverted at every
  provider init by `apply_jetson_settings` (`pond-adapters-local-inference`) —
  the constants in code are the only knob.
