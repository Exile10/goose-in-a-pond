//! Network-egress tracker (#113) — shared, cross-crate.
//!
//! Records every outbound HTTP call built-in tools make into the unified event
//! store and classifies its privacy sensitivity, so the activity API (#114) can
//! answer "what did the system phone home to, and when?".
//!
//! This lives in `pond-core` (rather than `pond-mcp-server`) so that *any*
//! adapter — the MCP servers AND outboard adapters like `pond-adapters-weather`
//! — can report egress into one place without depending on each other. Adapters
//! report; core owns the policy.
//!
//! ## Request context
//! `current_session_id` / `current_tool` are process-global, set by the engine
//! before/around each turn and tool call; the recorder reads them so every
//! egress event is correlated to the turn and attributed to the tool that
//! triggered it. (Same shape as the rest of GIAP's per-turn context.)

use std::sync::{Arc, OnceLock, RwLock};

use crate::security::domain::event::{Event, EventCategory, PrivacySensitivity};
use crate::security::ports::event_log::EventLog;

// ── Request context ──────────────────────────────────────────────────────────

static CURRENT_SESSION_ID: RwLock<String> = RwLock::new(String::new());
static CURRENT_TOOL: RwLock<String> = RwLock::new(String::new());

/// Record the session ID for the turn currently in flight.
pub fn set_current_session_id(sid: &str) {
    if let Ok(mut guard) = CURRENT_SESSION_ID.write() {
        *guard = sid.to_string();
    }
}

/// The session ID for the in-flight turn, or an empty string if none is set.
pub fn current_session_id() -> String {
    CURRENT_SESSION_ID
        .read()
        .map(|g| g.clone())
        .unwrap_or_default()
}

/// Record the built-in tool about to run, so its egress can be attributed.
pub fn set_current_tool(tool: &str) {
    if let Ok(mut guard) = CURRENT_TOOL.write() {
        *guard = tool.to_string();
    }
}

/// The tool currently in flight, or an empty string if none is set.
pub fn current_tool() -> String {
    CURRENT_TOOL.read().map(|g| g.clone()).unwrap_or_default()
}

// ── Sink ─────────────────────────────────────────────────────────────────────

/// The unified event store every outbound call is recorded into. Set once at
/// startup; when unset (tests, standalone adapter use) the tracker is a silent
/// no-op.
static EGRESS_SINK: OnceLock<Arc<dyn EventLog>> = OnceLock::new();

/// Install the durable sink the egress tracker writes to. Call once at startup;
/// later calls are ignored (the first wins).
pub fn set_egress_sink(sink: Arc<dyn EventLog>) {
    let _ = EGRESS_SINK.set(sink);
}

fn egress_sink() -> Option<Arc<dyn EventLog>> {
    EGRESS_SINK.get().cloned()
}

// -- Network mode (PAI-2 P5) --------------------------------------------------

/// How hard egress is gated. Parsed from `settings.network_mode`.
///
/// The tiers reuse [`classify_host`] rather than inventing a second notion of
/// "allowed": `Allowlist` refuses exactly what is already classified
/// `Sensitive`, and `Offline` permits only what is already `Internal`. That is
/// deliberate -- one classification, one place to audit, and the fail-Sensitive
/// default does the work in both directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkMode {
    /// Record every outbound call, refuse none. What every install has today.
    Open,
    /// Refuse hosts that classify as `Sensitive`.
    Allowlist,
    /// Refuse everything that is not loopback.
    Offline,
}

impl NetworkMode {
    /// Parse, defaulting to [`NetworkMode::Open`] for anything unrecognised.
    ///
    /// This does NOT follow `PolicyMode::parse`, and the difference is the
    /// point. `PolicyMode` has a tier (`audit`) that is wrong in neither
    /// direction, so an unreadable value can land there safely. This setting has
    /// no such tier: its middle value refuses real traffic, so absorbing a typo
    /// into `allowlist` would take a home assistant off the internet with no
    /// diagnostic anyone could act on. The narrowing happens at the edge
    /// instead -- `PUT /api/v1/settings` refuses an unrecognised `network_mode`
    /// with 422 -- so a typo cannot reach the store through the supported path,
    /// and one that arrives some other way is loud rather than quietly
    /// restrictive.
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "allowlist" => Self::Allowlist,
            "offline" => Self::Offline,
            "open" => Self::Open,
            other => {
                tracing::warn!(
                    target: "giap::trace",
                    value = %other,
                    "unrecognised network_mode; falling back to \"open\""
                );
                Self::Open
            }
        }
    }

    /// The stored spelling, for log lines and event attributes.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Allowlist => "allowlist",
            Self::Offline => "offline",
        }
    }
}

