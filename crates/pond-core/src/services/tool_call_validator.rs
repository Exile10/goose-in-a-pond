//! Tool call validation and repair for malformed LLM outputs.
//!
//! Small local models (3B-4B) frequently produce broken JSON when attempting
//! tool calls. This module applies a pipeline of repair strategies to recover
//! valid JSON from common failure modes:
//!
//! 1. **Extract JSON** — find the first `{...}` block (models wrap JSON in markdown)
//! 2. **Fix trailing commas** — remove `,` before `}` or `]`
//! 3. **Fix single quotes** — replace `'` with `"` for JSON strings
//! 4. **Fix unquoted keys** — `{key: value}` -> `{"key": value}`
//! 5. **Fix missing closing braces** — append `}` if `{` count > `}` count
//! 6. **Validate structure** — parse and check for expected fields
//!
//! All operations are pure string manipulation + one `serde_json` parse.
//! No async, no I/O, no allocations beyond the repair buffer.

use super::tool_call_schemas;

/// Result of validating (and optionally repairing) a tool call output.
#[derive(Debug, Clone)]
pub struct ToolCallValidation {
    /// Whether the (possibly repaired) output is valid JSON with expected structure.
    pub is_valid: bool,
    /// The repaired JSON string, if any repairs were applied. `None` if the
    /// original was already valid or if repair failed entirely.
    pub repaired: Option<String>,
    /// Human-readable descriptions of issues found.
    pub errors: Vec<String>,
    /// The original raw output, preserved for logging/debugging.
    pub original: String,
}

/// Validate and attempt to repair a raw tool call output from an LLM.
///
/// Applies repair strategies in order, then validates the result as JSON.
/// Returns a `ToolCallValidation` describing what was found and fixed.
pub fn validate_and_repair_tool_call(raw_output: &str) -> ToolCallValidation {
    let original = raw_output.to_string();
    let mut errors: Vec<String> = Vec::new();
    let mut repaired = false;

    // Step 1: Extract JSON block (models often wrap in markdown or explanation)
    let mut json_str = match extract_json_block(raw_output) {
        Some(extracted) => {
            if extracted != raw_output.trim() {
                repaired = true;
                errors.push("extracted JSON from surrounding text".to_string());
            }
            extracted
        }
        None => {
            return ToolCallValidation {
                is_valid: false,
                repaired: None,
                errors: vec!["no JSON object found in output".to_string()],
                original,
            };
        }
    };

    // Step 2: Fix trailing commas (e.g. {"key": "value",})
    let fixed_commas = fix_trailing_commas(&json_str);
    if fixed_commas != json_str {
        repaired = true;
        errors.push("removed trailing comma(s)".to_string());
        json_str = fixed_commas;
    }

    // Step 3: Fix single quotes (e.g. {'key': 'value'})
    let fixed_quotes = fix_single_quotes(&json_str);
    if fixed_quotes != json_str {
        repaired = true;
        errors.push("replaced single quotes with double quotes".to_string());
        json_str = fixed_quotes;
    }

    // Step 4: Fix unquoted keys (e.g. {key: "value"})
    let fixed_keys = fix_unquoted_keys(&json_str);
    if fixed_keys != json_str {
        repaired = true;
        errors.push("quoted unquoted key(s)".to_string());
        json_str = fixed_keys;
    }

    // Step 5: Fix missing closing braces
    let fixed_braces = fix_missing_braces(&json_str);
    if fixed_braces != json_str {
        repaired = true;
        errors.push("added missing closing brace(s)".to_string());
        json_str = fixed_braces;
    }

    // Step 6: Try to parse as JSON
    match serde_json::from_str::<serde_json::Value>(&json_str) {
        Ok(value) => {
            // Additional structural validation: should be an object
            if !value.is_object() {
                errors.push("parsed JSON is not an object".to_string());
                return ToolCallValidation {
                    is_valid: false,
                    repaired: if repaired { Some(json_str) } else { None },
                    errors,
                    original,
                };
            }

            // If we know the tool name, validate against its schema
            if let Some(tool_name) = extract_tool_name_from_value(&value) {
                if let Some(args_str) = extract_tool_args_from_value(&value) {
                    let schema_errors =
                        tool_call_schemas::validate_against_schema(&tool_name, &args_str);
                    if !schema_errors.is_empty() {
                        errors.extend(schema_errors);
                        return ToolCallValidation {
                            is_valid: false,
                            repaired: if repaired { Some(json_str) } else { None },
                            errors,
                            original,
                        };
                    }
                }
            }

            ToolCallValidation {
                is_valid: true,
                repaired: if repaired { Some(json_str) } else { None },
                errors,
                original,
            }
        }
        Err(e) => {
            errors.push(format!("JSON parse failed after repairs: {e}"));
            ToolCallValidation {
                is_valid: false,
                repaired: None,
                errors,
                original,
            }
        }
    }
}

