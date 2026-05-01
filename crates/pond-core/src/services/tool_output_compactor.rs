//! Semantic tool output compaction for constrained context windows.
//!
//! Tool outputs (weather, Wikipedia, schedules, devices) are injected into
//! the LLM context as part of the augmented user message. On Jetson Orin Nano
//! with 3K-8K token context windows, every token counts.
//!
//! This module provides `compact_tool_output()` — a pure function that
//! compresses raw tool output into concise summaries while preserving key
//! facts. No I/O, no external deps, no LLM calls.
//!
//! ## Token savings (typical)
//!
//! | Tool       | Raw size  | Compacted  | Savings |
//! |------------|-----------|------------|---------|
//! | Weather    | ~150 ch   | ~150 ch    | 0% (already compact) |
//! | Wikipedia  | 500-2000  | ~400 ch    | 60-80%  |
//! | Schedules  | ~200/item | ~60/item   | 70%     |
//! | Devices    | ~80/item  | ~30/item   | 62%     |

/// Maximum length for the fallback truncation of unknown tool outputs.
const FALLBACK_MAX_CHARS: usize = 200;

/// Maximum number of sentences to keep from Wikipedia articles.
const WIKI_MAX_SENTENCES: usize = 3;

/// Maximum character length for compacted Wikipedia output.
const WIKI_MAX_CHARS: usize = 500;

/// Compact a tool's raw output into a concise summary.
///
/// The function selects a per-tool compaction strategy based on `tool_name`,
/// preserving key information while minimising token usage. Unknown tools
/// get a simple truncation fallback.
///
/// This is a pure function — no I/O, no async, no allocations beyond the
/// returned `String`.
pub fn compact_tool_output(tool_name: &str, raw_output: &str) -> String {
    if raw_output.is_empty() {
        return String::new();
    }

    match tool_name {
        "weather" => compact_weather(raw_output),
        "wikipedia" => compact_wikipedia(raw_output),
        "schedules" => compact_schedules(raw_output),
        "devices" => compact_devices(raw_output),
        "recall_memory" => compact_memories(raw_output),
        "save_memory" => raw_output.to_string(), // confirmations are already terse
        "create_schedule" => raw_output.to_string(), // confirmations are already terse
        _ => compact_fallback(raw_output),
    }
}

/// Weather output from `WeatherData::as_context_block()` is already compact
/// (~150 chars). Pass through unchanged.
fn compact_weather(raw: &str) -> String {
    // The weather adapter already formats as a single-line summary:
    // "[Current Weather — Location]\nCondition | Temp | Humidity | Wind | Precip"
    // No further compaction needed.
    raw.to_string()
}

/// Extract the first N sentences from a Wikipedia article summary.
///
/// Wikipedia tool output is raw article text that can be 500-2000+ chars.
/// We keep the first few sentences (the lead, which Wikipedia's own style
/// guide says should define the topic) and discard the rest.
fn compact_wikipedia(raw: &str) -> String {
    // Handle "No saved memories found." style messages
    if raw.len() <= WIKI_MAX_CHARS {
        return raw.to_string();
    }

    let sentences = extract_sentences(raw, WIKI_MAX_SENTENCES);
    if sentences.is_empty() {
        return truncate_safe(raw, WIKI_MAX_CHARS);
    }

    let result = sentences.join(" ");
    if result.len() > WIKI_MAX_CHARS {
        truncate_safe(&result, WIKI_MAX_CHARS)
    } else {
        result
    }
}

/// Condense schedule listings to essential fields.
///
/// Input format (from `try_list_schedules()`):
/// ```text
/// - Label [id]: 0 30 8 * * * Africa/Nairobi (agent, active)
/// ```
///
/// Output: compact name + next-time style:
/// ```text
/// - Label: 08:30 daily (active)
/// ```
fn compact_schedules(raw: &str) -> String {
    if raw.starts_with("No scheduled") {
        return raw.to_string();
    }

    let lines: Vec<&str> = raw.lines().collect();
    let mut compacted = Vec::with_capacity(lines.len());

    for line in &lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Try to parse the structured format: "- Label [id]: cron tz (kind, status)"
        if let Some(compact_line) = compact_schedule_line(trimmed) {
            compacted.push(compact_line);
        } else {
            // Unrecognized format — keep but trim
            compacted.push(truncate_safe(trimmed, 80));
        }
    }

    compacted.join("\n")
}

