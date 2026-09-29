# pondcredentials deployment

## Scope and trust boundary

This is one small service that holds **one Apple MusicKit key** and answers **one question**: a
signed developer token, good for 30 days. It exists so a household can play Apple Music without
opening the Apple developer portal.

What it holds: the key, in a file mounted read-only. It is never in an environment variable, an
image, a log line or a response.

What it learns: that some address asked for a token. It does not read a body, set a cookie, or ask
who anyone is. It keeps no address: the rate limit is memory only, and the health counters are two
numbers. Caddy has no access log, on purpose.

What a token is worth: **anyone who can reach the service can get one**, exactly as anyone can read
the token out of any MusicKit web page. A token is not a user's account: playing still needs that
person's own Apple Music subscription and sign-in, which never leaves their pond. So the only abuse
control is the per-address rate limit (20 a minute by default) and, in the end, revoking the key.

The service is not a public offer of Jarida's key to third-party software: it is run so official
ponds need no key. Anyone can still call it; that is the limit above, stated once.

The image is pinned by digest, runs as a non-root user with no shell, on a read-only filesystem
with every capability dropped. The service's own port is on the private Compose network; only
`POST /v1/musickit/developer-token` is reachable from outside.

## What you need

1. **A MusicKit key from Apple** (Certificates, Identifiers & Profiles, Keys, with MusicKit ticked
   and a Media ID chosen). Apple lets a team hold two such keys. **Make a separate key for this
   service** and keep the one you use for testing; see "Rotating" for why the two matter.
2. **A DigitalOcean account and its command line** (`doctl`).
3. **A domain you control**, so the service can have a certificate. A subdomain is fine.

## Create the droplet

Nothing here needs the Apple key yet, and **no step should be run by an assistant**: `doctl auth init`
asks for your DigitalOcean API token, and that token stays yours.

```bash
doctl auth init
doctl compute ssh-key list
```

```bash
doctl compute droplet create pondcredentials \
  --region fra1 --size s-1vcpu-1gb --image ubuntu-24-04-x64 \
  --ssh-keys <KEY_ID_OR_FINGERPRINT> --enable-monitoring \
  --user-data-file deploy/pondcredentials/cloud-init.yaml --wait
```

`fra1` and `s-1vcpu-1gb` are examples, not advice: pick a region near your households and check
sizes and prices with `doctl compute size list`. This service is a few MB of memory and a signature
a day; the smallest droplet is plenty. The cloud-init file installs Docker and nothing else.

Lock the droplet down: SSH only from your address, HTTP and HTTPS from anywhere (Caddy needs port 80
to obtain its certificate).

```bash
doctl compute firewall create --name pondcredentials --droplet-ids <DROPLET_ID> \
  --inbound-rules "protocol:tcp,ports:22,address:<YOUR_IP>/32 protocol:tcp,ports:80,address:0.0.0.0/0,address:::/0 protocol:tcp,ports:443,address:0.0.0.0/0,address:::/0" \
  --outbound-rules "protocol:tcp,ports:all,address:0.0.0.0/0,address:::/0 protocol:udp,ports:all,address:0.0.0.0/0,address:::/0"
```

If `doctl` objects to the rule syntax, `doctl compute firewall create --help` shows the current form.

Point the name at it, then wait for DNS before the first start (Caddy asks for the certificate on
boot, and a name that does not resolve yet just delays it):

```bash
doctl compute domain records create <YOUR_DOMAIN> \
  --record-type A --record-name credentials --record-data <DROPLET_IP>
```

## First boot, on the droplet

```bash
ssh root@<DROPLET_IP>
cd /opt/pondcredentials
git clone --depth 1 --filter=blob:none --sparse <REPO_URL> repo
cd repo && git sparse-checkout set services/pondcredentials deploy/pondcredentials
cd deploy/pondcredentials
cp .env.example .env            # set CREDENTIALS_HOST, APPLE_TEAM_ID, APPLE_KEY_ID
mkdir -p runtime/secrets runtime/caddy-data runtime/caddy-config
```

Then, **from your own computer**, copy the key on. It never goes through this repository or an
assistant:

```bash
scp ~/Downloads/AuthKey_XXXXXXXXXX.p8 root@<DROPLET_IP>:/opt/pondcredentials/repo/deploy/pondcredentials/runtime/secrets/apple.p8
```

Back on the droplet, hand the file to the container's user and start:

```bash
chown 65532:65532 runtime/secrets/apple.p8 && chmod 0400 runtime/secrets/apple.p8
docker compose up -d --build
```

## Check it

```bash
curl -s -X POST https://<YOUR_DOMAIN>/v1/musickit/developer-token | head -c 160     # a token
curl -s -o /dev/null -w '%{http_code}\n' https://<YOUR_DOMAIN>/healthz                 # 404: not public
docker compose logs pondcredentials      # the key id and lifetimes; never the key, never an address
docker compose exec gateway wget -qO- pondcredentials:8080/healthz                     # the two counters
```

The service refuses to start with a missing setting or a key that cannot sign, and says which.

## Pointing ponds at it

To try one pond, set `POND_CREDENTIALS_URL=https://<YOUR_DOMAIN>` in its environment. It uses the
service only when the household has stored no key of its own, fetches about once a month, and every
fetch goes through `network_mode` and shows in the pond's egress log as `giap-credentials`.

To make it the default for every pond, put the address in `DEFAULT_MANAGED_URL` in
`crates/pond-api/src/musickit.rs`. That is a decision, not a step: from the next release every pond
without its own key calls this server, and the privacy notes (`docs/architecture/pondcredentials.md`)
should be read first.

## Rotating the key

Apple keeps a token valid until it expires **or its key is revoked**, and a pond only refetches when
its token has a fifth of its life left. So **never revoke first**:

1. Create a new MusicKit key. Put it on the droplet, change `APPLE_KEY_ID` in `.env`, and
   `docker compose up -d`. New tokens are signed by the new key at once.
2. Wait 30 days, the life of the last token signed by the old key.
3. Revoke the old key in Apple's portal.

## If the key leaks

Revoke it in Apple's portal now and deploy a new one as above. Every pond holding a token signed by
the revoked key then fails at Apple until it refetches, which can be up to about three weeks for a
token fetched a week ago. That window is the price of a monthly call, and the reason a fast rotation
is not free; if it proves too long, lower `TOKEN_TTL_DAYS` and accept more calls.

## Not verified

The Docker image has not been built and the compose stack has not been started: the machine this was
written on had no running Docker daemon. What was run: the service's tests and its real binary, the
exact `cargo build --release --locked` the Dockerfile uses, `docker compose config`, a real pond
fetching a real token from the real binary, and the `doctl` flag names against `doctl`'s own help.
The Caddyfile and every `doctl` command are unrun, and no droplet exists.
