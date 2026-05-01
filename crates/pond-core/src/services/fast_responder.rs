//! Deterministic fast-path responder for trivial queries.
//!
//! Handles greetings, farewells, thanks, and acknowledgments in <10ms
//! without invoking the LLM. Conservative matching ensures only
//! near-exact phrases match — messages with substantive content
//! (e.g. "Hello, what's the weather?") fall through to the LLM.
//!
//! No I/O, no async. Fully deterministic and testable.

/// Category of trivial message that was fast-path matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FastPathCategory {
    Greeting,
    Farewell,
    Acknowledgment,
    Thanks,
}

/// Result of a successful fast-path match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FastPathResult {
    pub response: String,
    pub category: FastPathCategory,
}

/// Canned responses for each category.
const GREETING_RESPONSE: &str = "Hello! How can I help you today?";
const FAREWELL_RESPONSE: &str = "Goodbye! Feel free to come back anytime.";
const THANKS_RESPONSE: &str = "You're welcome! Let me know if you need anything else.";
const ACKNOWLEDGMENT_RESPONSE: &str = "Understood. What would you like to do next?";

/// Known greeting phrases (after normalization).
const GREETINGS: &[&str] = &[
    "hi",
    "hello",
    "hey",
    "howdy",
    "good morning",
    "good afternoon",
    "good evening",
    "morning",
    "afternoon",
    "evening",
];

/// Optional filler words allowed after a greeting root.
/// e.g. "hi there", "hello everyone", "hey all"
const GREETING_FILLERS: &[&str] = &["there", "everyone", "all", "friend"];

/// Known farewell phrases (after normalization).
const FAREWELLS: &[&str] = &[
    "bye",
    "goodbye",
    "good bye",
    "see you",
    "see ya",
    "good night",
    "goodnight",
    "later",
    "cya",
    "farewell",
];

/// Known thanks phrases (after normalization).
const THANKS: &[&str] = &[
    "thanks",
    "thank you",
    "thx",
    "ty",
    "thanks a lot",
    "thank you so much",
    "thanks so much",
    "much appreciated",
    "many thanks",
];

/// Known acknowledgment phrases (after normalization).
const ACKNOWLEDGMENTS: &[&str] = &[
    "ok",
    "okay",
    "k",
    "got it",
    "understood",
    "sure",
    "alright",
    "all right",
    "noted",
    "cool",
    "right",
    "yep",
    "yup",
    "yes",
    "yeah",
    "np",
    "no problem",
];

/// Strip trailing punctuation characters (., !, ?, ...) from the message.
fn strip_trailing_punctuation(s: &str) -> &str {
    s.trim_end_matches(|c: char| c == '.' || c == '!' || c == '?' || c == ',')
}

