//! How a connector gets permission to read an account, wherever that
//! permission comes from.
//!
//! Today every credential is a username and a password in the encrypted secret
//! store. That is about to stop being the only source: GNOME Online Accounts on
//! a Linux pond holds OAuth tokens the household has already granted and
//! refreshes them itself, which is the difference between "set up an
//! app-specific password" and "you are already signed in" — and on Google's
//! CalDAV it is the difference between working and not, because Google rejects
//! Basic auth outright.
//!
//! The risk this port exists to remove is not the second source. It is the
//! `if` that would otherwise appear in the syncer, and then again in the connect
//! route, and then a third time when macOS arrives — the shape this codebase
//! keeps finding in its own audits.
//!
//! # What a caller may know, and what it may not
//!
//! A caller asks for the credentials of a source and receives an
//! [`AccountAuth`] it can hand to an adapter. It does not learn which provider
//! answered, and it must not branch on that. What it DOES branch on is the
//! shape of the auth — a password and a bearer token are used differently on
//! the wire, and pretending otherwise would push the branch into the protocol
//! code instead of removing it.
//!
//! # Why a token is not just a longer password
//!
//! A password is a value. A token is a value with an expiry and someone
//! responsible for renewing it. That difference is why [`AccountAuth::Bearer`]
//! carries `expires_at`, and why the port's method is `async` and takes
//! `&mut`-free `&self` but is expected to REFRESH rather than fail when a token
//! has aged out: the provider that owns the refresh is the only thing that can
//! do it, and a caller holding a stale token has no way to tell the difference
//! between "expired" and "revoked".

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Proof a connector may read an account.
///
/// Deliberately not a struct with optional fields: a value that carries both a
/// password and a token, or neither, is not a state any adapter should have to
/// consider.
#[derive(Clone, PartialEq, Eq)]
pub enum AccountAuth {
    /// A username and a password or app-specific password.
    ///
    /// IMAP sends this as `LOGIN`; CalDAV as HTTP Basic.
    Password { username: String, password: String },
    /// An OAuth 2.0 bearer token the pond did not mint and does not refresh.
    ///
    /// IMAP sends this as SASL `XOAUTH2`, which needs the account name as well
    /// as the token; CalDAV as an `Authorization: Bearer` header.
    Bearer {
        username: String,
        token: String,
        /// When the token stops working, when the provider says. `None` means
        /// the provider did not say — not that it never expires.
        expires_at: Option<DateTime<Utc>>,
    },
}

// Hand-written so a token or password cannot reach a log through `{:?}`. The
// same reason `ImapConfig` writes its own.
impl std::fmt::Debug for AccountAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Password { username, .. } => f
                .debug_struct("AccountAuth::Password")
                .field("username", username)
                .field("password", &"<redacted>")
                .finish(),
            Self::Bearer {
                username,
                expires_at,
                ..
            } => f
                .debug_struct("AccountAuth::Bearer")
                .field("username", username)
                .field("token", &"<redacted>")
                .field("expires_at", expires_at)
                .finish(),
        }
    }
}

impl AccountAuth {
    /// The account this proof is for. Both shapes have one; IMAP's `XOAUTH2`
    /// needs it alongside the token, so it is not a password-only field.
    pub fn username(&self) -> &str {
        match self {
            Self::Password { username, .. } | Self::Bearer { username, .. } => username,
        }
    }

    /// Whether this proof is known to have aged out at `now`.
    ///
    /// `false` for a password and for a token whose provider gave no expiry —
    /// in both cases there is nothing to conclude, and a caller that treated
    /// "unknown" as "expired" would re-authenticate on every sync.
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        match self {
            Self::Password { .. } => false,
            Self::Bearer { expires_at, .. } => expires_at.is_some_and(|at| at <= now),
        }
    }
}

/// Where a source's credentials come from, parsed from its `secret_ref`.
///
/// The existing refs are opaque store keys like
/// `caldav:mail:gmail:<profile-uuid>` — a scheme the secret store invented for
/// itself. Rather than reinterpret those, a NEW scheme is introduced for the
/// new providers, and anything unrecognised means the secret store: every
/// credential written before this port existed keeps resolving exactly as it
/// did, with no migration and no rewrite of live rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSource<'a> {
    /// The encrypted secret store, under this key.
    SecretStore(&'a str),
    /// A desktop account manager, under this account id.
    ///
    /// `goa:<account-id>` today. The variant is named for the ROLE rather than
    /// for GNOME so a second manager does not need a third variant and a fourth
    /// `if`.
    DesktopAccount { provider: &'a str, id: &'a str },
}

/// The prefix marking a ref that a desktop account manager owns.
const DESKTOP_PREFIX: &str = "desktop:";

