use serde::{Deserialize, Serialize};

use crate::security::domain::secret::SecretRequirement;

/// A curated MCP extension available for one-click installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceExtension {
    /// Unique identifier (e.g. "filesystem", "github").
    pub id: String,
    /// Display name.
    pub name: String,
    /// Short description of what the extension does.
    pub description: String,
    /// Transport kind: `"stdio"` or `"streamable_http"`.
    pub kind: String,
    /// Stdio: command to run (e.g. `"npx"`).
    #[serde(default)]
    pub command: Option<String>,
    /// Stdio: arguments (e.g. `["-y", "@modelcontextprotocol/server-filesystem", "/"]`).
    #[serde(default)]
    pub args: Vec<String>,
    /// HTTP: URI of the MCP endpoint.
    #[serde(default)]
    pub uri: Option<String>,
    /// Category for filtering (e.g. "productivity", "development", "data").
    pub category: String,
    /// Author/publisher name.
    pub author: String,
    /// List of tool names this extension provides.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Whether to feature this extension prominently.
    #[serde(default)]
    pub featured: bool,
    /// Secrets (API keys, tokens) this extension needs to function.
    #[serde(default)]
    pub required_secrets: Vec<SecretRequirement>,
}
