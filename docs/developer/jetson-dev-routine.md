# Jetson dev routine

The Jetson Orin Nano (`nano.local`) is a standing deployment target: GIAP runs
there as an always-on service, and a one-command deploy pushes the current
`main` to it from the dev machine.

## Access

```bash
ssh nano          # alias in ~/.ssh/config → nano@nano.local, key ~/.ssh/nano_jetson
```

mDNS (`nano.local`) occasionally flakes; the box usually holds `192.168.1.14`
on the LAN, so an IP-based fallback alias is useful if resolution fails.

## The service

GIAP runs as a **user-level** systemd unit (no sudo needed to manage it, and
`loginctl enable-linger` is set, so it starts at boot and survives logout):

```bash
ssh nano systemctl --user status goose-in-a-pond    # status
ssh nano systemctl --user restart goose-in-a-pond   # restart
ssh nano journalctl --user -u goose-in-a-pond -f    # follow logs
```

- Unit file: `~/.config/systemd/user/goose-in-a-pond.service` on the Jetson
- Binary: `~/goose-in-a-pond/target/release/pond-server` (CUDA build)
- Dashboard: **http://nano.local:8080** (8080 because a user service cannot
  bind 80 without capabilities; the embedded web UI serves same-origin)
- Inference: Ollama system service (`llama3.2:3b` — ~22 tok/s, fits the 8GB
  GPU budget; see `docs/developer/inference_optimization.md` for the
  fit-verdict rules and the NvMap drop_caches gotcha)

## Deploying

```bash
bash scripts/jetson-deploy.sh                 # deploy origin/main
bash scripts/jetson-deploy.sh --branch mybr   # any branch pushed to the Jetson's origin
```

The script: hard-resets the Jetson checkout to the pushed branch (submodule
included), builds the web UI **on the dev machine** (the Jetson's Node is v12;
Vite needs ≥ 18) and rsyncs `pond-desktop/dist` over, runs the on-device CUDA
release build (`--features pond-adapters-local-inference/cuda`;
`target-cpu=native` is correct on-device), restarts the user service, and
health-checks `GET /api/v1/health`.

Deploys build on the Jetson's `origin` remote (the GitHub mirror), so push
there first — or push to `jarida-io` and sync the mirror.

## Gotchas

- **No passwordless sudo** on the Jetson — anything touching system units,
  apt, or root-owned files needs an interactive `ssh -t nano "sudo …"`.
- A cold CUDA build takes well over an hour on the Nano; the deploy script's
  build step is incremental after the first run. Keep `target/release`;
  `target/debug` is pure waste on this box (`cargo clean` reclaims ~20GB).
- The GPU is memory-bandwidth-bound: keep models ≤ the GPU budget
  (`tok/s ≈ 102 / model_GB`, ~1GB headroom for KV cache).
