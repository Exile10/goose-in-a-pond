use serde::{Serialize, Deserialize};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnboardingStep {
    VerifyDevice,
    CreateProfile,
    ConfigurePersonality,
    ConnectDevices,
    Completed,
}

impl OnboardingStep {
    pub fn next(self) -> Self {
        match self {
            Self::VerifyDevice => Self::CreateProfile,
            Self::CreateProfile => Self::ConfigurePersonality,
            Self::ConfigurePersonality => Self::ConnectDevices,
            Self::ConnectDevices => Self::Completed,
            Self::Completed => Self::Completed,
        }
    }

    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Completed)
    }
}

impl ToString for OnboardingStep {
    fn to_string(&self) -> String {
        format!("{:?}", self)
    }
}

impl FromStr for OnboardingStep {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "VerifyDevice" => Ok(Self::VerifyDevice),
            "CreateProfile" => Ok(Self::CreateProfile),
            "ConfigurePersonality" => Ok(Self::ConfigurePersonality),
            "ConnectDevices" => Ok(Self::ConnectDevices),
            "Completed" => Ok(Self::Completed),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_step_progression_order() {
        assert_eq!(
            OnboardingStep::VerifyDevice.next(),
            OnboardingStep::CreateProfile
        );

        assert_eq!(
            OnboardingStep::CreateProfile.next(),
            OnboardingStep::ConfigurePersonality
        );

        assert_eq!(
            OnboardingStep::ConnectDevices.next(),
            OnboardingStep::Completed
        );
    }

    #[test]
    fn test_completed_is_terminal() {
        assert_eq!(
            OnboardingStep::Completed.next(),
            OnboardingStep::Completed
        );
    }

    #[test]
    fn test_is_complete() {
        assert!(OnboardingStep::Completed.is_complete());
        assert!(!OnboardingStep::VerifyDevice.is_complete());
    }
}