use serde::{Serialize, Deserialize};
use std::str::FromStr;

/// Steps in the onboarding wizard.
///
/// The middleware only gates on `Completed` — intermediate step names are
/// informational only.  Unknown step strings (e.g. old DB rows with
/// "VerifyDevice", "Identity", "Assistant") parse as `Err(())`, which causes
/// the wizard to restart from the beginning; the user finishes and the DB is
/// updated to `Completed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnboardingStep {
    /// Step 0: Welcome screen — GIAP intro, handshake happens here.
    Welcome,
    /// Step 1: Basics — display name, preferred name, birthday, avatar.
    Basics,
    /// Step 2: Language & Location — locale, timezone, city, weather.
    Location,
    /// Step 3: Accessibility — atypical speech, slow TTS, high contrast, reduce motion.
    Accessibility,
    /// Step 4: Personality — prompt style and assistant personality hint.
    Personality,
    /// Step 5: Goose's Identity — assistant name, TTS voice, household note.
    GooseIdentity,
    /// Step 6: Wake Word — choose a wake phrase or enter a custom one.
    WakeWord,
    /// Step 7: AI Model — LLM provider and model selection.
    Model,
    /// Step 8: Extensions — enable curated extensions (Weather, MCP Memory).
    Extensions,
    /// Terminal state — unlocks all protected routes.
    Completed,
}

impl OnboardingStep {
    pub fn next(self) -> Self {
        match self {
            Self::Welcome      => Self::Basics,
            Self::Basics       => Self::Location,
            Self::Location     => Self::Accessibility,
            Self::Accessibility => Self::Personality,
            Self::Personality  => Self::GooseIdentity,
            Self::GooseIdentity => Self::WakeWord,
            Self::WakeWord     => Self::Model,
            Self::Model        => Self::Extensions,
            Self::Extensions   => Self::Completed,
            Self::Completed    => Self::Completed,
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
            "Welcome"       => Ok(Self::Welcome),
            "Basics"        => Ok(Self::Basics),
            "Location"      => Ok(Self::Location),
            "Accessibility" => Ok(Self::Accessibility),
            "Personality"   => Ok(Self::Personality),
            "GooseIdentity" => Ok(Self::GooseIdentity),
            "WakeWord"      => Ok(Self::WakeWord),
            "Model"         => Ok(Self::Model),
            "Extensions"    => Ok(Self::Extensions),
            "Completed"     => Ok(Self::Completed),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_progression_order() {
        assert_eq!(OnboardingStep::Welcome.next(),       OnboardingStep::Basics);
        assert_eq!(OnboardingStep::Basics.next(),        OnboardingStep::Location);
        assert_eq!(OnboardingStep::Location.next(),      OnboardingStep::Accessibility);
        assert_eq!(OnboardingStep::Accessibility.next(), OnboardingStep::Personality);
        assert_eq!(OnboardingStep::Personality.next(),   OnboardingStep::GooseIdentity);
        assert_eq!(OnboardingStep::GooseIdentity.next(), OnboardingStep::WakeWord);
        assert_eq!(OnboardingStep::WakeWord.next(),      OnboardingStep::Model);
        assert_eq!(OnboardingStep::Model.next(),         OnboardingStep::Extensions);
        assert_eq!(OnboardingStep::Extensions.next(),    OnboardingStep::Completed);
    }

    #[test]
    fn completed_is_terminal() {
        assert_eq!(OnboardingStep::Completed.next(), OnboardingStep::Completed);
    }

    #[test]
    fn is_complete_only_for_completed() {
        assert!(OnboardingStep::Completed.is_complete());
        assert!(!OnboardingStep::Welcome.is_complete());
        assert!(!OnboardingStep::Basics.is_complete());
        assert!(!OnboardingStep::Model.is_complete());
        assert!(!OnboardingStep::Extensions.is_complete());
    }

    #[test]
    fn unknown_step_string_returns_err() {
        // Old DB rows with legacy names must not panic — they return Err(())
        // which the repository converts to None, restarting from Welcome.
        assert!(OnboardingStep::from_str("VerifyDevice").is_err());
        assert!(OnboardingStep::from_str("CreateProfile").is_err());
        assert!(OnboardingStep::from_str("ConfigurePersonality").is_err());
        assert!(OnboardingStep::from_str("ConnectDevices").is_err());
        // Old 4-step names also restart cleanly
        assert!(OnboardingStep::from_str("Identity").is_err());
        assert!(OnboardingStep::from_str("Assistant").is_err());
    }

    #[test]
    fn roundtrip_to_string_and_back() {
        for step in [
            OnboardingStep::Welcome,
            OnboardingStep::Basics,
            OnboardingStep::Location,
            OnboardingStep::Accessibility,
            OnboardingStep::Personality,
            OnboardingStep::GooseIdentity,
            OnboardingStep::WakeWord,
            OnboardingStep::Model,
            OnboardingStep::Extensions,
            OnboardingStep::Completed,
        ] {
            let s = step.to_string();
            assert_eq!(OnboardingStep::from_str(&s), Ok(step));
        }
    }
}
