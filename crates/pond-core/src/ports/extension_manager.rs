use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionInfo {
    pub name: String,
    pub kind: String, // "builtin" | "stdio" | "http"
    pub description: String,
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddExtensionRequest {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub description: String,
    // For stdio extensions:
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    // For http extensions:
    pub uri: Option<String>,
}

#[async_trait]
pub trait ExtensionManagerPort: Send + Sync {
    async fn list_extensions(&self) -> Result<Vec<ExtensionInfo>>;
    async fn add_extension(&self, request: AddExtensionRequest) -> Result<ExtensionInfo>;
    async fn remove_extension(&self, name: &str) -> Result<()>;
    async fn list_tools(&self) -> Result<Vec<String>>;
}
