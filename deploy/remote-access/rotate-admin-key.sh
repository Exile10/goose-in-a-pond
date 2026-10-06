#!/usr/bin/env bash
# Rotate the Headscale administration key the enrollment service authenticates with.
# The key is read once at startup, so the order is: mint, install, recreate, verify,
# and only then expire the previous key. Never echoes the key value.
set -euo pipefail
cd "$(dirname "$0")"

EXPIRY="${1:-90d}"
SECRET=runtime/secrets/headscale_admin
strip() { sed 's/\x1b\[[0-9;]*m//g'; }
hs() { docker compose exec -T headscale headscale "$@" < /dev/null; }

# The key being replaced is the one enrollment holds, named by its own prefix
# (hskey-api-<prefix>-<secret>), not guessed from a table: scraping `apikeys list`
# expired every key it could parse, including ones other tools rely on.
# Headscale lists a key as `hskey-api-<12-character prefix>-***`; the prefix may hold `_`.
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

echo "minting a ${EXPIRY} key"
( umask 077; hs apikeys create --expiration "$EXPIRY" > "$SECRET.new" )
[ -s "$SECRET.new" ] || { echo "mint produced nothing; aborting with the old key intact" >&2; rm -f "$SECRET.new"; exit 1; }

cp -a "$SECRET" "$SECRET.prev"
mv "$SECRET.new" "$SECRET"
chown 65532:65532 "$SECRET"
chmod 400 "$SECRET"

echo "recreating enrollment so it reads the new key"
docker compose up -d --force-recreate enrollment >/dev/null 2>&1

for i in $(seq 1 30); do
  state="$(docker inspect -f '{{.State.Health.Status}}' goose-remote-access-enrollment-1 2>/dev/null || echo unknown)"
  [ "$state" = healthy ] && break
  sleep 2
done

if [ "${state:-unknown}" != healthy ]; then
  echo "enrollment did not become healthy (state=$state); restoring the previous key" >&2
  mv "$SECRET.prev" "$SECRET"
  chown 65532:65532 "$SECRET"; chmod 400 "$SECRET"
  docker compose up -d --force-recreate enrollment >/dev/null 2>&1
  exit 1
fi
echo "enrollment healthy on the new key"

new_prefix="$(key_prefix "$SECRET")"
if [ -z "$new_prefix" ] || [ "$new_prefix" = "$old_prefix" ] || [ -z "$(key_id "$new_prefix")" ]; then
  echo "the new key is not the one Headscale lists; keeping the previous key ($old_prefix) live" >&2
  exit 1
fi
echo "expiring previous key $old_prefix (id $old_id)"
hs apikeys expire --id "$old_id" | strip
rm -f "$SECRET.prev"

echo "=== keys after ==="
hs apikeys list | strip
echo "=== policy still installed ==="
hs policy get | strip
