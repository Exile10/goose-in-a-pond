#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# mlc-toolcall-probe.sh — Experiment A: is MLC-LLM "fast AND well" for GIAP?
#
# Run this ON the Jetson Orin Nano. It stands up MLC-LLM's OpenAI server with the
# prebuilt Llama-3.2-3B q4f16 weights and runs mlc_toolcall_probe.py — the
# decisive test of whether MLC emits reliable STREAMING tool_calls under GIAP's
# ~50-tool / ~12K-token payload (the exact thing that broke direct llama.cpp).
#
#   PASS (>=95% of turns) -> MLC is worth wiring behind GooseAdapter for ~2x
#     decode (43 vs 22 tok/s, NVIDIA Jetson AI Lab benchmark on the Super).
#   FAIL -> stay on Ollama; MLC's speed is a mirage for our tool loop.
#
# Usage (on the Nano):
#   bash scripts/jetson.sh probe-mlc            # serve, probe, tear down
#   bash scripts/jetson.sh probe-mlc --keep     # leave the server up for
#                                               # the GIAP end-to-end test
#
# Prereqs: Docker + nvidia runtime (already on the box), JetPack 6.2, MAXN SUPER.
# First run builds/pulls the MLC container and JIT-compiles the model lib for
# sm_87 — expect 15-40 min. Subsequent runs reuse ~/mlc-cache and are fast.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

KEEP=false
[ "${1:-}" = "--keep" ] && KEEP=true

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROBE_PY="${SCRIPT_DIR}/mlc_toolcall_probe.py"
JC_DIR="${HOME}/jetson-containers"
CACHE_DIR="${HOME}/mlc-cache"
CONTAINER="giap-mlc-probe"
MODEL_URL="HF://mlc-ai/Llama-3.2-3B-Instruct-q4f16_1-MLC"
PORT=8000
BASE="http://127.0.0.1:${PORT}"

[ -f "${PROBE_PY}" ] || { echo "ERROR: ${PROBE_PY} missing (rsync it alongside this script)"; exit 1; }

echo "═══ Experiment A: MLC-LLM streaming tool_calls under GIAP payload ═══"
nvpmodel -q 2>/dev/null | grep -i "power mode" || true

# ── 1. jetson-containers (provides the sm_87 MLC container + autotag) ─────────
if [ ! -d "${JC_DIR}" ]; then
  echo "==> cloning dusty-nv/jetson-containers"
  git clone --depth=1 https://github.com/dusty-nv/jetson-containers "${JC_DIR}"
  bash "${JC_DIR}/install.sh" || true
fi
export PATH="${JC_DIR}:${PATH}"

echo "==> resolving MLC container image (may build/pull on first run)…"
IMAGE="$(cd "${JC_DIR}" && ./autotag mlc 2>/dev/null | tail -1)"
[ -n "${IMAGE}" ] || { echo "ERROR: could not resolve the mlc image via autotag"; exit 1; }
echo "    image: ${IMAGE}"

# ── 2. serve MLC (detached, host network so the probe hits 127.0.0.1:PORT) ────
mkdir -p "${CACHE_DIR}"
docker rm -f "${CONTAINER}" >/dev/null 2>&1 || true
echo "==> starting mlc_llm serve on :${PORT} (first run compiles the sm_87 lib)…"
docker run -d --name "${CONTAINER}" --runtime nvidia --network host \
  -v "${CACHE_DIR}:/data" \
  -e MLC_JIT_POLICY=ON \
  "${IMAGE}" \
  mlc_llm serve "${MODEL_URL}" --host 0.0.0.0 --port "${PORT}" --mode server

cleanup() {
  if [ "${KEEP}" = false ]; then
    echo "==> stopping container"
    docker rm -f "${CONTAINER}" >/dev/null 2>&1 || true
  else
    echo "==> --keep: leaving '${CONTAINER}' serving at ${BASE}"
    echo "    GIAP end-to-end test next: set chat_provider so Goose posts to ${BASE}/v1"
    echo "    stop later with: docker rm -f ${CONTAINER}"
  fi
}
trap cleanup EXIT

# ── 3. wait for readiness (long — first run JIT-compiles the model lib) ───────
echo "==> waiting for the OpenAI endpoint (up to 40 min on first compile)…"
for i in $(seq 1 240); do
  if curl -sf -m 5 "${BASE}/v1/models" >/dev/null 2>&1; then
    echo "    ready after ~$((i*10))s"; break
  fi
  if ! docker ps --format '{{.Names}}' | grep -q "^${CONTAINER}$"; then
    echo "ERROR: container exited early. Last logs:"; docker logs --tail 40 "${CONTAINER}" 2>&1 || true
    exit 1
  fi
  sleep 10
  [ "$i" = 240 ] && { echo "ERROR: server not ready after 40 min"; docker logs --tail 40 "${CONTAINER}"; exit 1; }
done

# ── 4. the decisive probe ────────────────────────────────────────────────────
echo ""
echo "==> running the streaming tool_call probe (20 turns, ~12K-token tool payload)"
set +e
python3 "${PROBE_PY}" --base "${BASE}" --turns 20 --threshold 0.95
RC=$?
set -e

echo ""
if [ "${RC}" -eq 0 ]; then
  echo "RESULT: PASS — MLC survives the tool payload. Next: point GooseAdapter at ${BASE}/v1"
  echo "        and run real GIAP turns (Experiment A step 4)."
else
  echo "RESULT: FAIL — MLC breaks under GIAP's streaming tool loop. Stay on Ollama."
fi
exit "${RC}"
