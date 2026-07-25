#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# 06-install-services.sh — install systemd units so the box is "AI-ready" on
# every boot:
#
#   giap-jetson-perf.service  (oneshot)  -> MAXN_SUPER + jetson_clocks at boot
#   llama-server.service      (daemon)   -> llama-server with full GPU offload
#
# Usage:
#   ./06-install-services.sh                      # install both, serve newest model
#   MODEL=/home/nano/models/foo.gguf ./06-install-services.sh
#   HOST=0.0.0.0 PORT=8080 ./06-install-services.sh   # expose on LAN (careful!)
#   ./06-install-services.sh --perf-only          # only the boot perf service
#   ./06-install-services.sh --uninstall
# ---------------------------------------------------------------------------
set -uo pipefail
source "$(dirname "$0")/lib/common.sh"
require_jetson
# Must run as the normal user (it elevates per-command). Under sudo, $HOME would
# be /root and BIN_DIR/MODELS_DIR would resolve wrong.
[ "$(id -u)" -eq 0 ] && [ -n "${SUDO_USER:-}" ] && \
  die "Run this as '$SUDO_USER' (NOT via sudo) — it elevates individual steps itself."

SVC_USER="${SUDO_USER:-$USER}"
HOST="${HOST:-127.0.0.1}"            # default: localhost only (safe)
PORT="${PORT:-8080}"
CTX="${CTX:-4096}"
NGL="${NGL:-99}"
BATCH="${BATCH:-512}"                # drop to 256 for a 7B model
UBATCH="${UBATCH:-512}"
PERF_BIN="/usr/local/sbin/giap-jetson-perf"
MAXN="$(awk -F'[ =]' '/POWER_MODEL/ && /MAXN/ {for(i=1;i<=NF;i++) if($i=="ID"){print $(i+1); exit}}' /etc/nvpmodel.conf 2>/dev/null)"; MAXN="${MAXN:-2}"

uninstall() {
  sudo_prime
  for u in llama-server giap-jetson-perf; do
    as_root systemctl disable --now "$u.service" 2>/dev/null || true
    as_root rm -f "/etc/systemd/system/$u.service"
  done
  as_root rm -f "$PERF_BIN"
  as_root systemctl daemon-reload
  ok "Services removed."; exit 0
}
[ "${1:-}" = "--uninstall" ] && uninstall

sudo_prime

# --- boot-time performance service ----------------------------------------
hdr "Installing giap-jetson-perf.service (MAXN_SUPER + jetson_clocks at boot)"
as_root tee "$PERF_BIN" >/dev/null <<EOF
#!/usr/bin/env bash
# Applies maximum sustained-performance mode. Installed by 06-install-services.sh
/usr/sbin/nvpmodel -m $MAXN || true
/usr/bin/jetson_clocks || true
for g in /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor; do echo performance > "\$g" 2>/dev/null || true; done
EOF
as_root chmod +x "$PERF_BIN"
as_root tee /etc/systemd/system/giap-jetson-perf.service >/dev/null <<EOF
[Unit]
Description=GIAP Jetson max-performance mode (MAXN_SUPER + jetson_clocks)
After=nvpmodel.service
[Service]
Type=oneshot
ExecStart=$PERF_BIN
RemainAfterExit=yes
[Install]
WantedBy=multi-user.target
EOF
as_root systemctl daemon-reload
as_root systemctl enable --now giap-jetson-perf.service && ok "Perf service enabled & applied"

[ "${1:-}" = "--perf-only" ] && { ok "Done (perf service only)."; exit 0; }

# --- llama-server service --------------------------------------------------
hdr "Installing llama-server.service"
SERVER_BIN="$(command -v llama-server 2>/dev/null || echo "$BIN_DIR/llama-server")"
[ -x "$SERVER_BIN" ] || die "llama-server not found — run ./03-build-llama-cpp.sh first."

MODEL="${MODEL:-$(ls -t "$MODELS_DIR"/*.gguf 2>/dev/null | head -1)}"
[ -n "$MODEL" ] && [ -f "$MODEL" ] || die "No model found in $MODELS_DIR — run ./04-download-model.sh first (or set MODEL=)."
ok "Serving model: $MODEL"
[ "$HOST" = "0.0.0.0" ] && warn "HOST=0.0.0.0 exposes the server to your whole LAN with no auth."

as_root tee /etc/systemd/system/llama-server.service >/dev/null <<EOF
[Unit]
Description=llama.cpp server (CUDA, GPU-offloaded) — GIAP
After=network-online.target giap-jetson-perf.service
Wants=network-online.target
# If a model persistently OOMs on load, fail instead of looping forever.
StartLimitIntervalSec=120
StartLimitBurst=4
[Service]
Type=simple
User=$SVC_USER
Environment=LD_LIBRARY_PATH=/usr/local/cuda/lib64
# Free page cache (as root, hence '+') so full GPU offload doesn't ENOMEM on the
# Tegra NvMap allocator when loading the model into unified memory.
ExecStartPre=+/bin/sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'
# GGUF load from NVMe into unified memory is slow; don't let systemd kill it mid-load.
TimeoutStartSec=600
ExecStart=$SERVER_BIN -m "$MODEL" --host $HOST --port $PORT -ngl $NGL -fa on -c $CTX -b $BATCH -ub $UBATCH -t $(nproc) --parallel 1
Restart=on-failure
RestartSec=3
[Install]
WantedBy=multi-user.target
EOF
as_root systemctl daemon-reload
# enable + restart (NOT enable --now): on a re-run to swap models, --now would
# no-op on the already-running unit, leaving the old model loaded.
as_root systemctl enable llama-server.service >/dev/null 2>&1
as_root systemctl restart llama-server.service && ok "llama-server.service enabled & (re)started"
sleep 2
as_root systemctl --no-pager --lines=8 status llama-server.service 2>/dev/null || true
echo
ok "Test:  curl http://$HOST:$PORT/v1/models"
ok "Logs:  journalctl -fu llama-server.service"
warn "Note: Ollama (if running) uses port 11434 — no port conflict with $PORT,"
warn "but it shares the 7.4GiB unified pool. 'sudo systemctl stop ollama' before"
warn "loading a large model. To swap the served model, edit MODEL= and re-run this"
warn "script (restarting the process also clears IOVA buildup — load largest first)."
if [ "$NGL" = "99" ]; then
  warn "If the service fails to load a model with a CUDA OOM (see logs), lower NGL:"
  warn "  NGL=24 ./06-install-services.sh   (L4T r36.4.x allocator quirk)"
fi
