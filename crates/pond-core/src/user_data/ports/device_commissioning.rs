//! Driven Port: Device Commissioning
//!
//! Adding a Matter device is not "fill in a form" — the device is not GIAP's to
//! name until it has been *commissioned* onto the local fabric, after which the
//! controller reports it and the bridge registers it automatically. So the
//! enrollment input is a **setup code**, not a device description, and this port
//! is the seam for it. Non-Matter devices keep using the plain registry
//! (`DeviceRegistry::register`), which is a catalogue entry and nothing more.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// What a device printed on its label (or an app shows) resolves to.
///
/// Real Matter products ship an 11-digit manual pairing code and/or a `MT:` QR
/// payload. Development devices — notably Google's Matter Virtual Device — show
/// only the 8-digit setup passcode, which is commissioned by on-network
/// discovery instead. Both are legitimate ways in, so both are accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetupCode {
    /// `MT:` QR payload, or an 11/21-digit manual pairing code.
    PairingCode(String),
    /// 8-digit setup passcode; the device is found on the network.
    Passcode(u32),
}

/// A device that has just joined the fabric. Carries enough to register the
/// device deterministically from the commission response, without waiting for
/// the bridge's asynchronous discovery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommissionedDevice {
    /// GIAP device id (`matter-<node_id>`) — the same id the bridge registers.
    pub device_id: String,
    pub name: String,
    pub node_id: u64,
    pub device_type: String,
    pub capabilities: Vec<String>,
}

/// Parse and validate what the user typed.
///
/// Validation is not cosmetic: the value is forwarded to the controller, so
/// anything that is not a recognised code shape is rejected here rather than
/// passed through. Spaces and dashes are stripped so a code copied off a label
/// works as printed.
pub fn parse_setup_code(raw: &str) -> Result<SetupCode> {
    // Whitespace is never meaningful; dashes are, inside a QR payload (the
    // base-38 alphabet includes '-'), so they are only stripped from the
    // numeric forms below.
    let unspaced: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    if unspaced.is_empty() {
        return Err(anyhow!("enter the device's setup code"));
    }

    // QR payload. Accept the base-38 character set and let the controller do
    // the final decode.
    if let Some(payload) = unspaced.strip_prefix("MT:") {
        if payload.is_empty()
            || !payload
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        {
            return Err(anyhow!("that QR payload is not a valid Matter code"));
        }
        return Ok(SetupCode::PairingCode(unspaced));
    }

    let cleaned: String = unspaced.chars().filter(|c| *c != '-').collect();
    if !cleaned.chars().all(|c| c.is_ascii_digit()) {
        return Err(anyhow!(
            "setup codes are digits, or a QR payload starting with \"MT:\""
        ));
    }

    match cleaned.len() {
        // Manual pairing code (short or long form).
        11 | 21 => Ok(SetupCode::PairingCode(cleaned)),
        // Setup passcode, e.g. the Matter Virtual Device's 20202021.
        8 => {
            let pin = cleaned
                .parse::<u32>()
                .map_err(|_| anyhow!("that passcode is out of range"))?;
            Ok(SetupCode::Passcode(pin))
        }
        n => Err(anyhow!(
            "expected an 8-digit passcode or an 11-digit pairing code, got {n} digits"
        )),
    }
}

/// The Matter node id behind a `matter-<node_id>` device id, or `None` for any
/// other id. Lets the generic delete path tell a Matter device (which must be
/// decommissioned from the fabric) from a plain catalogue entry, without
/// depending on the Matter adapter.
pub fn matter_node_id(device_id: &str) -> Option<u64> {
    device_id.strip_prefix("matter-")?.parse().ok()
}

/// Why this Pond cannot talk to a Matter controller.
///
/// Four very different problems used to surface as one sentence telling the user
/// to "turn it on in Settings", which is unactionable when the setting is
/// already on and the controller is simply down — or when the build has no
/// Matter support at all. Each variant carries its own next step instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatterUnavailable {
    /// `matter_enabled` is off.
    Disabled,
    /// Enabled, but `matter_ws_url` is blank.
    NoUrl,
    /// Enabled and addressed, but the controller did not answer at startup.
    Unreachable { url: String },
    /// Built without the `goose-agent` feature, so there is no Matter adapter.
    Unsupported,
}

impl MatterUnavailable {
    /// The cause and its remedy, as one sentence for the user.
    ///
    /// The three recoverable variants name the restart explicitly: the
    /// controller connection is established once at startup, so flipping the
    /// setting alone changes nothing until the Pond is restarted.
    pub fn reason(&self) -> String {
        match self {
            Self::Disabled => "Matter is turned off. Turn it on under Settings > Extensions \
                 > Matter, then restart the Pond so it connects to the controller."
                .to_string(),
            Self::NoUrl => "Matter is on but no controller address is set. Set one under \
                 Settings > Extensions > Matter, then restart the Pond."
                .to_string(),
            Self::Unreachable { url } => format!(
                "Matter is on, but the controller at {url} did not answer when the Pond \
                 started. Check that it is running and reachable, then restart the Pond."
            ),
            Self::Unsupported => "This build has no Matter support: it was compiled without \
                 the `goose-agent` feature."
                .to_string(),
        }
    }
}

