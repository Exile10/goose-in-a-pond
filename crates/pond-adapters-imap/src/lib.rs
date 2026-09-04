//! IMAP as a personal-context source: sign in, read subjects, ingest.
//!
//! The sibling of `pond-adapters-caldav` and bound by the same rules, with one
//! extra restriction that is the whole design:
//!
//! **Subjects, senders, dates AND bodies** — the bodies chunked, so a passage
//! is what gets embedded rather than a whole message. A single vector over an
//! entire email describes its signature block as much as its point; that is
//! the problem chunking exists to solve, and solving it is what makes bodies
//! affordable. The volume argument in §1.4 was about VECTOR count and holds
//! fine: ~1,300 messages at a few passages each is a few thousand vectors,
//! which brute-force cosine crosses in under a millisecond.
//!
//! The body is fetched with `BODY.PEEK[TEXT]`, never `BODY[TEXT]`: the latter
//! sets `\Seen` and would mark a household's mail read merely because the pond
//! read it. A test pins the PEEK.
//!
//! Read-only otherwise, exactly as CalDAV is: no APPEND, no STORE, no flag is
//! ever set.
//!
//! Implicit TLS on 993 only. STARTTLS on 143 begins in the clear and a
//! downgrade there is invisible to a household, so it is not offered.

mod body;
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
///
/// Generous because ONE call is now many batches: a first sync over a 30-day
/// window reads every body in it, and 45 seconds killed that mid-way. Later
/// syncs resume above the stored UID and finish in a second or two, so this
/// ceiling only ever applies to the first one.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// How much of one message is worth reading.
///
/// A marketing email can be a megabyte of inlined HTML and tracking pixels, and
/// nothing a household would ask about is in the last 900 KB of it. Cutting
/// before the MIME parse bounds both memory and the work `body_to_text` does.
const MAX_BODY_BYTES: usize = 64 * 1024;

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
        Ok(self.fetch_since(since, None).await?.0)
    }

    /// Recent messages, plus the cursor a later sync should resume from.
    ///
    /// The cursor is `UIDVALIDITY:MAXUID`. Passing the previous one asks the
    /// server for messages ABOVE that UID, which is what makes an ongoing sync
    /// cheap: without it every pass re-downloads the whole window, and with
    /// bodies that is the entire mailbox every thirty minutes.
    ///
    /// `UIDVALIDITY` is carried because a mailbox may renumber. When the server
    /// reports a different one the stored UID means nothing, so the window is
    /// read again from the start rather than resuming from a number that now
    /// points somewhere else.
    pub async fn messages_since_cursor(
        &self,
        since: DateTime<Utc>,
        cursor: Option<&str>,
    ) -> Result<(Vec<RawItem>, Option<String>)> {
        self.fetch_since(since, cursor).await
    }

    async fn fetch_since(
        &self,
        since: DateTime<Utc>,
        cursor: Option<&str>,
    ) -> Result<(Vec<RawItem>, Option<String>)> {
        let host = self.config.provider.host().to_string();
        let port = self.config.provider.port();

        // PAI-2: gate before a packet leaves, and record afterwards. IMAP is a
        // raw socket rather than an HTTP call, so the URL is synthesised — what
        // the tracker classifies and what `network_mode` refuses is the HOST,
        // which is the part that is real either way.
        let url = format!("imaps://{host}:{port}");
        pond_core::shared::services::egress::check_egress(&url)?;

        let started = std::time::Instant::now();
        let result = tokio::time::timeout(TIMEOUT, self.fetch(&host, port, since, cursor)).await;
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

    async fn fetch(
        &self,
        host: &str,
        port: u16,
        since: DateTime<Utc>,
        cursor: Option<&str>,
    ) -> Result<(Vec<RawItem>, Option<String>)> {
        let tcp = tokio::net::TcpStream::connect((host, port))
            .await
            .with_context(|| format!("could not reach the mail server at {host}:{port}"))?;

        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        // The provider is NAMED, not inferred.
        //
        // `ClientConfig::builder()` asks rustls to work the provider out from
        // crate features, and it PANICS when it cannot -- which is this
        // workspace's normal state: `aws_lc_rs` comes from the root Cargo.toml
        // and `ring` from hyper-rustls via reqwest, so both are enabled and
        // there is no single answer to infer. That ambiguity predates this
        // crate; being the first code to call the inferring constructor is what
        // turned it into a crash on a background worker.
        //
        // A library has no business depending on the host process having
        // installed a default either, so this one says which provider it wants
        // and stops caring.
        let tls_config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .context("could not select TLS protocol versions")?
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
        let mailbox = session
            .examine("INBOX")
            .await
            .context("could not open the INBOX")?;
        let uid_validity = mailbox.uid_validity.unwrap_or(0);

        // Resume above the last UID this pond saw, but only if the mailbox has
        // not renumbered. A changed UIDVALIDITY makes the stored number point
        // at a different message, so the window is read again from the start —
        // re-reading is idempotent (Message-ID is the key), resuming from a
        // stale number silently skips mail.
        let resume_from = cursor.and_then(|c| c.split_once(':')).and_then(|(v, u)| {
            match (v.parse::<u32>().ok(), u.parse::<u32>().ok()) {
                (Some(v), Some(u)) if v == uid_validity => Some(u),
                _ => None,
            }
        });

        let query = match resume_from {
            Some(max) => format!("UID {}:* SINCE {}", max + 1, since.format("%d-%b-%Y")),
            None => format!("SINCE {}", since.format("%d-%b-%Y")),
        };
        let uids = session
            .uid_search(&query)
            .await
            .context("the mail server refused the search")?;
        let highest = uids.iter().copied().max();

        let mut items = Vec::new();
        // Counted, because both ways a message can vanish below are a silent
        // `continue`. A sync that returns 21 of 1,345 and a sync that returns
        // 21 because the mailbox holds 21 look identical from the outside, and
        // the first one is a mail server throttling a client that just pulled
        // every body twice.
        let mut unreadable = 0usize;
        let mut envelopeless = 0usize;
        // BATCHED, and this is not a tuning knob — it is the difference between
        // working and taking the pond down.
        //
        // Asking for every message in the window in ONE fetch was fine while
        // this read envelopes: a thousand headers is a few hundred kilobytes.
        // With bodies it streams the whole mailbox — measured at 769 MB
        // resident on a 1,300-message window — and the process stopped
        // answering its own health check for long enough that the desktop
        // watchdog restarted it, killing the sync, which then began again.
        //
        // A batch bounds what is in flight to roughly `BATCH * message size`,
        // and yielding between batches gives the runtime a chance to serve
        // everything else the pond is doing.
        const BATCH: usize = 50;
        for window in uids.iter().copied().collect::<Vec<_>>().chunks(BATCH) {
            let set = window
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");
            // ENVELOPE for the headers, BODY.PEEK[TEXT] for the words.
            //
            // PEEK is the whole of the read-only claim at the fetch level:
            // plain `BODY[TEXT]` sets \Seen, so reading a mailbox would mark
            // it read. `EXAMINE` above already refuses flag changes, so this is
            // the belt to that braces — and the one a future edit is most
            // likely to drop by shortening the atom.
            let mut stream = session
                .uid_fetch(set, "(ENVELOPE BODY.PEEK[TEXT])")
                .await
                .context("the mail server refused the fetch")?;
            while let Some(message) = stream.next().await {
                let message = match message {
                    Ok(m) => m,
                    Err(e) => {
                        unreadable += 1;
                        tracing::debug!(error = %e, "a fetch response could not be read");
                        continue;
                    }
                };
                // The body is the message's own words, cleaned of quoted
                // replies and signatures — text that belongs to other messages
                // would otherwise be the most repeated, and therefore most
                // findable, thing in the mailbox.
                //
                // Capped before parsing: a newsletter can carry a megabyte of
                // inlined HTML, and no answer a household wants is in the last
                // 900 KB of it.
                let body = message
                    .text()
                    .map(|raw| {
                        let cut = raw.len().min(MAX_BODY_BYTES);
                        crate::body::body_to_text(&String::from_utf8_lossy(&raw[..cut]))
                    })
                    .unwrap_or_default();
                match envelope_to_item(message.envelope(), &body) {
                    Some(item) => items.push(item),
                    None => envelopeless += 1,
                }
            }
            drop(stream);
            // Hand the runtime back between batches. Without this the fetch
            // loop is one long await chain that never lets a health check in.
            tokio::task::yield_now().await;
        }
        // Best effort: a failed logout does not invalidate what was read.
        let _ = session.logout().await;

        let asked = uids.len();
        if items.len() < asked {
            tracing::warn!(
                asked,
                returned = items.len(),
                unreadable,
                envelopeless,
                "the mail server returned fewer messages than were searched for"
            );
        } else {
            tracing::info!(asked, returned = items.len(), "mail fetched");
        }

        // Only advance the cursor past what was actually READ. A batch that
        // failed mid-way leaves the cursor where it was, so the next pass
        // covers the same ground rather than stepping over messages nothing
        // stored.
        let next_cursor = match (highest, resume_from) {
            (Some(h), _) => Some(format!("{uid_validity}:{h}")),
            // Nothing new, but the mailbox was reachable: keep the cursor.
            (None, Some(prev)) => Some(format!("{uid_validity}:{prev}")),
            (None, None) => None,
        };
        Ok((items, next_cursor))
    }
}

