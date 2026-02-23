# Component Documentation

Each crate in the `crates/` directory represents a distinct module with a specific responsibility in the Hexagonal system.

---

## 1. `pond-core` (The Domain)
**Purpose**: Contains the "Brains" of the assistant.
- **`src/domain/`**: Pure data structures (e.g., `Message`, `Session`, `AgentConfig`).
- **`src/ports/`**: Trait definitions. If the core needs to do something external (save a file, ask an AI), it defines a trait here.
- **`src/services/`**: Use Cases. For example, `ChatService` handles the flow: *Receive Input -> Retrieve History (Port) -> Ask AI (Port) -> Save Result (Port) -> Return Output*.

## 2. `pond-adapters-goose` (AI Integration)
**Purpose**: Implements the `AgentBackend` port using the official `goose` crates.
- **Goose Mapping**: Translates Goose's internal events into our Domain events.
- **Lifecycle**: Manages the initialization of the Goose agent and its connection to MCP servers.

## 3. `pond-infra` (Infrastructure)
**Purpose**: Implements persistence and OS-level ports.
- **`src/sqlite/`**: SQLx-backed repository. Contains the actual SQL queries.
- **`src/filesystem/`**: Config file loaders and log handlers.

## 4. `pond-api` (Communication Layer)
**Purpose**: Shared types for the external world.
- **DTOs**: Data Transfer Objects designed for JSON serialization.
- **Validation**: Logic to ensure incoming requests from the UI are well-formed.

## 5. `pond-server` (Composition Root)
**Purpose**: The runnable binary.
- **Wiring**: This is where logic meets infrastructure. It creates a `SqliteUserRepo`, a `GooseAgentAdapter`, and injects them into the `ChatService`.
- **Server**: Hosts the API (Axum/HTTP) that the Electron UI talks to.