/// Extract the tool/function name from a JSON string.
///
/// Looks for keys: "tool", "name", "function" (in that priority order).
pub fn extract_tool_name(json_str: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json_str).ok()?;
    extract_tool_name_from_value(&value)
}

/// Extract tool arguments from a JSON string.
///
/// Looks for keys: "arguments", "args", "parameters", "params" (in priority order).
/// Returns the arguments as a JSON string.
pub fn extract_tool_args(json_str: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json_str).ok()?;
    extract_tool_args_from_value(&value)
}

// ── Internal helpers ────────────────────────────────────────────────────────

/// Extract tool name from a parsed JSON value.
fn extract_tool_name_from_value(value: &serde_json::Value) -> Option<String> {
    let obj = value.as_object()?;
    for key in &["tool", "name", "function"] {
        if let Some(serde_json::Value::String(s)) = obj.get(*key) {
            return Some(s.clone());
        }
    }
    None
}

/// Extract tool arguments from a parsed JSON value, returned as a JSON string.
fn extract_tool_args_from_value(value: &serde_json::Value) -> Option<String> {
    let obj = value.as_object()?;
    for key in &["arguments", "args", "parameters", "params"] {
        if let Some(v) = obj.get(*key) {
            // Arguments can be a string (already JSON) or an object
            return match v {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Object(_) => serde_json::to_string(v).ok(),
                _ => None,
            };
        }
    }
    None
}

/// Find and extract the first balanced `{...}` block from raw text.
///
/// Handles cases where the model wraps JSON in markdown code blocks,
/// explanatory text, or other decoration.
fn extract_json_block(raw: &str) -> Option<String> {
    // First, try stripping markdown code fences
    let stripped = strip_markdown_code_block(raw);
    let input = stripped.as_deref().unwrap_or(raw);

    // Find the first '{' and match it to its closing '}'
    let start = input.find('{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape_next = false;

    for (i, ch) in input[start..].char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        match ch {
            '\\' if in_string => {
                escape_next = true;
            }
            '"' => {
                in_string = !in_string;
            }
            '{' if !in_string => {
                depth += 1;
            }
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(input[start..start + i + 1].to_string());
                }
            }
            _ => {}
        }
    }

    // If we have unbalanced braces, return what we have from '{' to end
    // (the missing-brace fixer will handle it)
    if depth > 0 {
        Some(input[start..].to_string())
    } else {
        None
    }
}

/// Strip markdown code block fences from around JSON.
///
/// Handles `\`\`\`json\n...\n\`\`\`` and `\`\`\`\n...\n\`\`\`` patterns.
fn strip_markdown_code_block(raw: &str) -> Option<String> {
    let trimmed = raw.trim();

    // Check for ``` or ```json prefix
    if !trimmed.starts_with("```") {
        return None;
    }

    // Find end of opening fence line
    let first_line_end = trimmed.find('\n')?;
    let after_fence = &trimmed[first_line_end + 1..];

    // Strip closing fence
    let content = if let Some(stripped) = after_fence.strip_suffix("```") {
        stripped.trim()
    } else {
        after_fence.trim()
    };

    Some(content.to_string())
}