/// Compact a single schedule line.
///
/// Input:  `- Label [uuid]: 0 30 8 * * * Africa/Nairobi (agent, active)`
/// Output: `- Label: 08:30 daily (active)`
fn compact_schedule_line(line: &str) -> Option<String> {
    let content = line.strip_prefix("- ")?;

    // Split at " [" to get label
    let (label, rest) = content.split_once(" [")?;

    // Skip past the ID ("]:")
    let (_, after_id) = rest.split_once("]: ")?;

    // Extract status from parenthesized suffix
    let status = after_id
        .rfind('(')
        .and_then(|start| {
            after_id[start..].strip_prefix('(').and_then(|s| s.strip_suffix(')'))
        })
        .and_then(|inner| {
            // inner is "agent, active" — extract the status part
            inner.split(", ").last()
        })
        .unwrap_or("active");

    // Extract cron expression (between ]: and the timezone/parenthesized part)
    let cron_part = after_id
        .rfind('(')
        .map(|pos| after_id[..pos].trim())
        .unwrap_or(after_id.trim());

    let schedule_desc = humanize_cron(cron_part);

    Some(format!("- {}: {} ({})", label, schedule_desc, status))
}

/// Convert a cron expression + timezone into a human-readable schedule.
///
/// Input examples:
/// - `"0 30 8 * * * Africa/Nairobi"` -> `"08:30 daily"`
/// - `"0 0 * * * * UTC"` -> `"hourly"`
/// - `"0 0 9 * * 1 UTC"` -> `"09:00 weekly"`
fn humanize_cron(cron_with_tz: &str) -> String {
    let parts: Vec<&str> = cron_with_tz.split_whitespace().collect();
    if parts.len() < 6 {
        return cron_with_tz.to_string();
    }

    // 6-field cron: sec min hour dom month dow [tz]
    let min = parts[1];
    let hour = parts[2];
    let dom = parts[3];
    let dow = parts[5];

    // Hourly: "0 0 * * * *"
    if hour == "*" && dom == "*" && dow == "*" {
        return "hourly".to_string();
    }

    // Build time string
    let time = format!(
        "{}:{}",
        hour.parse::<u32>().map(|h| format!("{:02}", h)).unwrap_or_else(|_| hour.to_string()),
        min.parse::<u32>().map(|m| format!("{:02}", m)).unwrap_or_else(|_| min.to_string()),
    );

    // Daily: specific hour, all days
    if dom == "*" && dow == "*" {
        return format!("{} daily", time);
    }

    // Weekly: specific dow
    if dom == "*" && dow != "*" {
        return format!("{} weekly", time);
    }

    // Monthly: specific dom
    if dom != "*" && dow == "*" {
        return format!("{} monthly", time);
    }

    format!("{} (custom)", time)
}

/// Condense device listings to name + status pairs.
///
/// Input format (from `try_list_devices()`):
/// ```text
/// - Living Room Light (smart_light): online
/// - Kitchen Sensor (temperature_sensor): offline
/// ```
///
/// Output:
/// ```text
/// - Living Room Light: online
/// - Kitchen Sensor: offline
/// ```
fn compact_devices(raw: &str) -> String {
    if raw.starts_with("No devices") {
        return raw.to_string();
    }

    let lines: Vec<&str> = raw.lines().collect();
    let mut compacted = Vec::with_capacity(lines.len());

    for line in &lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(compact_line) = compact_device_line(trimmed) {
            compacted.push(compact_line);
        } else {
            compacted.push(trimmed.to_string());
        }
    }

    compacted.join("\n")
}

/// Compact a single device line.
///
/// Input:  `- Living Room Light (smart_light): online`
/// Output: `- Living Room Light: online`
fn compact_device_line(line: &str) -> Option<String> {
    let content = line.strip_prefix("- ")?;

    // Split at " (" to get name, then find status after "): "
    let (name, rest) = content.split_once(" (")?;
    let (_, status) = rest.split_once("): ")?;

    Some(format!("- {}: {}", name, status))
}

