//! Device MCP Server — devices, user profile, model config, skills, recipes.
//!
//! Provides 5 tools: `list_registered_devices`, `get_user_profile`,
//! `get_model_assignments`, `list_skills`, `get_recipe`.
//! Depends on [`DeviceRegistry`], [`SettingsRepository`],
//! [`UserSkillRepository`], and [`AgentRecipeRepository`].

use pond_core::user_data::ports::device_registry::DeviceRegistry;
use pond_core::user_data::ports::recipe::AgentRecipeRepository;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::ports::skill::UserSkillRepository;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, ErrorCode, ErrorData, Implementation, InitializeResult,
        ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

// ── Parameter structs ──────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetRecipeParams {
    /// Recipe name (slug).
    pub name: String,
}

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct DeviceMcpServer {
    device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    skill_repo: Arc<dyn UserSkillRepository + Send + Sync>,
    recipe_repo: Arc<dyn AgentRecipeRepository + Send + Sync>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl DeviceMcpServer {
    pub fn new(
        device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
        settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
        skill_repo: Arc<dyn UserSkillRepository + Send + Sync>,
        recipe_repo: Arc<dyn AgentRecipeRepository + Send + Sync>,
    ) -> Self {
        Self {
            device_registry,
            settings_repo,
            skill_repo,
            recipe_repo,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "List registered devices with online status.")]
    async fn list_registered_devices(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.device_registry.list_devices().await {
            Ok(devices) => {
                let text = if devices.is_empty() {
                    "No devices registered.".to_string()
                } else {
                    devices
                        .iter()
                        .map(|d| {
                            format!(
                                "- {} ({}): {}",
                                d.name,
                                d.device_type,
                                if d.is_online { "online" } else { "offline" }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };

                if !devices.is_empty() {
                    let ui_devices: Vec<serde_json::Value> = devices
                        .iter()
                        .map(|d| {
                            serde_json::json!({
                                "name": d.name,
                                "is_online": d.is_online,
                                "device_type": d.device_type,
                                "room": d.room.clone().unwrap_or_default(),
                            })
                        })
                        .collect();
                    let ui_data = serde_json::json!({ "devices": ui_devices });
                    let hint = format!("[[[mcp-ui:devices:{}]]]\n", ui_data);
                    let full_result = format!("{}{}", hint, text);
                    return Ok(CallToolResult::success(vec![Content::text(full_result)]));
                }

                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
            Err(e) => Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Error listing devices: {}", e),
                None,
            )),
        }
    }

    #[tool(description = "Get user profile: name, assistant name, timezone, location.")]
    async fn get_user_profile(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let settings = self.settings_repo.get().await.map_err(|e| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Settings error: {}", e),
                None,
            )
        })?;
        let location = if settings.weather_location_name.is_empty() {
            "not configured".to_string()
        } else {
            settings.weather_location_name.clone()
        };
        let text = format!(
            "User: {}\nAssistant name: {}\nTimezone: {}\nLocation: {}",
            settings.user_name, settings.assistant_name, settings.timezone, location,
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Get active model config: main LLM and tool-calling model.")]
    async fn get_model_assignments(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let s = self.settings_repo.get().await.map_err(|e| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Settings error: {}", e),
                None,
            )
        })?;
        let tool = s.tool_model.as_deref().unwrap_or("(none)");
        let text = format!(
            "Main LLM:    {}/{}\nTool caller: {}",
            s.chat_provider, s.chat_model, tool,
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "List active user skills.")]
    async fn list_skills(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let skills = self.skill_repo.list_active().await.map_err(|e| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Skills error: {}", e),
                None,
            )
        })?;
        let text = if skills.is_empty() {
            "No active skills.".to_string()
        } else {
            skills
                .iter()
                .map(|s| format!("- {} ({})", s.name, s.id))
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Get a named agent recipe's YAML.")]
    async fn get_recipe(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<GetRecipeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let recipe = self
            .recipe_repo
            .get_by_name(&params.0.name)
            .await
            .map_err(|e| {
                ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Recipe error: {}", e),
                    None,
                )
            })?;
        match recipe {
            None => Ok(CallToolResult::success(vec![Content::text(format!(
                "Recipe '{}' not found.",
                params.0.name
            ))])),
            Some(r) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Recipe: {}\n{}\n\n{}",
                r.name, r.description, r.yaml
            ))])),
        }
    }
}

