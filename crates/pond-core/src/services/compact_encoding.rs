//! Compact TOON-style encoding for structured data injected into LLM prompts.
//!
//! On Jetson Orin Nano with 3K-8K context windows, every token matters.
//! This module encodes memories, tool results, and key-value data into a
//! token-efficient format that LLMs can still parse reliably.
//!
//! Typical savings: 30-60% fewer tokens compared to verbose JSON or
//! bullet-point formats.
//!
//! # Encoding formats
//!
//! ## Memories
//! ```text
//! [identity:0.80] User is a data scientist
//! [preference:0.70] Prefers dark mode
//! ```
//! One line per memory, sorted by importance descending. The segment tag
//! and importance score give the LLM ranking context without verbose labels.
//!
//! ## Tool results (key-value)
//! ```text
//! # devices
//! Living Room Light:on
//! Kitchen Sensor:idle
//! ```
//!
//! ## Tool results (tabular)
//! ```text
//! # schedules
//! Morning Brief|08:30 daily|active
//! Weather Check|*/6h|paused
//! ```

use crate::domain::memory::MemoryFragment;

/// Encode memory fragments into a compact, one-line-per-memory format.
///
/// Output format: `[segment:importance] content`
/// Memories are sorted by importance descending so the LLM sees the most
/// relevant facts first (important for truncation under tight budgets).
///
/// Returns an empty string if `memories` is empty.
pub fn encode_memories_compact(memories: &[MemoryFragment]) -> String {
    if memories.is_empty() {
        return String::new();
    }

    // Sort by importance descending; memories without importance sort last.
    let mut sorted: Vec<&MemoryFragment> = memories.iter().collect();
    sorted.sort_by(|a, b| {
        let imp_a = a.importance.unwrap_or(0.0);
        let imp_b = b.importance.unwrap_or(0.0);
        imp_b
            .partial_cmp(&imp_a)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut buf = String::with_capacity(sorted.len() * 60);
    for (i, m) in sorted.iter().enumerate() {
        if i > 0 {
            buf.push('\n');
        }

        let seg_label = m
            .segment
            .as_ref()
            .map(|s| format!("{:?}", s).to_lowercase())
            .unwrap_or_else(|| "general".to_string());

        let importance = m.importance.unwrap_or(0.5);
        // Format: [segment:importance] content
        buf.push('[');
        buf.push_str(&seg_label);
        buf.push(':');
        // Two decimal places — enough precision, minimal tokens
        buf.push_str(&format!("{importance:.2}"));
        buf.push_str("] ");
        buf.push_str(m.content.trim());
    }

    buf
}

/// Encode a tool result into a compact format.
///
/// The function inspects `result` to determine its shape:
/// - If it looks like a JSON array of objects, encodes as pipe-delimited rows
/// - If it looks like a JSON object, encodes as key:value pairs
/// - Otherwise returns the result with a header as-is (already compact)
///
/// The `tool_name` is used as a section header: `# tool_name`
pub fn encode_tool_result_compact(tool_name: &str, result: &str) -> String {
    let trimmed = result.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let header = format!("# {tool_name}");

    // Try JSON array of objects → pipe-delimited tabular
    if trimmed.starts_with('[') {
        if let Some(tabular) = try_encode_json_array(trimmed) {
            return format!("{header}\n{tabular}");
        }
    }

    // Try JSON object → key:value pairs
    if trimmed.starts_with('{') {
        if let Some(kv) = try_encode_json_object(trimmed) {
            return format!("{header}\n{kv}");
        }
    }

    // Fallback: return as-is with header
    format!("{header}\n{trimmed}")
}

/// Encode a list of key-value pairs into a compact format.
///
/// Output: one `key:value` per line. Returns empty string if `pairs` is empty.
pub fn encode_kv_compact(pairs: &[(String, String)]) -> String {
    if pairs.is_empty() {
        return String::new();
    }

    let mut buf = String::with_capacity(pairs.len() * 30);
    for (i, (key, value)) in pairs.iter().enumerate() {
        if i > 0 {
            buf.push('\n');
        }
        buf.push_str(key);
        buf.push(':');
        buf.push_str(value);
    }
    buf
}

// ── Internal helpers ────────────────────────────────────────────────────────

/// Try to parse a JSON array of objects and encode as pipe-delimited rows.
///
/// Uses a minimal JSON parser (no serde dependency in pond-core).
/// Only handles flat objects — nested values are stringified.
fn try_encode_json_array(input: &str) -> Option<String> {
    // Minimal JSON array-of-objects parser for pond-core (no serde_json).
    // We need to handle: [{"key":"val","key2":"val2"}, ...]
    // Strategy: hand-parse the outermost structure.

    let items = parse_json_array_of_objects(input)?;
    if items.is_empty() {
        return None;
    }

    // Collect all keys from the first object to define column order
    let keys: Vec<&str> = items[0].iter().map(|(k, _)| k.as_str()).collect();
    if keys.is_empty() {
        return None;
    }

    let mut buf = String::new();
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            buf.push('\n');
        }
        for (j, key) in keys.iter().enumerate() {
            if j > 0 {
                buf.push('|');
            }
            let val = item
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
                .unwrap_or("");
            buf.push_str(val);
        }
    }
    Some(buf)
}

