//! Where a household's calendar lives. CalDAV is one protocol with four commercially important
//! dialects that differ only in setup, so the household picks a name from a list instead of
//! finding a URL. Every preset needs an app-specific password, not the account password (all
//! four require it once two-factor is on, two unconditionally), so the instruction lives here.

/// A calendar host this pond knows how to reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalDavProvider {
    /// Google Calendar over CalDAV. **Cannot be connected by this pond**: Google's CalDAV v2 guide
    /// allows only HTTPS with OAuth 2.0; an app password IS Basic auth, hence a 401 that reads
    /// like a wrong password. The variant stays so stored rows stay readable and disconnectable;
    /// [`is_connectable`](Self::is_connectable) refuses new ones. Gmail over IMAP is unaffected.
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
    /// The URL discovery starts from: the well-known entry point rather than a calendar, since
    /// RFC 6764 discovers the principal, then the calendar home; a hard-coded calendar path breaks
    /// the first time somebody has two calendars.
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

    /// What the household has to go and do before this can work. Carried in the type so the
    /// connect surface can show it at the moment it is needed, next to the password box.
    pub fn setup_hint(&self) -> &'static str {
        match self {
            Self::Google => {
                "Google Calendar needs you to sign in with Google, which this pond cannot do \
                 yet. An app password will not work for it — Google refuses those for \
                 calendars. Gmail still works under Mail."
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

    /// Whether a NEW source may be connected. Separate from the variant existing at all: a pond
    /// that already stored a Google source must still read and disconnect it, so the variant
    /// stays and only new connections are refused.
    pub fn is_connectable(&self) -> bool {
        !matches!(self, Self::Google)
    }

    /// Rebuild a provider from what was stored, for a source being re-synced. `base_url` is only
    /// consulted for the two variants that carry one, so a stored `google` row cannot be turned
    /// into a custom host by editing a column.
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

#[cfg(test)]
mod connectability_tests {
    use super::*;

    /// Google's own CalDAV guide: Basic auth gets a 401, OAuth 2.0 is required.
    /// Offering it with a password box produced a refusal that read like the
    /// household had typed the wrong thing.
    #[test]
    fn google_calendar_is_not_connectable_with_a_password() {
        assert!(!CalDavProvider::Google.is_connectable());
        assert!(
            CalDavProvider::Google
                .setup_hint()
                .contains("sign in with Google"),
            "the hint must say WHY, or it reads as a bug in the pond"
        );
    }

    #[test]
    fn every_other_preset_is_connectable() {
        for p in [
            CalDavProvider::ICloud,
            CalDavProvider::Fastmail,
            CalDavProvider::Nextcloud {
                base_url: "https://x".into(),
            },
            CalDavProvider::Custom {
                base_url: "https://x".into(),
            },
        ] {
            assert!(p.is_connectable(), "{p:?}");
        }
    }

    /// A stored Google row must still resolve, or a household cannot disconnect
    /// the thing this change stops them creating.
    #[test]
    fn a_stored_google_source_can_still_be_rebuilt() {
        assert_eq!(
            CalDavProvider::from_stored("google", None),
            Some(CalDavProvider::Google)
        );
    }
}