/// Remove trailing commas before `}` or `]`.
///
/// Handles `{"key": "value",}` -> `{"key": "value"}`.
fn fix_trailing_commas(json: &str) -> String {
    let mut result = String::with_capacity(json.len());
    let mut in_string = false;
    let mut escape_next = false;
    let chars: Vec<char> = json.chars().collect();

    for i in 0..chars.len() {
        let ch = chars[i];

        if escape_next {
            escape_next = false;
            result.push(ch);
            continue;
        }

        if ch == '\\' && in_string {
            escape_next = true;
            result.push(ch);
            continue;
        }

        if ch == '"' {
            in_string = !in_string;
            result.push(ch);
            continue;
        }

        if ch == ',' && !in_string {
            // Look ahead past whitespace for } or ]
            let rest = &chars[i + 1..];
            let next_non_ws = rest.iter().find(|c| !c.is_whitespace());
            if matches!(next_non_ws, Some('}') | Some(']')) {
                // Skip this comma
                continue;
            }
        }

        result.push(ch);
    }

    result
}

/// Replace single quotes with double quotes for JSON strings.
///
/// Handles `{'key': 'value'}` -> `{"key": "value"}`.
/// Preserves single quotes inside double-quoted strings. Outside of
/// double-quoted strings, all single quotes are treated as structural
/// JSON delimiters -- this is the correct behavior for LLM tool outputs
/// where single-quoted JSON is a Python-style formatting error, not
/// natural language with apostrophes.
fn fix_single_quotes(json: &str) -> String {
    let mut result = String::with_capacity(json.len());
    let mut in_double_string = false;
    let mut escape_next = false;

    for ch in json.chars() {
        if escape_next {
            escape_next = false;
            result.push(ch);
            continue;
        }

        if ch == '\\' && in_double_string {
            escape_next = true;
            result.push(ch);
            continue;
        }

        if ch == '"' {
            in_double_string = !in_double_string;
            result.push(ch);
            continue;
        }

        if ch == '\'' && !in_double_string {
            result.push('"');
            continue;
        }

        result.push(ch);
    }

    result
}

/// Fix unquoted keys in JSON-like strings.
///
/// Handles `{key: "value"}` -> `{"key": "value"}`.
/// Only operates outside of string contexts.
fn fix_unquoted_keys(json: &str) -> String {
    let mut result = String::with_capacity(json.len());
    let mut in_string = false;
    let mut escape_next = false;
    let chars: Vec<char> = json.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];

        if escape_next {
            escape_next = false;
            result.push(ch);
            i += 1;
            continue;
        }

        if ch == '\\' && in_string {
            escape_next = true;
            result.push(ch);
            i += 1;
            continue;
        }

        if ch == '"' {
            in_string = !in_string;
            result.push(ch);
            i += 1;
            continue;
        }

        if !in_string && (ch == '{' || ch == ',') {
            result.push(ch);
            i += 1;

            // Skip whitespace after { or ,
            while i < chars.len() && chars[i].is_whitespace() {
                result.push(chars[i]);
                i += 1;
            }

            // Check if we have an unquoted key (alphanumeric/underscore followed
            // eventually by a colon)
            if i < chars.len() && (chars[i].is_alphabetic() || chars[i] == '_') {
                // Collect the potential key
                let key_start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let key = &chars[key_start..i];

                // Skip whitespace after key
                let mut colon_idx = i;
                while colon_idx < chars.len() && chars[colon_idx].is_whitespace() {
                    colon_idx += 1;
                }

                if colon_idx < chars.len() && chars[colon_idx] == ':' {
                    // This is an unquoted key — wrap it in quotes
                    result.push('"');
                    for &c in key {
                        result.push(c);
                    }
                    result.push('"');
                    // Push any whitespace between key and colon
                    for idx in i..colon_idx {
                        result.push(chars[idx]);
                    }
                    i = colon_idx;
                } else {
                    // Not a key:value pattern — push chars as-is
                    for &c in key {
                        result.push(c);
                    }
                }
            }
            continue;
        }

        result.push(ch);
        i += 1;
    }

    result
}