/// Try to match a message to a fast-path response.
///
/// Returns `Some(FastPathResult)` if the message is trivial and can be
/// answered deterministically. Returns `None` if the message has
/// substantive content and should go through the LLM pipeline.
///
/// Matching is conservative: only near-exact phrases match. The input is
/// normalized (lowercase, trimmed, trailing punctuation stripped) before
/// comparison. A small set of filler words is allowed after greeting roots.
pub fn try_fast_path(message: &str) -> Option<FastPathResult> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lower = trimmed.to_lowercase();
    let normalized = strip_trailing_punctuation(&lower).trim();

    if normalized.is_empty() {
        return None;
    }

    // Check thanks first — "thanks" is unambiguous and common
    if THANKS.iter().any(|&phrase| normalized == phrase) {
        return Some(FastPathResult {
            response: THANKS_RESPONSE.to_string(),
            category: FastPathCategory::Thanks,
        });
    }

    // Check greetings — allow optional filler word after root
    if GREETINGS.iter().any(|&phrase| normalized == phrase) {
        return Some(FastPathResult {
            response: GREETING_RESPONSE.to_string(),
            category: FastPathCategory::Greeting,
        });
    }

    // Check "greeting + filler" pattern: "hi there", "hello everyone", etc.
    for &greeting in GREETINGS {
        if let Some(rest) = normalized.strip_prefix(greeting) {
            let rest = rest.trim();
            if !rest.is_empty() && GREETING_FILLERS.iter().any(|&f| rest == f) {
                return Some(FastPathResult {
                    response: GREETING_RESPONSE.to_string(),
                    category: FastPathCategory::Greeting,
                });
            }
        }
    }

    // Check farewells
    if FAREWELLS.iter().any(|&phrase| normalized == phrase) {
        return Some(FastPathResult {
            response: FAREWELL_RESPONSE.to_string(),
            category: FastPathCategory::Farewell,
        });
    }

    // Check acknowledgments last — broadest category
    if ACKNOWLEDGMENTS.iter().any(|&phrase| normalized == phrase) {
        return Some(FastPathResult {
            response: ACKNOWLEDGMENT_RESPONSE.to_string(),
            category: FastPathCategory::Acknowledgment,
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Greetings ──────────────────────────────────────────────────

    #[test]
    fn hello_matches_greeting() {
        let result = try_fast_path("hello").unwrap();
        assert_eq!(result.category, FastPathCategory::Greeting);
        assert_eq!(result.response, GREETING_RESPONSE);
    }

    #[test]
    fn hi_there_matches_greeting() {
        let result = try_fast_path("Hi there").unwrap();
        assert_eq!(result.category, FastPathCategory::Greeting);
    }

    #[test]
    fn hey_matches_greeting() {
        let result = try_fast_path("hey").unwrap();
        assert_eq!(result.category, FastPathCategory::Greeting);
    }

    #[test]
    fn good_morning_matches_greeting() {
        let result = try_fast_path("Good Morning!").unwrap();
        assert_eq!(result.category, FastPathCategory::Greeting);
    }

    #[test]
    fn good_afternoon_matches_greeting() {
        let result = try_fast_path("good afternoon").unwrap();
        assert_eq!(result.category, FastPathCategory::Greeting);
    }

    #[test]
    fn good_evening_matches_greeting() {
        let result = try_fast_path("Good evening.").unwrap();
        assert_eq!(result.category, FastPathCategory::Greeting);
    }

    #[test]
    fn hello_with_exclamation_matches() {
        let result = try_fast_path("Hello!").unwrap();
        assert_eq!(result.category, FastPathCategory::Greeting);
    }

    #[test]
    fn hello_everyone_matches_greeting() {
        let result = try_fast_path("hello everyone").unwrap();
        assert_eq!(result.category, FastPathCategory::Greeting);
    }

    #[test]
    fn hello_with_substantive_content_does_not_match() {
        assert!(try_fast_path("Hello, what's the weather?").is_none());
    }

    #[test]
    fn hello_can_you_help_does_not_match() {
        assert!(try_fast_path("Hello, can you help me with something?").is_none());
    }

    #[test]
    fn hi_how_are_you_does_not_match() {
        assert!(try_fast_path("Hi, how are you?").is_none());
    }

    // ── Farewells ──────────────────────────────────────────────────

    #[test]
    fn goodbye_matches_farewell() {
        let result = try_fast_path("goodbye").unwrap();
        assert_eq!(result.category, FastPathCategory::Farewell);
        assert_eq!(result.response, FAREWELL_RESPONSE);
    }

    #[test]
    fn bye_matches_farewell() {
        let result = try_fast_path("Bye!").unwrap();
        assert_eq!(result.category, FastPathCategory::Farewell);
    }

    #[test]
    fn see_you_matches_farewell() {
        let result = try_fast_path("see you").unwrap();
        assert_eq!(result.category, FastPathCategory::Farewell);
    }

    #[test]
    fn good_night_matches_farewell() {
        let result = try_fast_path("Good night").unwrap();
        assert_eq!(result.category, FastPathCategory::Farewell);
    }

    #[test]
    fn goodbye_with_extra_content_does_not_match() {
        assert!(try_fast_path("Goodbye, but before you go, what time is it?").is_none());
    }

    // ── Thanks ─────────────────────────────────────────────────────

    #[test]
    fn thanks_matches() {
        let result = try_fast_path("thanks").unwrap();
        assert_eq!(result.category, FastPathCategory::Thanks);
        assert_eq!(result.response, THANKS_RESPONSE);
    }

    #[test]
    fn thank_you_matches() {
        let result = try_fast_path("Thank you").unwrap();
        assert_eq!(result.category, FastPathCategory::Thanks);
    }

    #[test]
    fn thx_matches() {
        let result = try_fast_path("thx").unwrap();
        assert_eq!(result.category, FastPathCategory::Thanks);
    }

    #[test]
    fn ty_matches() {
        let result = try_fast_path("ty").unwrap();
        assert_eq!(result.category, FastPathCategory::Thanks);
    }

    #[test]
    fn thank_you_with_question_does_not_match() {
        assert!(try_fast_path("Thank you, can you also check my schedule?").is_none());
    }

    #[test]
    fn thanks_a_lot_matches() {
        let result = try_fast_path("Thanks a lot!").unwrap();
        assert_eq!(result.category, FastPathCategory::Thanks);
    }

    // ── Acknowledgments ────────────────────────────────────────────

    #[test]
    fn ok_matches_acknowledgment() {
        let result = try_fast_path("ok").unwrap();
        assert_eq!(result.category, FastPathCategory::Acknowledgment);
        assert_eq!(result.response, ACKNOWLEDGMENT_RESPONSE);
    }

    #[test]
    fn okay_matches_acknowledgment() {
        let result = try_fast_path("Okay").unwrap();
        assert_eq!(result.category, FastPathCategory::Acknowledgment);
    }

    #[test]
    fn got_it_matches_acknowledgment() {
        let result = try_fast_path("Got it").unwrap();
        assert_eq!(result.category, FastPathCategory::Acknowledgment);
    }

    #[test]
    fn understood_matches_acknowledgment() {
        let result = try_fast_path("understood").unwrap();
        assert_eq!(result.category, FastPathCategory::Acknowledgment);
    }

    #[test]
    fn sure_matches_acknowledgment() {
        let result = try_fast_path("Sure!").unwrap();
        assert_eq!(result.category, FastPathCategory::Acknowledgment);
    }

    #[test]
    fn alright_matches_acknowledgment() {
        let result = try_fast_path("alright").unwrap();
        assert_eq!(result.category, FastPathCategory::Acknowledgment);
    }

    // ── Non-matches (substantive content) ──────────────────────────

    #[test]
    fn quantum_physics_does_not_match() {
        assert!(try_fast_path("Tell me about quantum physics").is_none());
    }

    #[test]
    fn empty_string_does_not_match() {
        assert!(try_fast_path("").is_none());
    }

    #[test]
    fn whitespace_only_does_not_match() {
        assert!(try_fast_path("   ").is_none());
    }

    #[test]
    fn punctuation_only_does_not_match() {
        assert!(try_fast_path("...").is_none());
    }

    #[test]
    fn question_does_not_match() {
        assert!(try_fast_path("What time is it?").is_none());
    }

    #[test]
    fn command_does_not_match() {
        assert!(try_fast_path("Turn on the lights").is_none());
    }

    #[test]
    fn help_request_does_not_match() {
        assert!(try_fast_path("Help me debug this code").is_none());
    }

    // ── Edge cases ─────────────────────────────────────────────────

    #[test]
    fn mixed_case_matches() {
        let result = try_fast_path("HELLO").unwrap();
        assert_eq!(result.category, FastPathCategory::Greeting);
    }

    #[test]
    fn extra_whitespace_matches() {
        let result = try_fast_path("  hello  ").unwrap();
        assert_eq!(result.category, FastPathCategory::Greeting);
    }

    #[test]
    fn multiple_punctuation_stripped() {
        let result = try_fast_path("thanks!!!").unwrap();
        assert_eq!(result.category, FastPathCategory::Thanks);
    }

    #[test]
    fn trailing_period_stripped() {
        let result = try_fast_path("ok.").unwrap();
        assert_eq!(result.category, FastPathCategory::Acknowledgment);
    }

    #[test]
    fn ok_but_with_continuation_does_not_match() {
        assert!(try_fast_path("Ok, but can you explain more?").is_none());
    }

    #[test]
    fn sure_with_continuation_does_not_match() {
        assert!(try_fast_path("Sure, go ahead and do that").is_none());
    }

    #[test]
    fn howdy_matches_greeting() {
        let result = try_fast_path("Howdy!").unwrap();
        assert_eq!(result.category, FastPathCategory::Greeting);
    }

    #[test]
    fn cya_matches_farewell() {
        let result = try_fast_path("cya").unwrap();
        assert_eq!(result.category, FastPathCategory::Farewell);
    }

    #[test]
    fn yep_matches_acknowledgment() {
        let result = try_fast_path("yep").unwrap();
        assert_eq!(result.category, FastPathCategory::Acknowledgment);
    }

    #[test]
    fn no_problem_matches_acknowledgment() {
        let result = try_fast_path("no problem").unwrap();
        assert_eq!(result.category, FastPathCategory::Acknowledgment);
    }
}