static NETWORK_MODE: RwLock<NetworkMode> = RwLock::new(NetworkMode::Open);

/// Install the network mode. Called at startup and again on every
/// `PUT /api/v1/settings` that changes it -- a privacy control the user has to
/// restart the pond to apply is not one.
pub fn set_network_mode(mode: NetworkMode) {
    if let Ok(mut guard) = NETWORK_MODE.write() {
        *guard = mode;
    }
}

/// The network mode currently in force.
pub fn network_mode() -> NetworkMode {
    NETWORK_MODE.read().map(|g| *g).unwrap_or(NetworkMode::Open)
}

/// An outbound call the network mode refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressDenied {
    /// The destination host, as [`extract_host`] saw it.
    pub host: String,
    /// The mode that refused it.
    pub mode: NetworkMode,
    /// Why, in words the user can act on.
    pub reason: &'static str,
}

impl std::fmt::Display for EgressDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "network_mode = \"{}\" refused an outbound request to {}: {}",
            self.mode.as_str(),
            self.host,
            self.reason
        )
    }
}

impl std::error::Error for EgressDenied {}

/// The gate itself: pure and total, so every mode x classification cell is
/// testable with no client, no sink and no runtime.
///
/// Note the polarity. A URL whose host cannot be parsed becomes `"unknown"`,
/// which [`classify_host`] calls `Sensitive`, which both restrictive modes
/// refuse. Failure narrows.
pub fn egress_verdict(host: &str, mode: NetworkMode) -> Result<(), &'static str> {
    let sensitivity = classify_host(host);
    match mode {
        NetworkMode::Open => Ok(()),
        NetworkMode::Allowlist => {
            if sensitivity == PrivacySensitivity::Sensitive {
                Err("host is not a loopback or curated public API; \
                     set network_mode to \"open\" to permit it")
            } else {
                Ok(())
            }
        }
        NetworkMode::Offline => {
            if sensitivity == PrivacySensitivity::Internal {
                Ok(())
            } else {
                Err("network_mode is \"offline\"; only loopback destinations are permitted")
            }
        }
    }
}

/// Check one outbound URL against the network mode BEFORE the request is made.
///
/// A refusal is recorded as an `egress.denied` event carrying the same host /
/// tool / session attribution a permitted call gets, because an unexplained
/// refusal is worse than no refusal (PAI-2 invariant 1). The event keeps the
/// host's own sensitivity, so a refused `Sensitive` destination is still
/// visible as one in the activity feed.
pub fn check_egress(url: &str) -> Result<(), EgressDenied> {
    let mode = network_mode();
    let host = extract_host(url);
    match egress_verdict(host, mode) {
        Ok(()) => Ok(()),
        Err(reason) => {
            let denied = EgressDenied {
                host: host.to_string(),
                mode,
                reason,
            };
            tracing::warn!(
                target: "giap::trace",
                kind = "egress_denied",
                host = %denied.host,
                mode = %mode.as_str(),
                "{denied}"
            );
            append_event(denied_event(
                &denied,
                &current_tool(),
                &current_session_id(),
            ));
            Err(denied)
        }
    }
}

/// Build the `Network` event describing one refusal. Takes the tool and session
/// as arguments rather than reading the process-globals itself, for the same
/// two reasons [`egress_event`] does: it stays pure and total, and a test of it
/// does not have to write a process-global that another test in the same binary
/// is concurrently reading.
pub fn denied_event(denied: &EgressDenied, tool: &str, session_id: &str) -> Event {
    let mut event = Event::new(EventCategory::Network, "egress.denied")
        .attr("host", denied.host.as_str())
        .attr("network_mode", denied.mode.as_str())
        .attr("reason", denied.reason)
        .sensitivity(classify_host(&denied.host));
    if !tool.is_empty() {
        event = event.attr("tool", tool);
    }
    if !session_id.is_empty() {
        event = event.session(session_id);
    }
    event
}

