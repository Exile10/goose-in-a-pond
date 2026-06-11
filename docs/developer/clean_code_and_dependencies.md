# Tutorial: Clean Code & Dependency Management in Goose-in-a-Pond

Welcome to the development guide for **Goose-in-a-Pond**. This project uses **Hexagonal Architecture (Ports & Adapters)**. This tutorial will show you how to maintain this structure while adding and using dependencies.

---

## 1. How to Add a New Dependency

To keep our project modular and avoid version conflicts, we use **Workspace Inheritance**.

### Step A: Add to the Root `Cargo.toml`
Always search for the dependency on [crates.io](https://crates.io) and add it to the root `Cargo.toml` under `[workspace.dependencies]`.

```toml
# Example: Adding a library like 'uuid'
[workspace.dependencies]
uuid = { version = "1.0", features = ["v4"] }
```

### Step B: Enable it in a Specific Crate
Go to the `Cargo.toml` of the crate where you actually need the library (e.g., `crates/pond-infra/Cargo.toml`) and enable it using `workspace = true`.

```toml
[dependencies]
uuid = { workspace = true }
```

---

## 2. How to Call Dependencies in Code

Once the dependency is added to the crate's `Cargo.toml`, you can use it in your Rust files.

### Standard usage
```rust
// Use the 'use' keyword at the top of your file
use uuid::Uuid;

pub fn generate_id() -> String {
    Uuid::new_v4().to_string()
}
```

### Calling Internal Crates
If you are in `pond-server` and want to call logic from `pond-core`, ensure `pond-core` is listed in `pond-server/Cargo.toml`:

```toml
[dependencies]
pond-core = { workspace = true }
```

Then in `pond-server/src/main.rs`:
```rust
use pond_core::<quadrant>::services::AssistantService;
```

---

## 3. Writing Clean Code (The Hexagonal Way)

Clean code in this project means keeping the **Brain** (Logic) separate from the **Tools** (Frameworks).

### Rule 1: The Core is "Pure"
- **Don't** add database or network libraries (like `sqlx` or `reqwest`) to `pond-core`.
- **Do** define a **Port** (Trait) in `pond-core/src/<quadrant>/ports/`.

```rust
// In pond-core/src/<quadrant>/ports/storage.rs
pub trait UserStorage {
    fn save_user(&self, name: &str);
}
```

### Rule 2: Adapters do the "Dirty" Work
- **Do** implement the logic in `pond-infra` or `pond-adapters-goose`.
- This is where you use the heavy dependencies.

```rust
// In pond-infra/src/sqlite/user_repo.rs
impl UserStorage for SqliteUserRepo {
    fn save_user(&self, name: &str) {
        // Use sqlx here to save to the database
    }
}
```

### Rule 3: Use TDD (Test-Driven Development)
1. Write a test in `pond-core` using a **Mock** storage.
2. The core logic should pass the test without a real database.
3. This ensures your code is clean and truly decoupled.

---

## Summary Checklist
- [ ] Added version to root `Cargo.toml`?
- [ ] Added `dependency = { workspace = true }` to the specific crate?
- [ ] Is the business logic in `pond-core`?
- [ ] Is the implementation details (DB/Web) in an adapter?
