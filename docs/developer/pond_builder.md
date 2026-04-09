# Pond Builder: Developer Manual

> **Status: Removed.**
> `pond-builder` (`crates/pond-builder`) has been deleted from the workspace. The commands documented below no longer work.
>
> **Replacement:** Follow `docs/developer/creating-ports-and-adapters.md` for the manual step-by-step process of adding ports, adapters, and services. The guide covers the same ground with explicit code examples that are always up to date.

---

## What pond-builder used to do

The `pond-builder` was an automation tool that scaffolded code and managed workspace dependencies. It generated boilerplate for ports, adapters, and services to enforce the hexagonal architecture boundary.

### Commands (no longer available)

```bash
# Generate a port trait in pond-core
cargo run -p pond-builder -- make:port <NAME> --type [driving|driven]

# Generate an adapter implementing a port
cargo run -p pond-builder -- make:adapter <NAME> --for <PORT> --crate <CRATE_PATH>

# Generate a domain service
cargo run -p pond-builder -- make:service <NAME>

# Generate a TDD test scaffold
cargo run -p pond-builder -- make:test <NAME> --for <COMPONENT_PATH>
```

### Why it was removed

The builder required constant upkeep as the architecture evolved, and its generated code was often immediately edited. Direct implementation following the port/adapter pattern guide is faster and less error-prone.

---

## Current approach

See `docs/developer/creating-ports-and-adapters.md` for the authoritative step-by-step guide.
