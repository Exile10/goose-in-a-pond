//! CalDAV as a read-only personal-context source (PAI-8 §0: no write method exists). Every
//! request is gated by `check_egress` and tracked by `record_egress`; the only body ever sent is
//! a `calendar-query` naming a date range (invariant 7). Credentials come from `SecretRepository`
//! and are never logged. `<C:expand>` (RFC 4791 §9.6.5) leaves RRULE and timezones to the server.

mod ics;
mod provider;
mod xml;

pub use ics::events_from_ics;
pub use provider::CalDavProvider;
pub use xml::{parse_multistatus, DavResponse};

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use pond_core::context::ingest::RawItem;
use std::time::Duration as StdDuration;

/// How long any single CalDAV request may take. Explicit because the default is none: a hung
/// server would otherwise hold a scheduled sync task open forever, with no error anybody sees.
const REQUEST_TIMEOUT: StdDuration = StdDuration::from_secs(30);

/// A calendar this pond can read.
#[derive(Debug, Clone, PartialEq)]
pub struct Calendar {
    /// Absolute URL, resolved against the server's origin.
    pub url: String,
    pub display_name: String,
    /// The server's change tag, when it offered one. A sync whose ctag matches
    /// the stored one has nothing to fetch.
    pub ctag: Option<String>,
}

/// Everything needed to reach one household member's calendar account.
///
/// `password` is an app-specific password in every supported provider. The manual `Debug` impl
/// below redacts it so a `{:?}` in a trace line cannot leak the credential.
#[derive(Clone)]
pub struct CalDavConfig {
    pub provider: CalDavProvider,
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for CalDavConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CalDavConfig")
            .field("provider", &self.provider)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Reads a CalDAV account. Owns its HTTP client, per the adapter template.
pub struct CalDavAdapter {
    client: reqwest::Client,
    config: CalDavConfig,
    /// Overridden only by tests, exactly as the weather adapter does it: a
    /// wiremock server has no fixed URL, and the alternative is a connector
    /// nothing can exercise without the real account.
    base_url: Option<String>,
}

impl CalDavAdapter {
    pub fn new(config: CalDavConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("could not build the CalDAV HTTP client")?;
        Ok(Self {
            client,
            config,
            base_url: None,
        })
    }

    /// Point the adapter at a test server. Not for production use.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    fn root(&self) -> String {
        self.base_url
            .clone()
            .unwrap_or_else(|| self.config.provider.discovery_url())
    }

