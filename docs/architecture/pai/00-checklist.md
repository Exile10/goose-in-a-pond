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
| 4 | **Hard profile boundaries** | [PAI-1](./01-identity-and-profile-boundaries.md) | DESIGNED |
| 5 | **Large context**, using each model's window dynamically and to the fullest | [PAI-3](./03-context-governor.md) | **P1 LANDED**, P2-P6 designed |
| 6 | **Smart compaction** based on different models, time and cache age | [PAI-4](./04-smart-compaction.md) | DESIGNED |
| 7 | **Personal context streaming** — on-pond, on-mobile, and internet accounts | [PAI-8](./08-personal-context-streaming.md) | DESIGNED |
| 8 | **Privacy and security guardrails** to minimise data and secret exposure | [PAI-2](./02-privacy-and-security-guardrails.md) | DESIGNED |

They are equally weighted and mutually interdependent. `DESIGNED` means the document exists and its
current-state claims were verified against code. It does **not** mean any code has changed.

**Nothing has been implemented yet.** Every `crates/` and `pond-desktop/` change is still ahead.

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
3. **Re-verify the current-state claims you are about to rely on.** Every document is stamped
   *Verified against code 2026-08-03*. Line numbers rot faster than prose — grep for the symbol,
   do not trust the `file:line`. If a claim is now false, fix the document in the same change; a
   design doc that lies is worse than no design doc.
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
11. **Clear the documentation debt** attached to the workstream (roadmap section 4) while you are
    in the file. It is one line each and it never gets cheaper.

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

### Next: PAI-3 P2

`TokenCounter` port, GGUF-backed adapter, chars/4 as the declared fallback, overshoot-feedback
correction retained as the safety net. `turn_trimmer.rs:89-91` is the estimator to displace, and
`WindowSource::is_exact()` already exists to tell budget code when it can trust the window to the
token.
