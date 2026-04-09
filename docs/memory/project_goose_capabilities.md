---
name: Goose Submodule Capabilities
description: Inventory of Goose submodule features relevant to GIAP integration — on-device inference, MCP, providers, scheduling, memory
type: project
---

On-device inference lives at `goose/crates/goose/src/providers/local_inference/` — uses `llama-cpp-2` (0.1.137) + `candle` for GGUF model loading. Feature-gated: `local-inference`, `cuda` (CUDA accel), Metal macOS. Whisper STT via Candle is also here (`dictation/whisper.rs`).

MCP (Model Context Protocol) lives in `goose/crates/goose-mcp/src/` — builtin servers: MemoryServer, ComputerControllerServer, AutoVisualiserRouter, TutorialServer. MCP client in `goose/crates/goose/src/agents/mcp_client.rs`. Built on `rmcp` crate v1.2.0.

Scheduler at `goose/crates/goose/src/scheduler.rs` — tokio-cron-scheduler, SchedulerTrait abstraction, persistent schedule.json, YAML Recipe-based jobs.

Context management at `goose/crates/goose/src/context_mgmt/mod.rs` — auto-compaction at 80% context, summarization-based pruning, tool-pair batch summarization.

EmbeddingCapable trait at `goose/crates/goose/src/providers/embedding.rs` — `create_embeddings(session_id, texts) -> Vec<Vec<f32>>`.

Declarative providers JSON in `goose/crates/goose/src/providers/declarative/` — groq.json, mistral.json, deepseek.json, cerebras.json, etc.

Download manager at `goose/crates/goose/src/download_manager.rs` — resumable HTTP range requests, progress tracking for GGUF model downloads.

**Why:** Understanding what Goose already has prevents duplicate work and enables wrapping existing code in GIAP ports.
**How to apply:** Before building any new GIAP adapter, check if Goose already has it. Prefer `GooseProviderAdapter` wrapping over re-implementing.
