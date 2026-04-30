//! Request classifier — pure function that maps a user message to a `ModelRole`.
//!
//! No I/O, no async. Fully deterministic and testable.
//!
//! ## Priority order
//! 1. "think about" voice/text prefix → `Think` (highest)
//! 2. Any Think keyword match → `Think`
//! 3. Any Task keyword match → `Task`
//! 4. Default → `Chat`

use crate::domain::model_role::ModelRole;

/// Common wake-word prefixes to strip before keyword matching.
const WAKE_PREFIXES: &[&str] = &[
    "goose, ", "goose ", "hey goose, ", "hey goose ", "ok computer, ", "ok computer ",
];

/// Keywords that indicate a request needs deeper reasoning.
const THINK_KEYWORDS: &[&str] = &[
    "think about",
    "explain why",
    "explain how",
    "analyze",
    "analyse",
    "reason through",
    "compare",
    "evaluate",
    "debate",
    "pros and cons",
    "step by step",
    "how does",
    "why does",
    "why is",
    "what causes",
    "break down",
    "walk me through",
    "deep dive",
];

/// Keywords that indicate an agentic / tool-use request.
const TASK_KEYWORDS: &[&str] = &[
    "schedule",
    "remind me",
    "set alarm",
    "set a timer",
    "add to calendar",
    "turn on",
    "turn off",
    "switch on",
    "switch off",
    "search for",
    "look up",
    "run ",
    "execute",
    "create a ",
    "list devices",
    "list schedules",
    "add device",
];

/// Classify a user message into the most appropriate `ModelRole`.
///
/// The input is the raw message as typed or transcribed.  Leading whitespace
/// and wake-word prefixes are stripped before matching.
pub fn classify_request(message: &str) -> ModelRole {
    let lower = message.trim().to_lowercase();

    // Strip wake-word prefix
    let stripped = WAKE_PREFIXES
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix))
        .unwrap_or(&lower);

    // "think about …" prefix override — highest priority
    if stripped.starts_with("think about ") || stripped.starts_with("think about,") {
        return ModelRole::Think;
    }

    // Think keyword scan
    for kw in THINK_KEYWORDS {
        if stripped.contains(kw) {
            return ModelRole::Think;
        }
    }

    // Task keyword scan
    for kw in TASK_KEYWORDS {
        if stripped.contains(kw) {
            return ModelRole::Task;
        }
    }

    ModelRole::Chat
}

/// Tool routing result — which tool (if any) a message should be dispatched to.
///
/// Used by the parallel-prep phase of the chat handler to replace the old
/// LLM-based classifier with instant keyword matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolRouting {
    /// No tool needed — pass the message straight to the main LLM.
    None,
    Weather,
    Wikipedia,
    SaveMemory,
    RecallMemory,
    Devices,
    Schedules,
    CreateSchedule,
}

impl ToolRouting {
    /// Map to the tool name string expected by `pond_mcp_server::try_tool_agent`.
    pub fn tool_name(&self) -> &'static str {
        match self {
            Self::None => "",
            Self::Weather => "weather",
            Self::Wikipedia => "wikipedia",
            Self::SaveMemory => "save_memory",
            Self::RecallMemory => "recall_memory",
            Self::Devices => "devices",
            Self::Schedules => "schedules",
            Self::CreateSchedule => "create_schedule",
        }
    }
}