/// Compact memory recall output.
///
/// Input format:
/// ```text
/// - [2024-01-15] User prefers dark mode
/// - [2024-01-10] User's name is Jerry
/// ```
///
/// Output: strip dates for brevity (the LLM doesn't need them):
/// ```text
/// - User prefers dark mode
/// - User's name is Jerry
/// ```
fn compact_memories(raw: &str) -> String {
    if raw.starts_with("No saved") || raw.starts_with("No memories") {
        return raw.to_string();
    }

    let lines: Vec<&str> = raw.lines().collect();
    let mut compacted = Vec::with_capacity(lines.len());

    for line in &lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Strip "- [date] " prefix pattern
        if let Some(content) = trimmed.strip_prefix("- ") {
            if content.starts_with('[') {
                if let Some(after_bracket) = content.split_once("] ") {
                    compacted.push(format!("- {}", after_bracket.1));
                    continue;
                }
            }
        }

        compacted.push(trimmed.to_string());
    }

    compacted.join("\n")
}

/// Fallback: truncate to [`FALLBACK_MAX_CHARS`] for unknown tools.
fn compact_fallback(raw: &str) -> String {
    truncate_safe(raw, FALLBACK_MAX_CHARS)
}

/// Truncate a string at a char boundary, appending "..." if truncated.
fn truncate_safe(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }

    // Find the last char boundary at or before max
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }

    format!("{}...", &s[..end])
}

