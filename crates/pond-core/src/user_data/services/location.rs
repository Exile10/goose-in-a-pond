//! Where the pond is — asked once, answered the same way everywhere.
//!
//! Location was spread across four settings (`weather_location_name`, the two
//! coordinates, `timezone`) and read directly in seven places across five
//! crates. Each call site invented its own fallback, which is how the same pond
//! came to describe itself two different ways in the same breath: the system
//! prompt omitted the line entirely when the name was blank, while the device
//! tool reported "not configured".
//!
//! Worse, neither of them looked at the time zone. A household that picked
//! `Africa/Nairobi` during setup and never filled the weather box was told the
//! pond did not know where it was, by a pond holding the answer.
//!
//! So the fallbacks live here, once, and callers ask a question instead of
//! reading fields:
//!
//! ```text
//!   name   configured name → the time zone's own place → nothing
//!   coords as stored; (0, 0) is the unset sentinel, not the Atlantic
//!   zone   as stored, defaulting to UTC
//! ```
//!
//! This is a pure function over [`Settings`], not a port. Nothing here reaches
//! for a network or a device — deciding where the pond is and *discovering* it
//! are different jobs, and only the second one needs permission from anybody.

use crate::user_data::domain::settings::Settings;

/// How confident the pond is about the name it is using.
///
/// Carried so a caller can phrase itself honestly. "You are in Nairobi" and
/// "your time zone suggests Nairobi" are different claims, and a tool that
/// cannot tell them apart will state a guess as a fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Somebody typed it, or a detection wrote it.
    Configured,
    /// Derived from the time zone, which is a good guess and only a guess.
    Timezone,
    /// The pond does not know.
    Unknown,
}

/// Where the pond believes it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Location {
    /// Place name, empty when [`Origin::Unknown`].
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    /// IANA zone, e.g. `Africa/Nairobi`. Never empty — falls back to `UTC`.
    pub timezone: String,
    pub origin: Origin,
}

impl Location {
    /// Whether the coordinates are usable.
    ///
    /// `(0, 0)` is the struct's default and a real point in the Gulf of Guinea.
    /// Treating it as unset is a deliberate trade: a pond moored there gets to
    /// type its coordinates twice, and every other pond stops asking the
    /// weather for a forecast off the coast of Ghana.
    pub fn has_coordinates(&self) -> bool {
        self.latitude != 0.0 || self.longitude != 0.0
    }

    /// Whether there is a name worth saying out loud.
    pub fn is_named(&self) -> bool {
        !self.name.is_empty()
    }

    /// The place, phrased for a person, or `None` when there is nothing to say.
    ///
    /// Callers that used to write their own "not configured" string should use
    /// this and say nothing when it is `None` — an interface that reports its
    /// own missing configuration to a household is talking to the wrong person.
    pub fn describe(&self) -> Option<&str> {
        self.is_named().then_some(self.name.as_str())
    }
}

/// The place name a time zone implies.
///
/// `Africa/Nairobi` → `Nairobi`; `America/New_York` → `New York`. Zones without
/// a region part (`UTC`) imply nothing, which is correct — UTC is not a place
/// anybody lives.
pub fn place_from_timezone(zone: &str) -> Option<String> {
    let leaf = zone.rsplit('/').next()?;
    if leaf == zone || leaf.is_empty() {
        return None;
    }
    Some(leaf.replace('_', " "))
}

/// Resolve the pond's location from its settings.
pub fn resolve(settings: &Settings) -> Location {
    let timezone = if settings.timezone.trim().is_empty() {
        "UTC".to_string()
    } else {
        settings.timezone.trim().to_string()
    };

    let configured = settings.weather_location_name.trim();
    let (name, origin) = if !configured.is_empty() {
        (configured.to_string(), Origin::Configured)
    } else {
        match place_from_timezone(&timezone) {
            Some(p) => (p, Origin::Timezone),
            None => (String::new(), Origin::Unknown),
        }
    };

    Location {
        name,
        latitude: settings.weather_latitude,
        longitude: settings.weather_longitude,
        timezone,
        origin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(name: &str, zone: &str, lat: f64, lon: f64) -> Settings {
        Settings {
            weather_location_name: name.into(),
            timezone: zone.into(),
            weather_latitude: lat,
            weather_longitude: lon,
            ..Default::default()
        }
    }

    #[test]
    fn a_typed_name_wins() {
        let l = resolve(&with("Kisumu", "Africa/Nairobi", 0.0, 0.0));
        assert_eq!(l.name, "Kisumu");
        assert_eq!(l.origin, Origin::Configured);
    }

    /// The defect this service exists for: the pond knew, and said it did not.
    #[test]
    fn an_empty_name_falls_back_to_the_timezone() {
        let l = resolve(&with("", "Africa/Nairobi", 0.0, 0.0));
        assert_eq!(l.name, "Nairobi");
        assert_eq!(l.origin, Origin::Timezone);
        assert_eq!(l.describe(), Some("Nairobi"));
    }

    #[test]
    fn underscores_are_not_shown_to_anybody() {
        assert_eq!(
            place_from_timezone("America/New_York").as_deref(),
            Some("New York")
        );
    }

    #[test]
    fn utc_is_not_a_place() {
        let l = resolve(&with("", "UTC", 0.0, 0.0));
        assert_eq!(l.origin, Origin::Unknown);
        assert!(!l.is_named());
        assert_eq!(l.describe(), None);
    }

    #[test]
    fn an_empty_timezone_is_utc_rather_than_blank() {
        assert_eq!(resolve(&with("", "   ", 0.0, 0.0)).timezone, "UTC");
    }

    #[test]
    fn null_island_counts_as_unset() {
        assert!(!resolve(&with("Nairobi", "Africa/Nairobi", 0.0, 0.0)).has_coordinates());
        assert!(resolve(&with("Nairobi", "Africa/Nairobi", -1.286, 36.817)).has_coordinates());
        // One axis is enough — the prime meridian runs through inhabited places.
        assert!(resolve(&with("Accra", "Africa/Accra", 5.6, 0.0)).has_coordinates());
    }

    #[test]
    fn whitespace_is_not_a_location() {
        let l = resolve(&with("   ", "Africa/Lagos", 0.0, 0.0));
        assert_eq!(l.name, "Lagos");
        assert_eq!(l.origin, Origin::Timezone);
    }
}
