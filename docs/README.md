# Goose In A Pond — Documentation

Welcome to the GIAP documentation. This index covers architecture, development guides, and contribution workflows.

---

## Architecture

| Document | Description |
|---|---|
| [Component Breakdown](./architecture/components.md) | Purpose and structure of every crate in `crates/` and `pond-desktop/` |
| [Data Flow & Lifecycle](./architecture/data_flow.md) | How a chat request travels from HTTP → Core → Goose → response |
| [Agent Pipeline](./architecture/agent_pipeline.md) | ToolAgent pre-processor, Main LLM, AnswerReviewer post-processor |
| [Model Capabilities](./architecture/model_capabilities.md) | Runtime capability discovery: thinking, vision, context window |
| [Visual Workflow](./architecture/visual_workflow.md) | Flowcharts and sequence diagrams using the Weather feature as an example |
| [Data Pipeline](./architecture/data_pipeline.md) | Constrained inference architecture for edge hardware (Jetson Orin Nano) |
| [Scheduling System](./architecture/scheduling.md) | Cron-based automation: agent prompts, webhooks, MCP tools, run history |
| [Memory System](./architecture/memory_system.md) | Segments, importance scoring, decay, extraction, consolidation |
| [Token Tracking](./architecture/token_tracking.md) | Per-session usage, estimation, cost savings vs cloud API |

---

## Developer Guides

| Document | Description |
|---|---|
| [Installation Guide](./developer/installation.md) | First-time setup, prerequisites, install script, LLM providers, troubleshooting |
| [Creating Ports & Adapters](../docs/creating-ports-and-adapters.md) | Step-by-step guide for adding new capabilities while keeping the Core pure |
| [Inference Optimization](./developer/inference_optimization.md) | Platform settings, context management, classifier tuning, memory pressure |
| [Voice Pipeline Interrupt](./developer/voice_pipeline_interrupt.md) | Wake-word interrupt during inference/TTS, pipelined synthesis |
| [TDD Guide](./testing/tdd_guide.md) | Test-driven development practices — Core mocks first, then real adapters |
| [Onboarding System](./Onboarding_Doc/Onboarding_guide.md) | Multi-step device onboarding flow and API protection |

---

## Contributing

| Document | Description |
|---|---|
| [Contributing Guide](./CONTRIBUTING.md) | Fork, branch, test, and PR workflow |

---

## Key Concepts

### Hexagonal Architecture (Ports & Adapters)

The `pond-core` crate contains all domain logic. It never imports from Goose, SQLx, or Axum. Everything external is hidden behind a `trait` (a **port**). Concrete implementations (**adapters**) live in separate crates and are injected at startup in `pond-server/src/main.rs`.

This means:
- You can test all business logic with zero database or network setup
- Swapping LLM backends, databases, or voice engines requires no core changes
- The domain compiles and tests in seconds, not minutes

### The Agent Loop

`pond-core::services::chat::ChatService::run_loop()` implements the four-state machine:

```
Wait (wake word) → Listen (ASR) → Think (LLM via Goose) → Speak (TTS)
```

In production, all inference routes through `GooseAdapter` which wraps Block's Goose agent, loads prompt templates, injects memory and user skills, and manages the MCP extension lifecycle.

### GIAP as a Goose MCP Extension

GIAP exposes 9 MCP tools to the Goose agent under the `giap__` prefix:

| Tool | Description |
|---|---|
| `giap__get_current_weather` | Open-Meteo weather lookup |
| `giap__list_registered_devices` | Device registry query |
| `giap__list_schedules` | Cron task listing |
| `giap__get_user_profile` | User name, timezone, location |
| `giap__get_model_assignments` | Current LLM role assignments |
| `giap__recall_memories` | Search memory fragments |
| `giap__save_memory` | Persist a memory fragment |
| `giap__get_recipe` | Fetch a Goose recipe YAML |
| `giap__list_skills` | List active user skills |
