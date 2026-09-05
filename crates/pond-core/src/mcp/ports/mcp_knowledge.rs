//! McpKnowledgePort — structured persistent knowledge store for the assistant, backed by Goose's
//! `MemoryServer` (flat-file MCP storage). The `instructions()` output is prepended to the LLM
//! system prompt. The adapter is `pond-adapters-mcp-memory` (workspace-excluded); every
//! `MemoryServer` call is synchronous fs I/O, so wrap each one in `tokio::task::spawn_blocking`.

use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;

#[async_trait]
pub trait McpKnowledgePort: Send + Sync {
    /// Store `data` under `category` with optional `tags`.
    /// `global = true` stores in the shared memory dir; `false` is session-local.
    async fn remember(
        &self,
        category: &str,
        data: &str,
        tags: &[String],
        global: bool,
    ) -> Result<()>;

    /// Retrieve all entries for `category`.  Returns a map of
    /// `{ entry_key → [lines] }`.  Use `"*"` as category to get everything.
    async fn retrieve(&self, category: &str, global: bool) -> Result<HashMap<String, Vec<String>>>;

    /// Delete every entry under `category`.
    async fn remove_category(&self, category: &str, global: bool) -> Result<()>;

    /// Delete a single entry matching `content` within `category`.
    async fn remove_specific(&self, category: &str, content: &str, global: bool) -> Result<()>;

    /// Returns the full context string that should be prepended to the LLM
    /// system prompt.  Generated from all stored memories.
    fn instructions(&self) -> String;
}
