use serde::{Deserialize, Serialize};

/// Which secret a phone says it holds in `pake_init`, so the pond knows which limits apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Credential {
    /// The printed card's static code, accepted only while the pond is unclaimed.
    Setup,
    /// A short-lived six-digit pairing code, issued on the console or read aloud.
    Pairing,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_names_are_setup_and_pairing() {
        assert_eq!(
            serde_json::to_string(&Credential::Setup).unwrap(),
            "\"setup\""
        );
        assert_eq!(
            serde_json::to_string(&Credential::Pairing).unwrap(),
            "\"pairing\""
        );
        assert_eq!(
            serde_json::from_str::<Credential>("\"setup\"").unwrap(),
            Credential::Setup
        );
        assert!(serde_json::from_str::<Credential>("\"Setup\"").is_err());
        assert!(serde_json::from_str::<Credential>("\"token\"").is_err());
    }
}
