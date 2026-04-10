use anyhow::{anyhow, Result};
use async_trait::async_trait;
use goose::agents::{Agent as GooseAgent, ExtensionConfig};
use goose::agents::extension::Envs;
use pond_core::ports::extension_manager::{
    AddExtensionRequest, ExtensionInfo, ExtensionManagerPort,
};
use std::sync::Arc;

pub struct GiapGooseExtensionManager {
    agent: Arc<GooseAgent>,
    session_id: String,
}

impl GiapGooseExtensionManager {
    pub fn new(agent: Arc<GooseAgent>, session_id: String) -> Self {
        Self { agent, session_id }
    }
}

#[async_trait]
impl ExtensionManagerPort for GiapGooseExtensionManager {
    async fn list_extensions(&self) -> Result<Vec<ExtensionInfo>> {
        let tools = self.agent.list_tools(&self.session_id, None).await;

        // Group tools by extension name prefix (format: "ext_name__tool_name")
        let mut ext_map: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for tool in &tools {
            let name = tool.name.as_ref();
            if let Some(sep) = name.find("__") {
                let ext_name = &name[..sep];
                let tool_name = &name[sep + 2..];
                ext_map
                    .entry(ext_name.to_string())
                    .or_default()
                    .push(tool_name.to_string());
            } else {
                ext_map
                    .entry("default".to_string())
                    .or_default()
                    .push(name.to_string());
            }
        }

        Ok(ext_map
            .into_iter()
            .map(|(name, tools)| ExtensionInfo {
                kind: "builtin".to_string(),
                description: String::new(),
                name,
                tools,
            })
            .collect())
    }

    async fn add_extension(&self, request: AddExtensionRequest) -> Result<ExtensionInfo> {
        let config = match request.kind.as_str() {
            "builtin" => ExtensionConfig::Builtin {
                name: request.name.clone(),
                description: request.description.clone(),
                display_name: None,
                timeout: None,
                bundled: Some(false),
                available_tools: vec![],
            },
            "stdio" => {
                let cmd = request
                    .command
                    .ok_or_else(|| anyhow!("stdio extension requires 'command'"))?;
                let mut env_map = std::collections::HashMap::new();
                for (k, v) in &request.env {
                    env_map.insert(k.clone(), v.clone());
                }
                ExtensionConfig::Stdio {
                    name: request.name.clone(),
                    description: request.description.clone(),
                    cmd,
                    args: request.args,
                    envs: Envs::new(env_map),
                    env_keys: vec![],
                    timeout: None,
                    bundled: None,
                    available_tools: vec![],
                }
            }
            "http" | "streamable_http" => {
                let uri = request
                    .uri
                    .ok_or_else(|| anyhow!("http extension requires 'uri'"))?;
                ExtensionConfig::StreamableHttp {
                    name: request.name.clone(),
                    description: request.description.clone(),
                    uri,
                    envs: Envs::default(),
                    env_keys: vec![],
                    headers: std::collections::HashMap::new(),
                    timeout: None,
                    bundled: None,
                    available_tools: vec![],
                }
            }
            other => return Err(anyhow!("Unknown extension kind: {}", other)),
        };

        self.agent
            .add_extension(config, &self.session_id)
            .await
            .map_err(|e| anyhow!("Failed to add extension: {}", e))?;

        Ok(ExtensionInfo {
            name: request.name,
            kind: request.kind,
            description: request.description,
            tools: vec![],
        })
    }

    async fn remove_extension(&self, name: &str) -> Result<()> {
        self.agent
            .remove_extension(name, &self.session_id)
            .await
            .map_err(|e| anyhow!("Failed to remove extension: {}", e))
    }

    async fn list_tools(&self) -> Result<Vec<String>> {
        let tools = self.agent.list_tools(&self.session_id, None).await;
        Ok(tools.iter().map(|t| t.name.as_ref().to_string()).collect())
    }
}
