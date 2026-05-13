# Scratchpad

## Current: KV Cache Prefix Reuse — Next Steps

### Status
The system prompt is now token-stable across turns (dynamic content in `<system-context>` user message). The `state_save_file` approach was attempted and reverted — serialising a 32K context to disk blocks the inference thread.

### Better approaches to try
1. **Persistent LlamaContext** — keep one context alive alongside LoadedModel. Use `clear_kv_cache_seq(seq_id, prefix_len, -1)` to trim the suffix after each turn, then prefill only new tokens. Challenge: `LlamaContext<'model>` borrows `&'model LlamaModel` — self-referential struct. Solution: split borrows (`&model` + `&mut prefix_cache`) or use `Pin` + unsafe.

2. **Sequence forking** — keep a base context with the system+tools prefix. On each turn, `copy_kv_cache_seq(0, 1, 0, prefix_len)` to fork into a working sequence, generate on seq 1, then discard seq 1.

### Available llama-cpp-2 v0.1.146 API
- `clear_kv_cache_seq(seq_id, p0, p1)` — clear range
- `copy_kv_cache_seq(src, dest, p0, p1)` — fork sequence
- `kv_cache_seq_add(seq_id, p0, p1, delta)` — shift positions
- `state_save_file(path, tokens)` / `state_load_file(path, max)` — file-based (too slow for 32K)

### Target improvement
35-64s/turn → 12-25s/turn (skip ~3K token prefix prefill on repeat turns)
