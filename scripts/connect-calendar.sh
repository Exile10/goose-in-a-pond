#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# connect-calendar.sh — connect a CalDAV calendar to a running pond.
#
#   bash scripts/connect-calendar.sh <provider> <username> <password> [base_url]
#
#   provider : google | icloud | fastmail | nextcloud | custom
#   password : an APP-SPECIFIC password. All four providers refuse the account
#              password once two-factor is on, and two refuse it always.
#   base_url : required for nextcloud and custom, ignored otherwise.
#
# Pairs this script as a device, exchanges the code for a token, and POSTs the
# source. Everything after that is the pond's own scheduled sync.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

# The messages below carry no apostrophes and no pipes on purpose: bash parses
# the text inside ${x:?...} as ordinary shell words, so one apostrophe opens a
# quote that swallows the next several lines of the script.
PROVIDER="${1:?usage - provider is google, icloud, fastmail, nextcloud or custom}"
USERNAME="${2:?usage - the account username or e-mail}"
PASSWORD="${3:?usage - an app-specific password}"
BASE_URL="${4:-}"

API="${GIAP_API:-http://127.0.0.1:8080}"
BIN="${GIAP_BIN:-./target/release/pond-server}"
CLIENT_ID="connect-calendar-$$"

say() { printf '==> %s\n' "$*"; }

say "asking the pond for a pairing code"
# Captured whole first, then matched. `... | grep | head` under `pipefail` fails
# on the SIGPIPE `head` causes, which reads as "no pairing code" on a pond that
# printed one perfectly well.
PAIRING_OUT="$("$BIN" pairing --refresh 2>/dev/null || true)"
CODE="$(printf '%s' "$PAIRING_OUT" | grep -oE 'code:[[:space:]]+[0-9]{6}' | grep -oE '[0-9]{6}' || true)"
CODE="$(printf '%s' "$CODE" | head -1)"
[ -n "$CODE" ] || { echo "could not read a pairing code from '$BIN pairing'" >&2; exit 1; }

say "starting the handshake"
INIT="$(curl -sf -X POST "$API/api/v1/handshake/init" \
  -H 'Content-Type: application/json' \
  -d "{\"client_id\":\"$CLIENT_ID\",\"client_type\":\"cli\",\"client_version\":\"1\"}")"
CHALLENGE_ID="$(printf '%s' "$INIT" | grep -oE '"challenge_id":"[^"]+"' | cut -d'"' -f4)"
CHALLENGE_B64="$(printf '%s' "$INIT" | grep -oE '"challenge":"[^"]+"' | cut -d'"' -f4)"
[ -n "$CHALLENGE_ID" ] || { echo "handshake/init did not answer a challenge: $INIT" >&2; exit 1; }

# HMAC-SHA256(key = pairing code, message = raw challenge bytes || client_id).
MAC="$( { printf '%s' "$CHALLENGE_B64" | base64 -d; printf '%s' "$CLIENT_ID"; } \
  | openssl dgst -sha256 -mac HMAC -macopt "key:$CODE" -hex | awk '{print $NF}')"

say "proving the code and taking a token"
VERIFY="$(curl -sf -X POST "$API/api/v1/handshake/verify" \
  -H 'Content-Type: application/json' \
  -d "{\"challenge_id\":\"$CHALLENGE_ID\",\"mac\":\"$MAC\",\"device_name\":\"calendar setup\"}")"
TOKEN="$(printf '%s' "$VERIFY" | grep -oE '"(session_token|token)":"[^"]+"' | head -1 | cut -d'"' -f4)"
[ -n "$TOKEN" ] || { echo "handshake/verify did not answer a token: $VERIFY" >&2; exit 1; }

say "connecting the $PROVIDER calendar"
CREDS="{\"username\":\"$USERNAME\",\"password\":\"$PASSWORD\""
[ -n "$BASE_URL" ] && CREDS="$CREDS,\"base_url\":\"$BASE_URL\""
CREDS="$CREDS}"

RESP="$(curl -s -X POST "$API/api/v1/context/sources" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"kind\":\"calendar\",\"provider\":\"$PROVIDER\",\"session_id\":\"calendar-setup\",\"credentials\":$CREDS}")"
printf '%s\n' "$RESP"

case "$RESP" in
  *'"id"'*) say "connected. The pond syncs on its own schedule; the first sweep runs 90s after start, then every 30 minutes." ;;
  *) echo "the pond refused this source -- the message above says why" >&2; exit 1 ;;
esac
