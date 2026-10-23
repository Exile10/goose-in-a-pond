# pondcredentials

Serves Apple Music developer tokens to ponds so a household needs no Apple developer key, and
optionally relays Uber sign-ins, whose client secret cannot ship in a pond.

- `token/` (`pond-apple-token`): signs the ES256 token from a MusicKit `.p8`. **Shared**: the pond's
  `crates/pond-api` depends on it by path, so the pond and the service sign identically.
- `server/` (`pondcredentials`): the HTTP service. `POST /v1/musickit/developer-token`; the Uber
  relay's `GET /v1/uber/client`, `POST /v1/uber/token` and `POST /v1/uber/refresh` (503 unless
  `UBER_CLIENT_ID` and `UBER_CLIENT_SECRET_FILE` are set); and `GET /healthz`, which the deploy
  kit's gateway does not publish. `pondcredentials --health-check` asks its own `/healthz`.

This is its own Cargo workspace on purpose: the pond's workspace pulls in the goose submodule and
every native engine, and this has to build in a small Docker image with only this directory copied in.

```bash
cd services/pondcredentials
cargo test
```

Design, threat model and what it does to the privacy picture:
[docs/architecture/pondcredentials.md](../../docs/architecture/pondcredentials.md).
How to deploy it: [deploy/pondcredentials/README.md](../../deploy/pondcredentials/README.md).
