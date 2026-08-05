#!/usr/bin/env bash
#
# Live-test a real pond-server against a scratch data directory.
#
# Green unit tests are not evidence that the pond starts. Every test in the
# Rust workspace runs against a database built by applying every migration to an
# empty file, in one process, with the adapter under test constructed by hand.
# None of that exercises startup ordering, migration application against a
# database that already has rows, route registration, the auth middleware, or
# the wiring in main.rs -- which is where several real defects have been.
#
#   scripts/live-test.sh              build if needed, run the API checks
#   scripts/live-test.sh --ui         also build the web UI and run the live
#                                     Playwright suite against this server
#   scripts/live-test.sh --keep       leave the server running when done
#   scripts/live-test.sh --no-build   use the existing binary
#
# Runs on macOS and Linux. Uses a scratch POND_DATA_DIR so it can never touch a
# real pond.

set -uo pipefail
set +m   # no job-control chatter when we kill background servers

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DATA_DIR="${POND_DATA_DIR:-${TMPDIR:-/tmp}/pond-live-$$}"
PORT="${POND_LIVE_PORT:-4000}"
DO_UI=0
DO_BUILD=1
KEEP=0

for arg in "$@"; do
  case "$arg" in
    --ui)       DO_UI=1 ;;
    --no-build) DO_BUILD=0 ;;
    --keep)     KEEP=1 ;;
    -h|--help)  sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown flag: $arg" >&2; exit 2 ;;
  esac
done

SERVER_PID=""
cleanup() {
  if [ -n "$SERVER_PID" ] && [ "$KEEP" -eq 0 ]; then
    kill -9 "$SERVER_PID" 2>/dev/null
  fi
  if [ "$KEEP" -eq 0 ]; then
    rm -rf "$DATA_DIR"
  else
    echo
    echo "Left running: pid $SERVER_PID on port $PORT, data in $DATA_DIR"
  fi
}
trap cleanup EXIT

say() { printf '\n=== %s ===\n' "$1"; }

# ── Build ────────────────────────────────────────────────────────────────────
#
# RUSTFLAGS="" matches ci.yml. .cargo/config.toml sets -C target-cpu=native for
# on-device performance, and a native-CPU artifact built on one machine SIGILLs
# on another -- which presents as `signal: 4, SIGILL: illegal instruction` from
# a test binary and reads exactly like a miscompile. It is not one.
export RUSTFLAGS=""

if [ "$DO_BUILD" -eq 1 ]; then
  say "building pond-server (RUSTFLAGS empty, per ci.yml)"
  if ! cargo build -p pond-server; then
    echo "BUILD FAILED." >&2
    echo "Before reading the diff, check two things that present as code faults:" >&2
    echo "  1. disk  -- 'No space left on device', or a linker Bus error / cc failure" >&2
    echo "  2. alsa  -- pond-server needs libasound2-dev on Linux (brew has it on Mac)" >&2
    exit 1
  fi
fi

BIN="target/debug/pond-server"
[ -x "$BIN" ] || { echo "no binary at $BIN (drop --no-build?)" >&2; exit 1; }

if [ "$DO_UI" -eq 1 ]; then
  say "building the web UI"
  ( cd pond-desktop && npm ci --silent && npm run build ) || {
    echo "UI BUILD FAILED" >&2; exit 1; }
fi

# ── Start ────────────────────────────────────────────────────────────────────
mkdir -p "$DATA_DIR"
say "starting pond-server against $DATA_DIR"

# `serve` shuts down on stdin EOF when detached, so stdin is held open.
# POND_DEV_ALLOW_LOOPBACK lets the checks reach protected routes without
# pairing a device. The auth section below deliberately runs a SECOND server
# without it -- with the bypass on, every auth assertion is vacuous.
POND_DATA_DIR="$DATA_DIR" POND_DEV_ALLOW_LOOPBACK=1 RUST_LOG=info \
  "$BIN" serve --port "$PORT" --static-dir pond-desktop/dist \
  > "$DATA_DIR/server.out" 2>&1 < /dev/zero &
SERVER_PID=$!

# --port is a request, not a promise: the fallback walks 4000..4009 and the
# port it actually bound is written to .runtime_api_port.
for _ in $(seq 1 60); do
  [ -f "$DATA_DIR/.runtime_api_port" ] && break
  sleep 1
done
if [ -f "$DATA_DIR/.runtime_api_port" ]; then
  PORT="$(cat "$DATA_DIR/.runtime_api_port")"
fi

for _ in $(seq 1 60); do
  curl -sf -o /dev/null "http://127.0.0.1:$PORT/api/v1/health" && break
  sleep 1
done
curl -sf -o /dev/null "http://127.0.0.1:$PORT/api/v1/health" || {
  echo "server never became healthy. Last 30 lines:" >&2
  tail -30 "$DATA_DIR/server.out" >&2
  exit 1
}
echo "healthy on port $PORT (pid $SERVER_PID)"

# Onboarding fronts every non-public route; lift it or everything 403s.
curl -s -o /dev/null -X PUT "http://127.0.0.1:$PORT/api/v1/settings" \
  -H 'Content-Type: application/json' \
  -d '{"user_name":"LiveTest","assistant_name":"Goose","timezone":"UTC","chat_model":"mock"}'
curl -s -o /dev/null -X POST "http://127.0.0.1:$PORT/api/v1/onboard/complete"

# ── API checks ───────────────────────────────────────────────────────────────
say "API checks"
RC=0
if [ -f scripts/live_checks.py ]; then
  POND_DATA_DIR="$DATA_DIR" python3 scripts/live_checks.py || RC=$?
