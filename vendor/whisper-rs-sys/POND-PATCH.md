# Whisper GGML isolation

This directory vendors the published `whisper-rs-sys` 0.15.0 crate, including its
original licensing and native sources. It is selected by the root Cargo patch.

The Jetson's static CUDA build links both Whisper and llama.cpp. Both libraries
embed different GGML revisions and exported the same symbols. The old link failed
with 533 duplicate definitions; permitting duplicate definitions would also let
one engine call the other engine's incompatible implementation.

On Linux, `namespace.rs` scans the pinned native sources and generates a forced
include that gives Whisper's GGML/GGUF, C++ `ggml` namespace, CUDA sum-row helper,
quantization, block types and IQ helpers a
`pond_whisper_` prefix. C, C++ and CUDA receive the same header. Bindgen preserves
the Rust API and assigns explicit private link names to corresponding FFI items.
Generation must succeed; Linux cannot fall back to bindings that name public GGML
symbols. Dynamic backend loading is disabled because it looks up public symbol
names; CPU and CUDA backends remain statically registered. Other platforms retain
the upstream build behavior.

Changes from the published crate: this note, `namespace.rs`, build-script wiring,
a standalone Cargo workspace declaration, the upstream Unlicense text
(restored from the whisper-rs repository because the published crate omitted it),
and two native source changes, below.
When updating Whisper, regenerate and inspect the linked symbol inventory and run
both GPU transcription and inference in the same production process before shipping.

## Native source change: graph buffer reservation failure

`whisper.cpp/ggml/src/ggml-backend.cpp`, in `ggml_backend_sched_alloc_splits`, is
upstream llama.cpp commit `911f6cdc8a` ("ggml : handle graph buffer reservation
failure", #26070, 2026-09-18), applied unchanged. Drop it when the vendored GGML
already contains that commit.

Without it, a compute buffer that cannot be allocated crashes the process instead of
returning an error. `ggml_gallocr_reserve_n` records the new graph layout, fails to
allocate the buffer and leaves it `NULL`; the scheduler ignored that result, and
`ggml_gallocr_alloc_graph` then found a matching layout, skipped reallocation and
dereferenced the `NULL` buffer. On a Jetson this is ordinary: the LLM and Whisper share
the GPU, and on 2026-10-04 a 90 MiB encoder buffer failed and the Pond took `SIGSEGV`.
The last log line before such a crash is `ggml_gallocr_reserve_n_impl: failed to
allocate CUDA0 buffer`; with the change it is followed by `failed to reserve graph
buffers` and Whisper's own `failed to init ... allocator`, and the transcription
returns an error.

## Native source change: decoder KV cache that cannot grow

`whisper.cpp/src/whisper.cpp`, in `whisper_full_with_state`, no longer frees the state
when the self-attention cache cannot be recreated for more decoders, and in
`whisper_kv_cache_init` a failed buffer allocation frees the ggml context it made.
This is Pond's own change, not an upstream one. Drop it when upstream whisper.cpp
stops calling `whisper_free_state` before `return -7`.

A state is created with a self-attention cache for one decoder. Beam search grows it
on the first decode to `beams + 2` decoders' worth, 42 MiB for `base` with five beams.
When that allocation failed, whisper.cpp freed the state and returned `-7`. But the
state belongs to the caller, and whisper-rs's `Drop` frees it again: a double free. On
2026-10-05 the Pond took `SIGBUS` on the Jetson one second after
`whisper_kv_cache_init() failed for self-attention cache`, while a build held most of
the memory. The `ggml_backend_sched_alloc_splits` change above does not reach this
path: it happens after the state, with its compute buffers, already exists. Now the
state is left freeable and reusable. The cache is marked absent, with no buffer and a
decoder count of zero, so another `whisper_full` on the state recreates it.
`running_out_of_gpu_memory_while_decoding_is_an_error_not_a_crash` in
`crates/pond-adapters-whisper` reproduces the failure on a CUDA build. Its doc comment
says how to read a run that aborts in GGML's CUDA scratch pool instead
(`cuMemAddressReserve` in `ggml-cuda.cu`). That is a separate failure this change does
not touch: GGML aborts when a state's first decode cannot reserve its pool.

`build.rs` declares the native sources with `rerun-if-changed` and refreshes the copy
in `OUT_DIR` file by file. Before, the copy was made once per `OUT_DIR` and only
`wrapper.h` and `namespace.rs` were watched, so an edit to a native source compiled
nothing and the earlier library shipped with no error. `namespace.rs` leaves the
forced-include header alone when its content is unchanged: every native file includes
it, so rewriting it recompiled all of them (44 minutes on the Jetson) for a one-file edit.