    fn auth_header(&self) -> String {
        let raw = format!("{}:{}", self.config.username, self.config.password);
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(raw)
        )
    }

    /// Resolve an href, which servers return as a path far more often than as
    /// an absolute URL, against the origin actually in use.
    fn absolute(&self, href: &str) -> String {
        if href.starts_with("http://") || href.starts_with("https://") {
            return href.to_string();
        }
        let root = self.root();
        let origin = match root.split_once("://") {
            Some((scheme, rest)) => {
                let host = rest.split('/').next().unwrap_or(rest);
                format!("{scheme}://{host}")
            }
            None => root.clone(),
        };
        format!(
            "{}/{}",
            origin.trim_end_matches('/'),
            href.trim_start_matches('/')
        )
    }

    /// Send a WebDAV request, gated and tracked. The gate is checked BEFORE the send and the
    /// record written after, so an `offline` pond refuses with no packet leaving, and the
    /// refusal names the host rather than surfacing as a timeout.
    async fn dav(
        &self,
        method: &str,
        url: &str,
        depth: &str,
        body: Option<String>,
    ) -> Result<String> {
        pond_core::shared::services::egress::check_egress(url)?;

        let method_owned = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|e| anyhow!("not a usable WebDAV method: {e}"))?;
        let mut req = self
            .client
            .request(method_owned, url)
            .header("Authorization", self.auth_header())
            .header("Depth", depth)
            .header("Content-Type", "application/xml; charset=utf-8");
        if let Some(b) = body {
            req = req.body(b);
        }

        let start = std::time::Instant::now();
        let result = req.send().await;
        let latency_ms = start.elapsed().as_millis() as u64;
        let status = result.as_ref().ok().map(|r| r.status().as_u16());
        pond_core::shared::services::egress::record_egress(url, method, status, latency_ms);

        let response = result.context("the calendar server could not be reached")?;
        let code = response.status();
        if code == reqwest::StatusCode::UNAUTHORIZED || code == reqwest::StatusCode::FORBIDDEN {
            // Named separately because the answer is different: this is the one
            // failure a household can fix, and it must not be retried into a
            // rate limit by the scheduler.
            return Err(anyhow!(
                "the calendar account refused these credentials -- most providers need an \
                 app-specific password rather than the account password"
            ));
        }
        if !code.is_success() && code.as_u16() != 207 {
            return Err(anyhow!("the calendar server answered {code}"));
        }
        response
            .text()
            .await
            .context("the calendar server's reply could not be read")
    }

    /// Walk `well-known -> principal -> calendar home -> calendars`.
    ///
    /// Discovery rather than a hard-coded path because a household with two
    /// calendars is normal and a guessed path finds one of them at best.
    pub async fn discover_calendars(&self) -> Result<Vec<Calendar>> {
        const PRINCIPAL: &str = r#"<d:propfind xmlns:d="DAV:"><d:prop><d:current-user-principal/></d:prop></d:propfind>"#;
        const HOME: &str = r#"<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><c:calendar-home-set/></d:prop></d:propfind>"#;
        const LIST: &str = r#"<d:propfind xmlns:d="DAV:" xmlns:cs="http://calendarserver.org/ns/"><d:prop><d:displayname/><d:resourcetype/><cs:getctag/></d:prop></d:propfind>"#;

        let root = self.root();
        let principal_doc = self
            .dav("PROPFIND", &root, "0", Some(PRINCIPAL.into()))
            .await?;
        let principal = parse_multistatus(&principal_doc)?
            .into_iter()
            .find_map(|r| r.nested_href)
            .ok_or_else(|| {
                anyhow!("the server did not say which principal these credentials are")
            })?;

        let home_doc = self
            .dav(
                "PROPFIND",
                &self.absolute(&principal),
                "0",
                Some(HOME.into()),
            )
            .await?;
        let home = parse_multistatus(&home_doc)?
            .into_iter()
            .find_map(|r| r.nested_href)
            .ok_or_else(|| anyhow!("the server did not say where this account's calendars are"))?;

        let list_doc = self
            .dav("PROPFIND", &self.absolute(&home), "1", Some(LIST.into()))
            .await?;
        Ok(parse_multistatus(&list_doc)?
            .into_iter()
            // The calendar home is itself a collection and comes back in this
            // list; only entries that declare `calendar` are calendars.
            .filter(|r| r.resource_types.iter().any(|t| t == "calendar"))
            .map(|r| Calendar {
                url: self.absolute(&r.href),
                display_name: r.display_name.clone().unwrap_or_else(|| "Calendar".into()),
                ctag: r.ctag.clone(),
            })
            .collect())
    }

    /// Events in a window, as items the ingest pipeline can take. A window, not everything,
    /// because §3's volume argument is what keeps this corpus inside brute-force retrieval.
    pub async fn events_in_window(
        &self,
        calendar_url: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<RawItem>> {
        let query = format!(
            r#"<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop><d:getetag/><c:calendar-data><c:expand start="{start}" end="{end}"/></c:calendar-data></d:prop>
  <c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT">
    <c:time-range start="{start}" end="{end}"/>
  </c:comp-filter></c:comp-filter></c:filter>
</c:calendar-query>"#,
            start = from.format("%Y%m%dT%H%M%SZ"),
            end = to.format("%Y%m%dT%H%M%SZ"),
        );
        let doc = self.dav("REPORT", calendar_url, "1", Some(query)).await?;
        let mut items = Vec::new();
        for response in parse_multistatus(&doc)? {
            if let Some(data) = response.calendar_data {
                items.extend(events_from_ics(&data));
            }
        }
        Ok(items)
    }

    /// The default sync window: recent past, near future. Asymmetric on purpose: "what did I
    /// agree to last week" is a real question and "what is happening in eleven months" is not.
    pub fn default_window(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
        (now - Duration::days(30), now + Duration::days(90))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> CalDavConfig {
        CalDavConfig {
            provider: CalDavProvider::Fastmail,
            username: "jerry@example.org".into(),
            password: "app-specific-secret".into(),
        }
    }

    /// A credential that prints itself in a trace line is a leak waiting for a
    /// bad day, and `{:?}` on a config struct is how it happens.
    #[test]
    fn the_password_is_not_printable_by_accident() {
        let rendered = format!("{:?}", cfg());
        assert!(!rendered.contains("app-specific-secret"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("jerry@example.org"), "{rendered}");
    }

    #[test]
    fn hrefs_resolve_against_the_origin_actually_in_use() {
        let a = CalDavAdapter::new(cfg())
            .unwrap()
            .with_base_url("https://dav.example.org/dav/");
        assert_eq!(
            a.absolute("/calendars/jerry/"),
            "https://dav.example.org/calendars/jerry/"
        );
        assert_eq!(
            a.absolute("https://other.example.org/c/"),
            "https://other.example.org/c/"
        );
    }

    /// This crate must contain no method that writes to somebody's account.
    /// Stated as a test because "we decided not to" is not enforcement, and the
    /// first person to add a helpful `create_event` will not read PAI-8 §0.
    #[test]
    fn nothing_here_can_write_to_the_account() {
        let source = include_str!("lib.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        for forbidden in ["\"PUT\"", "\"POST\"", "\"DELETE\"", "\"MKCALENDAR\""] {
            assert!(
                !production.contains(forbidden),
                "this crate is read-only by design (PAI-8 §0 defers write-back), but it \
                 issues {forbidden}"
            );
        }
    }

    #[test]
    fn the_default_window_looks_back_less_than_it_looks_forward() {
        let now = Utc::now();
        let (from, to) = CalDavAdapter::default_window(now);
        assert!(from < now && to > now);
        assert!(
            to - now > now - from,
            "the window should favour what is coming"
        );
    }
}
