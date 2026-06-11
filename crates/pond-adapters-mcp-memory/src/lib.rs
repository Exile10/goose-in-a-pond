//! Flat-file persistent memory adapter for GIAP.
//!
//! Implements [`McpKnowledgePort`] using the same storage format as Goose's
//! built-in `MemoryServer` so memories are portable between the two systems.
//!
//! **Storage layout** (`memory_dir/`):
//! ```text
//! memory_dir/
//!   preferences.txt
//!   devices.txt
//!   routines.txt
//!   ...
//! ```
//! Each category file contains entries separated by blank lines.  An entry
//! begins with optional `#tag` lines, followed by the content lines.
//!
//! **Usage:**
//! ```ignore
//! let memory = GooseMcpMemoryAdapter::new(data_dir.join("memory"));
//! // Wire into ChatService system prompt:
//! let system = format!("{BASE}\n\n{}", memory.instructions());
//! ```

use anyhow::{Context, Result};
use async_trait::async_trait;
use pond_core::ports::mcp_knowledge::McpKnowledgePort;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Flat-file memory adapter — compatible with Goose's `MemoryServer` format.
#[derive(Clone)]
pub struct GooseMcpMemoryAdapter {
    memory_dir: Arc<PathBuf>,
}

impl GooseMcpMemoryAdapter {
    /// Create (or reuse) the memory store at `memory_dir`.
    ///
    /// The directory is created on first use; it is safe to call `new()` even
    /// if the directory does not yet exist.
    pub fn new(memory_dir: impl Into<PathBuf>) -> Self {
        Self {
            memory_dir: Arc::new(memory_dir.into()),
        }
    }

    fn category_path(&self, category: &str) -> PathBuf {
        // Sanitize: strip path separators so callers can't escape the dir.
        let safe = category.replace(['/', '\\', '.'], "_");
        self.memory_dir.join(format!("{}.txt", safe))
    }

    fn ensure_dir(&self) -> Result<()> {
        std::fs::create_dir_all(self.memory_dir.as_ref())
            .context("failed to create memory directory")
    }

    /// Load all entries from a category file.
    ///
    /// Returns a map of `{ first_line_of_entry → all_lines_of_entry }`.
    fn read_category(path: &Path) -> Result<HashMap<String, Vec<String>>> {
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let content = std::fs::read_to_string(path).context("failed to read memory file")?;

        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        let mut current: Vec<String> = Vec::new();

        for line in content.lines() {
            if line.is_empty() {
                // Blank line = entry separator
                if !current.is_empty() {
                    let key = current[0].clone();
                    map.entry(key).or_default().extend(current.drain(..));
                }
            } else {
                current.push(line.to_string());
            }
        }
        // Flush last entry (file may not end with blank line)
        if !current.is_empty() {
            let key = current[0].clone();
            map.entry(key).or_default().extend(current.drain(..));
        }
        Ok(map)
    }

    /// Write a map of entries back to the category file.
    fn write_category(path: &Path, entries: &HashMap<String, Vec<String>>) -> Result<()> {
        let mut parts: Vec<String> = entries.values().map(|lines| lines.join("\n")).collect();
        parts.sort(); // stable order
        let content = parts.join("\n\n");
        std::fs::write(path, content).context("failed to write memory file")
    }

    /// List all category names (file stems) in the memory directory.
    fn list_categories(&self) -> Result<Vec<String>> {
        if !self.memory_dir.exists() {
            return Ok(vec![]);
        }
        let entries = std::fs::read_dir(self.memory_dir.as_ref())
            .context("failed to read memory directory")?;
        let mut cats = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "txt") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    cats.push(stem.to_string());
                }
            }
        }
        cats.sort();
        Ok(cats)
    }
}

#[async_trait]
impl McpKnowledgePort for GooseMcpMemoryAdapter {
    async fn remember(
        &self,
        category: &str,
        data: &str,
        tags: &[String],
        _global: bool,
    ) -> Result<()> {
        let adapter = self.clone();
        let category = category.to_string();
        let data = data.to_string();
        let tags = tags.to_vec();

        tokio::task::spawn_blocking(move || {
            adapter.ensure_dir()?;
            let path = adapter.category_path(&category);

            let mut entries = Self::read_category(&path)?;

            // Build the entry text: optional #tags + data
            let mut lines = Vec::new();
            for tag in &tags {
                if !tag.is_empty() {
                    lines.push(format!("#{}", tag));
                }
            }
            lines.push(data);

            let key = lines[0].clone();
            entries.insert(key, lines);

            Self::write_category(&path, &entries)
        })
        .await
        .context("memory write task panicked")??;

        Ok(())
    }

    async fn retrieve(
        &self,
        category: &str,
        _global: bool,
    ) -> Result<HashMap<String, Vec<String>>> {
        let adapter = self.clone();
        let category = category.to_string();

        let map = tokio::task::spawn_blocking(move || -> Result<HashMap<String, Vec<String>>> {
            if category == "*" {
                let cats = adapter.list_categories()?;
                let mut all = HashMap::new();
                for cat in cats {
                    let path = adapter.category_path(&cat);
                    let entries = Self::read_category(&path)?;
                    all.extend(entries);
                }
                return Ok(all);
            }
            let path = adapter.category_path(&category);
            Self::read_category(&path)
        })
        .await
        .context("memory read task panicked")??;

        Ok(map)
    }

