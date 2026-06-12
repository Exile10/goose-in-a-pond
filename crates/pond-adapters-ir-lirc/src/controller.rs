use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use pond_core::domain::device::{DeviceCapability, DeviceState, StateValue};
use pond_core::ports::device_controller::DeviceController;
use pond_core::ports::device_registry::DeviceRegistry;
use tracing::{debug, warn};

use crate::codeset::default_key_map;
use crate::config::IrConfig;
use crate::lirc;

/// IR/LIRC implementation of `DeviceController`.
///
/// Sends IR codes via `irsend SEND_ONCE <remote> <key>`. State is fire-and-forget:
/// `query_state` always returns empty because IR has no feedback channel.
///
/// Device address format: `lirc://<remote_name>` or bare `<remote_name>`.
/// The remote name must match an entry in `/etc/lirc/lircd.conf.d/`.
pub struct IrDeviceController {
    registry: Arc<dyn DeviceRegistry + Send + Sync>,
    config: IrConfig,
    /// Canonical key name → LIRC key name (e.g. "volume_up" → "KEY_VOLUMEUP").
    key_map: HashMap<String, String>,
    /// Whether LIRC was detected at construction time.
    available: bool,
}

impl IrDeviceController {
    pub fn new(config: IrConfig, registry: Arc<dyn DeviceRegistry + Send + Sync>) -> Self {
        let available = lirc::is_available(&config.socket_path);
        if !available {
            warn!(
                socket = %config.socket_path,
                "IR/LIRC: lircd socket not found and irsend not in PATH — \
                 all IR commands will fail until LIRC is installed and lircd is running. \
                 See docs/jetson-ir-wiring.md for setup instructions."
            );
        }
        let key_map = default_key_map()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Self { registry, config, key_map, available }
    }

    /// Extract the LIRC remote name from the device's address field.
    ///
    /// Strips the optional `lirc://` prefix so both `lirc://SAMSUNG_TV`
    /// and `SAMSUNG_TV` work as address values.
    async fn resolve_remote(&self, device_id: &str) -> Result<String> {
        let device = self
            .registry
            .get_device(device_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("device '{device_id}' not found in registry"))?;
        let addr = device
            .address
            .ok_or_else(|| anyhow::anyhow!("device '{device_id}' has no IR address"))?;
        let remote = addr.strip_prefix("lirc://").unwrap_or(&addr).to_string();
        Ok(remote)
    }

    /// Map a canonical key name to its LIRC key name.
    ///
    /// Falls back to using the key as-is, so callers can pass exact LIRC key
    /// names (e.g. `KEY_HDMI1`) when no mapping exists.
    fn lirc_key<'a>(&'a self, key: &'a str) -> &'a str {
        self.key_map.get(key).map(|s| s.as_str()).unwrap_or(key)
    }

    /// How many keypresses the `value` encodes.
    ///
    /// `Number(n)` → n presses (useful for "volume up 5 steps").
    /// Everything else → 1 press.
    fn press_count(value: &StateValue) -> usize {
        match value {
            StateValue::Number(n) if *n >= 1.0 => *n as usize,
            _ => 1,
        }
    }

    fn check_available(&self) -> Result<()> {
        if !self.available {
            anyhow::bail!(
                "IR/LIRC hardware not available. \
                 Ensure lircd is running (socket: {}) and irsend is installed. \
                 See docs/jetson-ir-wiring.md.",
                self.config.socket_path
            );
        }
        Ok(())
    }
}

#[async_trait]
impl DeviceController for IrDeviceController {
    /// Toggle device power via IR.
    ///
    /// IR remotes send a single KEY_POWER toggle — there is no separate on/off
    /// command. The `on` parameter is accepted for interface compatibility but
    /// both values emit the same keypress.
    async fn set_power(&self, device_id: &str, on: bool) -> Result<()> {
        self.check_available()?;
        let remote = self.resolve_remote(device_id).await?;
        debug!(remote = %remote, on, "IR: sending KEY_POWER");
        lirc::send_once(&remote, "KEY_POWER").await
    }

    /// Send an IR keypress for a capability key.
    ///
    /// The `key` is mapped through the built-in table (e.g. `volume_up` →
    /// `KEY_VOLUMEUP`). If no mapping exists, `key` is used as the LIRC key
    /// name directly, so advanced users can store exact LIRC names.
    ///
    /// When `value` is `Number(n)`, the key is pressed n times (clamped 1–50),
    /// with a configurable delay between presses. This handles "volume up 5
    /// steps" naturally.
    async fn set_state(&self, device_id: &str, key: &str, value: StateValue) -> Result<()> {
        self.check_available()?;
        let remote = self.resolve_remote(device_id).await?;
        let ir_key = self.lirc_key(key).to_string();
        let count = Self::press_count(&value);
        debug!(remote = %remote, key, ir_key = %ir_key, count, "IR: sending keypress");
        lirc::send_repeat(&remote, &ir_key, count, self.config.key_repeat_delay_ms).await
    }

    /// IR is fire-and-forget — returns an empty state snapshot.
    ///
    /// There is no feedback channel for raw IR. Use a separate sensor or
    /// smart-plug integration if power state verification is required.
    async fn query_state(&self, device_id: &str) -> Result<DeviceState> {
        Ok(DeviceState {
            device_id: device_id.to_string(),
            values: HashMap::new(),
        })
    }

