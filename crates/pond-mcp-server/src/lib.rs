pub mod giap_server;
pub mod registry;

pub use giap_server::GiapMcpServer;
pub use registry::{init_giap_services, set_last_user_message, spawn_giap_server, GiapServiceHandles};