/// Whether this Pond can commission Matter devices, and if not, why.
///
/// Decided once at startup, when the controller connection is attempted. The
/// commissioner and the reason live in the same enum so the two cannot drift out
/// of sync — there is no way to hold a commissioner and still report a cause, or
/// to lose the commissioner without recording one.
#[derive(Clone)]
pub enum MatterAvailability {
    Ready(Arc<dyn DeviceCommissioningPort>),
    Unavailable(MatterUnavailable),
}

impl MatterAvailability {
    /// Matter off — the state of any Pond that has not enabled it.
    pub fn off() -> Self {
        Self::Unavailable(MatterUnavailable::Disabled)
    }

    /// The commissioner, or the reason there isn't one.
    pub fn ready(
        &self,
    ) -> std::result::Result<&Arc<dyn DeviceCommissioningPort>, &MatterUnavailable> {
        match self {
            Self::Ready(c) => Ok(c),
            Self::Unavailable(why) => Err(why),
        }
    }
}

/// Driven Port: bring a device onto — and off — the local fabric.
#[async_trait]
pub trait DeviceCommissioningPort: Send + Sync {
    /// Commission a device using its setup code. Slow by nature — pairing
    /// involves discovery, attestation, and fabric join.
    ///
    /// When `name` is `Some`, it is written to the device's NodeLabel attribute
    /// so the name lives on the device itself (durable across re-registration
    /// and visible to any controller), and the returned device carries it.
    async fn commission(&self, code: SetupCode, name: Option<String>)
        -> Result<CommissionedDevice>;

    /// Remove a node from the fabric. Deleting a Matter device must go through
    /// here first: without it the controller keeps the node and re-announces it
    /// on the next `start_listening`, so a "deleted" device reappears.
    async fn decommission(&self, node_id: u64) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_mvd_passcode() {
        // Exactly what the Matter Virtual Device shows.
        assert_eq!(
            parse_setup_code("20202021").unwrap(),
            SetupCode::Passcode(20202021)
        );
    }

    #[test]
    fn accepts_manual_pairing_codes_as_printed() {
        // 11-digit short form, and the same code with label spacing/dashes.
        let expected = SetupCode::PairingCode("34970112332".into());
        assert_eq!(parse_setup_code("34970112332").unwrap(), expected);
        assert_eq!(parse_setup_code("3497-011-2332").unwrap(), expected);
        assert_eq!(parse_setup_code(" 3497 011 2332 ").unwrap(), expected);

        // 21-digit long form.
        assert!(matches!(
            parse_setup_code("123456789012345678901").unwrap(),
            SetupCode::PairingCode(_)
        ));
    }

    #[test]
    fn accepts_qr_payloads() {
        assert_eq!(
            parse_setup_code("MT:-24J0AFN00KA0648G00").unwrap(),
            SetupCode::PairingCode("MT:-24J0AFN00KA0648G00".into())
        );
    }

    #[test]
    fn matter_node_id_only_matches_matter_ids() {
        assert_eq!(matter_node_id("matter-1"), Some(1));
        assert_eq!(matter_node_id("matter-42"), Some(42));
        // Not a Matter device — the plain delete path handles these.
        assert_eq!(matter_node_id("pond-desktop"), None);
        assert_eq!(matter_node_id("fe78fa4d-0f85-4d48-bfc9-cc0cfd9f7d45"), None);
        assert_eq!(matter_node_id("matter-"), None);
        assert_eq!(matter_node_id("matter-abc"), None);
    }

    #[test]
    fn every_unavailable_cause_reads_differently() {
        let causes = [
            MatterUnavailable::Disabled,
            MatterUnavailable::NoUrl,
            MatterUnavailable::Unreachable {
                url: "ws://127.0.0.1:5580/ws".into(),
            },
            MatterUnavailable::Unsupported,
        ];
        let reasons: Vec<String> = causes.iter().map(MatterUnavailable::reason).collect();

        // The whole point of the enum: four causes, four distinct messages. A
        // user who cannot commission must be able to tell which one they hit.
        for reason in &reasons {
            assert!(!reason.is_empty());
        }
        for (i, a) in reasons.iter().enumerate() {
            for b in &reasons[i + 1..] {
                assert_ne!(a, b, "two causes share one message");
            }
        }

        // The unreachable case names the address that failed, so the user knows
        // which controller to go and look at.
        assert!(reasons[2].contains("ws://127.0.0.1:5580/ws"));

        // The three fixable causes say a restart is needed; the connection is
        // made once at startup, so flipping the setting alone is not enough.
        for reason in &reasons[..3] {
            assert!(reason.contains("restart"), "missing the restart step");
        }
    }

    #[test]
    fn off_reports_disabled_rather_than_a_commissioner() {
        match MatterAvailability::off().ready() {
            Err(why) => assert_eq!(why, &MatterUnavailable::Disabled),
            Ok(_) => panic!("Matter is off; there should be no commissioner"),
        }
    }

    #[test]
    fn rejects_anything_that_is_not_a_code() {
        // Empty / whitespace.
        assert!(parse_setup_code("").is_err());
        assert!(parse_setup_code("   ").is_err());
        // Wrong digit counts.
        assert!(parse_setup_code("123").is_err());
        assert!(parse_setup_code("202020210").is_err());
        // Not digits, and not a QR payload — must not reach the controller.
        assert!(parse_setup_code("../../etc/passwd").is_err());
        assert!(parse_setup_code("20202021; rm -rf /").is_err());
        assert!(parse_setup_code("MT:").is_err());
    }
}