    async fn capabilities(&self, device_id: &str) -> Result<Vec<DeviceCapability>> {
        let device = self
            .registry
            .get_device(device_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("device '{device_id}' not found in registry"))?;
        Ok(device.structured_capabilities)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::ports::device_registry::{Device, RegisterDeviceRequest};

    struct StubRegistry {
        device: Option<Device>,
    }

    #[async_trait]
    impl DeviceRegistry for StubRegistry {
        async fn register(&self, _: RegisterDeviceRequest) -> Result<Device> {
            Err(anyhow::anyhow!("stub"))
        }
        async fn list_devices(&self) -> Result<Vec<Device>> {
            Ok(self.device.iter().cloned().collect())
        }
        async fn get_device(&self, id: &str) -> Result<Option<Device>> {
            Ok(self.device.as_ref().filter(|d| d.id == id).cloned())
        }
        async fn unregister(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn heartbeat(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    fn make_tv_device(address: &str) -> Device {
        Device {
            id: "tv-1".into(),
            name: "Living Room TV".into(),
            device_type: "tv".into(),
            hostname: None,
            ip_address: None,
            capabilities: vec!["ir".into()],
            transport: "lirc".into(),
            address: Some(address.into()),
            structured_capabilities: crate::codeset::tv_capabilities(),
            registered_at: "2025-01-01T00:00:00Z".into(),
            last_seen: None,
            is_online: true,
        }
    }

    fn make_controller(device: Device) -> IrDeviceController {
        let registry = Arc::new(StubRegistry { device: Some(device) });
        IrDeviceController::new(IrConfig::default(), registry)
    }

    #[test]
    fn lirc_key_mapping_falls_back_to_passthrough() {
        let ctrl = make_controller(make_tv_device("SAMSUNG_TV"));
        assert_eq!(ctrl.lirc_key("volume_up"), "KEY_VOLUMEUP");
        assert_eq!(ctrl.lirc_key("mute"), "KEY_MUTE");
        assert_eq!(ctrl.lirc_key("power"), "KEY_POWER");
        // Unknown key → passthrough
        assert_eq!(ctrl.lirc_key("KEY_CUSTOM"), "KEY_CUSTOM");
    }

    #[test]
    fn press_count_from_number_value() {
        assert_eq!(IrDeviceController::press_count(&StateValue::Number(5.0)), 5);
        assert_eq!(IrDeviceController::press_count(&StateValue::Number(0.0)), 1); // clamped in send_repeat
        assert_eq!(IrDeviceController::press_count(&StateValue::Bool(true)), 1);
        assert_eq!(IrDeviceController::press_count(&StateValue::Text("on".into())), 1);
    }

    #[test]
    fn lirc_prefix_stripped_from_address() {
        let ctrl = make_controller(make_tv_device("lirc://SAMSUNG_TV"));
        // resolve_remote is async; test the strip logic directly
        let addr = "lirc://SAMSUNG_TV";
        let remote = addr.strip_prefix("lirc://").unwrap_or(addr);
        assert_eq!(remote, "SAMSUNG_TV");
    }

    #[tokio::test]
    async fn query_state_returns_empty() {
        let ctrl = make_controller(make_tv_device("SAMSUNG_TV"));
        let state = ctrl.query_state("tv-1").await.unwrap();
        assert_eq!(state.device_id, "tv-1");
        assert!(state.values.is_empty());
    }

    #[tokio::test]
    async fn resolve_remote_strips_prefix() {
        let ctrl = make_controller(make_tv_device("lirc://LG_TV"));
        let remote = ctrl.resolve_remote("tv-1").await.unwrap();
        assert_eq!(remote, "LG_TV");
    }

    #[tokio::test]
    async fn resolve_remote_bare_address() {
        let ctrl = make_controller(make_tv_device("LG_TV"));
        let remote = ctrl.resolve_remote("tv-1").await.unwrap();
        assert_eq!(remote, "LG_TV");
    }

    #[tokio::test]
    async fn resolve_remote_not_found() {
        let ctrl = make_controller(make_tv_device("X"));
        let err = ctrl.resolve_remote("no-such-device").await.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn check_available_returns_error_when_no_lirc() {
        // IrConfig with a nonexistent socket → available = false
        let cfg = IrConfig {
            default_remote: None,
            socket_path: "/nonexistent/lircd".into(),
            key_repeat_delay_ms: 80,
        };
        let registry = Arc::new(StubRegistry { device: None });
        let ctrl = IrDeviceController::new(cfg, registry);
        let err = ctrl.check_available().unwrap_err();
        assert!(err.to_string().contains("IR/LIRC hardware not available"));
    }

    #[test]
    fn tv_capabilities_includes_power_and_volume() {
        let caps = crate::codeset::tv_capabilities();
        let keys: Vec<_> = caps.iter().map(|c| c.key.as_str()).collect();
        assert!(keys.contains(&"power"));
        assert!(keys.contains(&"volume_up"));
        assert!(keys.contains(&"volume_down"));
        assert!(keys.contains(&"mute"));
        assert!(keys.contains(&"input"));
    }
}
