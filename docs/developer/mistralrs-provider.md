# The `mistralrs` provider — Mac-only, for evaluation

`chat_provider = "mistralrs"` routes GIAP at a local mistral.rs server over its
OpenAI-compatible API. It exists so the engine can be driven and judged from the
real serving path. **It is not a Jetson candidate**: the Orin budget analysis is in
`~/Documents/Jarida/mistralrs-bakeoff/ORIN-BUDGET.md`, a lab directory outside this
repository.

## Running it

```bash
~/Documents/Jarida/mistralrs-bakeoff/run.sh     # starts mistral.rs on :9002 (lab, outside the repo)
MODE=goose scripts/try-mistralrs.sh             # GIAP against it through goose, scratch pond, --native
```

`GIAP_MISTRALRS_URL` defaults to `http://127.0.0.1:9002`.

A second path reaches the same server without goose at all; see
[`mistralrs-direct-agent.md`](mistralrs-direct-agent.md). This document
describes the goose-hosted provider; that one describes the direct backend, and
`scripts/try-mistralrs.sh` switches between them with `MODE` (`direct` is its default).

## Where it is wired

| file | what |
|---|---|
| `crates/pond-core/src/models/services/context/model_class.rs` | `"mistralrs"` is in `ON_DEVICE_PROVIDERS`, so the context governor treats it like the other local engines |
| `crates/pond-adapters-goose/src/goose_agent.rs` | a `"mistralrs"` arm building goose's **OpenAI** provider against `GIAP_MISTRALRS_URL`; mistral.rs speaks OpenAI, not the Ollama protocol the llamafile arm borrows |
| `crates/pond-adapters-goose/src/goose_agent.rs` | `native_tools_json` includes `"mistralrs"`: tools travel structurally, so the prompt must not also list them |
| `crates/pond-server/src/main.rs` | a `build_provider` arm for the direct `LlmProvider`: `LlamafileProvider` is already OpenAI-compatible, so it needs only the URL |

## Two traps found while wiring it

**The provider setting does not survive a restart.** `sync_assignments_to_settings`
runs at startup (`crates/pond-server/src/main.rs`) and rewrites `chat_provider` from role
assignments through `category_to_provider`, whose catch-all is `"llamafile"`.
So the setting must be PUT *after* the server is up, which is what
`try-mistralrs.sh` does. A stored `mistralrs` will silently become `llamafile`
on the next boot.

**`ModelCategory::for_chat_provider` maps unknown providers to `Llamafile`**, so
startup logs "Model 'llamafile/default' not found in catalog" and threatens to
fall back to mock echo. Harmless — chat still routes correctly — but it is noise
that would need a new category to fix properly.

## The bug that makes it unusable

Measured 2026-09-10. mistral.rs's **prefix cache corrupts on the second request carrying a large tool
payload**, and every subsequent one fails. Measured on a fresh server, 40 tools:

```
request 1: ok        request 2..5: FAIL
```

The failure is invisible at the HTTP layer: it returns **200** with the error
inside the SSE body, so the server's own metrics show twelve healthy 200s.

```
data: {"error":{"message":"Internal server error.","type":"server_error","code":"internal_error"}}
```

Requests without tools are unaffected, and `--prefix-cache-n 0` fixes it
completely (5/5). That is the workaround `run.sh` now applies.

**And it is a trap, because the prefix cache is the only reason mistral.rs was
competitive.** With it on, a repeated 1,441-token prompt costs 25 ms instead of
950. With it off, every GIAP turn re-prefills ~7,300 tokens:

| | TTFT | decode |
|---|---:|---:|
| three real GIAP turns through mistral.rs | 7.5–7.9 s | 13.8–18.4 t/s |

So on GIAP's actual workload you can have prefix caching **or** working tool
calls, not both. That is not a tuning problem.
