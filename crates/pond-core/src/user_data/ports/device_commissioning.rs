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

/// The `matter-` prefix. The one place the grammar's shape is written down.
const MATTER_PREFIX: &str = "matter-";

/// Is this a Matter device id at all?
///
/// A PREFIX test, deliberately, and not "does it parse". The difference matters
/// because callers use this to decide whether a device belongs to the Matter
/// adapter, and a parse-based answer fails OPEN: an id this grammar cannot read
/// would be routed to whatever handles non-Matter devices, which for
/// `SwitchableDeviceControl` is a stub that reports success for every verb. That
/// is how "the fan is on" gets said about a fan nothing was ever sent to. A
/// prefix test fails closed — a malformed Matter id gets a Matter error.
pub fn is_matter_device_id(device_id: &str) -> bool {
    device_id.starts_with(MATTER_PREFIX)
}

/// The FABRIC node behind a device id, or `None` for any other id.
///
/// `matter-90-2` yields 90. A bridged device — one endpoint of a hub that speaks
/// for several — is not separately commissioned, so every fabric operation acts on
/// its hub. Callers that need to tell a hub from one of its children ask
/// [`matter_bridged_endpoint`]; this answers "which node do I talk to".
///
/// Canonical form only. `matter-01` is refused rather than read as node 1, because
/// `"01".parse::<u64>()` succeeds and two ids that differ as strings but agree as
/// nodes would give the registry two rows for one device.
pub fn matter_node_id(device_id: &str) -> Option<u64> {
    let (node, _) = matter_parts(device_id)?;
    Some(node)
}

/// The bridged endpoint a device id names, if it names one.
///
/// `None` both for a non-Matter id and for a whole node — the caller that cares
/// about the difference has already established the id is Matter's.
pub fn matter_bridged_endpoint(device_id: &str) -> Option<u16> {
    matter_parts(device_id)?.1
}

/// `matter-<node>` or `matter-<node>-<endpoint>`, in canonical form.
fn matter_parts(device_id: &str) -> Option<(u64, Option<u16>)> {
    let rest = device_id.strip_prefix(MATTER_PREFIX)?;
    let (node_text, endpoint_text) = match rest.split_once('-') {
        Some((node, endpoint)) => (node, Some(endpoint)),
        None => (rest, None),
    };

    let node: u64 = canonical(node_text)?;
    let endpoint = match endpoint_text {
        Some(text) => Some(canonical::<u16>(text)?),
        None => None,
    };
    Some((node, endpoint))
}

/// Parse a decimal component, refusing any spelling but the canonical one — so no
/// leading zeroes, no sign, no trailing text.
fn canonical<T>(text: &str) -> Option<T>
where
    T: std::str::FromStr + std::fmt::Display,
{
    let value: T = text.parse().ok()?;
    (value.to_string() == text).then_some(value)
}

/// GIAP device id for a Matter node, or for one bridged endpoint of it.
pub fn matter_device_id(node_id: u64, bridged_endpoint: Option<u16>) -> String {
    match bridged_endpoint {
        Some(endpoint) => format!("{MATTER_PREFIX}{node_id}-{endpoint}"),
        None => format!("{MATTER_PREFIX}{node_id}"),
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
    fn a_bridged_id_names_the_hub_for_fabric_operations() {
        // A bridged device is one endpoint of a hub that speaks for several. It is
        // not separately commissioned, so `decommission` can only ever act on the
        // hub — which is why this returns the hub and not `None`.
        assert_eq!(matter_node_id("matter-90-2"), Some(90));
        assert_eq!(matter_bridged_endpoint("matter-90-2"), Some(2));

        // A whole node names no endpoint.
        assert_eq!(matter_bridged_endpoint("matter-90"), None);
        // Neither does something that is not Matter's at all.
        assert_eq!(matter_bridged_endpoint("pond-desktop"), None);
    }

    #[test]
    fn only_the_canonical_spelling_of_an_id_is_accepted() {
        // `"01".parse::<u64>()` is Some(1), so without this `matter-01` and
        // `matter-1` would be two ids for one device — and the registry keys rows
        // on the string.
        assert_eq!(matter_node_id("matter-01"), None);
        assert_eq!(matter_node_id("matter-1-02"), None);
        assert_eq!(matter_node_id("matter-+1"), None);
        assert_eq!(matter_node_id("matter-1-"), None);
        assert_eq!(matter_node_id("matter-1-2-3"), None);
        // And an endpoint is a u16, as Matter defines it.
        assert_eq!(matter_node_id("matter-1-70000"), None);
    }

    #[test]
    fn a_matter_id_is_recognised_however_malformed() {
        // The property that matters, and the reason this is a prefix test rather
        // than a parse. Callers route on it, and the non-Matter route ends at a stub
        // that answers every verb with success — so an id this grammar cannot read
        // must still be recognised as Matter's, or a device nothing was sent to gets
        // reported as switched on.
        assert!(is_matter_device_id("matter-1"));
        assert!(is_matter_device_id("matter-90-2"));
        assert!(is_matter_device_id("matter-01"));
        assert!(is_matter_device_id("matter-nonsense"));
        assert!(is_matter_device_id("matter-"));

        assert!(!is_matter_device_id("pond-desktop"));
        assert!(!is_matter_device_id("matter"));
    }

    #[test]
    fn device_ids_round_trip_through_the_grammar() {
        assert_eq!(matter_device_id(18, None), "matter-18");
        assert_eq!(matter_device_id(90, Some(2)), "matter-90-2");
        assert_eq!(matter_node_id(&matter_device_id(90, Some(2))), Some(90));
        assert_eq!(
            matter_bridged_endpoint(&matter_device_id(90, Some(2))),
            Some(2)
        );
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
