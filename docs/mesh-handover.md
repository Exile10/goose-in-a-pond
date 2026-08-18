# Private Pond Compute (#132) — Handover

**Author:** Purity (wanjiru@jarida.io)
**Date:** 2026-08-18
**Branch:** `feat/mesh-inference` (reset — see "What happened to the branch" below)
**Status:** Feature-complete for mesh borrowing/lending + settlement scaffolding. Not committed, not pushed. Live-verified.

---

## What this is

Trust-scoped P2P inference mesh: a Pond can borrow chat completions from another
trusted Pond's compute over libp2p, track usage per peer, and (once an exchange
rate is set) settle the balance over Lightning. Built in milestones:

1. Mesh domain types + ports (`pond-core`)
2. libp2p transport (`pond-adapters-mesh-libp2p`) — handshake/trust-pin gated
3. Inference borrowing (`pond-adapters-mesh-inference`) — `MeshInferenceService`,
   wire protocol for requests/chunks
3.5. Wired into `GooseAdapter` so `chat_provider = "mesh"` drives a real chat turn
4. Peer capability query (live "what does this peer offer right now")
5. Invoice exchange + Lightning payment rail (`pond-adapters-lightning`, Breez/Spark)
6. Settlement service — periodic job that pays down pending usage once a real
   exchange rate is configured

---

## What happened to the branch

`feat/mesh-inference` was built from an old base and had drifted **332 commits
behind `fork/main`** — not just in mesh, in everything (frontend rewrite, TTS
engine swap Kokoro↔Piper, dependency major bumps, etc.). A real `git rebase`
would have replayed the whole history through unrelated churn.

Instead:
- The old branch tip is preserved at **`feat/mesh-inference-old-base`** (full
  248-commit history, nothing lost).
- `feat/mesh-inference` was **reset to `fork/main`** (currently `517f3a07`).
- Only the mesh-specific delta was ported forward as a fresh, working-tree
  change set on top of current main — see "What's included" below.
- **Deliberately dropped**: the branch's own `pond-adapters-mesh-libp2p`
  implementation, in favor of main's newer one (main had independently
  hardened the reconnection logic — likely fixes the flakiness chased during
  live testing earlier in this effort).

Nothing is committed. The full delta sits as uncommitted changes on
`feat/mesh-inference`, verified building and passing tests on top of main.

---

## What's included

**Backend**
- `pond-mesh-protocol/src/wire.rs` — `MeshFrame` wrapper, `InferenceRequest`/
  `InferenceChunk`, `InvoiceRequest`/`InvoiceResponse`, `CapabilityRequest`/
  `CapabilityResponse`
