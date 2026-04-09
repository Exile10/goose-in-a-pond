use std::sync::{Arc, OnceLock};
use pond_adapters_weather::WeatherProvider;
use pond_core::ports::device_registry::DeviceRegistry;
use pond_core::ports::memory_repository::MemoryRepository;
use pond_core::ports::recipe::AgentRecipeRepository;
use pond_core::ports::scheduler::SchedulerPort;
use pond_core::ports::settings::SettingsRepository;
use pond_core::ports::skill::UserSkillRepository;
use crate::giap_server::GiapMcpServer;
use rmcp::ServiceExt;
use tokio::io::DuplexStream;

pub struct GiapServiceHandles {
    pub weather:         Option<Arc<dyn WeatherProvider>>,
    pub device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    pub scheduler:       Option<Arc<dyn SchedulerPort>>,
    pub settings_repo:   Arc<dyn SettingsRepository + Send + Sync>,
    pub memory_repo:     Arc<dyn MemoryRepository + Send + Sync>,
    pub skill_repo:      Arc<dyn UserSkillRepository + Send + Sync>,
    pub recipe_repo:     Arc<dyn AgentRecipeRepository + Send + Sync>,
}

static GIAP_SERVICES: OnceLock<Arc<GiapServiceHandles>> = OnceLock::new();

pub fn init_giap_services(services: Arc<GiapServiceHandles>) {
    let _ = GIAP_SERVICES.set(services);
}

pub fn spawn_giap_server(reader: DuplexStream, writer: DuplexStream) {
    let services = GIAP_SERVICES
        .get()
        .expect("GIAP services not initialized — call init_giap_services() first")
        .clone();
    let server = GiapMcpServer::new(services);
    tokio::spawn(async move {
        if let Ok(running) = server.serve((reader, writer)).await {
            let _ = running.waiting().await;
        }
    });
}
