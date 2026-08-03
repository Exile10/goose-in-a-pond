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
| 5 | **Large context**, using each model's window dynamically and to the fullest | [PAI-3](./03-context-governor.md) | DESIGNED |
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
