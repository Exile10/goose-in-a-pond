# Goose in a Pond 🦢🏡

**Goose in a Pond** is a privacy-first smart home assistant powered by the [Goose](https://github.com/block/goose) agent framework. It runs entirely locally, ensuring your data never leaves your environment.

---

## 🏛️ Architecture

We use a **Hexagonal (Ports & Adapters)** architecture to keep our core logic ("The Brain") independent from external frameworks and technologies.

- **[Architecture Overview](./docs/architecture/overview.md)**: Deep dive into our design philosophy.
- **[Visual Architecture & Example Flow](./docs/architecture/visual_workflow.md)**: Flowcharts and Sequence diagrams (Weather Example).
- **[Component Breakdown](./docs/architecture/components.md)**: Purpose of each crate in the `crates/` directory.
- **[Data Flow & Lifecycle](./docs/architecture/data_flow.md)**: How requests travel through the system.

---

## 📚 Development & Documentation

### Guides
- **[Developer Tutorial: Clean Code & Dependencies](./docs/developer/clean_code_and_dependencies.md)**: Maintaining modularity.
- **[End-to-End Workflow Example: Weather Feature](./docs/developer/example_workflow_weather.md)**: A complete implementation walkthrough.
- **[Pond Builder Manual](./docs/developer/pond_builder.md)**: Using our custom `pond-builder` CLI.
- **[TDD Guide](./docs/testing/tdd_guide.md)**: Writing unit and integration tests.
- **[Contributing Guide](./docs/CONTRIBUTING.md)**: Standard workflow for contributions.

---

## 🛠️ Tech Stack
- **Core**: Rust
- **Agent Framework**: [Goose](https://github.com/block/goose)
- **Database**: SQLite (via SQLx)
- **API**: Axum

---

## 🚀 Getting Started

1. **Clone & Submodules**:
   ```bash
   git clone --recursive https://github.com/jarida-io/goose-in-a-pond.git
   ```
2. **Setup Environment**:
   ```bash
   cargo build --workspace
   ```
3. **Run the Server**:
   ```bash
   cargo run -p pond-server
   ```
