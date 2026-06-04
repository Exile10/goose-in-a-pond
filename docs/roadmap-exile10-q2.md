# Roadmap proposal — Exile10 (`jarida-io/goose-in-a-pond`)

_Generated 2026-06-04. Covers all **17** open issues assigned to **Exile10**.
With #109 now yours, **every dependency is self-owned — there are no external blockers.**_

## Dependency graph (all self-owned)

```
AUTH / MOBILE
  #4  Handshake protocol (Q1) ─┐
                               ├─► #93 Real handshake + DB token store ─┬─► #8  Auth middleware
  (your local WIP realizes ────┘   (M)                                  ├─► #94 Auth hardening (contract/loopback/CORS)
   #4 & #93)                                                            └─► #95 Wire gotg + push-token store ─► #99 Push notifications (L)

DEVICES / SENSORS / AUTOMATION
  #84 DeviceController port (S) ─┐
                                 ├─► #92 Rules engine (L) ─► (feeds) #130 Vision Event Detection MCP (L, stretch)
  #91 EventBus port (S) ─────────┘                            ▲
                                  └──────────────────────────┘ (#130 also needs #91)

OBSERVABILITY / PRIVACY  (now a fully-owned chain)
  #108 Event domain + EventLog port (M) ─► #109 Durable SQLite adapter + migration (M) ─► #114 Activity query API (M) ─┐
        │                                                                                                              ├─► #115 Audit MCP (M)
        ├─► #113 Egress tracker (M) ───────────────────────────────────────────────────────────────────────────────┘
        └─► #117 Retention controls (S)

VOICE (standalone)
  #105 Command-chaining validation (S)   — no dependencies
```

✅ **No external dependencies.** Every issue on every chain is assigned to you.

## Critical path (longest chains)

1. **Observability:** `#108 (M) → #109 (M) → #114 (M) → #115 (M)` — **four mediums deep, the longest pole.**
2. **Automation+Vision:** `(#84 + #91) → #92 (L) → #130 (L)` — two larges stacked.
3. **Mobile:** `#93 (M) → #95 (S) → #99 (L)`.

The unlock move is unchanged: **finish auth (in flight) + land the three small/medium ports
(#91, #84, #108) early.** They gate the three big items and the whole observability chain. The
observability chain is now the longest, so start `#108 → #109` as soon as the auth thread is closed.

---

## Proposed roadmap

### Phase 0 — Close the auth thread you're already mid-merge on  ⏱ now
You have uncommitted handshake WIP (`sqlite_handshake`, two-phase pairing). Land it before it rots.
- **#93** Q2-16 Real handshake/pairing + DB-backed token store (M) — _realizes #4_
- **#8** Authentication middleware (S) — enforce `validate_token` on all routes but `/health`+handshake
- **#94** Q2-17 Auth hardening: align `session_token`/`expires_in` contract, kill the loopback
  bypass, scope CORS, rate-limit remote (S–M)
- **#4** closes automatically once #93 lands — dedupe/close it (don't double-build)

➡ **Ship:** real, persisted, validated auth. Closes a live security hole (any Bearer accepted from anywhere).

### Phase 1 — Foundational ports (small, independent, unblock everything)  ⏱ next, can overlap Phase 0
Three independent ports — highest leverage in the whole plan.
- **#91** Q2-14 `EventBus` port + publish on sensor/camera ingest (S) → unblocks #92, #130
- **#84** Q2-07 `DeviceController` port + mock + tests (S) → unblocks #92 (and Q2-09…12 for others)
- **#108** Q2-31 Unified `Event` domain + `EventLog` port (M) → head of the observability chain

➡ **Ship:** the three abstractions the rest of Q2 is built on.

### Phase 2 — Observability & privacy stack  ⏱ start right after #108 (longest pole — begin early)
- **#109** Q2-32 Durable `EventLog` SQLite adapter + migration, wire the writers (M) — _needs #108_
- **#113** Q2-36 Network-egress / privacy-risk tracker (M) — _needs #108_ (records via #109)
- **#117** Q2-40 Per-category retention + "clear my activity" (S) — _needs #108_ (parallel with #113)
- **#114** Q2-37 Activity query API (M) — _needs #109_
- **#115** Q2-38 Logs / Privacy-Audit MCP (M) — _needs #113 + #114_

➡ **Ship:** "what did you do today?" with egress visibility + user-controlled retention.
Because this is the longest chain, kick off `#109` the moment `#108` lands.

### Phase 3 — Finish the mobile loop  ⏱ after Phase 0
- **#95** Q2-18 Wire `pond-adapters-gotg` + add `push_tokens` storage (S) — _needs #93_
- **#99** Q2-22 Push-notification path (L) — _needs #95_; foreground SSE/WS + offline queue first,
  then FCM/APNs

➡ **Ship:** a paired phone receives real push notifications.

### Phase 4 — Automation engine  ⏱ after Phase 1
- **#92** Q2-15 Sensor/event-triggered rules engine (L) — _needs #84 + #91_; reuses the scheduler
  execution path; actions = device control (#84) + notify (#99 path)

➡ **Ship:** "motion after sunset → lights on + notify" fires end-to-end.

### Phase 5 — Stretch: vision  ⏱ last
- **#130** Q2-53 Vision Event Detection MCP (L) — _needs #91 + #92 (both yours, done by Phase 4)_
  capture → on-device motion/pet/package → `camera_events` → EventBus → automation reacts

➡ **Ship:** camera frame triggers an automation, fully on-device.

### Quick-win track — slot anytime
- **#105** Q2-28 Command-chaining validation harness + tuning (S) — zero dependencies; capability
  already exists, so it's mostly scripted tests + `agent_max_turns` tuning. Good palate-cleanser.

---

## One-line sequence

```
NOW   Phase 0  #93 → #8 → #94   (+ close #4)          ← finish your in-flight auth work
 ║    Phase 1  #91 ∥ #84 ∥ #108                        ← foundational ports, overlap Phase 0
 ╠══► Phase 2  #109 → (#113 ∥ #117) ; #109 → #114 → #115   ← observability (longest pole — start early)
 ╠══► Phase 3  #95 → #99                                ← mobile push
 ╠══► Phase 4  #92                                      ← automation (needs #84+#91)
 ╚══► Phase 5  #130                                     ← vision stretch (needs #91+#92)
  ·   anytime  #105                                      ← quick win
```

## Effort & reality check

- **Small (S ×6):** #8, #84, #91, #95, #105, #117
- **Medium (M ×8):** #4, #93, #94*, #108, #109, #113, #114, #115
- **Large (L ×3):** #92, #99, #130

17 issues — 3 large + 8 medium — is a **full quarter-plus of solo work**, and taking the blockers
means you now own four-deep dependency chains with no one to parallelize against. Honest call:
this is more than one person can ship in a quarter at quality.

**If/when help frees up, delegate the leaves** (they don't block your other work):
- **#105** (standalone), **#117** (only needs #108), **#109** or **#113** (only need #108),
  and the **FCM/APNs phase of #99**.
- **Keep yourself:** the auth thread (#93/#8/#94) and the three ports (#91/#84/#108) — they're the
  spine everything else hangs off, and you already have the auth context in your worktree.
