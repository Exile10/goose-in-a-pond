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
| 4 | **Hard profile boundaries** | [PAI-1](./01-identity-and-profile-boundaries.md) | **P1, P2 LANDED**; P3 half in; P4-P8 designed |
| 5 | **Large context**, using each model's window dynamically and to the fullest | [PAI-3](./03-context-governor.md) | **P1, P2 LANDED**; P3-P6 designed |
| 6 | **Smart compaction** based on different models, time and cache age | [PAI-4](./04-smart-compaction.md) | DESIGNED |
| 7 | **Personal context streaming** — on-pond, on-mobile, and internet accounts | [PAI-8](./08-personal-context-streaming.md) | DESIGNED |
| 8 | **Privacy and security guardrails** to minimise data and secret exposure | [PAI-2](./02-privacy-and-security-guardrails.md) | DESIGNED — **P0 is a live auth defect, fix first** |

They are equally weighted and mutually interdependent. `DESIGNED` means the document exists and its
current-state claims were verified against code; it does **not** mean any code has changed. `LANDED`
is stamped per phase, and means the gates in 2.3 were run and passed.

Implementation has begun: PAI-3 P1/P2 and PAI-1 P1/P2 are in. Everything else is still design only.

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
catalog; PAI-8 undercounted the multipart upload routes (`/voice/calibrate` and
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

### Next: PAI-1 P3 wiring, then PAI-3 P3

`TokenCounter` port, GGUF-backed adapter, chars/4 as the declared fallback, overshoot-feedback
correction retained as the safety net. `turn_trimmer.rs:89-91` is the estimator to displace, and
`WindowSource::is_exact()` already exists to tell budget code when it can trust the window to the
token.
