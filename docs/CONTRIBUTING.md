# Contributing to  Goose in a Pond

---

Thank you for your interest in contributing to **Goose in a Pond**.
Goose in a Pond is a project that seeks to develop privacy first home assistants that utilise AI offline.

We welcome contributions from developers, writers, designers, researchers, and community members.

---

## Ways to Contribute

You can contribute to Jarida in the following ways:

* Reporting bugs or issues
* Suggesting features or improvements
* Submitting code contributions
* Improving documentation
* Reviewing pull requests
* Contributing articles, datasets, or research (where applicable)

---

## Before You Start

* Check existing **Issues** and **Pull Requests** to avoid duplication
* For major changes, open an issue first to discuss your proposal
* Ensure your contribution aligns with Jarida’s mission and values

---

## Getting Started (Code Contributions)

1. **Fork the repository** and clone it locally.
2. **Setup your environment**:
   - Ensure the latest stable **Rust** is installed.
   - Initialize the `goose` submodule:
     ```bash
     git submodule update --init --recursive
     ```
3. **Build the Workspace**:
   ```bash
   cargo build --workspace
   ```
4. **Create a new branch**:
   ```bash
   git checkout -b feature/short-description
   ```
5. **Make your changes** following the **Hexagonal Architecture** pattern.
6. **Verify your work**:
   ```bash
   cargo test --workspace
   ```
7. **Commit & Push**.

---

## Technical Contribution Guidelines

To keep the project healthy and maintainable, we follow the **Ports & Adapters (Hexagonal)** pattern:

- **Logic First**: Implement business logic in `crates/pond-core` using **TDD (Test-Driven Development)**.
- **Port-Based**: Define traits in the Core and implement them in `crates/pond-infra` or `crates/pond-adapters-goose`.
- **Pure Core**: Keep the `pond-core` crate free of database or network dependencies.
- **Clean Code**: Follow the styles defined in the [Clean Code & Dependencies Guide](./docs/developer/clean_code_and_dependencies.md).
- **Format & Lint**: Run `cargo fmt` and `cargo clippy` before submitting.

---

## Licensing of Contributions (Important)

By submitting a contribution to Jarida, you agree that:

* Your contribution will be licensed under the **Apache License 2.0** and the **CC by 4.0**.

If you do not agree with these terms, please do not submit a contribution.

---

## Intellectual Property & Patents

* You confirm that you have the right to submit the contribution
* You agree not to knowingly contribute code or content that infringes third-party rights
* Patent protection and retaliation clauses apply as defined in the applicable license

---

## Reporting Issues

When reporting bugs or issues, please include:

* A clear and descriptive title
* Steps to reproduce the issue
* Expected vs actual behavior
* Screenshots, logs, or error messages where helpful

---

## Code of Conduct

All contributors are expected to:

* Be respectful and inclusive
* Engage constructively and professionally
* Assume good faith in discussions

Harassment, discrimination, or abusive behavior will not be tolerated.

---

## Review Process

* Maintainers will review contributions as time allows
* Feedback or changes may be requested
* Approved contributions will be merged by a maintainer

---

## Questions or Clarifications

If you have questions about contributing, licensing, or commercial use, please contact:

**Jarida**
[info@jarida.io]

---

Thank you for contributing to **Goose In a Pond** and helping build a privacy first smart home assistant.
