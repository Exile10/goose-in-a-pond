//! IMAP as a personal-context source: sign in, read subjects, ingest.
//!
//! The sibling of `pond-adapters-caldav` and bound by the same rules, with one
//! extra restriction that is the whole design:
//!
//! **Subjects, senders and dates. Never bodies.** PAI-8 §3 says so, and the
//! reason is volume as much as privacy: a household's mail is hundreds of items
//! a week against a calendar's tens, and admitting bodies takes the corpus out
//! of the brute-force range §1.4 depends on. This crate never issues a `FETCH
//! BODY`, and a test walks its own source to prove it.
//!
//! Read-only otherwise, exactly as CalDAV is: no APPEND, no STORE, no flag is
//! ever set, and the mail is not marked read by looking at it — `BODY.PEEK`
//! semantics are inherent to `ENVELOPE`, which is one reason to prefer it.
//!
//! Implicit TLS on 993 only. STARTTLS on 143 begins in the clear and a
//! downgrade there is invisible to a household, so it is not offered.

mod header;
mod provider;

pub use header::decode_rfc2047;
pub use provider::ImapProvider;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, Utc};
use futures::StreamExt;
use pond_core::context::domain::ItemKind;
use pond_core::context::ingest::RawItem;
use std::sync::Arc;

/// How long a whole IMAP conversation may take.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

/// Everything needed to reach one household member's mailbox.
///
/// `Debug` redacts the password by hand, because a struct that prints its own
/// credential the first time somebody adds `{:?}` to a trace line is a leak
/// waiting for a bad day.
#[derive(Clone)]
pub struct ImapConfig {
    pub provider: ImapProvider,
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for ImapConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImapConfig")
            .field("provider", &self.provider)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

pub struct ImapAdapter {
    config: ImapConfig,
}

impl ImapAdapter {
    pub fn new(config: ImapConfig) -> Self {
        Self { config }
    }

    /// The default sync window.
    ///
    /// Short, and shorter than the calendar's, because mail volume is the thing
    /// that decides whether this corpus stays in brute-force range. Thirty days
    /// of subjects is a few hundred rows; a year is thousands and answers no
    /// question the household actually asks.
    pub fn default_window(now: DateTime<Utc>) -> DateTime<Utc> {
        now - Duration::days(30)
    }

    /// Recent messages, as items the ingest pipeline can take.
    ///
    /// One connection, one `SEARCH SINCE`, one `FETCH ENVELOPE`, then logout.
    /// A long-lived IDLE connection would be lower latency and is deliberately
    /// not here: a home server holding open sockets to several providers is a
    /// reliability problem before it is a feature (PAI-8 §3.5).
    pub async fn recent_messages(&self, since: DateTime<Utc>) -> Result<Vec<RawItem>> {
        let host = self.config.provider.host().to_string();
        let port = self.config.provider.port();

        // PAI-2: gate before a packet leaves, and record afterwards. IMAP is a
        // raw socket rather than an HTTP call, so the URL is synthesised — what
        // the tracker classifies and what `network_mode` refuses is the HOST,
        // which is the part that is real either way.
        let url = format!("imaps://{host}:{port}");
        pond_core::shared::services::egress::check_egress(&url)?;

        let started = std::time::Instant::now();
        let result = tokio::time::timeout(TIMEOUT, self.fetch(&host, port, since)).await;
        let latency_ms = started.elapsed().as_millis() as u64;
        let status = match &result {
            Ok(Ok(_)) => Some(200),
            Ok(Err(_)) => Some(500),
            Err(_) => None,
        };
        pond_core::shared::services::egress::record_egress(&url, "IMAP", status, latency_ms);

        match result {
            Ok(inner) => inner,
            Err(_) => Err(anyhow!("the mail server did not answer within {TIMEOUT:?}")),
        }
    }

    async fn fetch(&self, host: &str, port: u16, since: DateTime<Utc>) -> Result<Vec<RawItem>> {
        let tcp = tokio::net::TcpStream::connect((host, port))
            .await
            .with_context(|| format!("could not reach the mail server at {host}:{port}"))?;

        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls_config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let server_name = rustls_pki_types::ServerName::try_from(host.to_string())
            .map_err(|_| anyhow!("{host} is not a usable server name"))?;
        let tls = tokio_rustls::TlsConnector::from(Arc::new(tls_config))
            .connect(server_name, tcp)
            .await
            .context("the mail server's TLS handshake failed")?;

        let client = // No compat shim: with `runtime-tokio` async-imap uses tokio's own
        // AsyncRead/AsyncWrite (client.rs:16), which this stream already is.
        async_imap::Client::new(tls);
        let mut session = client
            .login(&self.config.username, &self.config.password)
            .await
            .map_err(|(e, _)| {
                // The one failure a household can fix, phrased so they can, and
                // distinguishable so the scheduler does not retry it.
                anyhow!(
                    "the mail account refused these credentials -- most providers need an \
                     app-specific password rather than the account password ({e})"
                )
            })?;

        // INBOX only, and `examine` rather than `select`: examine opens the
        // mailbox READ-ONLY, so this connector cannot change a flag even by
        // accident and mail does not become "read" because the pond looked.
        session
            .examine("INBOX")
            .await
            .context("could not open the INBOX")?;

        let query = format!("SINCE {}", since.format("%d-%b-%Y"));
        let uids = session
            .search(&query)
            .await
            .context("the mail server refused the search")?;

        let mut items = Vec::new();
        if !uids.is_empty() {
            let set = uids
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");
            // ENVELOPE and nothing else. No BODY, no BODYSTRUCTURE, no RFC822.
            let mut stream = session
                .fetch(set, "ENVELOPE")
                .await
                .context("the mail server refused the fetch")?;
            while let Some(message) = stream.next().await {
                let Ok(message) = message else { continue };
                if let Some(item) = envelope_to_item(message.envelope()) {
                    items.push(item);
                }
            }
        }
        // Best effort: a failed logout does not invalidate what was read.
        let _ = session.logout().await;
        Ok(items)
    }
}

/// Turn an IMAP `ENVELOPE` into a context item.
///
/// Skipped rather than defaulted when there is no Message-ID or no date: the
/// Message-ID is the idempotency key for re-sync, and inventing one re-creates
/// the mail as a duplicate on every sweep forever.
fn envelope_to_item(envelope: Option<&async_imap::imap_proto::Envelope<'_>>) -> Option<RawItem> {
    let envelope = envelope?;
    let text = |field: &Option<std::borrow::Cow<'_, [u8]>>| -> Option<String> {
        field
            .as_ref()
            .map(|b| decode_rfc2047(&String::from_utf8_lossy(b)))
    };