    async fn remove_category(&self, category: &str, _global: bool) -> Result<()> {
        let adapter = self.clone();
        let category = category.to_string();

        tokio::task::spawn_blocking(move || -> Result<()> {
            if category == "*" {
                let cats = adapter.list_categories()?;
                for cat in cats {
                    let path = adapter.category_path(&cat);
                    if path.exists() {
                        std::fs::remove_file(&path).context("failed to remove memory file")?;
                    }
                }
                return Ok(());
            }
            let path = adapter.category_path(&category);
            if path.exists() {
                std::fs::remove_file(&path).context("failed to remove memory file")?;
            }
            Ok(())
        })
        .await
        .context("memory remove task panicked")??;

        Ok(())
    }

    async fn remove_specific(&self, category: &str, content: &str, _global: bool) -> Result<()> {
        let adapter = self.clone();
        let category = category.to_string();
        let content = content.to_string();

        tokio::task::spawn_blocking(move || -> Result<()> {
            let path = adapter.category_path(&category);
            let mut entries = Self::read_category(&path)?;

            entries.retain(|key, lines| {
                !key.contains(&content) && !lines.iter().any(|l| l.contains(&content))
            });

            Self::write_category(&path, &entries)
        })
        .await
        .context("memory remove-specific task panicked")??;

        Ok(())
    }

    fn instructions(&self) -> String {
        // Synchronous fs I/O — memory files are small and infrequently read.
        let cats = match self.list_categories() {
            Ok(c) => c,
            Err(_) => return String::new(),
        };
        if cats.is_empty() {
            return String::new();
        }

        let mut parts = vec!["## Remembered Context".to_string()];
        for cat in &cats {
            let path = self.category_path(cat);
            match Self::read_category(&path) {
                Ok(entries) if !entries.is_empty() => {
                    parts.push(format!("### {}", cat));
                    for lines in entries.values() {
                        for line in lines {
                            if !line.starts_with('#') {
                                // Skip tag lines, only show content
                                parts.push(format!("- {}", line));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        parts.join("\n")
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_adapter(tmp: &TempDir) -> GooseMcpMemoryAdapter {
        GooseMcpMemoryAdapter::new(tmp.path().join("memory"))
    }

    #[tokio::test]
    async fn remember_and_retrieve_round_trip() {
        let tmp = TempDir::new().unwrap();
        let adapter = make_adapter(&tmp);

        adapter
            .remember("devices", "living room TV is a Sony 65\"", &[], false)
            .await
            .unwrap();

        let result = adapter.retrieve("devices", false).await.unwrap();
        assert!(!result.is_empty());
        let all_text: String = result
            .values()
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(all_text.contains("Sony"));
    }

    #[tokio::test]
    async fn retrieve_all_with_wildcard() {
        let tmp = TempDir::new().unwrap();
        let adapter = make_adapter(&tmp);

        adapter.remember("devices", "TV", &[], false).await.unwrap();
        adapter
            .remember("preferences", "dark mode", &[], false)
            .await
            .unwrap();

        let all = adapter.retrieve("*", false).await.unwrap();
        let all_text: String = all
            .values()
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(all_text.contains("TV"));
        assert!(all_text.contains("dark mode"));
    }

    #[tokio::test]
    async fn remove_category_clears_file() {
        let tmp = TempDir::new().unwrap();
        let adapter = make_adapter(&tmp);

        adapter.remember("temp", "data", &[], false).await.unwrap();
        adapter.remove_category("temp", false).await.unwrap();

        let result = adapter.retrieve("temp", false).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn remove_specific_removes_matching_entry() {
        let tmp = TempDir::new().unwrap();
        let adapter = make_adapter(&tmp);

        adapter
            .remember("facts", "user likes coffee", &[], false)
            .await
            .unwrap();
        adapter
            .remember("facts", "user has a dog named Max", &[], false)
            .await
            .unwrap();
        adapter
            .remove_specific("facts", "coffee", false)
            .await
            .unwrap();

        let result = adapter.retrieve("facts", false).await.unwrap();
        let all_text: String = result
            .values()
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!all_text.contains("coffee"));
        assert!(all_text.contains("Max"));
    }

    #[tokio::test]
    async fn instructions_reflects_stored_memories() {
        let tmp = TempDir::new().unwrap();
        let adapter = make_adapter(&tmp);

        assert!(
            adapter.instructions().is_empty(),
            "no memories → empty instructions"
        );

        adapter
            .remember("preferences", "prefers Celsius", &[], false)
            .await
            .unwrap();
        let instr = adapter.instructions();
        assert!(instr.contains("Remembered Context"));
        assert!(instr.contains("Celsius"));
    }

    #[tokio::test]
    async fn tags_are_stored_but_not_shown_in_instructions() {
        let tmp = TempDir::new().unwrap();
        let adapter = make_adapter(&tmp);

        adapter
            .remember(
                "devices",
                "smart bulb in kitchen",
                &["iot".to_string()],
                false,
            )
            .await
            .unwrap();

        let instr = adapter.instructions();
        assert!(instr.contains("smart bulb"));
        assert!(
            !instr.contains("#iot"),
            "tag lines should not appear in instructions"
        );
    }
}