else
  echo "scripts/live_checks.py not present; skipping the assertion suite"
fi

# ── Migration idempotence ────────────────────────────────────────────────────
#
# A migration that only works on an empty database works exactly once, and
# every install after the first is an upgrade. Restart against the SAME
# directory, which now has rows.
say "restart against the populated database"
kill -9 "$SERVER_PID" 2>/dev/null; wait "$SERVER_PID" 2>/dev/null
POND_DATA_DIR="$DATA_DIR" POND_DEV_ALLOW_LOOPBACK=1 RUST_LOG=info \
  "$BIN" serve --port "$PORT" --static-dir pond-desktop/dist \
  > "$DATA_DIR/server2.out" 2>&1 < /dev/zero &
SERVER_PID=$!
for _ in $(seq 1 60); do
  curl -sf -o /dev/null "http://127.0.0.1:$PORT/api/v1/health" && break
  sleep 1
done
if curl -sf -o /dev/null "http://127.0.0.1:$PORT/api/v1/health"; then
  echo "restart OK"
  python3 - "$DATA_DIR" <<'PY'
import sqlite3, sys
con = sqlite3.connect(sys.argv[1] + "/pond_system.db")
rows = con.execute(
    "SELECT version, COUNT(*) FROM _sqlx_migrations GROUP BY version "
    "HAVING COUNT(*) > 1"
).fetchall()
print("migrations applied more than once:", rows or "none")
assert not rows, "a migration was applied twice"
PY
else
  echo "RESTART FAILED -- a migration that only works on an empty database" >&2
  tail -30 "$DATA_DIR/server2.out" >&2
  RC=1
fi

# ── Auth, with the bypass OFF ────────────────────────────────────────────────
#
# Separate server on a separate port. With POND_DEV_ALLOW_LOOPBACK set, every
# assertion in this section passes regardless of what the allowlist does -- so
# running it against the server above would report the opposite of the truth.
say "auth allowlist (no token, no loopback bypass)"
AUTH_DIR="$DATA_DIR-auth"
AUTH_PORT=$((PORT + 20))
mkdir -p "$AUTH_DIR"
POND_DATA_DIR="$AUTH_DIR" RUST_LOG=warn \
  "$BIN" serve --port "$AUTH_PORT" > "$AUTH_DIR/server.out" 2>&1 < /dev/zero &
AUTH_PID=$!
for _ in $(seq 1 60); do
  curl -sf -o /dev/null "http://127.0.0.1:$AUTH_PORT/api/v1/health" && break
  sleep 1
done

# Onboarding must be complete here too, or `require_onboarding_complete`
# returns 403 BEFORE auth is evaluated and every route below looks protected
# whether it is or not. Found by running this script: the first version
# reported FAIL for /settings and /profiles on a 403, which reads like the
# right answer and is measuring the wrong gate.
#
# Both writes below are themselves on the public allowlist, which is why they
# work without a token -- that is the onboarding hole, not an accident here.
curl -s -o /dev/null -X PUT "http://127.0.0.1:$AUTH_PORT/api/v1/settings" \
  -H 'Content-Type: application/json' \
  -d '{"user_name":"AuthProbe","assistant_name":"Goose","timezone":"UTC","chat_model":"mock"}'
curl -s -o /dev/null -X POST "http://127.0.0.1:$AUTH_PORT/api/v1/onboard/complete"

for route in /settings /profiles /sessions /devices /memory; do
  code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$AUTH_PORT/api/v1$route")
  case "$code" in
    401) printf '  PASS  GET %-10s requires a token\n' "$route" ;;
    403) printf '  SKIP  GET %-10s 403 -- onboarding guard fired before auth\n' "$route" ;;
    *)   printf '  FAIL  GET %-10s returned %s with NO TOKEN\n' "$route" "$code"
         RC=1 ;;
  esac
done
kill -9 "$AUTH_PID" 2>/dev/null; wait "$AUTH_PID" 2>/dev/null
rm -rf "$AUTH_DIR"

# ── Logs ─────────────────────────────────────────────────────────────────────
#
# Read them for more than your own feature. WARN and ERROR lines that were
# already there are still findings.
say "log dig"
cat "$DATA_DIR"/server*.out 2>/dev/null \
  | sed 's/\x1b\[[0-9;]*m//g' \
  | grep -E 'WARN|ERROR|panic' \
  | sed 's/^[0-9TZ:.-]* *//' \
  | sort | uniq -c | sort -rn
if grep -qi panic "$DATA_DIR"/server*.out 2>/dev/null; then
  echo "PANIC in the server log" >&2
  RC=1
fi

# ── UI ───────────────────────────────────────────────────────────────────────
if [ "$DO_UI" -eq 1 ]; then
  say "live UI (Playwright against THIS server, no mocks)"
  ( cd pond-desktop && \
    POND_LIVE_URL="http://127.0.0.1:$PORT" \
    npx playwright test --config=playwright.live.config.ts ) || RC=1
fi

say "result"
if [ "$RC" -eq 0 ]; then
  echo "live test PASSED"
else
  echo "live test FAILED (rc=$RC)"
  echo
  echo "NOTE: this script currently FAILS BY DESIGN on the auth section."
  echo "GET /settings and GET /profiles answer with no token -- PAI-2 P0, a"
  echo "live defect in is_public_route, which matches on path while its entries"
  echo "read as method-scoped. It is not a regression in your change. When P0"
  echo "lands, this section should go green and stay green."
fi
exit "$RC"