impl<'a> CredentialSource<'a> {
    /// Read a stored `secret_ref`.
    ///
    /// Unrecognised is [`Self::SecretStore`], deliberately: this runs against
    /// rows written before the scheme existed, and a ref the pond cannot place
    /// is a store key, not an error.
    pub fn parse(secret_ref: &'a str) -> Self {
        match secret_ref.strip_prefix(DESKTOP_PREFIX) {
            Some(rest) => match rest.split_once(':') {
                Some((provider, id)) if !provider.is_empty() && !id.is_empty() => {
                    Self::DesktopAccount { provider, id }
                }
                // `desktop:` with nothing usable behind it is malformed, and a
                // malformed ref must not silently become a store lookup for a
                // key shaped like a prefix.
                _ => Self::SecretStore(secret_ref),
            },
            None => Self::SecretStore(secret_ref),
        }
    }

    /// The `secret_ref` to store for a desktop-managed account.
    pub fn desktop_ref(provider: &str, id: &str) -> String {
        format!("{DESKTOP_PREFIX}{provider}:{id}")
    }
}

/// Resolves a source's `secret_ref` into something an adapter can use.
///
/// One implementation per credential home; the caller holds one object and
/// never learns which answered.
#[async_trait]
pub trait AccountCredentialProvider: Send + Sync {
    /// Whether this provider owns `secret_ref`.
    fn owns(&self, secret_ref: &str) -> bool;

    /// The proof for `secret_ref`, refreshing it if that is this provider's job.
    ///
    /// `Ok(None)` means "not mine or not present" — a caller trying providers
    /// in turn needs that distinct from an error, which means "mine, and
    /// something is wrong".
    async fn auth_for(&self, secret_ref: &str) -> Result<Option<AccountAuth>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ref_written_before_this_port_existed_still_resolves_to_the_store() {
        // The live value on a real pond.
        assert_eq!(
            CredentialSource::parse("caldav:mail:gmail:d21b2618-96aa-4be6-8077-5cc256aadf2c"),
            CredentialSource::SecretStore("caldav:mail:gmail:d21b2618-96aa-4be6-8077-5cc256aadf2c")
        );
    }

    #[test]
    fn a_desktop_ref_round_trips_through_its_own_scheme() {
        let r = CredentialSource::desktop_ref("goa", "account_1712345");
        assert_eq!(r, "desktop:goa:account_1712345");
        assert_eq!(
            CredentialSource::parse(&r),
            CredentialSource::DesktopAccount {
                provider: "goa",
                id: "account_1712345"
            }
        );
    }

    /// A malformed ref must not become a store lookup for a key shaped like a
    /// prefix — that would read some other account's secret.
    #[test]
    fn a_malformed_desktop_ref_is_not_silently_a_store_key() {
        for bad in ["desktop:", "desktop:goa", "desktop::id", "desktop:goa:"] {
            assert_eq!(
                CredentialSource::parse(bad),
                CredentialSource::SecretStore(bad)
            );
        }
    }

    #[test]
    fn both_shapes_name_the_account_because_xoauth2_needs_it() {
        let pw = AccountAuth::Password {
            username: "jerry@example.com".into(),
            password: "app-specific".into(),
        };
        let tok = AccountAuth::Bearer {
            username: "jerry@example.com".into(),
            token: "ya29...".into(),
            expires_at: None,
        };
        assert_eq!(pw.username(), "jerry@example.com");
        assert_eq!(tok.username(), "jerry@example.com");
    }

    /// A password never expires and an unknown expiry is not an expiry —
    /// treating either as expired would re-authenticate on every sync.
    #[test]
    fn only_a_token_with_a_stated_expiry_can_be_expired() {
        let now = Utc::now();
        let pw = AccountAuth::Password {
            username: "u".into(),
            password: "p".into(),
        };
        assert!(!pw.is_expired_at(now));

        let no_expiry = AccountAuth::Bearer {
            username: "u".into(),
            token: "t".into(),
            expires_at: None,
        };
        assert!(!no_expiry.is_expired_at(now));

        let expired = AccountAuth::Bearer {
            username: "u".into(),
            token: "t".into(),
            expires_at: Some(now - chrono::Duration::seconds(1)),
        };
        assert!(expired.is_expired_at(now));

        let live = AccountAuth::Bearer {
            username: "u".into(),
            token: "t".into(),
            expires_at: Some(now + chrono::Duration::hours(1)),
        };
        assert!(!live.is_expired_at(now));
    }

    /// A credential must not reach a log through a derived Debug.
    #[test]
    fn neither_shape_prints_its_secret() {
        let pw = AccountAuth::Password {
            username: "u".into(),
            password: "hunter2".into(),
        };
        let tok = AccountAuth::Bearer {
            username: "u".into(),
            token: "ya29.super-secret".into(),
            expires_at: None,
        };
        assert!(!format!("{pw:?}").contains("hunter2"), "{pw:?}");
        assert!(!format!("{tok:?}").contains("super-secret"), "{tok:?}");
        // The account name is not a secret and is what makes a log useful.
        assert!(format!("{pw:?}").contains('u'));
    }
}