/// Try to parse a JSON object and encode as key:value pairs.
fn try_encode_json_object(input: &str) -> Option<String> {
    let pairs = parse_json_object(input)?;
    if pairs.is_empty() {
        return None;
    }
    let kv_pairs: Vec<(String, String)> = pairs;
    Some(encode_kv_compact(&kv_pairs))
}

// ── Minimal JSON parsers (no external deps) ─────────────────────────────────
//
// These are intentionally simple — they handle the common cases GIAP produces
// (flat objects with string/number/bool values). They do NOT handle:
// - Escaped quotes within strings (beyond basic \")
// - Unicode escapes (\uXXXX)
// - Nested objects/arrays (values are taken as raw strings)

/// Parse a JSON array of flat objects into Vec<Vec<(key, value)>>.
fn parse_json_array_of_objects(input: &str) -> Option<Vec<Vec<(String, String)>>> {
    let trimmed = input.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return None;
    }
    let inner = &trimmed[1..trimmed.len() - 1];

    let mut results = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;

    // Split at top-level commas between objects
    for (i, ch) in inner.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let obj_str = inner[start..=i].trim();
                    if let Some(pairs) = parse_json_object(obj_str) {
                        results.push(pairs);
                    }
                    start = i + 1;
                }
            }
            ',' if depth == 0 => {
                start = i + 1;
            }
            _ => {}
        }
    }

    if results.is_empty() {
        None
    } else {
        Some(results)
    }
}

/// Parse a flat JSON object into Vec<(key, value)> preserving insertion order.
fn parse_json_object(input: &str) -> Option<Vec<(String, String)>> {
    let trimmed = input.trim();
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return None;
    }
    let inner = trimmed[1..trimmed.len() - 1].trim();
    if inner.is_empty() {
        return Some(Vec::new());
    }

    let mut pairs = Vec::new();
    let mut pos = 0;
    let bytes = inner.as_bytes();

    while pos < bytes.len() {
        // Skip whitespace and commas
        while pos < bytes.len()
            && (bytes[pos] == b' '
                || bytes[pos] == b','
                || bytes[pos] == b'\n'
                || bytes[pos] == b'\r'
                || bytes[pos] == b'\t')
        {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }

        // Parse key (must be a quoted string)
        if bytes[pos] != b'"' {
            return None;
        }
        let key = parse_quoted_string(inner, &mut pos)?;

        // Skip whitespace and colon
        while pos < bytes.len() && (bytes[pos] == b' ' || bytes[pos] == b'\t') {
            pos += 1;
        }
        if pos >= bytes.len() || bytes[pos] != b':' {
            return None;
        }
        pos += 1; // skip ':'
        while pos < bytes.len() && (bytes[pos] == b' ' || bytes[pos] == b'\t') {
            pos += 1;
        }

        // Parse value
        let value = parse_json_value(inner, &mut pos)?;

        pairs.push((key, value));
    }

    Some(pairs)
}

