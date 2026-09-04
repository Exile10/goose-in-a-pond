//! Where a household's mail lives.
//!
//! The same shape as the CalDAV presets and for the same reason: the protocol
//! is uniform and the setup is not. Every one of these needs an app-specific
//! password, and the household meets that requirement while looking at a
//! password box rather than while reading documentation.

/// A mail host this pond knows how to reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImapProvider {
    /// Gmail. Needs 2FA plus an app password; IMAP must also be enabled in
    /// Gmail's own settings, which is the step people miss.
    Gmail,
    /// iCloud Mail. IMAP is the honest ceiling here, as CalDAV is for calendar.
    ICloud,
    Fastmail,
    /// Anything self-hosted or unlisted.
    Custom {
        host: String,
        port: u16,
    },
}

impl ImapProvider {
    pub fn host(&self) -> &str {
        match self {
            Self::Gmail => "imap.gmail.com",
            Self::ICloud => "imap.mail.me.com",
            Self::Fastmail => "imap.fastmail.com",
            Self::Custom { host, .. } => host,
        }
    }

    /// Implicit TLS on 993 throughout. STARTTLS on 143 is deliberately not
    /// offered: it begins in the clear, and a downgrade there is invisible to
    /// the household. Every provider above supports 993.
    pub fn port(&self) -> u16 {
        match self {
            Self::Custom { port, .. } => *port,
            _ => 993,
        }
    }

    /// Stable identifier written to `context_sources.provider`. Part of the
    /// schema: renaming one orphans every source a household connected.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Gmail => "gmail",
            Self::ICloud => "icloud",
            Self::Fastmail => "fastmail",
            Self::Custom { .. } => "custom",
        }
    }

    pub fn setup_hint(&self) -> &'static str {
        match self {
            Self::Gmail => {
                "Gmail needs two-factor turned on, then an app password from your Google \
                 account's security page. IMAP also has to be switched on in Gmail's own \
                 settings, under Forwarding and POP/IMAP."
            }
            Self::ICloud => {
                "iCloud needs an app-specific password, created from the Sign-In and Security \
                 section of your Apple account."
            }
            Self::Fastmail => {
                "Fastmail needs an app password with mail access, created under Settings, \
                 Privacy & Security, Connected apps."
            }
            Self::Custom { .. } => {
                "Use your provider's IMAP server address. This pond only connects over TLS on \
                 port 993."
            }
        }
    }

    /// Rebuild from what was stored, for a source being re-synced.
    ///
    /// `host_port` is `host:port`, and only the custom variant carries one — a
    /// stored `gmail` row cannot be pointed at another server by editing a
    /// column.
    pub fn from_stored(provider: &str, host_port: Option<&str>) -> Option<Self> {
        match provider {
            "gmail" => Some(Self::Gmail),
            "icloud" => Some(Self::ICloud),
            "fastmail" => Some(Self::Fastmail),
            "custom" => {
                let raw = host_port?;
                let (host, port) = match raw.rsplit_once(':') {
                    Some((h, p)) => (h.to_string(), p.parse().ok()?),
                    None => (raw.to_string(), 993),
                };
                (!host.is_empty()).then_some(Self::Custom { host, port })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_is_implicit_tls_on_993() {
        for p in [
            ImapProvider::Gmail,
            ImapProvider::ICloud,
            ImapProvider::Fastmail,
        ] {
            assert_eq!(p.port(), 993, "{p:?}");
            assert!(!p.host().is_empty());
            assert!(!p.setup_hint().is_empty(), "{p:?}");
        }
    }

    #[test]
    fn a_custom_host_round_trips_with_its_port() {
        let p = ImapProvider::from_stored("custom", Some("mail.example.org:1993")).unwrap();
        assert_eq!(p.host(), "mail.example.org");
        assert_eq!(p.port(), 1993);
    }

    #[test]
    fn a_custom_host_without_a_port_defaults_to_993() {
        let p = ImapProvider::from_stored("custom", Some("mail.example.org")).unwrap();
        assert_eq!(p.port(), 993);
    }

    /// Guessing a mail host is how a pond ends up sending an app password
    /// somewhere the household never named.
    #[test]
    fn a_custom_provider_without_a_host_is_not_rebuilt() {
        assert_eq!(ImapProvider::from_stored("custom", None), None);
        assert_eq!(ImapProvider::from_stored("custom", Some("")), None);
        assert_eq!(ImapProvider::from_stored("not-a-provider", None), None);
    }
}