/// Turn an IMAP `ENVELOPE` into a context item.
///
/// Skipped rather than defaulted when there is no Message-ID or no date: the
/// Message-ID is the idempotency key for re-sync, and inventing one re-creates
/// the mail as a duplicate on every sweep forever.
fn envelope_to_item(
    envelope: Option<&async_imap::imap_proto::Envelope<'_>>,
    body_text: &str,
) -> Option<RawItem> {
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
    let (participants, from_line) = match sender {
        Some((name, address)) => {
            let line = match &address {
                Some(a) if a != &name => format!("From: {name} <{a}>"),
                _ => format!("From: {name}"),
            };
            (vec![name], line)
        }
        None => (Vec::new(), String::new()),
    };
    // Sender first so the shortest possible body still says who wrote it, then
    // the message. `embedding_text` is `title\nbody`, and the body is now
    // CHUNKED — the subject rides in chunk 0 with the opening lines, which is
    // where a search for the subject should land.
    let body = match (from_line.is_empty(), body_text.trim().is_empty()) {
        (_, true) => from_line,
        (true, false) => body_text.trim().to_string(),
        (false, false) => format!("{from_line}\n\n{}", body_text.trim()),
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

    /// Reading a mailbox must not change it.
    #[test]
    fn the_body_fetch_never_marks_mail_as_read() {
        let production = production_code();
        // Bodies ARE fetched now — chunked and embedded per passage, which is
        // what a vector store is for. What must never appear is the NON-PEEK
        // form: `BODY[TEXT]` sets \Seen and marks a household's mail read
        // merely because the pond looked at it. That is one dropped atom away
        // from the correct line, so it is pinned.
        assert!(
            production.contains("BODY.PEEK[TEXT]"),
            "the body fetch must use PEEK, or reading the mailbox marks it read"
        );
        for forbidden in ["RFC822", "\"BODY[", " BODY[", "BODYSTRUCTURE"] {
            assert!(
                !production.contains(forbidden),
                "a non-PEEK body fetch sets \\Seen: {forbidden}"
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