    let external_id = text(&envelope.message_id)?.trim().to_string();
    if external_id.is_empty() {
        return None;
    }
    let occurred_at = text(&envelope.date)
        .and_then(|d| DateTime::parse_from_rfc2822(d.trim()).ok())
        .map(|d| d.with_timezone(&Utc))?;

    let title = match text(&envelope.subject) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => "(no subject)".to_string(),
    };

    // Sender, as a person would recognise them. The display name when there is
    // one, the address otherwise -- an address is a name a household knows.
    let sender = envelope.from.as_ref().and_then(|addrs| {
        addrs.first().map(|a| {
            let name = a
                .name
                .as_ref()
                .map(|b| decode_rfc2047(&String::from_utf8_lossy(b)))
                .filter(|n| !n.trim().is_empty());
            let address = match (&a.mailbox, &a.host) {
                (Some(m), Some(h)) => Some(format!(
                    "{}@{}",
                    String::from_utf8_lossy(m),
                    String::from_utf8_lossy(h)
                )),
                _ => None,
            };
            match (name, address) {
                (Some(n), Some(addr)) => (n, Some(addr)),
                (Some(n), None) => (n, None),
                (None, Some(addr)) => (addr.clone(), Some(addr)),
                (None, None) => ("someone".to_string(), None),
            }
        })
    });

    // Body is the sentence a person would say, because `embedding_text` is
    // `title\nbody` and this IS the retrieval surface. No message body: PAI-8
    // §3 keeps this to subject, sender and date.
    let (participants, body) = match sender {
        Some((name, address)) => {
            let line = match &address {
                Some(a) if a != &name => format!("From: {name} <{a}>"),
                _ => format!("From: {name}"),
            };
            (vec![name], line)
        }
        None => (Vec::new(), String::new()),
    };

    Some(RawItem {
        external_id,
        kind: ItemKind::Message,
        occurred_at,
        title,
        body,
        participants,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This crate's own production source, with whole-line comments removed.
    ///
    /// The comments have to go or the guards below trip on the prose EXPLAINING
    /// the rule -- a doc line saying "no BODY, no BODYSTRUCTURE" reads exactly
    /// like a violation. Only whole-line comments are dropped, so a real
    /// `session.fetch(set, "BODY[]")` with a trailing comment is still caught.
    fn production_code() -> String {
        include_str!("lib.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap()
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !(t.starts_with("//") || t.starts_with("*"))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn cfg() -> ImapConfig {
        ImapConfig {
            provider: ImapProvider::Fastmail,
            username: "jerry@example.org".into(),
            password: "app-specific-secret".into(),
        }
    }

    #[test]
    fn the_password_is_not_printable_by_accident() {
        let rendered = format!("{:?}", cfg());
        assert!(!rendered.contains("app-specific-secret"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    /// The design's sharpest line about this connector: subjects, senders and
    /// dates, never bodies. Stated as a test because it is a volume decision as
    /// much as a privacy one, and the person who later adds a helpful body
    /// preview will not read PAI-8 §3 first.
    #[test]
    fn this_connector_never_asks_for_a_message_body() {
        let production = production_code();
        for forbidden in ["RFC822", "BODY[", "BODYSTRUCTURE", "\"BODY\""] {
            assert!(
                !production.contains(forbidden),
                "this connector keeps subjects, senders and dates only (PAI-8 §3), but it \
                 asks for {forbidden}"
            );
        }
        // And it must never write to the mailbox either.
        for forbidden in ["APPEND", "STORE", "\"COPY\"", "EXPUNGE"] {
            assert!(
                !production.contains(forbidden),
                "this connector is read-only, but it issues {forbidden}"
            );
        }
    }

    /// `examine` opens the mailbox read-only; `select` does not. With `select`
    /// a household's unread mail would silently become read because the pond
    /// looked at it.
    #[test]
    fn the_mailbox_is_opened_read_only() {
        let production = production_code();
        assert!(
            production.contains(".examine("),
            "the INBOX must be opened read-only"
        );
        assert!(
            !production.contains(".select("),
            "`select` opens the mailbox for writing and marks mail read"
        );
    }

    #[test]
    fn the_mail_window_is_shorter_than_the_calendars() {
        let now = Utc::now();
        let since = ImapAdapter::default_window(now);
        assert!(since < now);
        assert!(
            now - since <= Duration::days(31),
            "mail volume is what decides whether this corpus stays brute-forceable"
        );
    }
}