/// Extract the first `n` sentences from text.
///
/// A sentence ends with `. `, `! `, `? `, or is the terminal `.`/`!`/`?`.
/// Handles abbreviations conservatively — a period followed by an uppercase
/// letter is treated as a sentence boundary.
fn extract_sentences(text: &str, n: usize) -> Vec<String> {
    if n == 0 || text.is_empty() {
        return vec![];
    }

    let mut sentences = Vec::new();
    let mut start = 0;
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();

    let mut i = 0;
    while i < len && sentences.len() < n {
        let ch = chars[i];

        if (ch == '.' || ch == '!' || ch == '?') && i + 1 < len {
            let next = chars[i + 1];
            // Sentence boundary: punctuation followed by space+uppercase or end
            if next == ' ' || next == '\n' {
                // Check if next non-space char is uppercase (new sentence)
                let next_content = chars.iter().skip(i + 2).find(|c| !c.is_whitespace());
                if next_content.map_or(true, |c| c.is_uppercase()) {
                    let byte_end = chars[..=i].iter().map(|c| c.len_utf8()).sum::<usize>();
                    let byte_start = chars[..start].iter().map(|c| c.len_utf8()).sum::<usize>();
                    let sentence = text[byte_start..byte_end].trim().to_string();
                    if !sentence.is_empty() {
                        sentences.push(sentence);
                    }
                    // Skip whitespace after sentence-ending punctuation
                    while i + 1 < len && chars[i + 1].is_whitespace() {
                        i += 1;
                    }
                    start = i + 1;
                }
            }
        } else if (ch == '.' || ch == '!' || ch == '?') && i + 1 == len {
            // Terminal punctuation at end of text
            let byte_end = chars[..=i].iter().map(|c| c.len_utf8()).sum::<usize>();
            let byte_start = chars[..start].iter().map(|c| c.len_utf8()).sum::<usize>();
            let sentence = text[byte_start..byte_end].trim().to_string();
            if !sentence.is_empty() {
                sentences.push(sentence);
            }
            start = i + 1;
        }

        i += 1;
    }

    // If we didn't find enough sentence boundaries, include remaining text
    if sentences.is_empty() && start < len {
        let byte_start = chars[..start].iter().map(|c| c.len_utf8()).sum::<usize>();
        let remaining = text[byte_start..].trim().to_string();
        if !remaining.is_empty() {
            sentences.push(remaining);
        }
    }

    sentences
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Weather ─────────────────────────────────────────────────────────

    #[test]
    fn weather_output_passes_through_unchanged() {
        let weather = "[Current Weather \u{2014} Nairobi, KE]\n\
            Partly cloudy | 24.3\u{b0}C (feels like 23.1\u{b0}C) | Humidity: 68% | Wind: 12 km/h | Precip: 0.0 mm";
        let result = compact_tool_output("weather", weather);
        assert_eq!(result, weather);
    }

    // ── Wikipedia ───────────────────────────────────────────────────────

    #[test]
    fn wikipedia_short_article_passes_through() {
        let short = "Rust is a programming language. It focuses on safety and performance.";
        let result = compact_tool_output("wikipedia", short);
        assert_eq!(result, short);
    }

    #[test]
    fn wikipedia_long_article_is_compacted() {
        // Simulate a ~1000 char Wikipedia article
        let article = "Albert Einstein was a German-born theoretical physicist. \
            He is widely held to be one of the greatest physicists of all time. \
            Einstein is best known for developing the theory of relativity. \
            He also made important contributions to quantum mechanics. \
            His work on the photoelectric effect earned him the Nobel Prize in Physics in 1921. \
            Einstein published more than 300 scientific papers and more than 150 non-scientific works. \
            His intellectual achievements and originality have made the word Einstein synonymous with genius. \
            He was born in Ulm, in the Kingdom of Württemberg in the German Empire, on 14 March 1879. \
            He moved to Switzerland in 1895, giving up his German citizenship the following year.";

        let result = compact_tool_output("wikipedia", &article);

        // Should keep first 3 sentences
        assert!(result.contains("Albert Einstein was a German-born theoretical physicist."));
        assert!(result.contains("He is widely held to be one of the greatest physicists of all time."));
        assert!(result.contains("Einstein is best known for developing the theory of relativity."));

        // Should NOT include later sentences
        assert!(!result.contains("He was born in Ulm"));

        // Should be significantly shorter
        assert!(result.len() < article.len());
    }

    #[test]
    fn wikipedia_preserves_key_facts_within_budget() {
        let article = "Photosynthesis is a biological process. Plants use sunlight to convert carbon dioxide and water into glucose and oxygen. This process occurs in the chloroplasts of plant cells.";
        let result = compact_tool_output("wikipedia", &article);
        assert!(result.contains("Photosynthesis"));
        assert!(result.contains("biological process"));
    }

    #[test]
    fn wikipedia_very_long_article_stays_under_limit() {
        // Generate a very long article (5000+ chars)
        let mut article = String::new();
        for i in 0..50 {
            article.push_str(&format!(
                "This is sentence number {} about a very important topic. ",
                i
            ));
        }
        let result = compact_tool_output("wikipedia", &article);
        assert!(result.len() <= WIKI_MAX_CHARS + 3); // +3 for "..."
    }

    // ── Schedules ───────────────────────────────────────────────────────

    #[test]
    fn schedule_compaction_preserves_names_and_times() {
        let schedules = "- Morning briefing [abc-123]: 0 30 8 * * * Africa/Nairobi (agent, active)\n\
                         - Weather check [def-456]: 0 0 * * * * UTC (agent, active)";
        let result = compact_tool_output("schedules", schedules);

        // Names preserved
        assert!(result.contains("Morning briefing"));
        assert!(result.contains("Weather check"));

        // Times humanized
        assert!(result.contains("08:30 daily"));
        assert!(result.contains("hourly"));

        // Status preserved
        assert!(result.contains("active"));

        // UUID stripped
        assert!(!result.contains("abc-123"));
        assert!(!result.contains("def-456"));

        // Shorter
        assert!(result.len() < schedules.len());
    }

    #[test]
    fn schedule_no_tasks_passes_through() {
        let empty = "No scheduled tasks.";
        let result = compact_tool_output("schedules", empty);
        assert_eq!(result, empty);
    }

    #[test]
    fn schedule_weekly_is_humanized() {
        let schedule = "- Report [id-1]: 0 0 9 * * 1 UTC (agent, active)";
        let result = compact_tool_output("schedules", &schedule);
        assert!(result.contains("09:00 weekly"));
    }

    // ── Devices ─────────────────────────────────────────────────────────

    #[test]
    fn device_compaction_strips_type_keeps_status() {
        let devices = "- Living Room Light (smart_light): online\n\
                       - Kitchen Sensor (temperature_sensor): offline";
        let result = compact_tool_output("devices", devices);

        // Names preserved
        assert!(result.contains("Living Room Light"));
        assert!(result.contains("Kitchen Sensor"));

        // Status preserved
        assert!(result.contains("online"));
        assert!(result.contains("offline"));

        // Device type stripped
        assert!(!result.contains("smart_light"));
        assert!(!result.contains("temperature_sensor"));
    }

    #[test]
    fn device_no_devices_passes_through() {
        let empty = "No devices registered.";
        let result = compact_tool_output("devices", empty);
        assert_eq!(result, empty);
    }

    // ── Memories ────────────────────────────────────────────────────────

    #[test]
    fn memory_compaction_strips_dates() {
        let memories = "- [2024-01-15] User prefers dark mode\n\
                        - [2024-01-10] User's name is Jerry";
        let result = compact_tool_output("recall_memory", memories);
        assert!(result.contains("User prefers dark mode"));
        assert!(result.contains("User's name is Jerry"));
        assert!(!result.contains("2024-01-15"));
        assert!(!result.contains("2024-01-10"));
    }

    #[test]
    fn memory_no_memories_passes_through() {
        let empty = "No saved memories found.";
        let result = compact_tool_output("recall_memory", empty);
        assert_eq!(result, empty);
    }

    // ── Unknown tool / fallback ─────────────────────────────────────────

    #[test]
    fn unknown_tool_truncates_to_fallback_limit() {
        let long_output = "x".repeat(500);
        let result = compact_tool_output("unknown_tool", &long_output);
        assert!(result.len() <= FALLBACK_MAX_CHARS + 3); // +3 for "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn unknown_tool_short_output_passes_through() {
        let short = "Some short result.";
        let result = compact_tool_output("unknown_tool", short);
        assert_eq!(result, short);
    }

    // ── Empty output ────────────────────────────────────────────────────

    #[test]
    fn empty_output_returns_empty() {
        assert_eq!(compact_tool_output("weather", ""), "");
        assert_eq!(compact_tool_output("wikipedia", ""), "");
        assert_eq!(compact_tool_output("unknown", ""), "");
    }

    // ── Sentence extraction ─────────────────────────────────────────────

    #[test]
    fn extract_sentences_basic() {
        let text = "First sentence. Second sentence. Third sentence.";
        let result = extract_sentences(text, 2);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], "First sentence.");
        assert_eq!(result[1], "Second sentence.");
    }

    #[test]
    fn extract_sentences_with_abbreviations() {
        // "Dr." shouldn't be treated as sentence end when followed by lowercase
        let text = "The U.S. is a country. It has 50 states.";
        let result = extract_sentences(text, 2);
        // Should handle this reasonably
        assert!(!result.is_empty());
    }

    #[test]
    fn extract_sentences_no_punctuation() {
        let text = "A paragraph without any sentence-ending punctuation";
        let result = extract_sentences(text, 2);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], text);
    }

    // ── Truncation safety ───────────────────────────────────────────────

    #[test]
    fn truncate_safe_handles_multibyte() {
        let text = "Hello \u{1f600} world!"; // emoji is 4 bytes
        let result = truncate_safe(text, 8);
        // Should not panic or split in the middle of the emoji
        assert!(result.len() <= 11); // 8 + "..."
    }

    // ── Humanize cron ───────────────────────────────────────────────────

    #[test]
    fn humanize_cron_daily() {
        assert_eq!(humanize_cron("0 30 8 * * *"), "08:30 daily");
    }

    #[test]
    fn humanize_cron_hourly() {
        assert_eq!(humanize_cron("0 0 * * * *"), "hourly");
    }

    #[test]
    fn humanize_cron_weekly() {
        assert_eq!(humanize_cron("0 0 9 * * 1 UTC"), "09:00 weekly");
    }

    #[test]
    fn humanize_cron_monthly() {
        assert_eq!(humanize_cron("0 0 9 15 * * UTC"), "09:00 monthly");
    }
}
