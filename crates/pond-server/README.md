# Pond Server

The Runtime and Composition Root for Goose-in-a-pond.

## Purpose
- **Orchestration**: Wires the Adapters (Goose, Infra) to the Core logic.
- **Entry Point**: The main binary that starts the system.
- **Configuration**: Manages environment variables and runtime settings.
- **API Host**: Runs the server that exposes the `pond-api` to the frontend.

## Folder Structure
- `src/main.rs`: The "Composition Root" where all dependencies are initialized.
- `src/config/`: Runtime configuration loading.

## TDD Approach
- **Smoke Tests**: Ensure the server can start and shut down gracefully.
- **Dependency Wiring Tests**: Verify that the correct implementations are injected into the core services.