- `pond-core/src/mesh/` — `capabilities` domain type, `PeerCapabilityQuery` +
  `InvoiceRequester` ports (+ mocks), `SettlementService`, `PaymentRail::batch_settle`
  gained a real `invoice: &str` parameter (closes the old "no invoice
  exchange protocol" gap)
- `pond-adapters-mesh-inference` (new crate) — `MeshInferenceService`
  (owns the mesh transport's single `recv()` consumer), `MeshInferenceProvider`
  (borrower-side `LlmProvider`)
- `pond-adapters-lightning` (new crate) — `LightningPaymentRail` over
  `breez-sdk-spark`. **Untested against a real wallet** — no `BREEZ_API_KEY` yet.
- `pond-adapters-goose/src/mesh_provider.rs` — bridges `LlmProvider` to Goose's
  `Provider` trait; `GooseAdapter` gained `.with_mesh_provider()` and a `"mesh"`
  arm in its provider-switch logic
- `pond-server/src/main.rs` — `build_payment_rail`, `build_mesh_provider`,
  `spawn_settlement_job` (always spawned, 15 min interval, no-ops until a real
  exchange rate is set)
- `pond-api` — routes: `GET/POST /mesh/peers`, `DELETE /mesh/peers/{id}`,
  `POST /mesh/peers/{id}/credit`, `GET /mesh/peers/{id}/capabilities`,
  `GET /mesh/self`, `GET /mesh/settlement` (read-only — no setter for the
  exchange rate; that's a settings-API-only knob until the rate is decided)
- `Settings`: `mesh_enabled` (now UI-wired, was headless), `lightning_enabled`,
  `mesh_settlement_millisats_per_token` (default `0` = deliberate no-op)

**Frontend**
- `Mesh.tsx` — trust-circle peer list, credit top-up modal, live capability
  chips (inference/lightning), mesh-enable toggle, read-only settlement panel
- `PondApiClient.ts` / `types.ts` — `topUpMeshPeer`, `getMeshPeerCapabilities`,
  `getMeshSettlementStatus`, `MeshPeerCapabilities`, `MeshSettlementStatus`

**Real bugs found and fixed while porting** (introduced by main's independent
evolution, not by this work):
- `pond-adapters-mesh-libp2p`'s `Libp2pMeshTransportConfig` gained a required
  `peer_directory` field (a real security improvement — connections are now
  gated on trust at the transport level). Updated the mesh-inference crate's
  own integration tests to grant mutual trust before connecting.
- Two independent "does this field name look like a secret?" guards
  (`pond-core` unit test + a `pond-api` integration test) both false-positived
  on `mesh_settlement_millisats_per_token` (contains "token"). Added to both
  allowlists with a reason, as the guards themselves require.

---

## Verified

- `cargo fmt --check`, clippy on all fast crates + `pond-adapters-mesh-libp2p`: clean
- Production-binary gate (`pond-server` + `pond-adapters-goose --all-targets`): clean
- 1,400+ tests across every touched crate: all passing
- `pond-desktop`: `npx tsc --noEmit` clean for all touched files (pre-existing
  unrelated errors elsewhere in the codebase, not from this work)
- **Live smoke test**: two real Ponds (A + B), fresh databases, real
  handshake/pairing flow, `mesh_enabled` toggled + restarted → real libp2p
  transport came up with a genuine peer ID and invite URL, settlement job
  spawned and logged correctly. All three mesh routes returned real data.

---

## Known issues — flagged, not fixed (out of scope for this pass)

1. **`lightning` feature has a real dependency conflict.** `breez-sdk-spark`
   (git-pinned) pulls `axum 0.7`, clashing with the workspace's `axum 0.8` —
   the duplicate dependency graph cascades into an unrelated build failure in
   `pond-adapters-whisper` (`MicHandle` methods not found). `mesh` alone
   builds clean; only `--features lightning` (with or without `mesh`) breaks.
   Moot until the Breez API key exists to test against anyway.

2. **GGUF filename-resolution bug — confirmed still present on main.**
   `LocalInferenceLlmAdapter::new_with_data_dir(model, dir)` (called from both
   `pond-api/src/routes.rs` and `pond-server/src/main.rs`) guesses a model's
   on-disk filename as `{catalog_name}.gguf`. For HF-sourced catalog entries
   where the real filename differs from the catalog's short alias (e.g.
   catalog name `llama-3.2-3b`, real file
   `Llama-3.2-3B-Instruct-Q4_K_M.gguf`), the guess fails and inference errors
   with "Model not found". A `model_repo` field exists on `AppState`/
   `GooseAdapter` already, but it's wired only for `ModelRecord.context_length`
   lookups (PAI-3's context governor) — not filename resolution. Neither call
   site pre-resolves the name through the catalog. Real, separate fix; not
   part of this mesh delta by explicit request.

---

## Open product decisions (need Jerry / team input)

- **Exchange rate** (`mesh_settlement_millisats_per_token`): genuinely
  undecided. Job is a safe no-op at `0` until someone picks a number.
- **Breez API key**: Purity requested it herself from Spiral grantees — not
  blocked on Jerry, just blocked on Breez's turnaround.
- Broader payment model (credit counting semantics, real crypto vs. play
  money, revenue share) — all still open per earlier discussion.

---

## Next steps

1. Decide: commit this delta as-is (clean history, fresh commits on top of
   main) vs. further review.
2. Push + open PR against `jarida-io/goose-in-a-pond` main — **needs explicit
   sign-off before opening**, this repo's standing rule for PRs to the
   external repo.
3. Once a Breez key exists: verify `pond-adapters-lightning` against a real
   testnet wallet, then tackle the `axum` version conflict for the
   `lightning` feature.
4. Separately: fix the GGUF filename-resolution bug (scoped, well-understood,
   just intentionally not bundled into this mesh work).