#[tool_handler]
impl ServerHandler for DeviceMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-device",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Device MCP server — device registry, user profile, model configuration, \
                 skills, and agent recipes.\n\n\
                 Tools: list_registered_devices, get_user_profile (name/timezone/location), \
                 get_model_assignments (active LLM config), list_skills (active user skills), \
                 get_recipe (YAML agent recipe by name).",
            )
    }
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use rmcp::ServiceExt;
use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct DeviceDeps {
    device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    skill_repo: Arc<dyn UserSkillRepository + Send + Sync>,
    recipe_repo: Arc<dyn AgentRecipeRepository + Send + Sync>,
}

static DEVICE_DEPS: OnceLock<DeviceDeps> = OnceLock::new();

/// Initialize device server dependencies. Call once at startup.
pub fn init_device_deps(
    device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    skill_repo: Arc<dyn UserSkillRepository + Send + Sync>,
    recipe_repo: Arc<dyn AgentRecipeRepository + Send + Sync>,
) {
    let _ = DEVICE_DEPS.set(DeviceDeps {
        device_registry,
        settings_repo,
        skill_repo,
        recipe_repo,
    });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_device_server(reader: DuplexStream, writer: DuplexStream) {
    let deps = DEVICE_DEPS.get().expect("init_device_deps() not called");
    let server = DeviceMcpServer::new(
        deps.device_registry.clone(),
        deps.settings_repo.clone(),
        deps.skill_repo.clone(),
        deps.recipe_repo.clone(),
    );
    tokio::spawn(async move {
        match server.serve((reader, writer)).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(e) => tracing::error!("giap-device MCP server failed: {e}"),
        }
    });
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use pond_core::user_data::domain::recipe::AgentRecipe;
    use pond_core::user_data::domain::settings::Settings;
    use pond_core::user_data::domain::skill::UserSkill;
    use pond_core::user_data::ports::device_registry::{Device, RegisterDeviceRequest};

    struct StubDeviceRegistry;
    #[async_trait]
    impl DeviceRegistry for StubDeviceRegistry {
        async fn register(&self, _: RegisterDeviceRequest) -> anyhow::Result<Device> {
            unimplemented!()
        }
        async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
            Ok(vec![])
        }
        async fn get_device(&self, _: &str) -> anyhow::Result<Option<Device>> {
            Ok(None)
        }
        async fn unregister(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn heartbeat(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct StubSettings;
    #[async_trait]
    impl SettingsRepository for StubSettings {
        async fn get(&self) -> anyhow::Result<Settings> {
            Ok(Settings::default())
        }
        async fn update(&self, _: &Settings) -> anyhow::Result<()> {
            Ok(())
        }
        async fn get_key(&self, _: &str) -> anyhow::Result<Option<String>> {
            Ok(None)
        }
        async fn set_key(&self, _: &str, _: String) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct StubSkills;
    #[async_trait]
    impl UserSkillRepository for StubSkills {
        async fn list_active(&self) -> anyhow::Result<Vec<UserSkill>> {
            Ok(vec![])
        }
        async fn list_all(&self) -> anyhow::Result<Vec<UserSkill>> {
            Ok(vec![])
        }
        async fn get(&self, _: &str) -> anyhow::Result<Option<UserSkill>> {
            Ok(None)
        }
        async fn create(&self, _: &UserSkill) -> anyhow::Result<()> {
            Ok(())
        }
        async fn update(&self, _: &UserSkill) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct StubRecipes;
    #[async_trait]
    impl AgentRecipeRepository for StubRecipes {
        async fn list(&self) -> anyhow::Result<Vec<AgentRecipe>> {
            Ok(vec![])
        }
        async fn get_by_name(&self, _: &str) -> anyhow::Result<Option<AgentRecipe>> {
            Ok(None)
        }
        async fn get_by_id(&self, _: &str) -> anyhow::Result<Option<AgentRecipe>> {
            Ok(None)
        }
        async fn upsert(&self, _: &AgentRecipe) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn test_server() -> DeviceMcpServer {
        DeviceMcpServer::new(
            Arc::new(StubDeviceRegistry),
            Arc::new(StubSettings),
            Arc::new(StubSkills),
            Arc::new(StubRecipes),
        )
    }

    #[test]
    fn server_constructs() {
        let _server = test_server();
    }
}
