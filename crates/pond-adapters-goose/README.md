# Pond Adapters: Goose

The bridge between the Pond Assistant and the [goose](https://github.com/block/goose) framework.

## Purpose
- **Agent Service Implementation**: Implements the `AgentService` port defined in `pond-core`.
- **Event Mapping**: Translates Goose events (tool calls, text, errors) into Pond domain events.
- **Goose Management**: Handles the lifecycle of the Goose agent and its MCP extensions.

## Folder Structure
- `src/mapping/`: Logic to translate between Goose types and Pond types.
- `src/client/`: The actual logic to invoke Goose and stream results.

## TDD Approach
- Integration tests in `tests/` ensure that Goose responses are correctly mapped to Pond domain objects.
- Mocking the Goose framework where possible to test error handling and edge cases.
