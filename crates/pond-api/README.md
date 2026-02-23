# Pond API

Shared Data Transfer Objects (DTOs) and API contracts used by the server and UI.

## Purpose
- **Contract Definition**: Ensures the Rust backend and React frontend share the same data types.
- **Serialization**: Definitions for JSON/Protobuf/Websocket messages.
- **Versionality**: Supports multiple API versions simultaneously.

## Folder Structure
- `src/v1/`: Stable API version 1 types.
- `src/dto/`: Plain data structures for serialization.

## TDD Approach
- **Serialization Tests**: Ensure every DTO can be round-tripped through JSON without data loss.
- **Contract Validation**: Optional schema exports to verify frontend compatibility.