/// Fix missing closing braces by appending `}` or `]` as needed.
fn fix_missing_braces(json: &str) -> String {
    let mut brace_depth = 0i32;
    let mut bracket_depth = 0i32;
    let mut in_string = false;
    let mut escape_next = false;

    for ch in json.chars() {
        if escape_next {
            escape_next = false;
            continue;
        }
        match ch {
            '\\' if in_string => {
                escape_next = true;
            }
            '"' => {
                in_string = !in_string;
            }
            '{' if !in_string => brace_depth += 1,
            '}' if !in_string => brace_depth -= 1,
            '[' if !in_string => bracket_depth += 1,
            ']' if !in_string => bracket_depth -= 1,
            _ => {}
        }
    }

    let mut result = json.to_string();
    // Close any unclosed brackets first (inner), then braces (outer)
    for _ in 0..bracket_depth {
        result.push(']');
    }
    for _ in 0..brace_depth {
        result.push('}');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Valid JSON passes through unchanged ──────────────────────────

    #[test]
    fn valid_json_passes_through() {
        let input = r#"{"tool": "weather", "arguments": {"location": "Nairobi"}}"#;
        let result = validate_and_repair_tool_call(input);
        assert!(result.is_valid);
        assert!(result.repaired.is_none(), "valid JSON should not be repaired");
        assert!(result.errors.is_empty());
    }

    #[test]
    fn valid_json_no_tool_name_still_valid() {
        let input = r#"{"action": "greet", "message": "hello"}"#;
        let result = validate_and_repair_tool_call(input);
        assert!(result.is_valid);
        assert!(result.repaired.is_none());
    }

    // ── JSON wrapped in markdown code block ─────────────────────────

    #[test]
    fn json_in_markdown_code_block() {
        let input = "```json\n{\"tool\": \"weather\", \"arguments\": {\"location\": \"Nairobi\"}}\n```";
        let result = validate_and_repair_tool_call(input);
        assert!(result.is_valid);
        assert!(result.repaired.is_some());
        assert!(result.errors.iter().any(|e| e.contains("extracted")));
    }

    #[test]
    fn json_in_plain_markdown_block() {
        let input = "```\n{\"tool\": \"weather\"}\n```";
        let result = validate_and_repair_tool_call(input);
        assert!(result.is_valid);
        assert!(result.repaired.is_some());
    }

    #[test]
    fn json_with_surrounding_text() {
        let input = "Sure! Here's the tool call:\n{\"tool\": \"weather\", \"arguments\": {\"location\": \"Nairobi\"}}\nLet me know if you need anything else.";
        let result = validate_and_repair_tool_call(input);
        assert!(result.is_valid);
        assert!(result.repaired.is_some());
    }

    // ── Trailing comma repair ───────────────────────────────────────

    #[test]
    fn trailing_comma_fixed() {
        let input = r#"{"tool": "weather", "arguments": {"location": "Nairobi",}}"#;
        let result = validate_and_repair_tool_call(input);
        assert!(result.is_valid);
        assert!(result.repaired.is_some());
        assert!(result.errors.iter().any(|e| e.contains("trailing comma")));
    }

    #[test]
    fn trailing_comma_in_array() {
        let input = r#"{"items": ["a", "b", "c",]}"#;
        let result = validate_and_repair_tool_call(input);
        assert!(result.is_valid);
        assert!(result.repaired.is_some());
    }

    // ── Single quote repair ─────────────────────────────────────────

    #[test]
    fn single_quotes_repaired() {
        let input = "{'tool': 'weather', 'arguments': {'location': 'Nairobi'}}";
        let result = validate_and_repair_tool_call(input);
        assert!(result.is_valid);
        assert!(result.repaired.is_some());
        assert!(result.errors.iter().any(|e| e.contains("single quotes")));
    }

    // ── Unquoted keys repair ────────────────────────────────────────

    #[test]
    fn unquoted_keys_fixed() {
        let input = r#"{tool: "weather", arguments: {location: "Nairobi"}}"#;
        let result = validate_and_repair_tool_call(input);
        assert!(result.is_valid);
        assert!(result.repaired.is_some());
        assert!(result.errors.iter().any(|e| e.contains("unquoted key")));
    }

    // ── Missing closing brace ───────────────────────────────────────

    #[test]
    fn missing_closing_brace_added() {
        let input = r#"{"tool": "weather", "arguments": {"location": "Nairobi"}"#;
        let result = validate_and_repair_tool_call(input);
        assert!(result.is_valid);
        assert!(result.repaired.is_some());
        assert!(result.errors.iter().any(|e| e.contains("closing brace")));
    }

    #[test]
    fn multiple_missing_braces() {
        let input = r#"{"tool": "weather", "arguments": {"location": "Nairobi""#;
        let result = validate_and_repair_tool_call(input);
        assert!(result.is_valid);
        assert!(result.repaired.is_some());
    }

    // ── Completely invalid output ───────────────────────────────────

    #[test]
    fn completely_invalid_output() {
        let input = "I don't understand what you want.";
        let result = validate_and_repair_tool_call(input);
        assert!(!result.is_valid);
        assert!(result.repaired.is_none());
        assert!(!result.errors.is_empty());
        assert!(result.errors.iter().any(|e| e.contains("no JSON object")));
    }

    #[test]
    fn empty_input() {
        let result = validate_and_repair_tool_call("");
        assert!(!result.is_valid);
        assert!(result.repaired.is_none());
    }

    // ── Tool name extraction ────────────────────────────────────────

    #[test]
    fn extract_tool_name_from_tool_key() {
        let json = r#"{"tool": "weather", "arguments": {}}"#;
        assert_eq!(extract_tool_name(json), Some("weather".to_string()));
    }

    #[test]
    fn extract_tool_name_from_name_key() {
        let json = r#"{"name": "save_memory", "args": {}}"#;
        assert_eq!(extract_tool_name(json), Some("save_memory".to_string()));
    }

    #[test]
    fn extract_tool_name_from_function_key() {
        let json = r#"{"function": "recall_memories", "parameters": {}}"#;
        assert_eq!(extract_tool_name(json), Some("recall_memories".to_string()));
    }

    #[test]
    fn extract_tool_name_priority_order() {
        // "tool" should take priority over "name"
        let json = r#"{"tool": "weather", "name": "other"}"#;
        assert_eq!(extract_tool_name(json), Some("weather".to_string()));
    }

    #[test]
    fn extract_tool_name_missing() {
        let json = r#"{"action": "something"}"#;
        assert_eq!(extract_tool_name(json), None);
    }

    #[test]
    fn extract_tool_name_invalid_json() {
        assert_eq!(extract_tool_name("not json"), None);
    }

    // ── Tool args extraction ────────────────────────────────────────

    #[test]
    fn extract_args_from_arguments_key() {
        let json = r#"{"tool": "weather", "arguments": {"location": "Nairobi"}}"#;
        let args = extract_tool_args(json).unwrap();
        assert!(args.contains("Nairobi"));
    }

    #[test]
    fn extract_args_from_args_key() {
        let json = r#"{"tool": "weather", "args": {"location": "Nairobi"}}"#;
        let args = extract_tool_args(json).unwrap();
        assert!(args.contains("Nairobi"));
    }

    #[test]
    fn extract_args_from_parameters_key() {
        let json = r#"{"function": "weather", "parameters": {"location": "Nairobi"}}"#;
        let args = extract_tool_args(json).unwrap();
        assert!(args.contains("Nairobi"));
    }

    #[test]
    fn extract_args_string_value() {
        let json = r#"{"tool": "weather", "arguments": "{\"location\": \"Nairobi\"}"}"#;
        let args = extract_tool_args(json).unwrap();
        assert!(args.contains("Nairobi"));
    }

    #[test]
    fn extract_args_missing() {
        let json = r#"{"tool": "weather"}"#;
        assert_eq!(extract_tool_args(json), None);
    }

    // ── Schema validation integration ───────────────────────────────

    #[test]
    fn valid_tool_call_with_schema_validation() {
        let input = r#"{"tool": "weather", "arguments": {"location": "Nairobi"}}"#;
        let result = validate_and_repair_tool_call(input);
        assert!(result.is_valid);
    }

    #[test]
    fn tool_call_missing_required_args() {
        let input = r#"{"tool": "weather", "arguments": {"city": "Nairobi"}}"#;
        let result = validate_and_repair_tool_call(input);
        assert!(!result.is_valid);
        assert!(result.errors.iter().any(|e| e.contains("location")));
    }

    #[test]
    fn tool_call_unknown_tool_passes() {
        let input = r#"{"tool": "custom_tool", "arguments": {"anything": true}}"#;
        let result = validate_and_repair_tool_call(input);
        assert!(result.is_valid);
    }

    // ── Combined repairs ────────────────────────────────────────────

    #[test]
    fn multiple_repairs_applied() {
        // Single quotes + trailing comma + surrounding text
        let input = "Here's the call: {'tool': 'weather', 'arguments': {'location': 'Nairobi',}}";
        let result = validate_and_repair_tool_call(input);
        assert!(result.is_valid);
        assert!(result.repaired.is_some());
        assert!(result.errors.len() >= 2, "expected multiple repairs: {:?}", result.errors);
    }

    #[test]
    fn unquoted_keys_plus_missing_brace() {
        let input = r#"{tool: "weather", arguments: {location: "Nairobi"}"#;
        let result = validate_and_repair_tool_call(input);
        assert!(result.is_valid);
        assert!(result.repaired.is_some());
    }

    // ── Edge cases ──────────────────────────────────────────────────

    #[test]
    fn json_with_nested_strings_containing_braces() {
        let input = r#"{"tool": "save_memory", "arguments": {"content": "Use { and } in code"}}"#;
        let result = validate_and_repair_tool_call(input);
        assert!(result.is_valid);
        assert!(result.repaired.is_none());
    }

    #[test]
    fn json_with_escaped_quotes() {
        let input = r#"{"tool": "save_memory", "arguments": {"content": "He said \"hello\""}}"#;
        let result = validate_and_repair_tool_call(input);
        assert!(result.is_valid);
    }

    #[test]
    fn preserves_original() {
        let input = "totally broken garbage";
        let result = validate_and_repair_tool_call(input);
        assert_eq!(result.original, input);
    }

    // ── Internal helper tests ───────────────────────────────────────

    #[test]
    fn extract_json_block_basic() {
        let raw = r#"Some text {"key": "value"} more text"#;
        let extracted = extract_json_block(raw).unwrap();
        assert_eq!(extracted, r#"{"key": "value"}"#);
    }

    #[test]
    fn extract_json_block_nested() {
        let raw = r#"{"outer": {"inner": true}}"#;
        let extracted = extract_json_block(raw).unwrap();
        assert_eq!(extracted, raw);
    }

    #[test]
    fn extract_json_block_no_json() {
        assert!(extract_json_block("no json here").is_none());
    }

    #[test]
    fn fix_trailing_commas_preserves_commas_in_strings() {
        let input = r#"{"msg": "a, b,"}"#;
        let fixed = fix_trailing_commas(input);
        assert_eq!(fixed, input); // commas inside strings are preserved
    }

    #[test]
    fn fix_single_quotes_preserves_content_in_double_quoted_strings() {
        // Single quotes inside double-quoted strings are untouched
        let input = r#"{"msg": "don't worry"}"#;
        let fixed = fix_single_quotes(input);
        assert_eq!(fixed, input);
    }
}
