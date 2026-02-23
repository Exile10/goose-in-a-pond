# Pond Infrastructure

Concrete implementations of system-level traits (Storage, Filesystem, Network).

## Purpose
- **Persistence**: SQLite implementation of the Repository port.
- **System Access**: Logging, file management, and hardware interactions.
- **Separation of Concerns**: Prevents database details from leaking into the core logic.

## Folder Structure
- `src/sqlite/`: SQLx-based implementation of the storage ports.
- `src/filesystem/`: OS interaction for config files and logs.

## TDD Approach
- **Database Tests**: Use `sqlx` in-memory SQLite to verify queries and migrations in `tests/`.
- **Filesystem Tests**: Use temporary directories to verify file operations.