/// Append fire-and-forget, and only when a runtime is actually running.
///
/// [`record_egress`] reaches `tokio::spawn` directly because it always runs
/// after an awaited request. The gate cannot: it runs BEFORE the request and is
/// callable from a synchronous caller, where `spawn` panics. Losing an audit
/// line is bad; panicking inside a privacy check is worse.
fn append_event(event: Event) {
    let Some(sink) = egress_sink() else {
        return;
    };
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::warn!(
            target: "giap::trace",
            "no tokio runtime on this thread; egress event not persisted"
        );
        return;
    };
    handle.spawn(async move {
        if let Err(e) = sink.append(event).await {
            tracing::warn!(target: "giap::trace", error = %e, "failed to record egress event");
        }
    });
}

/// One gated outbound call: the gate, then the timer, then the record.
///
/// New outbound adapters use this instead of copying `traced_send`. Building it
/// IS the gate -- an `Err` means the request must not be made -- and dropping it
/// without [`EgressCall::finish`] records nothing, which is why `finish`
/// consumes `self`.
#[must_use = "an EgressCall that is never finished records no egress"]
pub struct EgressCall {
    url: String,
    method: &'static str,
    started: std::time::Instant,
}

/// Open a gated outbound call to `url`. See [`EgressCall`].
pub fn begin(url: &str, method: &'static str) -> Result<EgressCall, EgressDenied> {
    check_egress(url)?;
    Ok(EgressCall {
        url: url.to_string(),
        method,
        started: std::time::Instant::now(),
    })
}

impl EgressCall {
    /// Record the completed call. `None` means the request never got a status.
    pub fn finish(self, status: Option<u16>) {
        let latency_ms = self.started.elapsed().as_millis() as u64;
        record_egress(&self.url, self.method, status, latency_ms);
    }
}

// ── Recording ──────────────────────────────────────────────────────────────--

/// Record one outbound HTTP call (any caller, any built-in tool). Reads the
/// in-flight tool + session from the request context, classifies the host, and
/// appends a `Network` event to the sink fire-and-forget so a slow observability
/// write never adds latency to the call. No-op when no sink is installed.
pub fn record_egress(url: &str, method: &str, status: Option<u16>, latency_ms: u64) {
    let Some(sink) = egress_sink() else {
        return;
    };
    let host = extract_host(url).to_string();
    // An in-machine hop is not egress. Recording it would put one event per
    // TURN in the feed on a pond whose model server is loopback — the normal
    // install — and three call sites (the transcription forwards and the
    // diagnostics probe) had already each invented their own "check but do
    // not record" suppression to avoid exactly that. The decision belongs
    // here, keyed on the CLASSIFICATION, so a caller records unconditionally
    // and the same URL setting pointed at a remote box starts appearing in
    // the feed the moment it stops being local — which is the moment the
    // feed needs it.
    if classify_host(&host) == PrivacySensitivity::Internal {
        return;
    }
    let tool = current_tool();
    let session_id = current_session_id();
    let event = egress_event(&host, &tool, &session_id, method, status, latency_ms);
    tokio::spawn(async move {
        if let Err(e) = sink.append(event).await {
            tracing::warn!(target: "giap::trace", error = %e, "failed to record egress event");
        }
    });
}

/// Build the `Network` event describing one outbound call. Pure and total so the
/// classification + shape can be unit-tested without a live client or sink.
pub fn egress_event(
    host: &str,
    tool: &str,
    session_id: &str,
    method: &str,
    status: Option<u16>,
    latency_ms: u64,
) -> Event {
    let mut event = Event::new(EventCategory::Network, "egress.http")
        .attr("host", host)
        .attr("method", method)
        .attr("latency_ms", latency_ms as i64)
        .sensitivity(classify_host(host));
    if !tool.is_empty() {
        event = event.attr("tool", tool);
    }
    if !session_id.is_empty() {
        event = event.session(session_id);
    }
    if let Some(code) = status {
        event = event.attr("status", code as i64);
    }
    event
}

// ── Privacy classification ─────────────────────────────────────────────────--

