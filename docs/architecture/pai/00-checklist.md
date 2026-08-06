# PAI working checklist — read this first, every time

This is the scratchpad and standing checklist for the
[Personal Agentic Intelligence programme](../personal-agentic-intelligence.md).

**If you are about to touch anything in PAI-1 through PAI-8, read this file before you start and
update it before you stop.** It is deliberately committed rather than left in `.ai/` (which is
gitignored) so it survives a fresh clone and a recycled container.

---

## 1. The eight requirements, as originally stated

Recorded verbatim in intent, because paraphrase drifts. These are the source of truth for what the
programme is for; the PAI numbers are only the order I chose to build them in.

| Req | As asked for | Doc | Status |
|---|---|---|---|
| 1 | GIAP needs to be **proactive**, not just reactive | [PAI-7](./07-proactive-intelligence.md) | DESIGNED |
| 2 | Requires the ability of the model to **think** | [PAI-5](./05-reasoning-and-thinking.md) | DESIGNED |
| 3 | **Multi-agent orchestration** | [PAI-6](./06-multi-agent-orchestration.md) | DESIGNED |
| 4 | **Hard profile boundaries** | [PAI-1](./01-identity-and-profile-boundaries.md) | **COMPLETE — P1-P8 LANDED** |
| 5 | **Large context**, using each model's window dynamically and to the fullest | [PAI-3](./03-context-governor.md) | **P1-P4, P6 LANDED** (P3 completed by P3b 2026-08-06); **P5 code landed 2026-08-06, awaiting the on-device TTFT measurement that decides it** |
| 6 | **Smart compaction** based on different models, time and cache age | [PAI-4](./04-smart-compaction.md) | **P1, P2, P3, P4, P6 LANDED 2026-08-06** (P1 `ModelClass` + strategy dispatch, domain only; P2 large-tier re-summarisation — the rolling summary rebuilt from the source messages instead of from its own previous output, gated on `ModelClass::Large` and wired into `run_compaction_pass`, so P1 now has its first consumer; P3 age-weighted retention — `compaction_verbatim_days` plus a tool-result rung between "leave it alone" and "drop the whole turn", fed by `Message::created` on the live Goose trim path and firing only when a conversation is already over budget; P4 compact-on-resume gate, wired to the session reopen; P6 `should_compact` now moves the server between turns, rate-limited by `claim_compaction`, and `reset_session` has its first production caller); **P5 code landed 2026-08-06, awaiting the warm-versus-cold TTFT measurement that decides it** (`prefix_cache.rs`: `PrefixCacheState` + the six `InvalidationReason`s recorded in `goose_agent.rs`, `Agent::prefix_cache_state` defaulting to `None`, and the trimmer's age rung now firing on a cold prefix as well as over budget — the warm half was already P3's); **P7a (the API half) LANDED 2026-08-06** (`POST /sessions/{id}/compact` — the manual axis, running the same `run_compaction_pass` behind the same `claim_compaction` rate limiter as P6, and reporting a `status`/`reason` pair with the session's real utilisation when it refuses); **P7b (the desktop control) RESPECIFIED and outstanding** — the bullet said "a control on the existing `ContextCard`", but `ContextCard.tsx` is the MCP-UI tool-result renderer and the shipped app has no context-pressure surface at all (zero consumers for the `context_warning` frame), so P7b is a new surface across both chat views, not a button |
| 7 | **Personal context streaming** — on-pond, on-mobile, and internet accounts | [PAI-8](./08-personal-context-streaming.md) | DESIGNED |
| 8 | **Privacy and security guardrails** to minimise data and secret exposure | [PAI-2](./02-privacy-and-security-guardrails.md) | **P0-P5, P7 LANDED** (P3, P5 partial); P6, P8 designed |

They are equally weighted and mutually interdependent. `DESIGNED` means the document exists and its
current-state claims were verified against code; it does **not** mean any code has changed. `LANDED`
is stamped per phase, and means the gates in 2.3 were run and passed.

**PAI-1 is COMPLETE** as of 2026-08-05: P1-P8 all landed, including P4's policy layer and the
repair of P5, which was recorded as landed while being inert on every default install. PAI-3 P1/P2
and PAI-2 P0 are landed, PAI-2 P4 encrypts `secrets.json` at rest, and PAI-2 P1 is complete: the
mode, the identity-assertion call site, and the draft-decision gate that gives a staged action an
owner. **PAI-2 P5 landed 2026-08-05**: `network_mode` (`open`/`allowlist`/`offline`) now refuses an
outbound call before the socket opens, at five of the eighteen HTTP-sending files in the tree.
Everything else is still design only.

Read `UNGATED_SENDERS` in `crates/pond-core/tests/egress_guard.rs` before you tell anyone
`network_mode = "offline"` means offline. Six real-egress files are still ungated — model
downloads, the OAuth refresh loop, Spotify, the HF blob cache, the vision-encoder download, the MCP
connectivity probe — and they belong to P6. The list is capped and the cap only moves down. Note
also that the guard classifies FILES, not crates: the crate-level rule the design asked for ("every
`reqwest`-using crate references `record_egress`") would have failed for eight of eleven crates on
the day it landed, and most of what it flagged was a health probe on 127.0.0.1.

One consequence of P4 worth knowing before you go looking for it: `giap.sh doctor` now FAILs on
every pond that has not yet been restarted on a P4 binary, because the store on those really is
plaintext. That is the check working. Do not downgrade it to a warning.

---

## 2. The recursive check

Run this every time, in order. It is short on purpose — a checklist nobody completes is worse than
none.

### 2.1 Before starting work on any PAI item

1. **Re-read this file and the [master roadmap](../personal-agentic-intelligence.md)**, then the
   specific PAI document. The roadmap's dependency graph decides whether this item is even eligible
   yet.
2. **Check the prerequisites are LANDED, not merely DESIGNED.** PAI-6 with PAI-3 unlanded gives
   every subagent a context budget derived from a number four code paths disagree about.
3. **Re-verify the current-state claims you are about to rely on.** Every document carries a
   verification date. Line numbers rot faster than prose — grep for the symbol, never trust the
   `file:line`. If a claim is now false, fix the document in the same change; a design doc that lies
   is worse than no design doc.

   Two failure modes seen in practice, both mine: **counted claims** go wrong when you grep for a
   pattern rather than the thing itself (the extension count was wrong twice because one
   registration uses a const, not a string literal), and **a correction that makes a discrepancy
   vanish** deserves more suspicion than one that creates work.
4. **Re-read the seven cross-cutting invariants** in the master roadmap section 3. They bind all
   eight workstreams.

### 2.2 While working

5. **Ask what this change does to the other seven.** That is what "interdependent" means in
   practice, and it is the check most likely to be skipped:
   - Does it move the **KV prefix**? (invariant 1 — measured 3.7 s re-prefill on the Orin)
   - Does it add tokens to the **preamble** rather than the working set? (PAI-3's asymmetry rule)
   - Does it introduce a path where `profile_id` can be `None`? (PAI-1's entire lesson)
   - Does it create a new **egress** point without `record_egress`? (invariant 4)
   - Does it put a **secret** on `Settings`? (PAI-2 — `GET /settings` serialises the whole struct)
   - Does it let a **`Guest`** session reach personal data? (PAI-1, PAI-8)
   - Does it make anything **block a turn on an LLM call**? (invariant 2)
   - Does it perform a side-effecting action **without approval**? (PAI-7's propose-do-not-act rule)
6. **Keep policy in `pond-core`, mechanism in adapters** (invariant 5). If it would need rewriting
   when Goose is swapped, it is in the wrong crate.

### 2.3 Before stopping

7. **Stamp the phase** `LANDED` or `DEFERRED` in its PAI document, matching the convention in
   `context-and-reasoning-roadmap.md`. A deferral is written down with its reason and its cost, not
   silently dropped.
8. **Update the status column** in section 1 above and in the roadmap's table.
9. **Update section 4 below** — the open questions and running notes.
10. **Run the gates.** `cargo fmt --check`, then the fast-crate `cargo clippy` / `cargo test` set
    from `.github/workflows/ci.yml`, then `cargo check -p pond-server -p pond-adapters-goose` for
    anything touching the live path. Note the submodule caveat in section 3.
11. **Run the live server** (section 2.4). Not optional for anything that changes a migration, a
    route, a handler or startup wiring.
12. **Clear the documentation debt** attached to the workstream (roadmap section 4) while you are
    in the file. It is one line each and it never gets cheaper.

### 2.4 Live server verification

Green unit tests are not evidence that the pond starts. Every test in this repo runs against a
database built by applying every migration to an empty file, in one process, with the adapter under
test constructed by hand. None of that exercises startup ordering, migration application against a
database that already has rows, route registration, the auth middleware, or the wiring in
`main.rs` — and those are where the last several defects in this programme actually were.

Run this for any change touching a **migration, a route, a handler, or startup wiring**:

1. **Build and start it against a scratch data dir**, so the run cannot touch a real pond:
   ```bash
   POND_DATA_DIR=/tmp/pond-live cargo run -p pond-server -- serve --port 4000
   ```
   Read the port it actually bound from `$POND_DATA_DIR/.runtime_api_port` — `--port` is a request,
   and the fallback walks `4000..4009`.
2. **Confirm the migration applied to a real file**, not just to a fresh in-memory fixture:
   ```bash
   sqlite3 "$POND_DATA_DIR/pond_system.db" "SELECT version, description, success FROM _sqlx_migrations ORDER BY version DESC LIMIT 3;"
   ```
   Then re-run the server against the **same** directory. A migration that only works on an empty
   database is a migration that only works once, and every install after the first is an upgrade.
3. **Exercise the routes you touched with real HTTP**, including the failure cases. A handler that
   compiles and a handler that returns the right status for a missing row are different claims.
4. **Read the logs, and read them for more than your own feature.** `WARN` and `ERROR` lines that
   were already there are still findings. Two of this programme's real defects were visible in
   startup output long before anyone looked.
   ```bash
   grep -iE 'error|warn|panic|failed' "$POND_DATA_DIR"/logs/*.log | grep -v <known-benign>
   ```
5. **Correct what the logs show**, in the same change. A log line you decided to ignore gets written
   down in section 4 with the reason, or it will be rediscovered as new.

`serve` shuts down on stdin EOF when detached, so background it with stdin held open (`sleep
infinity | cargo run ... &`) or it exits immediately and looks like a crash.

---

## 3. Known environment traps

- **The Goose submodule is uninitialized in a fresh clone.** `cargo test -p pond-core` fails at
  *manifest load*, before compiling anything, because the workspace path-depends on
  `goose/crates/goose/Cargo.toml`. This is not a regression and not something a code change caused.
  Fix with `git submodule update --init --recursive`; CI sidesteps it by cloning the fork branch tip
  directly (`.github/workflows/ci.yml:52-55`). Any Goose-side claim in these documents was verified
  against a separate checkout of `jarida-io/Goose:main` and is labelled as such.
- **`.cargo/config.toml` sets `-C target-cpu=native`.** Override it for any cross or containerised
  build or the binary can SIGILL on the target.
- **No emojis anywhere, including comments** — `no-emoji.test.ts` scans source and will fail the
  build.
- **`pond-server` needs ALSA headers, and a stale apt index looks like "no apt access".**
  `pond-server` depends unconditionally on `cpal`, so `alsa-sys` must find `libasound2-dev`; without
  it the production-binary gate cannot run at all, and neither can `pond-voice`, `pond-audio`,
  `pond-adapters-whisper` or `pond-adapters-piper`. A container may ship the runtime `libasound.so.2`
  and not the headers. `apt-get install libasound2-dev` on a fresh container 404s on every mirror,
  which reads as a sandbox restriction; it is a stale index. **Run `apt-get update` first.** I
  recorded "no apt access" in a commit message on the strength of the 404 alone and it was wrong.
- **A stray `pond-server` on port 4000 silently hijacks `scripts/live-test.sh`.** Check before
  running it: `lsof -nP -iTCP:4000-4009 -sTCP:LISTEN`. A long-lived `serve --native` with no
  `POND_DATA_DIR` runs against the *real* data directory, and the script used to assume port 4000 —
  so every assertion, and both onboarding writes, went to that pond instead of the scratch one. The
  script now reads `.runtime_api_port`, fails hard when it is absent, and refuses to drive a
  listener whose pid it did not start. See the 2026-08-05 entry in section 4.
- **Disk is a fixed allowance and the failure mode is disguised.** `cargo test -p pond-api` builds
  seventeen integration binaries at ~600 MB each, because every one links Goose statically. That
  alone exceeds the allowance. The symptom is
  `collect2: fatal error: ld terminated with signal 7 [Bus error]` or `No space left on device` from
  a random dependency — both read as a code fault or a broken toolchain. Run the targets one at a
  time with `cargo test -p pond-api --test <name>`, deleting `target/debug/deps/<name>-*` between
  runs. `df` shows low "Used" with zero "Avail" in this state; that is the allowance, not the disk.

---

## 4. Running notes and open questions

Append here as work proceeds. Dated entries, newest last.

**2026-08-03 — programme designed, nothing implemented.**

- All nine documents written and cross-checked: every referenced path resolves, every relative link
  works, no forward-pointing prerequisites.
- The single highest-value fix identified across the whole programme is one line: `trim_goose_history`
  resolves the context window from `std::env::var("GOOSE_CONTEXT_LIMIT")` with a hardcoded 8192
  fallback (`goose_agent.rs:1478-1481`), so a correctly configured 3K Jetson can be trimming against
  a window that does not exist. It belongs to PAI-3 P1.
- **Open:** PAI-2 lands the policy layer in `audit` mode, not `enforce`. Someone has to decide, from
  real audit logs, when to flip it. A permissions matrix written from first principles will be wrong
  in ways only real traffic reveals, and an authorisation regression in a home assistant looks like
  the lights not turning on.
- **Open:** PAI-1 phase P1 is specified as a behaviour-identical refactor. If it is not — if any
  existing single-user install sees different memory recall after it — the phase is wrong and should
  be reverted rather than patched forward.
- **Open:** the whole programme assumes one household per pond. PAI-7 states it as a deferral. If
  that assumption ever breaks, PAI-1's `ProfileScope::Household` is the type that has to change, and
  it is load-bearing for six of the eight.

**2026-08-03 (later) — documentation debt cleared; still no feature code.**

All six rows of the roadmap's documentation-debt table are done, each re-verified against code
first. Corrected: `token_tracking.md` (real provider usage is the primary path, chars/4 is only the
fallback when a provider emits no `Usage` events), `scheduling.md` (three `TaskKind` variants and
twelve MCP tools, not two and seven), `api.md` (handshake token validation is real — SHA-256 lookup
with revocation and expiry), `model_capabilities.md` (the sixth field, `tool_calling`), and
`security/ports/policy.rs` (the loopback bypass is off by default behind `POND_DEV_ALLOW_LOOPBACK`
since #94, not unconditional).

**One of the six was my own error, not the repo's.** I had recorded that `CLAUDE.md` understated the
extension count at 14 while `giap_registration.rs` registered 15. Recounting gives exactly **14**
`register_builtin_extension` calls, so `CLAUDE.md` was right all along. The roadmap row is corrected
and PAI-6 section 3.6 now says `giap-orchestrator` would make **15**, not 16. Worth noting as a
warning: that claim was wrong in the first commit of a document whose whole value is being accurate,
which is exactly why check 2.1.3 exists. Re-verify; do not trust a prior pass, including mine.

**The build baseline is unblocked.** `goose/` can be populated from a clone of `jarida-io/Goose:main`
when the submodule is uninitialized, which is what CI does. With it in place `cargo fmt --check`
passes and `cargo test -p pond-core --lib` runs. Do this before assuming a cargo failure is yours.

**2026-08-03 (evening) — PAI-3 P1 started. STOPPED MID-PHASE; read this before resuming.**

### Done and committed

`ContextGovernor` in `models/services/context/context_governor.rs`, exported through
`context/mod.rs` and the compatibility re-export in `models/services/mod.rs`. Types:
`WindowResolution { tokens, source }`, `WindowSource` (five rungs, with `label()` and `is_exact()`),
`EngineWindow { tokens, model }`, `ContextInputs`. Plus `prompt_window()`, which is the existing
local 8192 prompt-side clamp moved in unchanged.

**Domain only — no call site repointed, so behaviour is unchanged.** 13 unit tests;
`cargo test -p pond-core --lib` 704 passed / 0 failed; `cargo fmt --check` clean.

### The exact remaining work for P1

Four call sites, all in `crates/pond-adapters-goose/src/goose_agent.rs`. Verified 2026-08-03:

| Line | What it does now | What it should become |
|---|---|---|
| ~636 | `env GOOSE_CONTEXT_LIMIT` else 8192, in the session-hydration replay path | `ContextGovernor::resolve` with session scope; `engine_reported` available here |
| ~1478 | `env GOOSE_CONTEXT_LIMIT` else 8192, in `trim_goose_history` | same — **this is the bug the whole phase exists to fix** |
| ~907 | `effective_context_window` → `resolve_context_window` (pinned > override > heuristic) | delegate to the governor, passing `registry_pinned` |
| ~2182, ~2366 | `prompt_budget_ctx(provider, effective_context_window(...))` | `ContextGovernor::prompt_window(provider, resolve(...).tokens)` |

Also: `pond-agent/src/agent.rs:375` (`min(override, caps)`) and `pond-api/src/routes.rs:1674-1685`
(`TurnStats > override > caps`) are the other two of the four disagreeing paths named in the design.

### Three things to be careful about

1. **`GOOSE_CONTEXT_LIMIT` is still written and must stay written.** `goose_env_knobs`
   (`goose_agent.rs:131`) sets it to the raw resolved window, and it flows into Ollama's
   `options.num_ctx` — so the reported limit, the request's `num_ctx` and the KV cache agree.
   Deleting the *reads* is the goal; deleting the *write* would desynchronise them. Two adapter
   tests assert the written value (`goose_agent.rs:4224,4239`).
2. **The env var carries the raw window, not the prompt budget.** Sites ~636 and ~1478 feed
   `CompactionProfile::from_context_window` directly with it, while ~2182 and ~2366 clamp through
   `prompt_budget_ctx` first. That asymmetry is deliberate and already correct; preserve it when
   repointing, or the preamble silently grows on local providers.
3. **Wire `engine_reported` only where the value is session-scoped and model-matched.** For the
   settings path (`apply_goose_env_knobs`) pass `None` — it is process-wide, and a session's last
   turn is not evidence about it. This keeps that path behaviour-identical.

### Verification P1 still owes

The regression test from the design doc section 7: assert no file under `models/services/context/`
and no adapter budget path calls `std::env::var`. The module-level half of that already exists
inside `context_governor.rs`; the adapter half cannot be written until the two reads are gone.

`cargo check -p pond-adapters-goose` is the gate for all of the above and is slow from cold —
start it early.

**2026-08-04 — PAI-3 P1 LANDED.**

All four paths repointed; `prompt_budget_ctx` deleted from the adapter. Gates: `cargo fmt --check`
clean, `pond-core` 707 passed, `pond-adapters-goose` 100 passed / 1 ignored, `cargo check` clean on
`pond-api` and `pond-agent`.

Corrections to what the previous entry assumed:

- There were **two** env reads, not one — `trim_goose_history` and session hydration.
- The replacement is an adapter-owned `last_window` field written by `apply_goose_env_knobs`
  *before* its signature guard returns early, not a settings load per turn. A settings load remains
  only as the cold path, for a session hydrated before any turn has configured a provider.
- `routes.rs` fell back to live `agent.capabilities()`, which is better data than the name
  heuristic. `ContextInputs::capability_window` exists so that path keeps its accuracy; precedence
  there is unchanged (engine > override > caps).
- `pond-agent` used `min(override, caps)` and now lets the override win. A real behaviour change,
  taken deliberately because the crate is quarantined (Q2-05) and a fourth divergent precedence
  would defeat the phase.

**Two process lessons worth keeping.**

Adding a field to `ContextInputs` broke an existing literal with `E0063`. That is the design
working: every field is a precedence decision, so do **not** reach for `..Default::default()` at
construction sites — the compile error is the review.

A backgrounded `cargo ... | tail -N` reports the **exit code of `tail`**, so a run that failed to
compile was reported as success. Never trust the status of a piped cargo run; capture per-command
exit codes or read the output.

**2026-08-04 — PAI-3 P2 LANDED, and a four-way audit of all eight documents.**

P2: `TokenCounter` port, `HeuristicTokenCounter`, tiktoken-backed adapter on the live path.
**Exactness was not achievable** — the design assumed a GGUF tokenizer would be reachable and it is
not (private module in the fork; `pond-inference`'s belongs to the quarantined agent and would
double-load the model). Both counters report `is_exact() == false` and the overshoot-feedback
correction stays load-bearing. Gates: fmt clean, pond-core 710, adapter 103, `pond-api` and
`pond-agent` check clean.

I then ran four parallel read-only agents over all eight design docs, one pair each. Worth repeating
before any future phase — it found more than the phase itself did.

**The security finding, which outranks everything else in this programme:** `is_public_route` is
path-only while its entries read as method-scoped, and `public_routes.merge(protected_routes)` puts
every route behind that single check. `GET /settings` returns all API keys, and
`DELETE /profiles/{id}` deletes a household member, **with no token**. Now PAI-2 P0.

**I had corrected a correct claim into a wrong one.** The extension count is **15**, not 14 — two
agents found this independently. `giap-toolkit` registers through the `TOOLKIT_EXTENSION` const, so
my `grep -oE '"giap-[a-z-]+"'` could not see it, and I "fixed" the roadmap in the wrong direction
with confidence. Tool count is **61**, not 57. Lesson: **count call sites, not string literals**,
and be most suspicious of a correction that makes a discrepancy disappear.

Other corrections applied: PAI-5's prompt/engine thinking inconsistency is **already fixed** (its P3
is a no-op); `GOOSE_AUTO_COMPACT_THRESHOLD` is not local-gated, only the tool-pair knob is; there is
**no pairing lockout** and its absence is deliberate (a lockout would be a guest-triggerable DoS);
`Profile` has six fields not five; `ModelRecord.context_length` *is* written for the curated GGUF
catalog (and only there — see the 2026-08-06 entry: the correction was right and still incomplete,
because it stopped at "is written" without asking *by which providers*); PAI-8 undercounted the multipart upload routes (`/voice/calibrate` and
`/sessions/{id}/identify-user` also take uploads), which matters because PAI-1 and PAI-2 lean on
that absence argument.

**Line-number rot is systemic**, and PAI-3 caused some of it by editing the very files the docs
cite. Prefer symbol names over `file:line` when writing these documents.

**2026-08-04 — PAI-1 P1 and P2 LANDED.**

P1: `ProfileScope { Owner | Household | Guest }`, and `MemoryRepository`'s five search methods now
take `&ProfileScope` instead of `Option<&str>`. Every call site passes `Household`, whose SQL is
byte-identical to the old `None` branch, so nothing changed behaviourally — which was the point.

P2: migration `0037_session_identification.sql`, `SessionIdentity` / `IdentificationSource`, two new
`SessionStorage` methods, and `AppState.session_user_bindings` deleted with its three handlers
repointed at the column.

**Three things the design doc got wrong, all found by code rather than by reading.**

1. **`sessions.profile_id` already existed** — since migration `0003`, unwritten and unread for
   thirty-four migrations. A dead column. The design called for adding it. P2 shrank to wiring.
2. **Wiring it would have broken member deletion.** The 0003 column has no `ON DELETE` action, and
   `Database::init` sets `PRAGMA foreign_keys = ON`. That is harmless only while the column is
   always NULL. `0037` carries a `BEFORE DELETE` trigger standing in for the `ON DELETE SET NULL`
   SQLite will not let us add in place, and a test proves deleting a member with a live session now
   succeeds. **Nothing in the design pass predicted this.** The pattern worth generalising: a
   column nothing writes has no observable constraints, so its declaration has never been tested.
   Wiring a dead column is not a no-op — it activates whatever was declared around it.
3. **Three of the four `profile_id` foreign keys already cascade on delete** -- `memory_fragments`
   (`0005`), `face_embeddings` (`0013`), `face_profile_thresholds` (`0014`). `sessions` was the only
   one declared without an action. Most of P7 turned out to be built; its real job is per-category
   counts. The audit table is in PAI-1 section 3.7.

**P2 deliberately ships no `SessionIdentity -> ProfileScope` conversion.** Every session in every
existing pond is unattributed, so the method would have to answer "what scope is an unidentified
session" today, and the only behaviour-preserving answer — `Household` — is exactly the
scope-widening default invariant 2 calls a bug. P3 decides it with the paired-device, explicit and
face inputs in hand. Shipping a default now would mean un-shipping it later.

**Two defects found by reading my own diff, not by any test.** Both were in code that compiled and
passed everything:

- The identify handler read the existing binding with `unwrap_or_else(|_| unknown())`. A failed read
  would then look like "nobody is bound", letting a weak face match take over a paired-device
  session -- the exact downgrade `supersedes` was written to refuse. **A fallback default on an
  authorisation input is a widening default**, and invariant 2 says access narrows on failure. Fixed
  to refuse the write.
- `set_session_identity` was updating `updated_at`. `list_sessions` orders by it, so a camera
  recognising somebody would have reordered the user's chat history with no message sent. Metadata
  about a session is not activity in it. Fixed, and pinned with a test.

Neither was reachable by the tests I had written, because both are about what happens on a path the
tests do not take. Worth budgeting review time for the diff itself, separately from the gates.

**The live server run, which is now a standing gate (section 2.4).** Built `pond-server`, started it
against a scratch data dir, and drove the routes with `curl`. All of it passed:

- Migration `0037` applied to a real file, `success = 1`, and applied **once** across two starts.
  Both provenance columns and the trigger are present in `sqlite_master`.
- `GET /sessions/{id}/user` reports `profile_id: null, identification_source: "unknown"` for an
  unidentified session; a bound session reports its owner, `face`, and the confidence.
- `DELETE` on an unknown session is 404; `GET` on one is 200 and says nobody. The asymmetry is
  deliberate and it now holds over HTTP, not just in a unit test.
- **`DELETE /profiles/{id}` returned 204 with a session bound to that member**, and the session
  survived with its attribution released. That is the foreign-key trap from finding 2, fixed and
  proven live rather than argued.
- A binding written before a restart was still there after it. The deleted in-memory map could not
  have done that, which is the whole point of P2.

**Two false passes in my own check script, caught by reading its output.** The first run reported
PASS for `body.get("profile_id") is None` — against an `onboarding_required` error body, where every
lookup returns None. **A check that passes because the request failed reports the opposite of the
truth.** The script now asserts the status code first and only then any body predicate. Same class
of error as the vacuous tiktoken tests in the PAI-3 P2 entry; it is worth assuming I have written
one every time.

**And the live run reproduced PAI-2's P0 from a running server**, with the loopback bypass off and
no token: `GET /settings` returned API keys in plaintext, `GET /profiles` listed the household, and
`DELETE /profiles/{id}` returned 204. Meanwhile `/sessions`, `/devices` and `/memory` correctly
returned 401 — so the auth layer works and the defect is exactly the path-only allowlist match. Full
table in PAI-2's P0 entry. This is what section 2.4 exists for: the defect was in the code the whole
time, and one `curl` found it in seconds.

Startup logs across three runs held two WARNs, both environmental rather than defects: the embedding
model download is blocked by this container's proxy (403), and `auto_download` skips
`llamafile/mock` because my own live-check onboarding set that as the chat model. Recorded so the
next run does not rediscover them as new.

**The environment claim I got wrong.** I recorded "this container has no apt access" in a commit
message, on the strength of `apt-get install libasound2-dev` 404ing. It was a stale index;
`apt-get update` fixed it in one command, and `pond-server` builds. Section 3 now records both this
and the disk-allowance trap, which disguises itself as a linker bus error.

**2026-08-04 (later) — PAI-1 P3 resolver landed; the chain has a missing rung.**

`identity_resolution::resolve` is in, six tests, no call site yet — same shape as PAI-3 P1, domain
first so the wiring is small.

**Nothing links a paired device to a household member.** `session_tokens`, `push_tokens`,
`pairing_codes` and `handshake_challenges` all lack a profile column; the pairing flow never asks
who is pairing. That is the *strongest* rung of the designed chain and it does not exist. The input
is honoured and fed `None` by everyone.

**This is not only PAI-1's problem.** PAI-7 assumes it can address a notification to "the profile's
devices" and gives that as the first real producer for the dormant targeted-notification path. It
cannot, for the same reason. Capturing a member at pairing time unblocks both, and it should
probably be its own small phase rather than buried in either.

I did not fall back to `settings.primary_profile_id`. It would have made the chain look complete and
attributed every phone in the house to one person.

**One design decision worth arguing with.** An unidentified speaker resolves to `Household` in a
one-member pond and `Guest` in a shared one. The two scopes only differ when there is somebody to be
excluded from, so this is not a weakening — but it does mean adding a second household member
silently changes what an unidentified voice can reach. If that should instead be an explicit setting,
now is the time to say so, before P4 builds enforcement on top of it.

**2026-08-04 (later still) — PAI-1 P3 wired, P7 and P8 landed. Five parallel recon agents.**

I ran five read-only Explore agents in parallel over P3/P4/P5/P6/P7+P8 and then implemented
serially. **Parallel implementation agents are not viable here** and it is worth writing down why:
each needs its own cargo target dir, which means a fresh ~20 GB Goose build against a 2 GB
allowance, and sharing one target dir just serialises them on the cargo lock. The win from
multi-agent in this repo is read-only fan-out, not concurrent writes. Same conclusion PAI-6 reaches
about on-device subagents: the benefit is isolation, not wall-clock.

The recon paid for itself three times over:

- **`ProfileContext` reaches the model nowhere.** `routes.rs` binds the built prompt as
  `let mut _system_prompt` -- underscore-prefixed, deliberately unused -- and `goose_agent.rs`
  passes `None` for the profile with a TODO. So this checklist's own claim that
  `settings.primary_profile_id` "reaches the prompt" was **stale on both engine paths**. P6 is
  therefore "wire it at all", not "switch its source". Corrected in PAI-1 section 1.5.
- **P7 cannot cascade drafts or schedules.** `drafts` has no owner column (keyed by `session_id`, no
  FK) and **there is no `schedules` table at all** -- the scheduler is in-process tokio-cron.
  *(Drafts half fixed 2026-08-05 by PAI-2 P1, migration 0038 + a `BEFORE DELETE ON profiles`
  trigger. Schedules half still true.)*
- **`approve_draft` has no ownership check whatsoever.** Any session can approve any draft id. That
  is a live authorisation defect, not a Guest-degradation gap, and it belongs to PAI-2 rather than
  here. *(CLOSED 2026-08-05 by PAI-2 P1 -- see that document's P1 entry.)*

**The compiler found what a careful read-only sweep did not.** Making `AgentRequest.profile_scope`
non-optional broke twelve construction sites; the recon inventory listed eleven. `delegation.rs`
was the twelfth. Fan-out recon is good at mapping and still not a substitute for a type that
refuses to compile.

**Two more vacuous tests, again mine.** One P8 test used profile ids the fixture never creates, so
its "owner sees the shared row" assertion passed against an empty result set. Its sibling failed
loudly and exposed it. That is three vacuous-test incidents in this programme; the pattern is always
an assertion that holds trivially when the setup is wrong. Assert the *positive* case too --
"alice owns at least one row" -- not only the boundary.

**2026-08-04 (evening) — an audit agent found the defect the whole workstream turned on.**

I ran two read-only agents: one adversarial review of the five landed commits, one sweep for client
breakage. The review found something that invalidated a claim I had been making all session.

**`ProfileScope::Owner` was a no-op in production.** Every memory write path set `profile_id: None`,
so `scope_sql`'s `Owner(id)` predicate matched exactly the same rows as `Household`. Only `Guest`
restricted anything. All the read-side scoping in P1 and P3 was real plumbing with nothing flowing
through it -- resolve a session to Liz, ask what Jerry said, and you would get it.

**No test caught it, and the reason generalises.** The fixtures that produce an owned row set
`profile_id` by hand -- a state no production code path could reach. A test whose *fixture* is
unreachable tests a system that does not exist. Worth adding to 2.2: ask not only "does this assert
the right thing" but "could production ever produce this input".

Fixed: extraction stamps the owner from the turn's scope and refuses to write for a `Guest`. Three
tests assert it end to end, including that a `Household` turn stays unattributed so shared context
survives a member's deletion.

**Other findings acted on:** `POST /chat` never resolved a scope at all, so the same speaker got
different answers from `/chat` and `/chat/stream` -- and my comment there wrongly blamed voice.
`delete_profile` used `unwrap_or_default()` on the settings read, so a read failure meant the
dangling `primary_profile_id` was never cleared and the delete proceeded anyway; both failure paths
now abort. Four vacuous tests removed or strengthened, including one asserting that a function whose
body is `ProfileScope::Household` returns `ProfileScope::Household`.

**Findings recorded but NOT fixed**, both written into PAI-1's phase list:

- The `giap-memory` MCP tools bypass the scope entirely -- including `forget_memory`, which deletes.
  A guest gets no memories injected and can still have the model recall or destroy everything.
- A real TOCTOU between `PUT /sessions/{id}/user` and `POST /identify-user`: both read `Unknown`,
  both pass `supersedes`, and the later write wins regardless of rank. My comment claimed the only
  loser was a competing face match on the same frame. That understated it.

**The SIGILL landmine is real and I hit it.** `pond-api`'s test binary died with `signal: 4, SIGILL`
after a rebuild -- the `target-cpu=native` artifact problem CLAUDE.md documents for CI. `RUSTFLAGS=""`
fixes it and is now what I use for every gate, matching `ci.yml`. It costs a one-time rebuild of the
goose rlib. Do not diagnose this as a code fault.

**Client breakage check: none.** `pond-desktop` never calls profile-delete or any of the
`/sessions/{id}/user` routes, and its API client already handles both 204 and 200-with-body. The
breakage is documentation only -- `docs/api.md` still specifies `204 no body` for profile delete and
documents none of the four session-identity routes.

**2026-08-04 (late) — PAI-1's two named holes closed.**

**The guest-reachable memory tools.** Closed one layer above where the audit found them.
`recall_memories` and `forget_memory` carry no session of their own and `MemoryMcpServer` is
process-global, so there is nowhere inside them to check. Instead a `Guest` session is never given
`giap-memory` (or draft, audit, vision, sensors) at all -- subtracted **after** `select_groups`,
since those groups are `core` and selection puts them back unconditionally.

Worth remembering as a shape: **when the thing you want to check has no identity, move the check to
the layer that hands it out.**

I did not thread a scope into the MCP server per turn, which would additionally scope an *Owner*'s
tool calls. The only mechanism available is `set_current_session_id`, a process-global
`RwLock<String>` that the SSE semaphore already permits more than one turn to race on. Using it
would trade a guest hole for a misattribution bug, which is the worse of the two.

**"The only mechanism available" was wrong, and PAI-2 P1 found the other one on 2026-08-05.** Goose
stamps `agent-session-id` into every `CallToolRequest`'s `Meta`; rmcp serialises it as the wire
`_meta` and hands it to the tool handler in `RequestContext.meta`. That is per-call and race-free,
needs no Goose patch, and is what `giap-draft` now resolves its speaker from
(`crates/pond-mcp-server/src/session_meta.rs`). `giap-memory` and `giap-toolkit` still read the
raced global; adopting the reader is the follow-up. The lesson generalises: **"there is nowhere to
hang the identity" is a claim about the transport, and transports carry more than the parameters
you were looking at.**

**The TOCTOU.** `set_session_identity_if_stronger` does the rank comparison inside the `UPDATE`,
building the `CASE` from `IdentificationSource::ALL_RANKED` so the ordering stays domain policy. A
test pins `ALL_RANKED` against `rank()` in both directions -- if they ever drift, the conditional
write would enforce one ordering while every in-memory check enforced another, and the disagreement
would only ever show up as an occasional unreproducible downgrade.

Zero rows updated is ambiguous between "no such session" and "not superseded", so the adapter
disambiguates with a follow-up count rather than guessing. One is a 404, the other a normal refusal.

**Two disk incidents in one session, both disguised.** A `cc` linker failure on `pond-mcp-server`
that was the allowance, not the code; and the SIGILL earlier that was `target-cpu=native`. Section 3
covers both. The pattern: **on this container, a build failure is more likely to be the environment
than your change** -- check `df` and `RUSTFLAGS` before reading the diff.

### Next: the `SecurityPolicy` deny matrix (PAI-1 P4 / PAI-2 P1), then PAI-3 P3

`TokenCounter` port, GGUF-backed adapter, chars/4 as the declared fallback, overshoot-feedback
correction retained as the safety net. `turn_trimmer.rs:89-91` is the estimator to displace, and
`WindowSource::is_exact()` already exists to tell budget code when it can trust the window to the
token.

**2026-08-05 — the three open decisions are closed. Jerry's calls, recorded verbatim in intent.**

The previous three entries each ended by asking for a decision rather than taking one. All three are
now answered, so nothing downstream has to guess:

1. **Capturing a household member at pairing time gets its own phase.** Not folded into PAI-1 and not
   into PAI-7, both of which need it. Burying it in either makes the other's dependency invisible in
   the status ledger — and it is self-contained anyway: a migration, one question in the pairing
   flow, and a resolver input that already exists and is fed `None` by every caller. PAI-1's
   `paired_device_profile` and PAI-7's "that profile's devices" both become live when it lands.
2. **The unidentified-speaker posture stays implicit, and gets documented.** `Household` in a
   one-member pond, `Guest` once there are two — no setting. The argument in PAI-1 P3 holds: the two
   scopes only describe different rows when there is somebody to be excluded from, so in a one-member
   pond they are the same rows. It belongs in release notes, not in `Settings`. **Consequence for
   P4:** enforcement is being built on a posture the user never explicitly chose, so the audit-mode
   telemetry PAI-2 P1 collects is the thing that has to reveal it if the call was wrong.
3. **`approve_draft`'s missing ownership check belongs to PAI-2, with the deny matrix.** Not
   hoisted into P0 alongside the auth allowlist, and not landed as a standalone handler patch.
   It becomes a real production call site for `SecurityPolicy::allow`, which today returns `Ok(true)`
   with none — so the check and the layer meant to express it land together, in `audit` mode first
   per PAI-2's own plan. It stays a known live hole until then: any session can approve any draft id,
   and the guest tool-group gate does nothing about one member approving another's.

   **Done 2026-08-05, and the call was right for a reason I had not anticipated.** Landing it with
   the policy layer forced the question "who is the caller?", and the answer turned out not to exist
   yet: `drafts` had no owner column *and* its `session_id` was a model-supplied parameter
   defaulting to `"default"`. A standalone handler patch would have compared the approver's session
   against a field that is the same string for every draft on the pond, passed its tests, and shipped
   a check that could never fire.

**2026-08-05 — first macOS run of `live-test.sh`. It wrote to a real pond, and that is the finding.**

The script's own header promises "a scratch `POND_DATA_DIR` so it can never touch a real pond." That
promise did not hold, and the way it failed is worth more than the fix.

A `pond-server serve --native` had been running on this Mac for four days, with no `POND_DATA_DIR`,
holding port 4000. The script hardcoded `PORT=4000`, waited 60s for `.runtime_api_port`, and on
timeout **kept the assumed port and carried on**. Its own scratch server had meanwhile fallen back to
4001. So every HTTP assertion, and both onboarding lift writes, went to the real pond:
`PUT /settings {"user_name":"LiveTest","chat_model":"mock",...}` overwrote four real settings rows.
Restored from surviving evidence — `primary_profile_id`'s display name, `active_llm_model`, and the
model on the last 113 sessions — with a timestamped `.bak` of the database taken first.

**The 60s timeout was not arbitrary and was still wrong.** `.runtime_api_port` is written right after
`bind_with_fallback`, which on a cold macOS start lands *about* 60s in. The wait was sitting exactly
on the boundary. But the timeout length is the small half of the bug: the real defect is that
expiring was **not fatal**. Falling back to an assumed port is a widening default in the same family
invariant 2 names — on failure it reached *more*, not less.

**Three false readings this produced, all of which looked like real findings:**

- `no such table: _sqlx_migrations` — read as "migration 0037 did not apply". `sqlite3.connect`
  *creates* an empty database when the path does not exist, so a wrong data dir is indistinguishable
  from a failed migration. `db()` now refuses to invent one.
- Five `FAIL GET /... returned 000 with NO TOKEN` — read as a catastrophic auth hole. It was the
  auth-probe server not having finished starting; the health wait had no failure check and the route
  loop ran regardless. Exactly the class this suite's own docstring warns about: **a check that fails
  because the request failed reports the opposite of the truth.** Same trap, one layer out.
- A Playwright console-error failure — read as a UI defect. It was the browser driving the *real*
  server, which has no loopback bypass, so six calls 401'd. Once the port resolved correctly, all four
  live UI tests passed with **no change to the spec**. Worth recording as a near miss: the obvious
  move was to filter 401s out of that assertion, which would have masked the port bug permanently and
  left a correct test weaker.

The generalisable lesson, and it is the same one as `ProfileScope::Owner`: **ask what the check does
when its setup is wrong.** A harness that cannot tell "the thing I am testing is broken" from "I am
not testing the thing" reports the second as the first, confidently, while the parts that never ran
report nothing at all.

`live_checks.py` now resolves the port once and treats its absence as fatal setup rather than a
per-check failure — previously a `FileNotFoundError` escaped mid-`section_identity`, so P3 through P8
never ran while the run still reported on the sections that had.

**Fixed and re-run clean:** 37 API checks / 0 failed, restart OK, migrations applied once, 4/4 live UI
tests, one benign WARN in the log dig (`auto_download` skipping `llamafile/mock`, which is the live
check's own onboarding write). `rc=1` remains, from the auth section alone, exactly by design.

**2026-08-05 — PAI-2 P0 LANDED. `scripts/live-test.sh` is green end to end for the first time.**

`is_public_route` takes a `&Method`; the allowlist is a `(Method, &str)` table with segment-wise
`{brace}` matching. Details and the exception rationale are in PAI-2's P0 entry. Three points worth
carrying forward rather than repeating:

- **The router's public/protected split is about ONBOARDING; `is_public_route` is about AUTH.** They
  read like one axis and are two. That is why P0's own acceptance test — "every route in
  `protected_routes` requires a token" — could not be written as a blanket assertion:
  `GET /oauth/callback` and `POST /oauth/refresh` sit in the protected router and are legitimately
  reachable without a bearer token, each guarded by something else. Anyone writing that test from the
  design text alone would have concluded it had found two more leaks.
- **The guards were mutation-tested.** Re-adding `(Method::GET, "/settings")` to the table fails all
  three, naming the route. Given three recorded vacuous-test incidents in this programme, a guard is
  not evidence until it has been made to fail — and this one parses source text, which is exactly the
  kind of check that silently matches nothing. A `routes.len() > 50` assertion pins that too; the
  parser currently sees 100 protected registrations, and a sweep confirmed every one uses a verb it
  recognises (no `any`/`head`/`options` routers exist to slip past it).
- **The stale "FAILS BY DESIGN" epilogue is gone from `live-test.sh`.** It told the next person that
  an auth failure was expected. Leaving it would have trained them to ignore the one section most
  likely to catch a real regression.
- **A test was passing for the wrong reason, and only the fix could reveal it.**
  `settings_is_blocked_before_onboarding` asserted 403 with no token — which held only because
  `GET /settings` was public, so the request reached the onboarding guard instead of being refused
  at auth. Correct status, wrong gate. This is the fourth member of this programme's family of
  tests that assert something true for a reason that is not the one claimed, and the first found by
  a *fix* rather than by an audit. Add to 2.2: when a change makes a test fail, check whether the
  test was ever measuring what its name says before assuming the change is at fault.

Gates: `pond-api` 109 lib and 137 across all 17 integration targets (135 + 2), `pond-core` 736, fmt
and clippy clean, `cargo build -p pond-server` via the live run.

**One new WARN appeared in the log dig and is environmental, recorded so it is not rediscovered:**
`embedding provider failed to init: embedding provider init timed out after 30 s — ONNX Runtime may
be version-incompatible (need ORT 1.24.2)`. macOS-side ORT version, unrelated to this change and not
present on every run (it is a 30s timeout, so it is load-dependent). The other WARN,
`auto_download` skipping `llamafile/mock`, is the live check's own onboarding write and was already
recorded on 2026-08-04.

**2026-08-05 — PAI-1 COMPLETE. P4 landed; P5 was found inert and repaired.**

Two things I would want the next person to take from this rather than the code.

**A phase can be stamped LANDED and be inert.** P5's tool-group denial sat inside
`resolve_session_tool_groups`, reachable only from the `tool_selection_is_relevant()` branch, while
`default_tool_selection_mode()` returns `"all"`. Every test exercised the branch the feature lives
in, which is the natural thing to test and the reason nobody noticed that the default configuration
never enters it. For a day, a `Guest` on any default pond kept `giap-memory` and could read or
`forget_memory` the whole household. Add to 2.2: **verify a phase against the DEFAULT configuration,
not only against the code path it added.** "I tested the feature" and "the feature is reachable" are
different claims, and the second is the one that matters.

**The specified deny matrix would have been theatre, and building it would have felt like progress.**
Eight scopes x three `PrincipalKind`s is twenty-four cells and every one has to be `allow` — each
kind genuinely needs each scope, and denying `Internal` breaks background work silently. Keying on
`PrincipalKind` cannot express the thing that actually discriminates, which is whether a caller has
*proved* the identity it claims. So P4 landed one rule that means something —
`is_identity_assertion_proven` — against the live hole recon turned up: `PUT /sessions/{id}/user`
took a `profile_id` from the request body and bound it at `Explicit` strength with **no ownership
check at all**, after which every turn resolved to that member's scope. Any paired device could be
any member. Worth generalising: **when a specified design produces a uniform answer in every cell,
the axis is wrong, not the rules.**

`allowed` and `denied_reason` are deliberately separate on `PolicyDecision`. In `audit` a refusal
still proceeds, so recording only the effect would log "permitted" for exactly the requests
`enforce` would block, and the audit trail could not answer what flipping the mode would break.
`verdict()` reports `allow` / `would_deny` / `deny`. This is the same failure family as the three
false readings the live harness produced earlier today, and it is now four for this session.

**What PAI-1 completing does NOT mean.** `enforce` refuses every remote explicit identification,
because nothing can prove an identity — no schema links a paired device to a member, and that is its
own phase (Jerry's call, recorded above). The mode ships as `audit` for exactly that reason. Also
still open and unchanged: the `giap-memory` MCP tools pass `ProfileScope::Household` directly, so an
*Owner's* tool calls remain unscoped; the guest gate covers a visitor, not one member reading
another's through a tool call. Closing it still needs a per-turn cell in the MCP server.

Gates: `pond-core` 751, `pond-api` 109 lib + 39 in `agent_data_integration_test`, `pond-infra` 188,
`pond-adapters-goose` 103 lib with all test targets building again, fmt clean, and
`scripts/live-test.sh --ui` green end to end.

**2026-08-05 (later) — PAI-2 P4: the secret store is encrypted, and the interesting part was not the
cipher.**

`secrets.json` is now an XChaCha20-Poly1305 envelope under `<data_dir>/secrets/master.key`. The
crate was already in `Cargo.lock` and the cipher choice took ten minutes. What took the rest was the
same failure family as everything else in this programme: the old constructor parsed the file with
`serde_json::from_str(&content).unwrap_or_default()`, so a file it could not read became an **empty
store**, with no error and no log line, and the next `set()` wrote over it. On failure, access
widened. That is the P0 allowlist bug wearing different clothes, and it is now five for this
programme.

**A guarantee whose only evidence is a comment is not a guarantee.** The plan for this phase said
the migration ordering — key durable *before* ciphertext, the property standing between a power cut
and secrets nobody can ever open — was implemented and documented but not testable without fault
injection it was not adding. It is testable: force the ciphertext write to fail (pre-create
`secrets.json.tmp` as a *directory*, defeating `create_new(true)`) and assert the key is already on
disk. Reversing the order fails that test **and no other**, which is the whole argument for writing
it. When a plan says a property cannot be tested, the useful question is what would have to be true
for it to be observable, not whether the prose is convincing.

**A first start proves nothing about an on-disk format.** The process that encrypted the file is the
one reading it back, out of a cache it never dropped. `live-test.sh` restarted the server and then
asserted only over the database, so the store had no restart coverage at all; it now leaves a secret
behind on the first pass and reads it back on the second. Verified by swapping the key between the
two starts.

**Two operational facts that will bite somebody.** Downgrading below this commit destroys the store:
an older binary hits that `unwrap_or_default()`, reads the envelope as empty, and writes plaintext
over it. And `giap.sh doctor` now FAILs on every pond not yet restarted on a P4 binary, mine
included — those stores really are plaintext, and the check is right.

**Say what it protects, not what it sounds like.** Key and ciphertext share a directory by default,
so this defends a copied file, a backup, a support bundle — not a running pond, and not a lifted SD
card unless `POND_SECRET_KEY_FILE` puts the key elsewhere. The four `api_key_*` fields are still
plaintext rows in `settings` until P2 moves them, so "GIAP encrypts your API keys" is not yet true.

Gates: fmt clean, clippy and test green on the fast set (`pond-infra` 202 lib, up from 188), `cargo check` on
`pond-server` + `pond-adapters-goose`, and `scripts/live-test.sh --ui` green end to end including
the new restart pass.

**2026-08-05 — PAI-2 P3 (redaction). The reusable lesson is about where a guard lives.**

The rules were the easy half. The wiring was the half that was wrong, twice, in a plan whose author
had read the code carefully: `run_server` builds the memory repository once, but `main.rs` builds it
four times, and three of those are write paths reachable from `pond chat`, the voice loop and
`pond memories add`. `SqliteSecurityPolicy` is handed its own independently constructed
`SqliteEventLog`, so wrapping the shared binding covered every event except the audit trail. Both
mistakes compile, run, and look right in review. Grep for the CONSTRUCTOR across the whole file, not
for the binding you were told about.

**A guard belongs where CI runs it, not where the code is.** `ci.yml` has no `cargo test -p
pond-server`, so any wiring guard written next to `main.rs` would never fire on a pull request. The
guard for this phase reads `main.rs` with `include_str!` from `crates/pond-infra/tests/` — no
dependency edge, no link, and it runs in the fast pass. It found a fourth bypass on its first
execution. The same technique is available to P6 for the "an outbound body path has appeared and
nothing redacts it" guard that P3's own risk list said could not be written.

**Watch a guard fail before believing it.** Four mutations were run and all four produced messages
that name the defect: the audit sink unwrapped (`main.rs:2593 builds a write-path SqliteEventLog
outside RedactingEventLog`), `set_egress_sink` rebound to a fresh log, the UK inward-letter set
replaced with `is_ascii_alphabetic` (`mangled: Play B2 3AM by the band when I get home.`), and one
`MemoryRepository` forward deleted.

**A negative test is the product decision.** A redactor that eats ordinary prose is worse than none,
because the user stops trusting the transcript and turns it off. Every rule here has a real-shaped
false positive next to its positive, and it is the negatives that shaped the code: no dot in the
phone separators (IP addresses), a trunk prefix required on bare digit runs (epoch timestamps), Luhn
required on card-length runs (order numbers), the real inward-letter set on postcodes (`B2 3AM`),
and an unprefixed 64-hex secret deliberately left undetected because a git SHA is indistinguishable
from it.

**Live-run trap, already documented and still worth repeating.** `live-test.sh` dying at "never wrote
`.runtime_api_port` after 180s" is `ensure_onnx_runtime` downloading ~30 MB into a fresh scratch
directory, not a hang in your feature. Export `ORT_DYLIB_PATH` at an existing copy. The comment
above the start block says so; I lost a full run to not reading it.

**2026-08-05 (last of the day) — PAI-2 P7. The closure had to be reversible, and that decided the
design.**

The obvious implementation of "close the onboarding holes" is a flag: remember that this pond has
been set up, and stop answering the wizard's writes. It is wrong, and one route is what makes it
wrong. `POST /onboard/reset` is public and stays reachable after onboarding because it is the
recovery lever for a misconfigured pond — and it works by making the pond *not onboarded* again, so
the wizard's writes have to come back. A flag never comes back. Reset would have succeeded, dropped
the pond to the wizard, and left `PUT /settings`, `POST /profiles` and `POST /onboard/complete`
shut, with reflashing the device as the only repair. **Generalise: before closing a door, find the
route whose whole purpose is to reopen it.** The mirror of that error is leaving reset
unconditionally public, which makes the closure decorative — reset, then walk in. Both readings are
defects; neither is a trade-off. Reset is loopback-only once the pond is onboarded, which is not a
new boundary: `handshake_pairing_code` and `handshake_issue_pairing_code` already refuse a
non-loopback peer inside the handler, so a pond that has lost every token can only be re-paired from
the host anyway.

**I was told to use a defaulted trait method and I should not have been.** The plan gave
`OnboardingRepository::is_complete` a default body reading `get_current_step`. Eight implementors,
mostly test stubs — and a stub that inherits "not onboarded" makes every onboarding write route
public wherever it is used. That is a widening default, which is this programme's own most-repeated
bug, and PAI-1 invariant 2 exists to forbid it. The method is required instead. The compiler named
all eight in one pass and each one said what it meant. **A default on a trait that answers a
security question is a decision made by whoever forgot to override it.**

**A scope-widening default, found by asking what happens when the read fails.**
`SqlxOnboardingRepository::get_current_step` ends `.ok()??`: a database error becomes `None`, and
`None` means "not started" to every consumer — the exact state in which every onboarding write hole
is open. Keying auth on that answer would have reopened all of them on a set-up pond for as long as
SQLite returned `BUSY`. The port now has `is_complete() -> Result<bool>` and the gate treats a
failed read as onboarded. Add to 2.2: **when a new decision starts reading an existing value, read
what that value does on failure, not only what it means on success.**

**And a trap worth remembering for any port with an `Arc` blanket impl.** `AppState` holds
`Arc<dyn OnboardingRepository>`, so method resolution picks the impl on `Arc` before the concrete
adapter. A default plus a SQLite override would have compiled, read correctly, and never executed
the override — the default body would have run on the `Arc`, called `get_current_step`, and answered
"not onboarded". Requiring the method makes deleting the forwarding arm a compile error.

**A compile-time guard cannot ask a state-dependent question, so change the question, not the
guard.** The three `PUBLIC_ROUTES` drift guards parse `routes.rs` with `include_str!` and have no
pond to read. Picking a state would have reported the other state's answer as safety. They now ask
`reachable_without_token_in_some_state` — the worst case, the only thing a static check can answer
honestly — and a fourth guard pins the exact list of state-dependent entries so a route cannot
change class quietly. None was weakened to fit.

**The plan said close `POST /tts`; the shipped clients said otherwise.** It looks like an onboarding
hole — the wizard's voice preview is why it is public — but `playTtsSentence` (WebVoiceBackend.ts)
and `fetch_tts_bytes` (audio_cmd.rs) both call it with no `Authorization` header long after setup,
so closing it makes the assistant mute on a set-up pond. `POST /voice/calibrate` *is* closed,
because `calibrateWakeWord` does attach the token. The difference between the two decisions is that
I opened the client and read it. **Before narrowing a route, grep the clients that call it — a
classification derived from what the route is for is a guess about what calls it.**

**Third instance of a test asserting the right thing about the wrong fixture.**
`put_settings_without_a_token_is_still_allowed` ran against `OnboardingStep::Completed` and asserted
the write stays open. The assertion was correct for a pond mid-onboarding and described the hole for
a pond that was set up. P0 found the same family from the gate side
(`settings_is_blocked_before_onboarding`); this one came from the fixture side. Both times the test
name was true and the setup was the lie. The same fixture also had to stop using
`MockSettingsRepository`, which silently drops `chat_model` — the field `complete_onboarding`
refuses to proceed without — so a wizard round trip against it could never have finished.

**The mutation, and what it printed.** Replacing the live read with a process-wide `EVER_ONBOARDED`
latch failed `reset_then_recover_is_not_a_one_way_door` at step 3: `after a reset the wizard must be
able to save again, left: 401, right: 200`. That is the one-way door, named, in the message. A guard
for this class of defect is worth nothing until you have watched it print that.

Gates: fmt clean; `pond-core` 783 + 5, `pond-infra` 214 + 3 + 3, `pond-api` 112 lib + all 17
integration targets; clippy clean on the fast set; `cargo check -p pond-server
-p pond-adapters-goose`; `scripts/live-test.sh --ui` green, and its no-bypass auth section now
asserts the PAIR (`PUT /settings` 200 with no token before `POST /onboard/complete`, 401 after) plus
the whole reset-then-recover round trip over real HTTP. Asserting only the 401 would pass on a
server that never started.

**2026-08-06 — PAI-3 P3 PARTIAL: the catalog data landed, the rung is still dead.**

`WindowSource::CatalogRecord` — precedence rung 3 — **has never been producible in production.** All
three `ContextInputs` construction sites (`goose_agent.rs::resolve_window_with`, `routes.rs`'s
`turn_context_limit`, and the quarantined `pond-agent/src/agent.rs`) pass
`catalog_context_length: None`. The only thing that has ever produced that variant is the unit test
in `context_governor.rs`. This is the `ProfileScope::Owner` defect exactly: a branch with full test
coverage, reachable from no fixture production can build.

Two things it hid, both found by asking *who writes the field* rather than *is the field written*:

- The checklist's own 2026-08-03 correction — "`ModelRecord.context_length` *is* written for the
  curated GGUF catalog" — was true and stopped one question short. `gguf_record` writes it;
  `llamafile_record` and `ollama_entry_to_record` write `None`. **Ollama is the provider class the
  rung was designed for**, and it was the one with no data.
- Every Gemma 4 row declared `8192`, copy-pasted from the Gemma 2 rows directly above it in the same
  table. The real declared window is `131072`, verified against
  `model_info["gemma4.context_length"]` from a live `POST localhost:11434/api/show`. Nothing caught
  it in the seven commits since, because nothing read the field.

**A dormant column is not a safe place to put data.** It rots at the rate the catalog is edited and
nothing pushes back. That is the general lesson, and it is the same shape as the vacuous-test family:
the absence of a reader is the absence of a check.

Landed (all in files no other workstream holds): the llamafile table gained `context_length`; the
Gemma 4 rows were corrected; `OllamaCatalogProvider` now reads the window from `/api/show`, matching
the `model_info` key by `.context_length` suffix rather than an architecture allowlist that would go
silently dead on the next family; and rung 3 gained the clamp it was missing — a catalog value is a
DECLARED MAXIMUM, not an allocation, so for a local provider it is bounded by
`UNPINNED_LOCAL_CEILING`. Without that clamp, populating the field correctly would have been the
regression: 131,072 tokens of history budget on a Mac that allocated 32,768.

**Blocked, and deliberately not worked around.** Supplying the value at the two live call sites needs
`goose_agent.rs` and `routes.rs`; surfacing it in the Models UI needs `record_to_dto` (in
`routes.rs`) plus `ModelStatusEntry` and `types.ts`. All held by a concurrent session. Deferred to
**P3b** rather than edited carefully — PAI-2 lost an encrypted secret store to two phases writing one
file, and it compiled with every test green. Note for P3b: `Models.tsx`'s `inferCapabilities` derives
the context window from the model NAME in the frontend, a third copy of the heuristic this workstream
exists to delete; replacing it is the point of surfacing the field, not a bonus.

**The mutations, and what they printed.** Removing the local clamp from rung 3 failed
`a_catalog_length_cannot_widen_an_unpinned_local_window` with `left: 131072, right: 32768` — the
defect, in the numbers, in the message — with the other sixteen governor tests still green, so the
guard is doing the work and not a blast radius. Reverting `llamafile_record` to `context_length: None`
failed `every_chat_capable_entry_declares_a_context_window` with `catalog entry llama-1b declares no
usable context window (0); rung 3 of the context governor reads this field`, and took
`the_same_base_model_declares_the_same_window_in_both_tables` down with it. Both restored, both green
after.

**2026-08-06 — PAI-3 P6. A partial correction is how a document ends up contradicting itself.**

`token_tracking.md` and `model_capabilities.md` had both already been "corrected" in the 2026-08-03
documentation-debt pass, and both were still wrong in the same shape: the fix landed in one place
and not in its neighbours. `token_tracking.md`'s prose said real provider usage is the primary path
while the ASCII flow diagram three lines above it still had `chars / 4 heuristic` on the main arrow.
`model_capabilities.md` gained `tool_calling` in the struct listing while the detection table below
it kept five columns and the REST sample below that kept five keys — and since the handler
serialises the struct whole, the wire has carried six fields the entire time. **When a claim is
corrected in a document, grep the document for every other place that claim appears.** A partial
correction reads as authoritative and is harder to spot than the original error.

**Four claims were false, not merely incomplete**, and two were only findable by reading the
frontend:

- The `~` prefix does not mean "estimated". `formatTokens` in `UsageStatsCard.tsx` applies it to
  every count of 1000 or more as a rounding marker for the `k` suffix and prints smaller counts
  bare. It takes a number and nothing else; `UsageStats` carries no provenance to read.
- `trim_to_budget_for_model` was cited as what `context_window_tokens` drives. Its only call sites
  are its own unit tests, below the `#[cfg(test)]` line in the same file. A function with tests and
  no callers looks alive to any grep that stops at the first hit.
- `qwq` was documented at 32K. The context arm matches the substring `qwen`, which `qwq` does not
  contain, so it falls through to the 4,096 default. The table row grouped `qwen3 / qwq` together,
  which is what hid it — **a doc table that merges two patterns into one row cannot express them
  diverging.**
- `Models.tsx`'s `inferCapabilities` claims to mirror `from_model_name` and does not: no E1B
  exclusion, so `gemma-4-E1B-it` gets a Vision badge the backend deliberately refuses; no
  `gemma-3n` spellings; no `vl`/`vision` segment rule; and it computes neither `structured_output`
  nor `tool_calling`. Recorded, not fixed — it is badges only, but it is a duplicate of a rule whose
  entire design is an argument about which way it is allowed to be wrong.

**Line numbers, again.** `03-context-governor.md` cited `token_tracking.md:28`, and the roadmap's
debt table cited both files by line; the rewrites move every one. All three citations are now
symbols or section names. That is the second time PAI-3 has rotted a citation by editing the file it
points at.

**No gates were owed, and that is worth stating rather than leaving implicit**: the phase touches no
Rust, no `Settings` field, no migration, no route and no startup wiring, so `cargo` and
`live-test.sh` have nothing to say about it. They were run anyway, to prove that claim rather than
assert it. `cargo fmt --check` clean; `cargo test -p pond-core` 749 passed, **4 failed** — all four
in `context_budget.rs`, which is the continuous-curve work of a concurrent phase, mid-edit, in the
working tree. A documentation phase that reports a red suite it did not cause is more useful than
one that reports "green" by only running what it likes.

**The mutation, since a doc that asserts a guard owes evidence the guard bites.** Section 3 of
`token_tracking.md` now claims `GOOSE_CONTEXT_LIMIT` is written and never read, on the strength of
two `include_str!` source-scanning tests — exactly the class of check that can pass by matching
nothing. Putting `std::env::var("GOOSE_CONTEXT_LIMIT")` back into `ContextGovernor::prompt_window`
failed `this_module_does_not_read_the_environment` with `context_governor must not read process
environment variables`, with the other sixteen governor tests still green, so the guard is doing the
work and is not a blast radius. Restored; the file's md5 is byte-identical to before the mutation,
which matters because the file carries another phase's uncommitted work and `git checkout --` would
have destroyed it. Its twin in `goose_agent.rs` was **not** mutated — that file is held by a
concurrent session — and `token_tracking.md` says so rather than implying both were proven.

What the phase did owe beyond that was verification:
every claim was re-checked against `git show HEAD:<path>` rather than the working tree, because a
concurrent session is mid-edit in `goose_agent.rs`, `turn_trimmer.rs` and `routes.rs`, and
documenting somebody else's uncommitted work as current state is its own way of shipping a lie.

**What that discipline caught.** The recon for this phase reported the PAI-3 documents as clean;
they were not. PAI-3 P3 had landed its catalog work into the same three files in the meantime, so
the status rows this phase had to stamp were already carrying `P3 PARTIAL`. They were merged, not
overwritten. Both rewrites also had to stop describing rung 3 as "PAI-3 P3" future work: the durable
statement is that **no production caller supplies `catalog_context_length`**, which is true both
before and after P3's data work, and is the thing P3b changes.

---

**2026-08-06 — PAI-3 P5 code landed. The measurement that decides it did not.**

P5 is "asymmetric budgeting: preamble capped, working set scaled", and the first surprise was that
the cap already existed. P1 had moved it into `ContextGovernor::prompt_window` and two adapter sites
called it. The defect was that calling it was **optional**: `trim_goose_history` and
`hydrate_goose_session` built their profile straight from the raw window, so a single Orin turn
carried two different preamble budgets depending on which function you read — 3,600/700 on the
history side, 3,000/500 on the side that actually assembled the prompt. That is the four-paths-
disagree shape PAI-3 exists to remove, reproduced one layer down inside the fix for it.

So the asymmetry moved into the type. `CompactionProfile::for_windows(context, prompt)` takes both
windows, sources the preamble fields from the clamped one and everything else from the full one, and
**adds the difference to `history_token_budget`** — the preamble does not merely stay small, the
tokens it is denied go to history. Total budget is unchanged, which is what keeps P4's
"never promises more than the tiers did" property intact while the prefix/working-set split moves.
The profile gained one field, `prompt_window_tokens`, so `use_compact_prompt()` can be a preamble
decision rather than a context-window decision.

The second half is the clamp P4 deferred here **by name** in its own test comments: `turn_trimmer`
capped history at window-minus-reserve, which does not subtract the system prompt or the memory
block, so at 8,192 it let history claim 7,168 tokens on top of a 3,500-token preamble that was going
to be sent anyway. `usable_history_tokens()` subtracts it. `usable_prompt_tokens()` is deliberately
untouched, because the overshoot correction compares it against the engine's report of the **whole**
prompt and a history-only ceiling there would cry overshoot on every turn that merely spent its
budget. That distinction is the one thing in this phase most likely to be "tidied" wrongly later.

**What it costs, said out loud rather than buried.** At a symmetric 8,192 window effective history
drops from 4,000 to 3,668. That is less retained history at one operating point, and it is correct:
the 4,000 was never payable. Above the clamp history grows — 20,000 to 24,000 at 32,768 local, 7,200
to 8,000 at the Orin's pinned 16,384 — with the preamble frozen.

**Three mutations, because two of the three failure modes compile silently.** Ignoring the prompt
window: `a 4x window bought a bigger system prompt: 6000 vs 3000`. Reverting the trimmer clamp:
`history claimed 3810 tokens, past the 3668 the preamble leaves it`. Swapping the two `usize`
arguments at the adapter's `profile_for` — which the compiler cannot object to — `left: 8192,
right: 32768`. That third one is why the pure half was split out of `turn_profile`: `for_windows`
being right inside `pond-core` says nothing at all about the adapter feeding it correctly, and a
test that only exercised `pond-core` would have been the seventh vacuous test in this programme.

**Why the status cell says "code landed" and not "LANDED".** This document's own success criterion
for P5 is a measurement: `ttft_ms`, `prefill_ms`, `prompt_tokens` and retained turns, on Mac and on
Orin, before and after. There is no Jetson attached to the machine this landed on, and neither run
happened. The arithmetic says the local preamble is byte-for-byte what it was, because the same
clamp feeds the same two consumers — but "the budget did not change" and "TTFT did not change" are
different claims, and only the second one is what the phase promised. Stamping LANDED on unit tests
here would be exactly the substitution the PAI-2 P5 repair earlier in this log exists to warn about.

---

**2026-08-06 — PAI-4 P1. The design's tier table had two undefined cells and one overstated rule.**

`ModelClass { Small | Medium | Large }` and `CompactionStrategy` live in
`models/services/context/model_class.rs`, derived from the provider string plus whatever
`ContextGovernor::resolve` returned. Domain only, no call site — PAI-3 P1's shape, and P2 is the
first consumer. 15 tests; `pond-core` 733 lib, up from 718.

**The phase was specified as "derive `ModelClass` from the governor" and the work was almost entirely
deciding what the table in 3.1 actually means.** Each of its three rows names a window size *and* a
provider, which reads as one axis and is two. Two configurations that exist today fall between the
rows: an 8K hosted model, and a 131,072-token Ollama model on the Orin — the second of which PAI-3 P3
made reachable seven days ago by teaching `OllamaCatalogProvider` to read `context_length` from
`/api/show`, and rung 3 does not clamp it because Ollama is not a "local provider" by the governor's
definition. Implemented as two brackets plus a provider set, every row is reproduced and both cells
get an answer.

**The predicate that matters is not the one that was already there.** `ContextGovernor`'s
`is_local_provider` is `local | gguf`, and reusing it would have been the obvious move and wrong: it
asks whether the *preamble* is re-prefilled locally every turn. What the compaction tier needs to
know is whether an *extra* model call competes with the turn the user is waiting on, and on the Orin
Ollama and llamafile are HTTP to `127.0.0.1` off the same 102 GB/s. `ON_DEVICE_PROVIDERS` is
therefore four entries, not two, and it is a deny-list for the one permissive tier — safe when too
wide, unsafe when too narrow, which is why a provider that *could* point at another box is kept
inside it. Generalisable: **two predicates with the same name in English are not the same predicate,
and the cheap reuse is where a tier system quietly stops meaning anything.**

**I did not implement invariant 3 as written, and said so in three places rather than one.** The
small-tier row and the invariant both say no LLM in the on-device compaction path; the reason given
is a stall at 20 tok/s. The idle rolling summary cannot produce that stall — it runs after
`summary_idle_secs`, a new turn cancels it, and turns read `sessions.rolling_summary` without
awaiting it, which invariant 5 says in the same list. Following the wording would have removed the
rolling summary from the device whose window runs out first, to prevent a hang that path structurally
cannot cause. There *is* a real cost on-device and it is a different one — an idle summarisation
evicts the prefix cache, so the next turn pays a prefill it would not have — and it belongs to P5,
with a measurement, and it applies to the medium tier equally. The correction is written into 3.1,
into invariant 3, and into `strategy()`'s doc comment, because a claim corrected in one place and
left standing in its neighbours is how this programme has produced documents that contradict
themselves twice now.

**The consequence is that `Small` and `Medium` select the same strategy today**, which looks like the
classes are redundant. A test asserts the equality on purpose, naming the phase's own "today's
behaviour preserved" requirement, so that if it ever stops being true somebody has to mean it.

**`CompactionProfile` gained no field, and that was the load-bearing decision.** `turn_trimmer.rs`
builds it with exhaustive literals in two test helpers, so a new field is an `E0063` in a file this
phase does not own. The class is derived from `(provider, resolved_window)` — both of which every
budget call site already holds — rather than stored. Same property that let PAI-3 P4 land while
`turn_trimmer.rs` and `prompts.rs` were held.

**Two mutations, and the second one taught me something about the first.** Adding `ModelClass::Small`
to the `llm_resummarisation` arm failed `the_on_device_tiers_never_permit_an_llm_in_the_compaction_path`
with `the small tier selected a strategy that re-summarises with an LLM; on this tier that call
competes with the next turn's prefill (PAI-4 invariant 3)`, and took the behaviour-preservation test
with it — 13 of 15 still green, so the guard bites without being a blast radius. Dropping `ollama`
from `ON_DEVICE_PROVIDERS` failed three, including `ollama at 131072 tokens classified as large,
expected medium`. But it did **not** fail `an_on_device_provider_can_never_reach_the_large_tier`,
because that test iterates over the constant it is checking — it is a range property over whatever
the list happens to say, so shrinking the list makes it vacuously pass. That is the seventh member of
this programme's vacuous-test family and the first I caught by mutating rather than by review.
**A property test that quantifies over the data it is protecting protects nothing; the explicit table
next to it is what actually holds the line.** Both mutations restored, file md5 byte-identical.

Gates: `cargo fmt --check` clean, `cargo clippy -p pond-core --all-targets` with no new warnings,
`cargo test -p pond-core` 733 lib + 5 + 3 ignored, and `cargo check -p pond-server
-p pond-adapters-goose` clean. No live-server run: the phase touches no migration, no route, no
handler and no startup wiring, and adds no `Settings` field.

---

**2026-08-06 — PAI-4 P4. A gap is not a resume; a person is.**

`models/services/context/resume_compaction.rs` — `should_run(ResumeGateInputs) -> GateDecision`,
`idle_threshold_from_secs`, `idle_gap_since`, five skip reasons. 14 tests; `pond-core` 747 lib, up
from 733. New headless setting `resume_compaction_idle_secs`, full five-part ritual. Called from
`spawn_resume_compaction` in `routes.rs`, fired by `GET /api/v1/sessions/:id/messages`.

**The one-line rule in 3.2 hides the bug it would have caused.** It reads
`session resumed && idle_gap > resume_compaction_idle_secs -> compact`, and if `idle_gap` is the
only input, then at boot every stored session satisfies it — starting the server would compact the
entire history store and call each one a resume. That is consolidation's "never on startup" guard
wearing a different costume, and it needed a different implementation, not the same one: consolidation
asks "has a user done anything since I started?", which a background loop can answer. Here the gate
is not on a loop at all. `ResumeGateInputs::reopened` is set from a request somebody made, and
nothing in `pond-core` can set it from a timer. Generalisable: **when you port a guard, port the
question it answers, not the field it reads.**

**"Run full compaction on resume" could not be implemented as written, and the reason is that
compaction is two mechanisms with opposite timing.** The deterministic trimmer is a function of the
turn being assembled — there is nothing to pre-run and, being deterministic and model-free, nothing
to save. So the design's stated motive ("today compaction happens *during* the first turn back,
while the user waits on a token stream") does not describe what hybrid compaction actually costs a
turn. What it does describe, once you go looking, is a real hole one layer over: the idle
summary loop in `main.rs` skips any session whose `updated_at` predates process start, so a
conversation from before the last restart keeps a summary frozen at that restart, and the trimmer
splices the stale one into every turn until four fresh messages accumulate. That is what P4 fixes,
and I have said so in the phase stamp rather than claiming the larger thing. The large tier's
re-summarisation is P2's; this gate will call it when it exists.

**Both fallbacks lengthen, because short is the direction that costs.** 30 minutes for the default
(15x `summary_idle_secs`, 2x consolidation's inactivity threshold — the point at which the rest of
the system already considers the household asleep). `MIN_RESUME_IDLE_SECS = 300` floors a stored `0`,
which would otherwise mean "every reopen is a resume". `idle_gap_since` returns `Duration::ZERO` for
a future timestamp, so a skewed clock or a restored backup reads as *active*, never as stale. A
threshold that is too long only means the user pays what they already pay; one that is too short
spends a model call between every pair of turns on the device least able to afford it.

**Invariant 1 is held by where the code runs, not by a promise.** Both database reads the gate needs
happen inside the spawned task, so the reopen returns at exactly the speed it did before; the refresh
races a watcher on `last_user_activity` and persists nothing if cancelled. The in-flight set that
stops two rapid reopens queueing two model calls is process-local on purpose — a database row would
survive a `kill -9` and strand the session as permanently compacting.

**Three mutations.** Deleting the `reopened` check failed `a_huge_gap_alone_is_not_a_resume`
(`left: Run, right: Skip(NotAReopen)`) and `no_single_precondition_can_be_dropped` with
*"the gate ran with `reopened` unsatisfied"*. Dropping the `.max(MIN_RESUME_IDLE_SECS)` floor failed
`a_stored_zero_is_floored_rather_than_treated_as_no_threshold`: *"a stored 0 disabled the idle
threshold entirely — left: 0ns, right: 300s"*. Removing the `apply_key` arm failed pond-infra's
`roundtrip_persists_every_field` with *"resume_compaction_idle_secs: wrote 1807, read back
Some(Number(1800))"* — the settings ritual's own guard, checked because the field is the part of
this phase the compiler is least able to protect. A fourth, removing the `HEADLESS_BY_DESIGN` entry,
failed `every_settings_field_is_dispositioned` by name. All four restored; `git status --porcelain`
lists only the five files this phase owns.

**Learned, and not from the phase text.** `get_messages_paginated`'s doc comment says "returns the
100 most recent messages"; the SQL is `ORDER BY created_at ASC LIMIT ? OFFSET ?`, so the default page
is the *oldest* hundred. I had drafted the gap check against the last message of the page and it
would have measured the age of the conversation's opening line on any session over 100 messages —
i.e. it would have said "resume" forever. The gap is read from `sessions.updated_at` instead. The
doc comment is still wrong and is not mine to fix in this phase; it is recorded here so the next
person reads the SQL.

Gates: `cargo fmt --check` clean; `cargo clippy -p pond-core -p pond-infra -p pond-api
--all-targets` with no new warnings in the touched files; `cargo test -p pond-core` 747 lib + 5 + 3
ignored, `-p pond-infra` 214 + 3 + 4 + 1 ignored, `-p pond-api` 263 across 19 binaries; `cargo check
-p pond-server -p pond-adapters-goose` clean. No live-server run, and that is a gap rather than a
judgement: the phase adds a `Settings` field and a route side effect, both of which
`scripts/live-test.sh` exists to catch, and the integration assertion section 7 asks for — that the
refresh completes before the first token of the next turn — needs a real server and a real model.

---

**2026-08-06 — PAI-4 P6. A standing condition is not an event, and "reset after compaction" would
have undone the thing that makes it safe.**

`context_monitor.rs` — `claim_compaction`, `note_compacted`, `ContextState::turns_at_last_compaction`,
`COMPACTION_COOLDOWN_TURNS = 3`, and the health arithmetic lifted into a free `health_of`. Eight new
tests; `pond-core` 755 lib, up from 747. `routes.rs` — `spawn_pressure_compaction` fired from the
`should_compact` branch of `chat_stream`, `run_compaction_pass` shared with P4's
`spawn_resume_compaction`, and `reset_session` called from `delete_session`. One new wiring test,
`crates/pond-api/tests/context_monitor_reset_test.rs`.

**Where the code runs is what holds invariant 1, and the obvious placement breaks it.** The line the
phase is named after sits inside the SSE generator. The model has finished, but the `done` frame is
unsent and the client is still on the stream, so doing the work there puts a summarisation between
the user's last token and the end of their turn. `spawn_pressure_compaction` does nothing but
`tokio::spawn`; every read, the gate, and the model call happen in the detached task. Same shape as
P4, for the same reason, and it is worth stating as a rule: **"between turns" is a claim about the
stack you are on, not about the line number.** The `context_warning` frame is untouched.

**The design bullet contains no rate limit, and without one the phase is a regression.**
`should_compact` was safe to fire freely while it only decided whether to emit a frame. Driving a
model call makes it a rate, and utilisation is monotone — a session that crosses 75% is above 75% on
every later turn, so an unlimited rule summarises between every pair of turns on the device least
able to afford it. That is the identical failure P4 guarded against on the time axis with
`MIN_RESUME_IDLE_SECS`, arriving through a different door. `claim_compaction` grants one pass per
three recorded turns, recomputes health under its own lock so two turns finishing at once cannot both
be authorised by one snapshot, and stamps the cooldown on the **claim**: the model call is spent
whether or not the summariser finds anything to do.

**"Call `reset_session` on clear and after compaction" is two situations wearing one call, and the
second one is wrong.** A clear means the session stopped existing — drop everything. That is
`delete_session`, and it is the *only* such route: `DELETE /sessions/:id/user` releases an identity
binding and changes nothing about the context, so it is deliberately not a call site, which is the
narrower reading and therefore the right one. After a compaction the session is still here and its
window did **not** shrink: the rolling-summary refresh changes what the trimmer splices, not what the
engine reports next turn. Resetting there would zero utilisation, empty the growth window and clear
the cooldown stamp — so the pass would re-fire on the very next turn, the exact failure the cooldown
exists to prevent, reintroduced by the line meant to tidy up after it. `note_compacted` drops only
the growth samples (which genuinely measured a differently-shaped history) and runs only for
`RefreshOutcome::Refreshed`; `NothingToDo` and `Cancelled` changed nothing.

**Two things found while checking rather than assumed.** `reset_session` had no production caller at
all, so the growth map was also an unbounded leak — every session the process ever streamed a turn
for stayed in it for the life of the process. And section 3.4 cited "invariant 2" for the
between-turns rule; invariant 2 is the tool-result rule, and the one meant is invariant 1. Both fixed
in the same change.

**Mutations, both restored, `git status --porcelain` re-read after each.** Deleting the cooldown
comparison from `claim_compaction` failed three tests:
`a_saturated_session_claims_once_and_then_waits_out_the_cooldown` with *"claimed again only 1 turn(s)
into a 3-turn cooldown - a still-saturated session would summarise between every pair of turns"*,
`repeated_claims_within_one_turn_yield_exactly_one_pass` with *"5 concurrent claims were granted for
one turn — left: 5, right: 1"*, and `note_compacted_clears_growth_history_but_not_the_cooldown` with
*"note_compacted cleared the cooldown, so the next turn re-fired"*. Deleting the
`state.context_monitor.reset_session(&session_id)` line from `delete_session` failed
`deleting_a_session_clears_its_context_monitor_state` with *"the deleted session's utilisation
survived the delete — left: 85.44922, right: 0.0"*, which is the point of that test: it is a wiring
claim no unit test can reach.

**A restore that went wrong, recorded because it nearly cost the phase.** I reverted the first
mutation with `git checkout -- <file>`, which does not undo a mutation — it reverts the file to
`HEAD`, and took every edit of the phase with it. Nothing outside my own footprint was touched and
the work was re-applied, but the correct tool for an experiment on a file with uncommitted work is a
copy and a copy back. The second mutation was done that way.

Gates: `cargo fmt --check` clean; `cargo clippy -p pond-core -p pond-api --all-targets` with no new
warnings in the touched files; `cargo test -p pond-core` 755 lib + 5 + 3 ignored, `-p pond-api` 264
across 20 binaries; `cargo check -p pond-server -p pond-adapters-goose` clean. No live-server run,
and that is a gap rather than a judgement: this adds a route side effect that spawns a background
model call, which is what `scripts/live-test.sh` exists to catch, and section 7's integration
assertion — that a pass completes before the next turn's first token — needs a real server.

---

**2026-08-06 — PAI-4 P5. The cold half of the rule; P3 had already built the warm half without
being able to name it. Code landed, measurement not taken.**

`models/services/context/prefix_cache.rs` — `InvalidationReason` (the six designed reasons),
`PrefixCacheState { built_at, hash, turns_served, invalidated_by }` with
`invalidate`/`rebuilt`/`serve_turn`/`age_since`/`posture`/`posture_of`, and `CachePosture
{ Warm | Cold }`. Nine tests, no clock read in the file. `models/ports/agent.rs` —
`Agent::prefix_cache_state() -> Option<PrefixCacheState>`, default `None`, one test.
`turn_trimmer.rs` — an eighth argument and seven tests. `goose_agent.rs` — the state, the six
recording points, the port impl, and `provider_change_reason` with three tests. `pond-core` 802 lib,
up from 786; `pond-adapters-goose` 98, up from 95.

**This is stamped CODE LANDED, MEASUREMENT PENDING, not LANDED, and the distinction is the point.**
Section 7 of the design says P5 "is only correct if cold-cache recompaction shows no TTFT penalty
and warm-cache turns show no new re-prefills" — an Orin-and-Mac measurement, and its own acceptance
criterion. It has not been taken. That makes two measurement-pending P5s on the ledger alongside
PAI-3's, which is worth saying out loud rather than quietly accruing: a green unit suite proves the
rule fires where it was told to, not that firing there was cheap.

**The design reads as two rules and only one of them was new.** 3.3 says recompact when cold,
prefer byte-identical edits when warm. The warm half landed on 2026-08-06 as P3, whose age rung
fires only when the conversation is over budget and cites invariant 4 for it — P3 simply could not
tell "warm" from "already gone", so it used *over budget* as a proxy for both. P5 supplies the real
signal and spends it in one direction only: the rung also fires when the cache is provably cold on a
conversation that still fits. Nothing on the warm path was tightened, because gating the summary
splice or the `<system-context>` strip needs the number section 7 is waiting for.

**The ordering bug I wrote first.** The obvious `serve_turn` clears `invalidated_by`. It is wrong
exactly on the case 3.2 calls the most valuable cold moment there is: `SessionResumed` is recorded
while the engine session is hydrated, hundreds of lines before the static prefix is compared *in the
same turn*, so an eager clear had a resumed session clearing its own resume and reading `Warm`. The
clear is deferred by one turn instead. The residue is a reason sticky for one extra turn, which
over-reports `Cold` once; that is a bounded over-count of a permission to degrade already-aged
material, where the opposite error silently forfeits the free recompaction the phase exists to take.

**A latent defect in P3's rung that only became routine under P5.** `truncate_head_tail` is not a
fixed point of itself — it keeps `cap` characters and appends an elision marker, so its output
always exceeds the cap and handed its own output it cuts again. P3 never saw it because its rung
only ran when over budget and one cut usually brought it under. Running the same rung on cold turns
that comfortably fit would have ground one tool result away by degrees over a long cold session.
`AGED_FIXED_POINT_CHARS` is the guard and `the_aged_cap_is_a_fixed_point_after_one_cut` fails if the
marker outgrows its allowance, rather than letting the property rot. I found this because I wrote
the idempotence test before believing the rule worked; it failed on the second pass, which is the
argument for running the property on every new trigger and not only on the one that introduced it.

**Two mutations, both restored by file copy rather than `git checkout`, per the P6 lesson above.**
Flipping the step-4 guard to `cache == CachePosture::Warm` failed
`a_cold_prefix_recompacts_a_conversation_that_still_fits_and_a_warm_one_does_not` with *"a warm
prefix was perturbed for a token saving nothing had asked for — left: 1, right: 0"*, and also failed
the pre-existing P3 test `age_weighting_never_touches_a_conversation_that_already_fits`, which is the
useful part: it proves the warm path really is pre-P5 behaviour and not merely asserted to be.
Deleting the `AGED_FIXED_POINT_CHARS` line failed `a_cold_recompaction_is_idempotent` with *"left: 1,
right: 0"* on the second pass. Both restored with no residue; full suite green after each.

**Two test failures found and NOT fixed here, because they are not this phase's.**
`goose_agent::tests::the_adapter_reads_the_catalog_it_was_given` and
`a_catalog_window_reaches_the_governor_from_the_adapter` both fail at HEAD (`fc78ec03`), asserting
`131072` where the governor now yields `32768`. That is the same shape f770f4de already corrected
twice: tests encoding the pre-clamp behaviour for an on-device provider. These two survived because
`pond-adapters-goose` is outside the fast-crate test set and CI only `cargo check`s it, so nothing
runs them. I verified they fail with my changes stashed before concluding they were not mine.

> **Corrected 2026-08-06, and "pre-existing" was the wrong word.** The diagnosis above is right in
> every particular and the stash check was the correct way to reach it — but the label reads as
> "somebody else's, from before this work", and these were **broken by `f770f4de` earlier the same
> session**, which is the clamp fix two commits upstream. Not the phase's fault; still this
> session's, and worth attributing rather than filing under inherited debt.
>
> The reason they were not caught at the source is mine and is the general lesson: `f770f4de` ran
> `cargo check -p pond-adapters-goose` and not `cargo test`. **A `check` proves a crate compiles and
> says nothing about whether its assertions still hold**, and for the one crate CI never tests, that
> gap is the whole safety net. Anything touching `context_governor` precedence must run
> `cargo test -p pond-adapters-goose`, because the adapter keeps its own copies of those
> expectations.
>
> Both are fixed now: the on-device case asserts the ceiling, and a hosted `openai` fixture was added
> beside it so the clamp reads as a boundary rather than a blanket. That is the **fourth** test found
> encoding the pre-clamp belief — two in `f770f4de`, two here — which is what a value duplicated
> across a domain crate and its adapter costs when it changes.

Gates: `cargo fmt --check` clean; `cargo clippy -p pond-core --all-targets` with no new warnings in
the touched files (the two the trimmer reports — a `nonminimal_bool` on P3's age predicate and a
`duplicate_macro_attributes` on an untouched test — are both pre-existing); `cargo test -p pond-core`
802 lib passing; `cargo test -p pond-adapters-goose` 98 passing with the two pre-existing failures
above; `cargo check -p pond-server -p pond-adapters-goose` clean. No live-server run and no device
run — the second is the phase's own acceptance criterion and is why this is not stamped LANDED.

---

**2026-08-06 — PAI-4 P7a. The manual button is the one place a rate limiter is easiest to argue
away, and the design bullet was wrong about the UI it named.**

The bullet is `POST /sessions/{id}/compact` plus a desktop control on the existing `ContextCard`.
The API half landed; the UI half is respecified and outstanding, and finding out why is most of what
I did before writing any code.

**`ContextCard.tsx` is not what the bullet thinks it is.** It is the MCP-UI tool-result card
renderer — it takes a `ContextCard` off `state/reducer.ts` and dispatches to the weather, devices,
memory and schedules renderers. "Context" there is tool-call context, not context window. A grep of
all of `pond-desktop/src` for `context_warning`, `contextWarning`, `should_compact`, `context_health`
and `percent_used` returns nothing, and `context_warning` is not even in the `ChatEventType` union:
the frame the server has emitted since before PAI-4 has zero consumers in the shipped app. So P7b is
a new surface across both chat views, not a button on an existing card, and it is scoped as such
rather than smuggled into this landing. CLAUDE.md's "know what has no UI before writing a UI test for
it" is exactly this case — a Playwright test driving the assumed control would have exercised nothing
and passed, which is how this programme gets a seventh vacuous test.

**The endpoint does not bypass P6's rate limiter, and refusing to let it is the phase.** "The user
asked, so just do it" is the natural shape for a manual control and it is a regression here.
`should_compact` is monotone above 75%, so a press-driven pass is a *rate*: a button that skipped
`claim_compaction` would let anything holding a bearer token queue summarisation model calls in front
of the user's next turn on a serial on-device engine. The endpoint is therefore strictly a subset of
what the pressure axis already does — it can bring a pass forward within the rules, never past them.
The cost is real and named in the doc rather than hidden: a session below the threshold, and any
session the monitor has not recorded a turn for, answers `not_under_pressure`. Making the button work
under no pressure needs a wall-clock cooldown, because the existing one counts *recorded turns* and
with no turns happening it never expires. That is a phase, not a line.

**Two orderings deliberately differ from `spawn_pressure_compaction`.** It claims before reading the
provider, because the claim is the cheap check. This reads the provider first and peeks at
`COMPACTIONS_IN_FLIGHT` before claiming, because a claim spent on a pass that cannot run burns a
cooldown counted in turns — and a person pressing a button is very often taking no turns, so the
control would stay dead until they did. Two of the five tests assert that a refusal leaves the next
real claim available.

**The first draft of the guard would have passed against the bug it guards.** I wrote
`a_second_press_is_refused_by_the_cooldown` asserting `status == "skipped"`. Mutating the claim away
still produced "skipped", because a second pass finds the through-pointer already advanced and
answers `NothingToDo` — which is also a skip. The assertion that actually holds is `outcome is null`,
null exactly when no pass ran. With the claim disabled it fails with *"the second press ran a second
pass - the manual endpoint is bypassing P6's rate limiter, and a client hammering it would stack
summarisation model calls in front of the user's next turn on a serial on-device engine"* and the
body showing `"outcome":"nothing_to_do"`. Restored by reversing the edit; `grep "if false"` over
`routes.rs` returns nothing and the five tests are green again.

**A mock that was silently dropping the switch it was being asked about.** The first switch test
failed with `status: "compacted"` while `hybrid_compaction_enabled` was false:
`MockSettingsRepository` overlays a hand-picked subset of keys and neither compaction switch was in
it, so `update` wrote nothing and `get` returned the `true` default. Both switches now round-trip,
for the reason the mock's own `mic_enabled` comment already gives.

Gates: `cargo fmt --check` clean; `cargo test -p pond-api` all suites green including the 5 new
(115 lib + integration suites); `cargo test -p pond-core` 802 lib passing; `cargo clippy -p pond-api
-p pond-core --all-targets` with no new warnings from the touched files; `cargo check -p pond-server
-p pond-adapters-goose` clean. No live-server run — this adds a route, which is precisely what
`scripts/live-test.sh` exists to catch and no in-process `oneshot` router test can — and the endpoint
has never run against a real on-device summariser, only `MockProvider`.
