//! Which routes and extensions each listener serves, kept apart from `serve()` so the
//! composition can be tested without starting the server.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{Extension, Router};
use pond_api::network::CompanionTransport;
use pond_api::AppState;

#[cfg(unix)]
use crate::embedded_network::{self, Runtime};
#[cfg(unix)]
use pond_core::security::ports::remote_access::{DevicePresence, RemoteRevocation};

/// The routers for every listener `serve()` binds.
pub struct Listeners {
    /// Plain HTTP on `127.0.0.1`: the dashboard, its API, and remote-access management.
    pub dashboard: Router,
    /// HTTPS on every interface: the companion API, without desktop assets.
    pub companion: Router,
    /// The private Unix socket the embedded node forwards tailnet requests to.
    #[cfg(unix)]
    pub embedded: Router,
}

/// Build the routers for every listener from the shared application state.
pub fn compose(
    state: Arc<AppState>,
    static_dir: PathBuf,
    transport: CompanionTransport,
    #[cfg(unix)] embedded: Arc<Runtime>,
) -> Listeners {
    let companion =
        pond_api::build_companion_router(state.clone()).layer(Extension(transport.clone()));
    let dashboard = pond_api::build_router(state.clone(), static_dir).layer(Extension(transport));
    #[cfg(unix)]
    let companion = companion
        .layer(Extension(embedded.clone() as Arc<dyn RemoteRevocation>))
        .layer(Extension(embedded.address.clone()))
        .merge(embedded_network::companion_management(
            embedded.clone(),
            state,
        ));
    // Taken before presence is added: a request over the tailnet must never count as the
    // phone being at home. The middleware's LAN check says the same; this makes it structural.
    #[cfg(unix)]
    let tailnet = embedded_network::private_companion(companion.clone());
    // Phones reach the Pond over HTTPS, so that is where a LAN sighting renews remote access.
    #[cfg(unix)]
    let companion = companion.layer(Extension(embedded.clone() as Arc<dyn DevicePresence>));
    #[cfg(unix)]
    let dashboard = dashboard
        .layer(Extension(embedded.clone() as Arc<dyn RemoteRevocation>))
        .layer(Extension(embedded.address.clone()))
        .merge(embedded_network::management(embedded.clone()));
    Listeners {
        dashboard,
        companion,
        #[cfg(unix)]
        embedded: tailnet,
    }
}