/// Curated allowlist of public, read-only informational APIs the built-in tools
/// call. Matched as exact host or sub-domain. Kept narrow on purpose: anything
/// not on it (including user-configured search backends) is treated as
/// privacy-`Sensitive` by default.
const KNOWN_PUBLIC_SUFFIXES: &[&str] = &[
    "wikipedia.org",
    "wikimedia.org",
    "dictionaryapi.dev",
    "wolframalpha.com",
    "openlibrary.org",
    "restcountries.com",
    "openfoodfacts.org",
    "frankfurter.dev",
    "finnhub.io",
    "coingecko.com",
    "finance.yahoo.com",
    "hacker-news.firebaseio.com",
    "ycombinator.com",
    "guardianapis.com",
    "theguardian.com",
    "gnews.io",
    "open-meteo.com",
];

/// Classify a destination host's privacy sensitivity:
/// loopback → `Internal`, known public API → `Public`, anything else →
/// `Sensitive` (the safe default for unknown third parties).
pub fn classify_host(host: &str) -> PrivacySensitivity {
    let h = host.trim().to_ascii_lowercase();
    if is_loopback(&h) {
        return PrivacySensitivity::Internal;
    }
    if KNOWN_PUBLIC_SUFFIXES
        .iter()
        .any(|s| h == *s || h.ends_with(&format!(".{s}")))
    {
        return PrivacySensitivity::Public;
    }
    PrivacySensitivity::Sensitive
}

fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host == "::1"
        || host == "127.0.0.1"
        || host.starts_with("127.")
        || host.ends_with(".localhost")
}

