# Pond Builder: Developer Manual

The `pond-builder` is an automation tool designed to maintain the integrity of the **Goose-in-a-Pond** Hexagonal Architecture. It scaffolds code, manages workspace dependencies, and ensures that the boundaries between "Core" and "Adapters" are strictly followed.

---

## 🚀 Getting Started

To run the builder, use `cargo run`:
```powershell
cargo run -p pond-builder -- <COMMAND>
```

---

## 🛠️ Commands

### 1. `make:port`
Generates a new Port (Trait) in the `pond-core` crate.

**Usage**:
```powershell
pond-builder make:port <NAME> --type [driving|driven]
```

- **Driving Ports**: Input to the assistant (e.g., `trait AudioCommand`).
- **Driven Ports**: Output/Infrastructure needed by the assistant (e.g., `trait WeatherService`).

**What it generates**:
- A trait file in `crates/pond-core/src/ports/<NAME>.rs`.
- Boilerplate for Port errors.
- Automatic registration in `crates/pond-core/src/ports/mod.rs`.

---

### 2. `make:adapter`
Generates a concrete implementation of an existing Port.

**Usage**:
```powershell
pond-builder make:adapter <NAME> --for <PORT> --crate <CRATE_PATH>
```

- **Example**: `pond-builder make:adapter sqlite --for Storage --crate crates/pond-infra`

**What it generates**:
- A struct implementing the target trait.
- Necessary imports to the target crate's `Cargo.toml`.

---

### 3. `make:service`
Generates a Domain Service (Orchestrator) in `pond-core`.

**Usage**:
```powershell
pond-builder make:service <NAME>
```

**Philosophy**: Services should use injected Ports. This command scaffolds the service struct and its `new()` constructor that accepts trait objects.

---

### 4. `make:test`
Scaffolds a TDD test file with boilerplate for mocking.

**Usage**:
```powershell
pond-builder make:test <NAME> --for <COMPONENT_PATH>
```

---

## 📐 Why use the Builder?

### Consistency
Every port, adapter, and service follows the same naming convention (`PascalCase` for types, `snake_case` for files).

### Clean Metadata
The builder automatically handles the `mod.rs` registrations and `Cargo.toml` workspace inheritance, reducing manual "plumbing" errors.

### Hexagonal Safety
The builder prevents you from accidentally implementing an adapter inside `pond-core`. It encourages the correct "Ports & Adapters" flow by design.

---

## 🧪 Development of the Builder

The builder itself is a Rust binary in `crates/pond-builder`.
- **Templates**: Code templates are stored as constants within the binary.
- **Parsing**: Uses `toml_edit` to safely modify workspace files.
