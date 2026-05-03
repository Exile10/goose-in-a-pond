//! Tool domain mapping — resolves a [`ToolDomain`] to MCP tool-name filters
//! and domain-specific system-prompt hints.
//!
//! This module bridges the domain classifier (`classify_domain`) with the
//! agent's tool dispatch and prompt construction layers.  When a domain is
//! identified, the caller can:
//!
//! 1. **Filter available tools** via [`tool_filter_for_domain`] — returns `None`
//!    when all tools should remain available, or `Some(names)` to restrict the
//!    tool set.
//! 2. **Inject a hint** via [`domain_hint`] — a short instruction appended to
//!    the system prompt so the LLM knows which tools to prefer.

use super::domain_classifier::ToolDomain;

/// Returns a list of allowed MCP tool names for a given domain.
///
/// `None` means all tools are allowed (no filtering).  `Some(names)` means only
/// the listed tools should be visible to the agent for this request.
///
/// Currently only `Music` has an explicit filter — other domains can be refined
/// as MCP tool sets grow.
pub fn tool_filter_for_domain(domain: ToolDomain) -> Option<Vec<&'static str>> {
    match domain {
        ToolDomain::Music => Some(vec![
            "music__play",
            "music__status",
            "music__control",
        ]),
        // Other domains: no filter for now (all tools available).
        // Refine as domain-specific MCP servers are added.
        ToolDomain::General => None,
        _ => None,
    }
}

/// Returns a domain-specific hint to inject into the system prompt.
///
/// Empty string (`""`) means no hint is needed — the agent uses its default
/// behavior.  Non-empty strings are appended to the system prompt before the
/// agent processes the request.
pub fn domain_hint(domain: ToolDomain) -> &'static str {
    match domain {
        ToolDomain::Music => "\
CURRENT TASK: Music request. Use ONLY the music tools.\n\
- play: pass the song/artist name as 'query' — it searches and plays automatically\n\
- status: check what is currently playing\n\
- control: pause, resume, next, previous, volume, shuffle\n\
Do NOT use wikipedia or any other tools.",

        ToolDomain::Knowledge => "\
CURRENT TASK: Knowledge/information query.\n\
Use search_wikipedia or wikipedia_get_article to find information.",

        ToolDomain::Weather => "\
CURRENT TASK: Weather query.\n\
Use get_current_weather to get weather information.",

        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn music_has_tool_filter() {
        let filter = tool_filter_for_domain(ToolDomain::Music);
        assert!(filter.is_some());
        let names = filter.unwrap();
        assert!(names.contains(&"music__play"));
        assert!(names.contains(&"music__search_tracks"));
        assert!(names.contains(&"music__pause"));
        assert!(names.contains(&"music__now_playing"));
    }

    #[test]
    fn general_has_no_filter() {
        assert!(tool_filter_for_domain(ToolDomain::General).is_none());
    }

    #[test]
    fn non_music_domains_have_no_filter() {
        assert!(tool_filter_for_domain(ToolDomain::Knowledge).is_none());
        assert!(tool_filter_for_domain(ToolDomain::Weather).is_none());
        assert!(tool_filter_for_domain(ToolDomain::Home).is_none());
        assert!(tool_filter_for_domain(ToolDomain::Schedule).is_none());
        assert!(tool_filter_for_domain(ToolDomain::Memory).is_none());
        assert!(tool_filter_for_domain(ToolDomain::FileSystem).is_none());
        assert!(tool_filter_for_domain(ToolDomain::System).is_none());
    }

    #[test]
    fn music_hint_is_non_empty() {
        let hint = domain_hint(ToolDomain::Music);
        assert!(!hint.is_empty());
        assert!(hint.contains("music"));
    }

    #[test]
    fn knowledge_hint_is_non_empty() {
        let hint = domain_hint(ToolDomain::Knowledge);
        assert!(!hint.is_empty());
        assert!(hint.contains("wikipedia"));
    }

    #[test]
    fn weather_hint_is_non_empty() {
        let hint = domain_hint(ToolDomain::Weather);
        assert!(!hint.is_empty());
        assert!(hint.contains("weather"));
    }

    #[test]
    fn general_hint_is_empty() {
        assert!(domain_hint(ToolDomain::General).is_empty());
    }

    #[test]
    fn unhandled_domains_have_empty_hint() {
        assert!(domain_hint(ToolDomain::Home).is_empty());
        assert!(domain_hint(ToolDomain::Schedule).is_empty());
        assert!(domain_hint(ToolDomain::Memory).is_empty());
        assert!(domain_hint(ToolDomain::FileSystem).is_empty());
        assert!(domain_hint(ToolDomain::System).is_empty());
    }
}