/// Extract the bare host from a URL (no scheme, port, or path).
pub fn extract_host(url: &str) -> &str {
    url.find("://")
        .map(|i| &url[i + 3..])
        .and_then(|s| s.split('/').next())
        .and_then(|h| h.split(':').next())
        .unwrap_or("unknown")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Network mode (PAI-2 P5) ─────────────────────────────────────────────

    /// The noise rule lives HERE, not at call sites: a loopback hop is not
    /// egress, and per-call-site suppression is how three routes each grew
    /// their own copy of that decision.
    #[test]
    fn an_internal_host_is_below_the_recording_threshold() {
        assert_eq!(classify_host("127.0.0.1"), PrivacySensitivity::Internal);
        assert_eq!(classify_host("localhost"), PrivacySensitivity::Internal);
        // The same setting pointed off-box must clear the threshold.
        assert_ne!(
            classify_host("gpu-box.tailnet.example"),
            PrivacySensitivity::Internal
        );
    }

    #[test]
    fn network_mode_matrix_covers_every_mode_and_classification() {
        // (host, expected classification) -- real hosts, so a change to
        // KNOWN_PUBLIC_SUFFIXES moves this test rather than sliding past it.
        let internal = "127.0.0.1";
        let public = "en.wikipedia.org";
        let sensitive = "tracker.example.com";
        assert_eq!(classify_host(internal), PrivacySensitivity::Internal);
        assert_eq!(classify_host(public), PrivacySensitivity::Public);
        assert_eq!(classify_host(sensitive), PrivacySensitivity::Sensitive);

        // Open refuses nothing.
        for h in [internal, public, sensitive] {
            assert!(
                egress_verdict(h, NetworkMode::Open).is_ok(),
                "open must permit {h}"
            );
        }

        // Allowlist refuses exactly what classifies Sensitive.
        assert!(
            egress_verdict(internal, NetworkMode::Allowlist).is_ok(),
            "allowlist must permit loopback"
        );
        assert!(
            egress_verdict(public, NetworkMode::Allowlist).is_ok(),
            "allowlist must permit a curated public API"
        );
        assert!(
            egress_verdict(sensitive, NetworkMode::Allowlist).is_err(),
            "allowlist must refuse a Sensitive host"
        );

        // Offline permits only loopback -- including the public APIs.
        assert!(
            egress_verdict(internal, NetworkMode::Offline).is_ok(),
            "offline must permit loopback"
        );
        assert!(
            egress_verdict(public, NetworkMode::Offline).is_err(),
            "offline must refuse even a curated public API"
        );
        assert!(
            egress_verdict(sensitive, NetworkMode::Offline).is_err(),
            "offline must refuse a Sensitive host"
        );
    }

    #[test]
    fn an_unparseable_url_is_refused_by_both_restrictive_modes() {
        // extract_host gives up and says "unknown", which classifies Sensitive.
        let host = extract_host("not a url");
        assert_eq!(host, "unknown");
        assert!(
            egress_verdict(host, NetworkMode::Allowlist).is_err(),
            "an unparseable host must not be permitted under allowlist"
        );
        assert!(
            egress_verdict(host, NetworkMode::Offline).is_err(),
            "an unparseable host must not be permitted under offline"
        );

        // Bracketed IPv6 is a known extract_host limitation: it splits on ':'
        // and yields "[". Failure narrows -- the call is refused, not permitted.
        // If extract_host is ever taught IPv6, this assertion flips to is_ok
        // and that is a deliberate change, not a silent one.
        let v6 = extract_host("http://[::1]:8080/health");
        assert!(
            egress_verdict(v6, NetworkMode::Offline).is_err(),
            "bracketed IPv6 loopback is not recognised today; it must fail CLOSED"
        );
    }

    #[test]
    fn unrecognised_network_mode_parses_as_open_not_as_a_restriction() {
        assert_eq!(NetworkMode::parse("open"), NetworkMode::Open);
        assert_eq!(NetworkMode::parse("allowlist"), NetworkMode::Allowlist);
        assert_eq!(NetworkMode::parse("offline"), NetworkMode::Offline);
        assert_eq!(NetworkMode::parse("  OFFLINE "), NetworkMode::Offline);

        // The whole point: unrecognised widens, and the 422 at PUT /settings is
        // what stops an unrecognised value ever being stored. Absorbing a typo
        // into `allowlist` would break a working pond with no diagnostic.
        assert_eq!(
            NetworkMode::parse("offlien"),
            NetworkMode::Open,
            "a typo must not silently restrict the network"
        );
        assert_eq!(NetworkMode::parse(""), NetworkMode::Open);

        // Round-trip, so as_str and parse cannot drift.
        for m in [
            NetworkMode::Open,
            NetworkMode::Allowlist,
            NetworkMode::Offline,
        ] {
            assert_eq!(NetworkMode::parse(m.as_str()), m);
        }
    }

    #[test]
    fn a_refused_call_is_recorded_with_its_reason() {
        let denied = EgressDenied {
            host: "tracker.example.com".to_string(),
            mode: NetworkMode::Offline,
            reason: "network_mode is \"offline\"; only loopback destinations are permitted",
        };
        let ev = denied_event(&denied, "search_web", "sess-denied");

        assert_eq!(ev.category, EventCategory::Network);
        assert_eq!(ev.action, "egress.denied");
        assert_eq!(
            ev.attributes.get("host"),
            Some(&"tracker.example.com".into())
        );
        assert_eq!(ev.attributes.get("network_mode"), Some(&"offline".into()));
        assert_eq!(ev.attributes.get("reason"), Some(&denied.reason.into()));
        assert_eq!(ev.attributes.get("tool"), Some(&"search_web".into()));
        assert_eq!(ev.session_id.as_deref(), Some("sess-denied"));
        // A refused Sensitive destination is still Sensitive. Recording it as
        // Internal because "it never happened" would hide it from the retention
        // rules that exist for exactly this class of event.
        assert_eq!(ev.privacy_sensitivity, PrivacySensitivity::Sensitive);
        // The message a user actually sees has to name the setting and the host.
        let rendered = denied.to_string();
        assert!(
            rendered.contains("network_mode") && rendered.contains("tracker.example.com"),
            "a refusal must be actionable, got: {rendered}"
        );
    }

    #[test]
    fn extracts_host_from_url() {
        assert_eq!(
            extract_host("https://en.wikipedia.org/w/api.php?x=1"),
            "en.wikipedia.org"
        );
        assert_eq!(extract_host("http://127.0.0.1:8080/foo"), "127.0.0.1");
        assert_eq!(extract_host("not a url"), "unknown");
    }

    #[test]
    fn classifies_loopback_as_internal() {
        assert_eq!(classify_host("localhost"), PrivacySensitivity::Internal);
        assert_eq!(classify_host("127.0.0.1"), PrivacySensitivity::Internal);
        assert_eq!(classify_host("::1"), PrivacySensitivity::Internal);
        assert_eq!(
            classify_host("searxng.localhost"),
            PrivacySensitivity::Internal
        );
    }

    #[test]
    fn classifies_known_apis_as_public() {
        for host in [
            "en.wikipedia.org",
            "api.coingecko.com",
            "hacker-news.firebaseio.com",
            "api.open-meteo.com",
            "geocoding-api.open-meteo.com",
        ] {
            assert_eq!(
                classify_host(host),
                PrivacySensitivity::Public,
                "{host} should be Public"
            );
        }
        assert_eq!(
            classify_host("EN.WIKIPEDIA.ORG"),
            PrivacySensitivity::Public
        );
    }

    #[test]
    fn classifies_unknown_hosts_as_sensitive() {
        assert_eq!(
            classify_host("tracker.example.com"),
            PrivacySensitivity::Sensitive
        );
        // A look-alike suffix must not match (no substring/false-positive).
        assert_eq!(
            classify_host("notwikipedia.org.evil.com"),
            PrivacySensitivity::Sensitive
        );
    }

    #[test]
    fn egress_event_shape_and_classification() {
        let ev = egress_event(
            "en.wikipedia.org",
            "search_wikipedia",
            "sess-1",
            "GET",
            Some(200),
            42,
        );
        assert_eq!(ev.category, EventCategory::Network);
        assert_eq!(ev.action, "egress.http");
        assert_eq!(ev.privacy_sensitivity, PrivacySensitivity::Public);
        assert_eq!(ev.session_id.as_deref(), Some("sess-1"));
        assert_eq!(ev.attributes.get("host"), Some(&"en.wikipedia.org".into()));
        assert_eq!(ev.attributes.get("tool"), Some(&"search_wikipedia".into()));
        assert_eq!(ev.attributes.get("method"), Some(&"GET".into()));
        assert_eq!(ev.attributes.get("status"), Some(&200_i64.into()));
        assert_eq!(ev.attributes.get("latency_ms"), Some(&42_i64.into()));
    }

    #[test]
    fn egress_event_omits_empty_fields_and_missing_status() {
        let ev = egress_event("tracker.example.com", "", "", "GET", None, 5);
        assert_eq!(ev.privacy_sensitivity, PrivacySensitivity::Sensitive);
        assert!(ev.session_id.is_none());
        assert!(!ev.attributes.contains_key("tool"));
        assert!(!ev.attributes.contains_key("status"));
    }

    #[tokio::test]
    async fn record_egress_appends_to_sink_with_context() {
        use crate::security::domain::event::EventQuery;
        use crate::security::ports::event_log::EventLog;
        use async_trait::async_trait;
        use std::sync::Mutex;

        // Capturing sink.
        struct CapturingLog(Arc<Mutex<Vec<Event>>>);
        #[async_trait]
        impl EventLog for CapturingLog {
            async fn append(&self, event: Event) -> anyhow::Result<()> {
                self.0.lock().unwrap().push(event);
                Ok(())
            }
            async fn query(&self, _q: EventQuery) -> anyhow::Result<Vec<Event>> {
                Ok(self.0.lock().unwrap().clone())
            }
            async fn purge(&self, _q: EventQuery) -> anyhow::Result<u64> {
                Ok(0)
            }
        }

        let captured = Arc::new(Mutex::new(Vec::new()));
        set_egress_sink(Arc::new(CapturingLog(captured.clone())));
        set_current_session_id("sess-xyz");
        set_current_tool("get_current_weather");

        record_egress(
            "https://api.open-meteo.com/v1/forecast?x=1",
            "GET",
            Some(200),
            12,
        );

        // record_egress spawns the append; give it a moment to land.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let events = captured.lock().unwrap().clone();
        let ev = events
            .iter()
            .find(|e| e.attributes.get("host") == Some(&"api.open-meteo.com".into()))
            .expect("egress event recorded");
        assert_eq!(ev.category, EventCategory::Network);
        assert_eq!(ev.privacy_sensitivity, PrivacySensitivity::Public);
        assert_eq!(ev.session_id.as_deref(), Some("sess-xyz"));
        assert_eq!(
            ev.attributes.get("tool"),
            Some(&"get_current_weather".into())
        );
        assert_eq!(ev.attributes.get("status"), Some(&200_i64.into()));
    }
}
