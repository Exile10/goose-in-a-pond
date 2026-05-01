//! Tool call schemas for validating LLM-generated tool invocations.
//!
//! Small local models (3B-4B) often produce malformed JSON when
//! attempting tool calls. This module defines the expected schema
//! for each GIAP tool so the validator can check field presence
//! after repairing structural JSON issues.
//!
//! Pure Rust, no external dependencies beyond `serde_json` for
//! field-presence checks against parsed JSON values.

/// Expected schema for a GIAP tool's arguments.
#[derive(Debug, Clone)]
pub struct ToolSchema {
    /// Tool name as registered in MCP (e.g. "weather", "save_memory").
    pub name: &'static str,
    /// Fields that must be present in the arguments object.
    pub required_fields: &'static [&'static str],
    /// Fields that may be present but are not mandatory.
    pub optional_fields: &'static [&'static str],
}

/// All known GIAP tool schemas.
const SCHEMAS: &[ToolSchema] = &[
    ToolSchema {
        name: "weather",
        required_fields: &["location"],
        optional_fields: &[],
    },
    ToolSchema {
        name: "save_memory",
        required_fields: &["content"],
        optional_fields: &["segment", "importance", "tier"],
    },
    ToolSchema {
        name: "recall_memories",
        required_fields: &["query"],
        optional_fields: &["limit"],
    },
    ToolSchema {
        name: "forget_memory",
        required_fields: &["query"],
        optional_fields: &[],
    },
    ToolSchema {
        name: "list_schedules",
        required_fields: &[],
        optional_fields: &[],
    },
    ToolSchema {
        name: "create_schedule",
        required_fields: &["prompt", "cron"],
        optional_fields: &["name", "description"],
    },
    ToolSchema {
        name: "delete_schedule",
        required_fields: &["id"],
        optional_fields: &[],
    },
    ToolSchema {
        name: "list_devices",
        required_fields: &[],
        optional_fields: &[],
    },
    ToolSchema {
        name: "run_schedule_now",
        required_fields: &["id"],
        optional_fields: &[],
    },
    ToolSchema {
        name: "pause_schedule",
        required_fields: &["id"],
        optional_fields: &[],
    },
    ToolSchema {
        name: "resume_schedule",
        required_fields: &["id"],
        optional_fields: &[],
    },
    ToolSchema {
        name: "get_schedule_runs",
        required_fields: &["id"],
        optional_fields: &["limit"],
    },
];

/// Look up the schema for a tool by name.
///
/// Returns `None` for unknown tools, which means "no schema constraints
/// to enforce" (the tool call passes validation by default).
pub fn get_schema(tool_name: &str) -> Option<&'static ToolSchema> {
    SCHEMAS.iter().find(|s| s.name == tool_name)
}

/// Validate a JSON arguments string against the schema for `tool_name`.
///
/// Returns a list of human-readable error strings. An empty list means
/// all required fields are present. Unknown tool names produce no errors
/// (treated as unconstrained).
pub fn validate_against_schema(tool_name: &str, args_json: &str) -> Vec<String> {
    let schema = match get_schema(tool_name) {
        Some(s) => s,
        None => return Vec::new(), // unknown tool = no constraints
    };

    // If the tool has no required fields, any valid JSON object is acceptable.
    if schema.required_fields.is_empty() {
        return Vec::new();
    }

    let parsed: serde_json::Value = match serde_json::from_str(args_json) {
        Ok(v) => v,
        Err(e) => return vec![format!("failed to parse arguments JSON: {e}")],
    };

    let obj = match parsed.as_object() {
        Some(o) => o,
        None => return vec!["arguments must be a JSON object".to_string()],
    };

    let mut errors = Vec::new();
    for &field in schema.required_fields {
        if !obj.contains_key(field) {
            errors.push(format!("missing required field: {field}"));
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_schemas_are_found() {
        assert!(get_schema("weather").is_some());
        assert!(get_schema("save_memory").is_some());
        assert!(get_schema("recall_memories").is_some());
        assert!(get_schema("forget_memory").is_some());
        assert!(get_schema("list_schedules").is_some());
        assert!(get_schema("create_schedule").is_some());
        assert!(get_schema("list_devices").is_some());
    }

    #[test]
    fn unknown_tool_returns_none() {
        assert!(get_schema("nonexistent_tool").is_none());
        assert!(get_schema("").is_none());
    }

    #[test]
    fn valid_weather_args_pass() {
        let errors = validate_against_schema("weather", r#"{"location": "Nairobi"}"#);
        assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
    }

    #[test]
    fn missing_required_field_detected() {
        let errors = validate_against_schema("weather", r#"{"city": "Nairobi"}"#);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("location"));
    }

    #[test]
    fn multiple_missing_fields_detected() {
        let errors = validate_against_schema("create_schedule", r#"{"name": "test"}"#);
        assert_eq!(errors.len(), 2);
        assert!(errors.iter().any(|e| e.contains("prompt")));
        assert!(errors.iter().any(|e| e.contains("cron")));
    }

    #[test]
    fn save_memory_with_optional_fields() {
        let args = r#"{"content": "I like pizza", "segment": "Preference", "importance": 0.7}"#;
        let errors = validate_against_schema("save_memory", args);
        assert!(errors.is_empty());
    }

    #[test]
    fn save_memory_missing_content() {
        let errors = validate_against_schema("save_memory", r#"{"segment": "Preference"}"#);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("content"));
    }

    #[test]
    fn unknown_tool_passes_validation() {
        let errors = validate_against_schema("unknown_tool", r#"{"anything": "goes"}"#);
        assert!(errors.is_empty());
    }

    #[test]
    fn invalid_json_args_returns_error() {
        let errors = validate_against_schema("weather", "not json at all");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("failed to parse"));
    }

    #[test]
    fn non_object_json_returns_error() {
        let errors = validate_against_schema("weather", r#"["Nairobi"]"#);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("must be a JSON object"));
    }

    #[test]
    fn no_required_fields_tool_always_passes() {
        let errors = validate_against_schema("list_schedules", r#"{}"#);
        assert!(errors.is_empty());
        let errors = validate_against_schema("list_devices", r#"{"extra": true}"#);
        assert!(errors.is_empty());
    }

    #[test]
    fn schema_has_correct_fields() {
        let s = get_schema("save_memory").unwrap();
        assert_eq!(s.required_fields, &["content"]);
        assert!(s.optional_fields.contains(&"segment"));
        assert!(s.optional_fields.contains(&"importance"));
        assert!(s.optional_fields.contains(&"tier"));
    }
}