/// Parse a double-quoted JSON string, advancing `pos` past the closing quote.
fn parse_quoted_string(input: &str, pos: &mut usize) -> Option<String> {
    let bytes = input.as_bytes();
    if *pos >= bytes.len() || bytes[*pos] != b'"' {
        return None;
    }
    *pos += 1; // skip opening quote
    let start = *pos;
    let mut escaped = false;

    while *pos < bytes.len() {
        if escaped {
            escaped = false;
        } else if bytes[*pos] == b'\\' {
            escaped = true;
        } else if bytes[*pos] == b'"' {
            let s = &input[start..*pos];
            *pos += 1; // skip closing quote
                       // Basic unescape: \" → ", \\ → \, \n → newline
            let unescaped = s
                .replace("\\\"", "\"")
                .replace("\\\\", "\\")
                .replace("\\n", "\n");
            return Some(unescaped);
        }
        *pos += 1;
    }
    None // unterminated string
}

/// Parse a JSON value (string, number, bool, null), advancing `pos`.
/// Nested objects/arrays are captured as raw text.
fn parse_json_value(input: &str, pos: &mut usize) -> Option<String> {
    let bytes = input.as_bytes();
    if *pos >= bytes.len() {
        return None;
    }

    match bytes[*pos] {
        b'"' => parse_quoted_string(input, pos),
        b'{' | b'[' => {
            // Capture nested structure as raw text
            let start = *pos;
            let open = bytes[*pos];
            let close = if open == b'{' { b'}' } else { b']' };
            let mut depth = 1i32;
            *pos += 1;
            while *pos < bytes.len() && depth > 0 {
                match bytes[*pos] {
                    b if b == open => depth += 1,
                    b if b == close => depth -= 1,
                    b'"' => {
                        // Skip over strings to avoid counting braces inside them
                        *pos += 1;
                        while *pos < bytes.len() && bytes[*pos] != b'"' {
                            if bytes[*pos] == b'\\' {
                                *pos += 1; // skip escaped char
                            }
                            *pos += 1;
                        }
                    }
                    _ => {}
                }
                *pos += 1;
            }
            Some(input[start..*pos].to_string())
        }
        _ => {
            // Number, bool, null — read until delimiter
            let start = *pos;
            while *pos < bytes.len()
                && bytes[*pos] != b','
                && bytes[*pos] != b'}'
                && bytes[*pos] != b']'
                && bytes[*pos] != b' '
                && bytes[*pos] != b'\n'
                && bytes[*pos] != b'\r'
                && bytes[*pos] != b'\t'
            {
                *pos += 1;
            }
            let val = input[start..*pos].trim();
            if val == "null" {
                Some(String::new())
            } else {
                Some(val.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::memory::{MemoryFragment, MemorySegment};

    fn make_memory(
        content: &str,
        segment: Option<MemorySegment>,
        importance: Option<f32>,
    ) -> MemoryFragment {
        let mut m = MemoryFragment::from_chat(
            format!("mem-{}", content.len()),
            None,
            None,
            content.to_string(),
        );
        m.segment = segment;
        m.importance = importance;
        m
    }

    // ── Memory encoding tests ───────────────────────────────────────────────

    #[test]
    fn empty_memories_returns_empty() {
        assert_eq!(encode_memories_compact(&[]), "");
    }

    #[test]
    fn single_memory_with_segment_and_importance() {
        let m = make_memory(
            "User is a data scientist",
            Some(MemorySegment::Identity),
            Some(0.8),
        );
        let result = encode_memories_compact(&[m]);
        assert_eq!(result, "[identity:0.80] User is a data scientist");
    }

    #[test]
    fn memory_without_segment_uses_general() {
        let m = make_memory("Likes coffee", None, Some(0.5));
        let result = encode_memories_compact(&[m]);
        assert_eq!(result, "[general:0.50] Likes coffee");
    }

    #[test]
    fn memory_without_importance_defaults_to_half() {
        let m = make_memory("Some fact", Some(MemorySegment::Knowledge), None);
        let result = encode_memories_compact(&[m]);
        assert_eq!(result, "[knowledge:0.50] Some fact");
    }

    #[test]
    fn memories_sorted_by_importance_descending() {
        let memories = vec![
            make_memory("Low importance", Some(MemorySegment::Context), Some(0.3)),
            make_memory(
                "High importance",
                Some(MemorySegment::Correction),
                Some(0.9),
            ),
            make_memory(
                "Medium importance",
                Some(MemorySegment::Preference),
                Some(0.7),
            ),
        ];
        let result = encode_memories_compact(&memories);
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("High importance"));
        assert!(lines[1].contains("Medium importance"));
        assert!(lines[2].contains("Low importance"));
    }

    #[test]
    fn memory_content_is_trimmed() {
        let m = make_memory(
            "  extra whitespace  ",
            Some(MemorySegment::Knowledge),
            Some(0.5),
        );
        let result = encode_memories_compact(&[m]);
        assert!(result.ends_with("] extra whitespace"));
    }

    #[test]
    fn compact_memories_shorter_than_verbose() {
        let memories = vec![
            make_memory(
                "User is a data scientist",
                Some(MemorySegment::Identity),
                Some(0.8),
            ),
            make_memory(
                "Prefers dark mode",
                Some(MemorySegment::Preference),
                Some(0.7),
            ),
            make_memory(
                "Works at Acme Corp",
                Some(MemorySegment::Project),
                Some(0.6),
            ),
            make_memory(
                "My name is Jerry",
                Some(MemorySegment::Identity),
                Some(0.85),
            ),
            make_memory(
                "Uses Rust and Python",
                Some(MemorySegment::Knowledge),
                Some(0.5),
            ),
        ];

        let compact = encode_memories_compact(&memories);

        // Simulate the verbose format from goose_agent.rs
        let verbose = memories
            .iter()
            .map(|m| {
                let seg = m
                    .segment
                    .as_ref()
                    .map(|s| format!("{:?}", s).to_lowercase())
                    .unwrap_or_default();
                format!("- [{}] {}", seg, m.content)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let verbose_with_header = format!("Relevant memories:\n{verbose}");

        // Compact should be meaningfully shorter
        assert!(
            compact.len() < verbose_with_header.len(),
            "compact ({}) should be shorter than verbose ({})",
            compact.len(),
            verbose_with_header.len(),
        );
    }

    // ── Tool result encoding tests ──────────────────────────────────────────

    #[test]
    fn empty_tool_result_returns_empty() {
        assert_eq!(encode_tool_result_compact("devices", ""), "");
        assert_eq!(encode_tool_result_compact("devices", "   "), "");
    }

    #[test]
    fn json_object_encoded_as_kv() {
        let json = r#"{"name":"Living Room Light","status":"on","brightness":"80%"}"#;
        let result = encode_tool_result_compact("device", json);
        assert_eq!(
            result,
            "# device\nname:Living Room Light\nstatus:on\nbrightness:80%"
        );
    }

    #[test]
    fn json_array_encoded_as_pipe_delimited() {
        let json = r#"[{"name":"Living Room Light","status":"on"},{"name":"Kitchen Sensor","status":"idle"}]"#;
        let result = encode_tool_result_compact("devices", json);
        let expected = "# devices\nLiving Room Light|on\nKitchen Sensor|idle";
        assert_eq!(result, expected);
    }

    #[test]
    fn json_array_of_schedules() {
        let json = r#"[{"name":"Morning Brief","cron":"08:30 daily","status":"active"},{"name":"Weather Check","cron":"*/6h","status":"paused"}]"#;
        let result = encode_tool_result_compact("schedules", json);
        let expected = "# schedules\nMorning Brief|08:30 daily|active\nWeather Check|*/6h|paused";
        assert_eq!(result, expected);
    }

    #[test]
    fn plain_text_passes_through_with_header() {
        let result = encode_tool_result_compact("weather", "Sunny, 25C, humidity 40%");
        assert_eq!(result, "# weather\nSunny, 25C, humidity 40%");
    }

    #[test]
    fn tool_result_compact_shorter_than_json() {
        let json = r#"[{"name":"Living Room Light","status":"on","type":"smart_bulb"},{"name":"Kitchen Sensor","status":"idle","type":"motion"},{"name":"Thermostat","status":"heating","type":"hvac"}]"#;
        let compact = encode_tool_result_compact("devices", json);
        assert!(
            compact.len() < json.len(),
            "compact ({}) should be shorter than json ({})",
            compact.len(),
            json.len(),
        );
    }

    // ── Key-value encoding tests ────────────────────────────────────────────

    #[test]
    fn empty_kv_returns_empty() {
        assert_eq!(encode_kv_compact(&[]), "");
    }

    #[test]
    fn single_kv_pair() {
        let pairs = vec![("temp".to_string(), "25C".to_string())];
        assert_eq!(encode_kv_compact(&pairs), "temp:25C");
    }

    #[test]
    fn multiple_kv_pairs() {
        let pairs = vec![
            ("temp".to_string(), "25C".to_string()),
            ("humidity".to_string(), "40%".to_string()),
            ("wind".to_string(), "10 km/h NW".to_string()),
        ];
        let result = encode_kv_compact(&pairs);
        assert_eq!(result, "temp:25C\nhumidity:40%\nwind:10 km/h NW");
    }

    // ── JSON parser edge cases ──────────────────────────────────────────────

    #[test]
    fn json_object_with_numbers_and_bools() {
        let json = r#"{"temp":25,"humidity":40,"raining":false}"#;
        let result = encode_tool_result_compact("weather", json);
        assert_eq!(result, "# weather\ntemp:25\nhumidity:40\nraining:false");
    }

    #[test]
    fn json_object_with_null() {
        let json = r#"{"name":"Sensor","battery":null}"#;
        let result = encode_tool_result_compact("device", json);
        assert_eq!(result, "# device\nname:Sensor\nbattery:");
    }

    #[test]
    fn malformed_json_falls_through() {
        let bad = r#"{broken json"#;
        let result = encode_tool_result_compact("test", bad);
        assert_eq!(result, "# test\n{broken json");
    }

    #[test]
    fn json_with_escaped_quotes() {
        let json = r#"{"message":"He said \"hello\""}"#;
        let result = encode_tool_result_compact("chat", json);
        assert_eq!(result, "# chat\nmessage:He said \"hello\"");
    }

    // ── Readability / round-trip tests ──────────────────────────────────────

    #[test]
    fn compact_memory_format_is_human_readable() {
        let memories = vec![
            make_memory(
                "User's name is Jerry",
                Some(MemorySegment::Identity),
                Some(0.85),
            ),
            make_memory(
                "Prefers concise responses",
                Some(MemorySegment::Preference),
                Some(0.7),
            ),
            make_memory(
                "Working on GIAP project",
                Some(MemorySegment::Project),
                Some(0.6),
            ),
        ];
        let result = encode_memories_compact(&memories);

        // Each line should be parseable: [segment:score] content
        for line in result.lines() {
            assert!(line.starts_with('['), "line should start with '[': {line}");
            assert!(line.contains(':'), "line should contain ':': {line}");
            assert!(line.contains("] "), "line should contain '] ': {line}");

            // Extract segment and importance
            let bracket_end = line.find(']').unwrap();
            let tag = &line[1..bracket_end];
            let parts: Vec<&str> = tag.split(':').collect();
            assert_eq!(parts.len(), 2, "tag should be segment:score: {tag}");
            let _importance: f32 = parts[1].parse().expect("importance should be a float");
        }
    }

    #[test]
    fn compact_tool_result_is_human_readable() {
        let json = r#"[{"device":"Lamp","state":"on"},{"device":"Fan","state":"off"}]"#;
        let result = encode_tool_result_compact("devices", json);

        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines[0], "# devices");
        // Each data line should have pipe separators
        for line in &lines[1..] {
            assert!(line.contains('|'), "data line should contain '|': {line}");
        }
    }
}
