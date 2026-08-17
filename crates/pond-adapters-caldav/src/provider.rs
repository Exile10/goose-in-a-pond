//! Where a household's calendar actually lives, and what that implies.
//!
//! CalDAV is one protocol with four commercially important dialects, and the
//! differences are entirely in setup rather than in the wire format. Encoding
//! them here means the household picks a name from a list instead of finding a
//! URL, which is the difference between a source somebody connects and a source
//! somebody means to connect.
//!
//! Every one of these needs an **app-specific password**, not the account
//! password, because all four require it once two-factor is on and two of them
//! require it unconditionally. That is a setup instruction rather than a
//! protocol detail, so it travels with the preset.

/// A calendar host this pond knows how to reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalDavProvider {
    /// Google Calendar over CalDAV. Needs 2FA plus an app password; the
    /// account password is refused outright.
    Google,
    /// iCloud. `SourceKind::Calendar` over CalDAV is the honest ceiling here --
    /// there is no general iCloud API, so this is not a stepping stone to one.
    ICloud,
    /// Fastmail. The cleanest of the four: real CalDAV, documented, stable.
    Fastmail,
    /// Nextcloud, or anything else self-hosted, where the base URL is the
    /// household's own.
    Nextcloud { base_url: String },
    /// A server named by URL, for everything this list does not cover.
    Custom { base_url: String },
}

impl CalDavProvider {
    /// The URL discovery starts from.
    ///
    /// Returns the well-known entry point rather than a calendar: RFC 6764 says
    /// a client discovers the principal and then the calendar home, and hard
    /// coding a calendar path is how a connector breaks the first time somebody
    /// has two calendars.
    pub fn discovery_url(&self) -> String {
        match self {
            Self::Google => "https://apidata.googleusercontent.com/caldav/v2/".to_string(),
            Self::ICloud => "https://caldav.icloud.com/".to_string(),
            Self::Fastmail => "https://caldav.fastmail.com/dav/".to_string(),
            Self::Nextcloud { base_url } | Self::Custom { base_url } => {
                base_url.trim_end_matches('/').to_string()
            }
        }
    }

    /// Stable identifier written to `context_sources.provider`.
    ///
    /// Part of the schema, not a display string: it is read back to rebuild the
    /// adapter, so renaming one orphans every source a household connected.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::ICloud => "icloud",
            Self::Fastmail => "fastmail",
            Self::Nextcloud { .. } => "nextcloud",
            Self::Custom { .. } => "custom",
        }
    }

    /// What the household has to go and do before this can work.
    ///
    /// Carried in the type so the connect surface can show it at the moment it
    /// is needed, rather than in documentation nobody reads while looking at a
    /// password box.
    pub fn setup_hint(&self) -> &'static str {
        match self {
            Self::Google => {
                "Google needs two-factor turned on, then an app password from your Google \
                 account's security page. Your normal password will not work."
            }
            Self::ICloud => {
                "iCloud needs an app-specific password, created from the Sign-In and Security \
                 section of your Apple account."
            }
            Self::Fastmail => {
                "Fastmail needs an app password with calendar access, created under \
                 Settings, Privacy & Security, Connected apps."
            }
            Self::Nextcloud { .. } => {
                "Nextcloud needs a device password from Settings, Security. The server address \
                 is the one you use in a browser."
            }
            Self::Custom { .. } => {
                "Use the CalDAV address your provider documents, with an app password if the \
                 account has two-factor turned on."
            }
        }
    }

    /// Rebuild a provider from what was stored, for a source being re-synced.
    ///
    /// `base_url` is only consulted for the two variants that carry one; a
    /// stored `google` row cannot be turned into a custom host by editing a
    /// column, which is the point.
    pub fn from_stored(provider: &str, base_url: Option<&str>) -> Option<Self> {
        match provider {
            "google" => Some(Self::Google),
            "icloud" => Some(Self::ICloud),
            "fastmail" => Some(Self::Fastmail),
            "nextcloud" => base_url.map(|b| Self::Nextcloud {
                base_url: b.to_string(),
            }),
            "custom" => base_url.map(|b| Self::Custom {
                base_url: b.to_string(),
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_has_a_discovery_url_and_a_setup_hint() {
        // A preset with an empty hint is worse than no preset: the household
        // reaches a password box with nothing telling them where to get one.
        for p in [
            CalDavProvider::Google,
            CalDavProvider::ICloud,
            CalDavProvider::Fastmail,
            CalDavProvider::Nextcloud {
                base_url: "https://cloud.example.org/remote.php/dav".into(),
            },
            CalDavProvider::Custom {
                base_url: "https://dav.example.org".into(),
            },
        ] {
            assert!(p.discovery_url().starts_with("https://"), "{p:?}");
            assert!(!p.setup_hint().is_empty(), "{p:?}");
            assert!(!p.as_str().is_empty(), "{p:?}");
        }
    }

    #[test]
    fn a_self_hosted_base_url_loses_its_trailing_slash_exactly_once() {
        let p = CalDavProvider::Nextcloud {
            base_url: "https://cloud.example.org/dav/".into(),
        };
        assert_eq!(p.discovery_url(), "https://cloud.example.org/dav");
    }

    /// The round trip a re-sync depends on. A stored row that cannot rebuild
    /// its provider is a source that silently stops syncing.
    #[test]
    fn stored_providers_round_trip() {
        for p in [
            CalDavProvider::Google,
            CalDavProvider::ICloud,
            CalDavProvider::Fastmail,
            CalDavProvider::Nextcloud {
                base_url: "https://cloud.example.org".into(),
            },
        ] {
            let base = match &p {
                CalDavProvider::Nextcloud { base_url } | CalDavProvider::Custom { base_url } => {
                    Some(base_url.clone())
                }
                _ => None,
            };
            assert_eq!(
                CalDavProvider::from_stored(p.as_str(), base.as_deref()),
                Some(p)
            );
        }
    }

    /// A self-hosted variant with no stored URL must refuse rather than invent
    /// one: guessing a host is how a pond ends up authenticating somewhere the
    /// household never named.
    #[test]
    fn a_self_hosted_provider_without_its_url_is_not_rebuilt() {
        assert_eq!(CalDavProvider::from_stored("nextcloud", None), None);
        assert_eq!(CalDavProvider::from_stored("custom", None), None);
        assert_eq!(CalDavProvider::from_stored("not-a-provider", None), None);
    }
}
