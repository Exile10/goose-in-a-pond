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

/// A device that has just joined the fabric.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommissionedDevice {
    /// GIAP device id (`matter-<node_id>`) — the same id the bridge registers.
    pub device_id: String,
    pub name: String,
    pub node_id: u64,
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

/// Driven Port: bring a device onto the local fabric.
#[async_trait]
pub trait DeviceCommissioningPort: Send + Sync {
    /// Commission a device using its setup code. Slow by nature — pairing
    /// involves discovery, attestation, and fabric join.
    async fn commission(&self, code: SetupCode) -> Result<CommissionedDevice>;
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
