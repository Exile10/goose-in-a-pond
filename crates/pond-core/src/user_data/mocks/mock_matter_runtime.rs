//! Mock for `MatterRuntimePort` — a runtime pinned to a chosen state, which
//! records the reconcile requests it is given. Lets `pond-api` tests assert
//! that each state produces its own error, and that saving settings actually
//! asks the runtime to converge.

use crate::user_data::ports::device_commissioning::{
    CommissionedDevice, DeviceCommissioningPort, SetupCode,
};
use crate::user_data::ports::matter_runtime::{MatterRuntimePort, MatterState, MatterStatus};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

/// A commissioner that always succeeds, returning a device derived from the
/// code it was given. Paired with `StubMatterRuntime` in the connected state.
pub struct StubCommissioner {
    /// Node id handed back for every commission call.
    pub node_id: u64,
}

#[async_trait]
impl DeviceCommissioningPort for StubCommissioner {
    async fn commission(
        &self,
        _code: SetupCode,
        name: Option<String>,
    ) -> Result<CommissionedDevice> {
        Ok(CommissionedDevice {
            device_id: format!("matter-{}", self.node_id),
            name: name.unwrap_or_else(|| "Light 1".into()),
            node_id: self.node_id,
            device_type: "light".into(),
            capabilities: vec!["power".into()],
        })
    }

    async fn decommission(&self, _node_id: u64) -> Result<()> {
        Ok(())
    }
}

/// A runtime fixed in one state. `apply` does not change the state (tests set
/// it explicitly); it only records what was asked for.
pub struct StubMatterRuntime {
    status: Mutex<MatterStatus>,
    commissioner: Option<Arc<dyn DeviceCommissioningPort>>,
    applied: Mutex<Vec<(bool, String)>>,
    shutdowns: Mutex<usize>,
}

impl StubMatterRuntime {
    /// Off — nothing enabled, no commissioner.
    pub fn disabled() -> Self {
        Self::in_state(false, MatterState::Disabled, None)
    }

    /// Enabled and connected, with a commissioner that succeeds.
    pub fn connected() -> Self {
        Self::in_state(
            true,
            MatterState::Connected,
            Some(Arc::new(StubCommissioner { node_id: 7 })),
        )
    }

    /// Enabled, still converging.
    pub fn connecting() -> Self {
        Self::in_state(true, MatterState::Connecting, None)
    }

    /// Enabled, but the controller could not be reached.
    pub fn unreachable(error: &str) -> Self {
        Self::in_state(
            true,
            MatterState::Unreachable {
                error: error.to_string(),
            },
            None,
        )
    }

    fn in_state(
        enabled: bool,
        state: MatterState,
        commissioner: Option<Arc<dyn DeviceCommissioningPort>>,
    ) -> Self {
        Self {
            status: Mutex::new(MatterStatus {
                enabled,
                url: "ws://127.0.0.1:5580/ws".into(),
                state,
            }),
            commissioner,
            applied: Mutex::new(Vec::new()),
            shutdowns: Mutex::new(0),
        }
    }

    /// Every `(enabled, url)` pair the runtime was asked to converge to.
    pub fn applied(&self) -> Vec<(bool, String)> {
        self.applied.lock().unwrap().clone()
    }

    /// How many times `shutdown` was called.
    pub fn shutdown_count(&self) -> usize {
        *self.shutdowns.lock().unwrap()
    }
}

#[async_trait]
impl MatterRuntimePort for StubMatterRuntime {
    fn apply(&self, enabled: bool, url: String) {
        self.applied.lock().unwrap().push((enabled, url));
    }

    async fn status(&self) -> MatterStatus {
        self.status.lock().unwrap().clone()
    }

    async fn commissioner(&self) -> Option<Arc<dyn DeviceCommissioningPort>> {
        // Mirrors the real runtime: a commissioner exists only when connected.
        self.status
            .lock()
            .unwrap()
            .state
            .is_connected()
            .then(|| self.commissioner.clone())
            .flatten()
    }

    async fn shutdown(&self) {
        *self.shutdowns.lock().unwrap() += 1;
    }
}