/// Route a user message to the appropriate tool using keyword matching.
///
/// Instant, deterministic, zero LLM calls. Replaces the old LLM-based
/// classifier that cost 0.5-3s per message.
///
/// Priority order: CreateSchedule > SaveMemory > RecallMemory > Weather >
/// Devices > Schedules > Wikipedia > None.
pub fn route_to_tool(message: &str) -> ToolRouting {
    let mut stripped = message.to_lowercase();
    for prefix in WAKE_PREFIXES {
        if let Some(rest) = stripped.strip_prefix(prefix) {
            stripped = rest.to_string();
        }
    }
    let stripped = stripped.trim();

    // ── CreateSchedule (highest action priority) ────────────────────
    const SCHEDULE_CREATE_PATTERNS: &[&str] = &[
        "schedule to ", "schedule a ", "schedule me ",
        "every day at", "every morning", "every evening", "every night",
        "every hour", "every week",
        "remind me every", "remind me at ", "remind me to ",
        "at 1", "at 2", "at 3", "at 4", "at 5", "at 6", "at 7",
        "at 8", "at 9", "at 10", "at 11", "at 12",  // "at N am/pm"
        "set alarm", "set a timer", "set a reminder",
    ];
    if SCHEDULE_CREATE_PATTERNS.iter().any(|p| stripped.contains(p)) {
        // Disambiguate: "at 3" alone is too broad — require "am", "pm", or
        // another scheduling word nearby.
        let has_time_marker = stripped.contains("am") || stripped.contains("pm")
            || stripped.contains("every") || stripped.contains("schedule")
            || stripped.contains("remind") || stripped.contains("alarm")
            || stripped.contains("timer") || stripped.contains("daily")
            || stripped.contains("o'clock");
        if has_time_marker {
            return ToolRouting::CreateSchedule;
        }
    }

    // ── RecallMemory (before SaveMemory — recall queries often contain
    //    save-like words, e.g. "do you remember what food I like?") ───
    const RECALL_MEMORY_PATTERNS: &[&str] = &[
        "do you remember", "do you recall",
        "what did i tell you", "what did i say",
        "what do you know about me", "what do you remember",
        "what's my name", "what is my name",
        "what's my ", "what is my ",
        "did i mention", "did i tell you",
        "recall my", "recall what",
    ];
    if RECALL_MEMORY_PATTERNS.iter().any(|p| stripped.contains(p)) {
        return ToolRouting::RecallMemory;
    }

    // ── SaveMemory ──────────────────────────────────────────────────
    const SAVE_MEMORY_PATTERNS: &[&str] = &[
        "remember that", "remember this", "remember my", "remember i ",
        "don't forget", "do not forget",
        "save this", "save that", "note that", "note this",
        "keep in mind", "make a note",
        "my name is", "i am called", "call me ",
        "i live in", "i'm from", "i am from",
        "my birthday is", "my email is", "my phone",
        "i prefer", "i like", "i love", "i hate", "i dislike",
        "i'm allergic", "i am allergic",
    ];
    if SAVE_MEMORY_PATTERNS.iter().any(|p| stripped.contains(p)) {
        return ToolRouting::SaveMemory;
    }

    // ── Weather ─────────────────────────────────────────────────────
    if stripped.contains("weather") || stripped.contains("temperature outside")
        || stripped.contains("forecast") || stripped.starts_with("how cold")
        || stripped.starts_with("how hot") || stripped.starts_with("how warm")
        || stripped.starts_with("is it raining") || stripped.starts_with("will it rain")
    {
        return ToolRouting::Weather;
    }

    // ── Devices ─────────────────────────────────────────────────────
    const DEVICE_PATTERNS: &[&str] = &[
        "list devices", "my devices", "what devices",
        "connected devices", "online devices",
        "turn on the", "turn off the", "switch on", "switch off",
    ];
    if DEVICE_PATTERNS.iter().any(|p| stripped.contains(p)) {
        return ToolRouting::Devices;
    }

    // ── Schedules (list) ────────────────────────────────────────────
    const SCHEDULE_LIST_PATTERNS: &[&str] = &[
        "list schedules", "my schedules", "what schedules",
        "show schedules", "show my schedule",
        "what's scheduled", "what is scheduled",
        "upcoming tasks", "upcoming schedules",
    ];
    if SCHEDULE_LIST_PATTERNS.iter().any(|p| stripped.contains(p)) {
        return ToolRouting::Schedules;
    }

    // ── Wikipedia / Knowledge ───────────────────────────────────────
    const KNOWLEDGE_PREFIXES: &[&str] = &[
        "who is", "who was", "who are",
        "what is", "what are", "what was", "what were",
        "where is", "where are", "where was",
        "when was", "when did", "when is",
        "tell me about", "explain ", "describe ",
        "how does", "how do", "how did",
        "look up", "search for", "search ", "define ",
        "can you tell me about",
    ];
    if KNOWLEDGE_PREFIXES.iter().any(|p| stripped.starts_with(p))
        || stripped.contains("wikipedia")
    {
        return ToolRouting::Wikipedia;
    }

    ToolRouting::None
}

