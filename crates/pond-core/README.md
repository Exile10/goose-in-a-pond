# Pond Core

The "Brain" of Goose-in-a-pond. This crate contains the pure business logic and domain models.

## Purpose
- **Domain Independence**: No knowledge of databases, UI, or specific AI frameworks.
- **Rules & Orchestration**: High-level logic for assistant behavior (e.g., when to trigger a search, how to manage state).
- **Ports (Traits)**: Defines the interfaces that external adapters must implement.

## Folder Structure
- `src/domain/`: Core data structures and logic (e.g., `AssistantState`, `Memory`).
- `src/ports/`: Rust traits for AI Services, Storage, and System access.
- `src/services/`: Orchestrators that wire ports together to perform assistant tasks.

## TDD Approach
1. Define a Port in `src/ports/`.
2. Write a test in `src/services/` using a Mock implementation of the Port.
3. Implement the service logic to satisfy the test.
