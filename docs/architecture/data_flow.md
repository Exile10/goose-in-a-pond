# Data Flow & Lifecycle

Understanding how a single user request travels through the system.

---

## Example: A "Chat" Request

### 1. External Trigger (UI)
The React/Electron UI sends a JSON payload to the `pond-server` via a POST request to `/api/v1/chat`.

### 2. Adaptation (Server -> Core)
The `pond-server` receives the request. It uses the `pond-api` crate to deserialize the JSON into a `ChatRequest` DTO.
It then calls a service in `pond-core`:
```rust
chat_service.handle_message(request.text).await?;
```

### 3. Business Logic (Core)
The `ChatService.handle_message` function executes the orchestration:
1.  **Retrieve History**: Calls the `StoragePort.get_history()` (Implemented by `pond-infra`).
2.  **Generate Response**: Calls the `AgentPort.generate()` (Implemented by `pond-adapters-goose`).

### 4. External Execution (Adapter)
The `pond-adapters-goose` implementation takes over:
1.  Calls the `goose` crate functions.
2.  Wait for the AI agent to process and potentially call MCP tools.
3.  Returns a `DomainResponse`.

### 5. Final Delivery
The `ChatService` in the Core returns the result to the `pond-server`.
The `pond-server` serializes it into a JSON response and sends it back to the UI.

---

## Sequence Summary
1.  **UI** (React)
2.  **Server** (Axum)
3.  **Core** (Orchestrator)
4.  **Adapter** (Goose Wrapper)
5.  **External** (Goose Crate)
6.  *... and back again ...*
