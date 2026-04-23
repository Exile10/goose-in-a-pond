use anyhow::{anyhow, Result};
use async_trait::async_trait;
use goose::agents::{Agent as GooseAgent, ExtensionConfig};
use goose::agents::extension::Envs;
use pond_core::ports::extension_manager::{
    AddExtensionRequest, ExtensionInfo, ExtensionManagerPort,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct GiapGooseExtensionManager {
    agent: Arc<GooseAgent>,
    session_id: String,
    /// Names of extensions that have been disabled by the user.
    /// Kept in memory; persists until server restart.
    disabled: Arc<RwLock<HashSet<String>>>,
    /// Stored configurations for re-enabling extensions.
    extension_configs: Arc<RwLock<HashMap<String, ExtensionConfig>>>,
}

impl GiapGooseExtensionManager {
    pub fn new(agent: Arc<GooseAgent>, session_id: String) -> Self {
        Self {
            agent,
            session_id,
            disabled: Arc::new(RwLock::new(HashSet::new())),
            extension_configs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn register_config(&self, name: String, config: ExtensionConfig) {
        self.extension_configs.write().await.insert(name, config);
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

        let disabled = self.disabled.read().await;
        Ok(ext_map
            .into_iter()
            .map(|(name, tools)| ExtensionInfo {
                kind: "builtin".to_string(),
                description: String::new(),
                enabled: !disabled.contains(&name),
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

        // Store config for possible re-enabling later
        self.extension_configs
            .write()
            .await
            .insert(request.name.clone(), config.clone());

        self.agent
            .add_extension(config, &self.session_id)
            .await
            .map_err(|e| anyhow!("Failed to add extension: {}", e))?;

        Ok(ExtensionInfo {
            name: request.name,
            kind: request.kind,
            description: request.description,
            tools: vec![],
            enabled: true,
        })
    }

    async fn remove_extension(&self, name: &str) -> Result<()> {
        self.extension_configs.write().await.remove(name);
        self.disabled.write().await.remove(name);
        self.agent
            .remove_extension(name, &self.session_id)
            .await
            .map_err(|e| anyhow!("Failed to remove extension: {}", e))
    }

    async fn list_tools(&self) -> Result<Vec<String>> {
        let tools = self.agent.list_tools(&self.session_id, None).await;
        Ok(tools.iter().map(|t| t.name.as_ref().to_string()).collect())
    }

    async fn set_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        let mut disabled = self.disabled.write().await;
        if enabled {
            if disabled.contains(name) {
                // Re-enable: re-add to Goose agent
                let configs = self.extension_configs.read().await;
                if let Some(config) = configs.get(name) {
                    self.agent
                        .add_extension(config.clone(), &self.session_id)
                        .await
                        .map_err(|e| anyhow!("Failed to re-enable extension: {}", e))?;
                }
                disabled.remove(name);
            }
        } else {
            if !disabled.contains(name) {
                // Disable: remove from Goose agent
                self.agent
                    .remove_extension(name, &self.session_id)
                    .await
                    .map_err(|e| anyhow!("Failed to disable extension: {}", e))?;
                disabled.insert(name.to_string());
            }
        }
        Ok(())
    }
}