/// Legacy helper — returns true when any tool is needed.
/// Kept for backward compatibility with answer reviewer auto-mode.
pub fn needs_tool_call(message: &str) -> bool {
    route_to_tool(message) != ToolRouting::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_greeting_is_chat() {
        assert_eq!(classify_request("What time is it?"), ModelRole::Chat);
        assert_eq!(classify_request("Hello"), ModelRole::Chat);
        assert_eq!(classify_request("Tell me a joke"), ModelRole::Chat);
    }

    #[test]
    fn think_keywords_route_to_think() {
        assert_eq!(classify_request("explain why the sky is blue"), ModelRole::Think);
        assert_eq!(classify_request("analyze this situation"), ModelRole::Think);
        assert_eq!(classify_request("compare these two options"), ModelRole::Think);
        assert_eq!(classify_request("what causes inflation"), ModelRole::Think);
        assert_eq!(classify_request("pros and cons of remote work"), ModelRole::Think);
        assert_eq!(classify_request("step by step how to bake bread"), ModelRole::Think);
    }

    #[test]
    fn think_prefix_overrides_all() {
        // Even if task keywords are present, "think about" prefix wins
        assert_eq!(classify_request("think about scheduling my week"), ModelRole::Think);
        assert_eq!(classify_request("Goose, think about why I'm busy"), ModelRole::Think);
    }

    #[test]
    fn task_keywords_route_to_task() {
        assert_eq!(classify_request("remind me to take my meds at 9am"), ModelRole::Task);
        assert_eq!(classify_request("schedule a meeting tomorrow"), ModelRole::Task);
        assert_eq!(classify_request("turn on the living room lights"), ModelRole::Task);
        assert_eq!(classify_request("set alarm for 7am"), ModelRole::Task);
        assert_eq!(classify_request("search for the latest news"), ModelRole::Task);
        assert_eq!(classify_request("list devices"), ModelRole::Task);
    }

    #[test]
    fn wake_word_prefix_is_stripped() {
        assert_eq!(classify_request("Goose, explain why we sleep"), ModelRole::Think);
        assert_eq!(classify_request("hey goose, remind me at noon"), ModelRole::Task);
        assert_eq!(classify_request("Hey Goose, what is the weather"), ModelRole::Chat);
    }

    #[test]
    fn think_beats_task_in_body() {
        // If a message has both think and task keywords, Think wins
        assert_eq!(
            classify_request("analyze and then schedule the best approach"),
            ModelRole::Think
        );
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(classify_request("EXPLAIN WHY dogs bark"), ModelRole::Think);
        assert_eq!(classify_request("SCHEDULE a meeting"), ModelRole::Task);
    }

    // ── route_to_tool tests ─────────────────────────────────────────

    #[test]
    fn route_weather() {
        assert_eq!(route_to_tool("What's the weather like?"), ToolRouting::Weather);
        assert_eq!(route_to_tool("How's the temperature outside?"), ToolRouting::Weather);
        assert_eq!(route_to_tool("Will it rain tomorrow?"), ToolRouting::Weather);
        assert_eq!(route_to_tool("Is it raining?"), ToolRouting::Weather);
    }

    #[test]
    fn route_wikipedia() {
        assert_eq!(route_to_tool("Who is Albert Einstein?"), ToolRouting::Wikipedia);
        assert_eq!(route_to_tool("What is photosynthesis?"), ToolRouting::Wikipedia);
        assert_eq!(route_to_tool("Tell me about black holes"), ToolRouting::Wikipedia);
        assert_eq!(route_to_tool("Define osmosis"), ToolRouting::Wikipedia);
    }

    #[test]
    fn route_save_memory() {
        assert_eq!(route_to_tool("Remember that I love pizza"), ToolRouting::SaveMemory);
        assert_eq!(route_to_tool("Don't forget my birthday is March 5"), ToolRouting::SaveMemory);
        assert_eq!(route_to_tool("My name is Jerry"), ToolRouting::SaveMemory);
        assert_eq!(route_to_tool("I prefer dark mode"), ToolRouting::SaveMemory);
        assert_eq!(route_to_tool("I'm allergic to peanuts"), ToolRouting::SaveMemory);
        assert_eq!(route_to_tool("I live in Nairobi"), ToolRouting::SaveMemory);
    }

    #[test]
    fn route_recall_memory() {
        assert_eq!(route_to_tool("Do you remember what food I like?"), ToolRouting::RecallMemory);
        assert_eq!(route_to_tool("What's my name?"), ToolRouting::RecallMemory);
        assert_eq!(route_to_tool("What did I tell you about my job?"), ToolRouting::RecallMemory);
        assert_eq!(route_to_tool("What do you know about me?"), ToolRouting::RecallMemory);
    }

    #[test]
    fn route_devices() {
        assert_eq!(route_to_tool("List devices"), ToolRouting::Devices);
        assert_eq!(route_to_tool("What devices are connected?"), ToolRouting::Devices);
        assert_eq!(route_to_tool("Turn on the living room lights"), ToolRouting::Devices);
    }

    #[test]
    fn route_schedules() {
        assert_eq!(route_to_tool("Show my schedules"), ToolRouting::Schedules);
        assert_eq!(route_to_tool("What's scheduled for today?"), ToolRouting::Schedules);
        assert_eq!(route_to_tool("List schedules"), ToolRouting::Schedules);
    }

    #[test]
    fn route_create_schedule() {
        assert_eq!(route_to_tool("Schedule to get the weather at 10 am"), ToolRouting::CreateSchedule);
        assert_eq!(route_to_tool("Every day at 8 AM, give me a briefing"), ToolRouting::CreateSchedule);
        assert_eq!(route_to_tool("Remind me every morning to stretch"), ToolRouting::CreateSchedule);
        assert_eq!(route_to_tool("Set alarm for 7am"), ToolRouting::CreateSchedule);
    }

    #[test]
    fn route_none_for_general_conversation() {
        assert_eq!(route_to_tool("Hello!"), ToolRouting::None);
        assert_eq!(route_to_tool("Tell me a joke"), ToolRouting::None);
        assert_eq!(route_to_tool("Thanks"), ToolRouting::None);
        assert_eq!(route_to_tool("Would you recommend drinking milk?"), ToolRouting::None);
        assert_eq!(route_to_tool("Help me debug this code"), ToolRouting::None);
        assert_eq!(route_to_tool("Write me a poem"), ToolRouting::None);
    }

    #[test]
    fn route_with_wake_word() {
        assert_eq!(route_to_tool("Goose, what's the weather?"), ToolRouting::Weather);
        assert_eq!(route_to_tool("Hey goose, remember I like sushi"), ToolRouting::SaveMemory);
    }

    #[test]
    fn needs_tool_call_compat() {
        assert!(needs_tool_call("What's the weather?"));
        assert!(needs_tool_call("Who is Einstein?"));
        assert!(!needs_tool_call("Hello"));
        assert!(!needs_tool_call("Tell me a joke"));
    }
}
