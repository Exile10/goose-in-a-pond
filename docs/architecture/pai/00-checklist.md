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
| 5 | **Large context**, using each model's window dynamically and to the fullest | [PAI-3](./03-context-governor.md) | **P1, P2 LANDED**; P3-P6 designed |
| 6 | **Smart compaction** based on different models, time and cache age | [PAI-4](./04-smart-compaction.md) | DESIGNED |
| 7 | **Personal context streaming** — on-pond, on-mobile, and internet accounts | [PAI-8](./08-personal-context-streaming.md) | DESIGNED |
| 8 | **Privacy and security guardrails** to minimise data and secret exposure | [PAI-2](./02-privacy-and-security-guardrails.md) | **P0, P1, P2, P4 LANDED**; P3, P5-P8 designed |

They are equally weighted and mutually interdependent. `DESIGNED` means the document exists and its
current-state claims were verified against code; it does **not** mean any code has changed. `LANDED`
is stamped per phase, and means the gates in 2.3 were run and passed.

**PAI-1 is COMPLETE** as of 2026-08-05: P1-P8 all landed, including P4's policy layer and the
repair of P5, which was recorded as landed while being inert on every default install. PAI-3 P1/P2
and PAI-2 P0 are landed, PAI-2 P4 encrypts `secrets.json` at rest, and PAI-2 P1 is complete: the
mode, the identity-assertion call site, and the draft-decision gate that gives a staged action an
owner. Everything else is still design only.

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
