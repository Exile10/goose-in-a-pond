#!/usr/bin/env bash
# Rotate the Headscale administration key the enrollment service authenticates with.
# The key is read once at startup, so the order is: mint, install, recreate, verify,
# and only then expire the previous key. Never echoes the key value. Run as root: the
# secret is owned by the enrollment user, UID 65532, and readable by nobody else.
set -euo pipefail
cd "$(dirname "$0")"

EXPIRY="${1:-90d}"
SECRET=runtime/secrets/headscale_admin
strip() { sed 's/\x1b\[[0-9;]*m//g'; }
hs() { docker compose exec -T headscale headscale "$@" < /dev/null; }
# The enrollment service's own /health body, fetched from inside the gateway, the one
# container on its network with a shell and busybox wget (the service's image has
# neither). Docker's health check passes on any 200, and a service whose key the
# coordinator refuses still answers 200, with "degraded":true.
enrollment_health() { docker compose exec -T gateway wget -q -T 5 -O- http://enrollment:8081/health < /dev/null 2>/dev/null; }
# A minted key on a line of its own; anything else Headscale prints is not the key.
KEY_LINE='^hskey-api-[A-Za-z0-9_-]{12}-[A-Za-z0-9_-]+$'

# The key being replaced is the one enrollment holds, named by its own prefix
# (hskey-api-<prefix>-<secret>), not guessed from a table: scraping `apikeys list`
# expired every key it could parse, including ones other tools rely on.
# Headscale lists a key as `hskey-api-<12-character prefix>-***`; the prefix is base64url, so it
# may hold `_` and `-`.
key_prefix() { sed -n 's/^hskey-api-\(.\{12\}\)-.*/\1/p' "$1" | head -1; }
# The ID of the one key Headscale lists under this prefix, or nothing.
key_id() {
  hs apikeys list -o json | python3 -c '
import json, sys
wanted = "hskey-api-" + sys.argv[1] + "-***"
ids = [str(k["id"]) for k in (json.load(sys.stdin) or []) if k.get("prefix") == wanted]
print(ids[0] if len(ids) == 1 else "")' "$1"
}
old_prefix="$(key_prefix "$SECRET")"
[ -n "$old_prefix" ] || { echo "cannot read the current key's prefix from $SECRET; aborting" >&2; exit 1; }
old_id="$(key_id "$old_prefix")"
[ -n "$old_id" ] || { echo "the current key ($old_prefix) is not listed by Headscale; aborting" >&2; exit 1; }
echo "current key: $old_prefix (id $old_id)"
enrollment_health > /dev/null || { echo "cannot read enrollment's /health through the gateway; start the gateway first. Nothing was changed" >&2; exit 1; }

# Until the new key is confirmed, every exit, an error under set -e included, puts the
# previous key back in place and expires the new one, so a failed rotation leaves
# exactly the key that was working and no stray live key.
new_prefix=""
new_id=""
installed=0
confirmed=0
finish() {
  status=$?
  # A second interrupt must not stop the restore halfway.
  trap '' HUP INT TERM
  rm -f "$SECRET.raw" "$SECRET.new"
  if [ "$confirmed" = 1 ] || [ -z "$new_prefix" ]; then
    exit "$status"
  fi
  [ "$status" != 0 ] || status=1
  if [ "$installed" = 1 ]; then
    echo "restoring the previous key ($old_prefix) and recreating enrollment" >&2
    # Without the previous key back in place, recreating enrollment or expiring the new key
    # could lock enrollment out, so both keys stay live for a person to settle.
    if ! { mv "$SECRET.prev" "$SECRET" && chown 65532:65532 "$SECRET" && chmod 400 "$SECRET"; }; then
      echo "could not restore $SECRET from $SECRET.prev; both keys are still live: restore it by hand, recreate enrollment, then expire $new_prefix${new_id:+ (id $new_id)}" >&2
      exit "$status"
    fi
    docker compose up -d --force-recreate enrollment >/dev/null 2>&1 \
      || echo "could not recreate enrollment; run docker compose up -d --force-recreate enrollment" >&2
  fi
  if [ -n "$new_id" ]; then
    echo "expiring the unconfirmed new key $new_prefix (id $new_id)" >&2
    hs apikeys expire --id "$new_id" | strip >&2 \
      || echo "could not expire $new_prefix (id $new_id); expire it by hand" >&2
  else
    echo "the new key $new_prefix was not listed by Headscale; check headscale apikeys list and expire it by hand" >&2
  fi
  exit "$status"
}
trap finish EXIT
trap 'exit 1' HUP INT TERM

echo "minting a ${EXPIRY} key"
( umask 077; hs apikeys create --expiration "$EXPIRY" > "$SECRET.raw" )
# Only the key line goes into the secret: anything else in the output would be read
# as part of the key, and the coordinator would refuse it.
( umask 077; grep -E -m 1 "$KEY_LINE" "$SECRET.raw" > "$SECRET.new" ) \
  || { echo "mint printed no key line; the old key is intact, but expire any key it did mint (headscale apikeys list)" >&2; exit 1; }
rm -f "$SECRET.raw"
minted_prefix="$(key_prefix "$SECRET.new")"
[ "$minted_prefix" != "$old_prefix" ] || { echo "the new key has the old key's prefix; aborting with the old key intact" >&2; exit 1; }
new_prefix="$minted_prefix"
new_id="$(key_id "$new_prefix")"
[ -n "$new_id" ] || { echo "Headscale does not list the new key ($new_prefix); aborting" >&2; exit 1; }
echo "new key: $new_prefix (id $new_id)"

cp -a "$SECRET" "$SECRET.prev"
installed=1
mv "$SECRET.new" "$SECRET"
chown 65532:65532 "$SECRET"
chmod 400 "$SECRET"

echo "recreating enrollment so it reads the new key"
docker compose up -d --force-recreate enrollment >/dev/null 2>&1

state=unknown
for _ in $(seq 1 30); do
  state="$(docker inspect -f '{{.State.Health.Status}}' goose-remote-access-enrollment-1 2>/dev/null || echo unknown)"
  [ "$state" = healthy ] && break
  sleep 2
done
[ "$state" = healthy ] || { echo "enrollment did not become healthy (state=$state)" >&2; exit 1; }

# Healthy is not enough: confirm the service reconciled with the coordinator on the
# new key. It does so before it first answers, so this is quick when the key works.
health=""
for _ in $(seq 1 30); do
  health="$(enrollment_health || true)"
  case "$health" in
    *'"degraded":true'*) ;;
    *'"ok":true'*) break ;;
  esac
  sleep 2
done
case "$health" in
  *'"degraded":true'*) echo "enrollment is degraded on the new key: the coordinator did not accept it" >&2; exit 1 ;;
  *'"ok":true'*) ;;
  *) echo "enrollment's /health did not answer ok within 60 seconds" >&2; exit 1 ;;
esac
confirmed=1
echo "enrollment reconciled on the new key"

echo "expiring previous key $old_prefix (id $old_id)"
hs apikeys expire --id "$old_id" | strip \
  || { echo "could not expire the previous key $old_prefix (id $old_id); enrollment works on the new key, so expire the old one by hand" >&2; exit 1; }
rm -f "$SECRET.prev"

echo "=== keys after ==="
hs apikeys list | strip
echo "=== policy still installed ==="
hs policy get | strip
