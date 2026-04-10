pub mod giap_server;
pub mod registry;

pub use giap_server::GiapMcpServer;
pub use registry::{init_giap_services, spawn_giap_server, GiapServiceHandles};
