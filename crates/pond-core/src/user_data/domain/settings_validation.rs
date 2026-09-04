//! What a settings value is allowed to be — in one table, on the server.
//!
//! The real rules lived in the desktop: `settings/validation.ts` and the
//! `validate:` entries in `settings/catalogue.ts`. Its own header said as much
//! — that for several fields it was "the ONLY place a bad value is ever
//! reported". The server checked four, by hand, inside the settings route.
//!
//! So the two sides answered "is this valid?" differently, and the backend
//! stored what the browser rejected. Anything that is not the catalogue — a
//! `curl`, the CLI, a future mobile client — met no rule at all, and the
//! consumers downstream do not error on a bad value, they take an undisclosed
//! fallback:
//!
//! - `quiet_hours_start = "10pm"` does not parse, `quiet_hours_cover` fails
//!   CLOSED, and the pond goes silent for the whole day with nothing anywhere
//!   saying why.
//! - `agent_backend = "gosse"` passes the old denylist (which refused only the
//!   literal `pond`), survives the startup heal (which repairs only that same
//!   literal), and then fails the `!= "goose"` test that picks the real agent —
//!   so every request is answered by `MockAgent`, which presents as a model
//!   failure and gets debugged as one.
//! - `weather_latitude = 999` is stored and asked of the forecast API.
//!
//! # Refuse at the edge, because the edge is the only place anybody finds out
//!
//! This mirrors the argument the `network_mode` and `reasoning_effort` arms
//! already made in the route: both parse with a permissive fallback, so a typo
//! that reached the store would silently be the wrong setting. Refusing when it
//! arrives is the narrowing half of that bargain.
//!
//! # Canonicalise, don't only refuse
//!
//! `africa/nairobi` and `AUTO` are reasonable things to type and unreasonable
//! things to reject. A [`Check`] may rewrite the value; the STORED form is
//! always the canonical one, so everything downstream parses a single shape.

use std::collections::HashSet;

use serde_json::Value;

use crate::user_data::domain::settings::{
    Settings, AGENT_BACKENDS, CONSOLIDATION_MODES, EMBEDDING_PROVIDERS, NETWORK_MODES,
    PROMPT_STYLES, QUARANTINED_AGENT_BACKENDS, REASONING_EFFORTS, REVIEW_MODES,
    SECURITY_POLICY_MODES, THINKING_MODES, TOOL_SELECTION_MODES, TTS_QUALITIES,
};
use crate::user_data::services::location::normalize_zone;

/// One refusal, addressed to the field that caused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub field: String,
    /// Phrased for the person who typed it: what is wrong and what would work.
    pub message: String,
}

/// What one field permits.
#[derive(Debug, Clone, Copy)]
pub enum Check {
    /// Exactly one of these, matched after trimming and lower-casing.
    OneOf(&'static [&'static str]),
    /// An IANA zone, canonicalised through the zone catalogue.
    IanaZone,
    /// A local 24-hour time, canonicalised to zero-padded `HH:MM`.
    ClockTime,
    /// A URL whose scheme is one of these.
    Url {
        schemes: &'static [&'static str],
        example: &'static str,
    },
    /// A finite number within range.
    Number {
        min: f64,
        max: f64,
        integer: bool,
        unit: &'static str,
    },
}

/// A field and the rule it answers to.
pub struct FieldRule {
    pub key: &'static str,
    pub check: Check,
    /// Whether an empty value is accepted without running `check`. An optional
    /// URL is blank when unset; a time zone never is.
    pub blank_ok: bool,
}

const fn url(schemes: &'static [&'static str], example: &'static str) -> Check {
    Check::Url { schemes, example }
}
const fn int(min: f64, max: f64, unit: &'static str) -> Check {
    Check::Number {
        min,
        max,
        integer: true,
        unit,
    }
}
const fn real(min: f64, max: f64, unit: &'static str) -> Check {
    Check::Number {
        min,
        max,
        integer: false,
        unit,
    }
}

const HTTP: &[&str] = &["http://", "https://"];
const WS: &[&str] = &["ws://", "wss://"];

/// Every rule the pond has.
///
/// A field absent from this table is accepted as sent — which is the right
/// default for free text, but is a decision rather than an oversight: the
/// completeness guard in this module's tests fails the build when a new field
/// is neither ruled nor explicitly listed as needing no rule.
pub const FIELD_RULES: &[FieldRule] = &[
    // ── Closed vocabularies ────────────────────────────────────────────────
    FieldRule {
        key: "network_mode",
        check: Check::OneOf(NETWORK_MODES),
        blank_ok: false,
    },
    FieldRule {
        key: "reasoning_effort",
        check: Check::OneOf(REASONING_EFFORTS),
        blank_ok: false,
    },
    FieldRule {
        key: "security_policy_mode",
        check: Check::OneOf(SECURITY_POLICY_MODES),
        blank_ok: false,
    },
    FieldRule {
        key: "tool_selection_mode",
        check: Check::OneOf(TOOL_SELECTION_MODES),
        blank_ok: false,
    },
    FieldRule {
        key: "agent_backend",
        check: Check::OneOf(AGENT_BACKENDS),
        blank_ok: false,
    },
    FieldRule {
        key: "prompt_style",
        check: Check::OneOf(PROMPT_STYLES),
        blank_ok: false,
    },
    FieldRule {
        key: "thinking_mode",
        check: Check::OneOf(THINKING_MODES),
        blank_ok: false,
    },
    FieldRule {
        key: "review_mode",
        check: Check::OneOf(REVIEW_MODES),
        blank_ok: false,
    },
    FieldRule {
        key: "memory_consolidation_mode",
        check: Check::OneOf(CONSOLIDATION_MODES),
        blank_ok: false,
    },
    FieldRule {
        key: "embedding_provider",
        check: Check::OneOf(EMBEDDING_PROVIDERS),
        blank_ok: false,
    },
    FieldRule {
        key: "voice_tts_quality",
        check: Check::OneOf(TTS_QUALITIES),
        blank_ok: false,
    },
    // ── Time ───────────────────────────────────────────────────────────────
    FieldRule {
        key: "timezone",
        check: Check::IanaZone,
        blank_ok: false,
    },
    // The pair that silences the pond when it cannot be parsed.
    FieldRule {
        key: "quiet_hours_start",
        check: Check::ClockTime,
        blank_ok: true,
    },
    FieldRule {
        key: "quiet_hours_end",
        check: Check::ClockTime,
        blank_ok: true,
    },
    // ── Endpoints ──────────────────────────────────────────────────────────
    FieldRule {
        key: "matter_ws_url",
        check: url(WS, "ws://127.0.0.1:5580/giap"),
        blank_ok: true,
    },
    FieldRule {
        key: "searxng_url",
        check: url(HTTP, "http://127.0.0.1:9000"),
        blank_ok: true,
    },
    FieldRule {
        key: "voice_whisper_url",
        check: url(HTTP, "http://127.0.0.1:8081"),
        blank_ok: true,
    },
    FieldRule {
        key: "voice_kws_whisper_url",
        check: url(HTTP, "http://127.0.0.1:8081"),
        blank_ok: true,
    },
    // ── Where the pond is ──────────────────────────────────────────────────
    FieldRule {
        key: "weather_latitude",
        check: real(-90.0, 90.0, "degrees"),
        blank_ok: false,
    },
    FieldRule {
        key: "weather_longitude",
        check: real(-180.0, 180.0, "degrees"),
        blank_ok: false,
    },
    // ── Bounded numbers ────────────────────────────────────────────────────
    FieldRule {
        key: "agent_max_turns",
        check: int(1.0, 500.0, "turns"),
        blank_ok: false,
    },
    FieldRule {
        key: "llm_max_tokens",
        check: int(1.0, 1_000_000.0, "tokens"),
        blank_ok: false,
    },
];

/// Validate and canonicalise one field.
///
/// `Ok(None)` accepts the value as sent; `Ok(Some(v))` accepts it and asks the
/// caller to store `v` instead. A key with no rule is accepted unchanged.
pub fn check_field(key: &str, value: &Value) -> Result<Option<Value>, FieldError> {
    let Some(rule) = FIELD_RULES.iter().find(|r| r.key == key) else {
        return Ok(None);
    };
    let fail = |message: String| FieldError {
        field: key.to_string(),
        message,
    };

    // A number rule reads the JSON number; everything else reads a string.
    if let Check::Number {
        min,
        max,
        integer,
        unit,
    } = rule.check
    {
        let Some(n) = value.as_f64() else {
            // `null` on a numeric field is an emptied box, not a value: no
            // numeric setting is optional server-side, and sending null earns
            // a deserialise failure that names the whole body instead of the
            // field.
            return Err(fail(format!("{key} needs a number.")));
        };
        if !n.is_finite() {
            return Err(fail(format!("{key} needs a real number.")));
        }
        if integer && n.fract() != 0.0 {
            return Err(fail(format!("{key} must be a whole number of {unit}.")));
        }
        if n < min || n > max {
            return Err(fail(format!(
                "{key} must be between {min} and {max} {unit}; got {n}."
            )));
        }
        return Ok(None);
    }

    // `null` is how an `Option<String>` field says "unset" — the same state as
    // an empty string, and it must be judged the same way. Reading it as a type
    // error would make a pond's own shipped defaults invalid, which is how this
    // was caught.
    if value.is_null() {
        return if rule.blank_ok {
            Ok(None)
        } else {
            Err(fail(format!("{key} cannot be empty.")))
        };
    }
    let Some(raw) = value.as_str() else {
        return Err(fail(format!("{key} needs text.")));
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return if rule.blank_ok {
            Ok(None)
        } else {
            Err(fail(format!("{key} cannot be empty.")))
        };
    }

    match rule.check {
        Check::Number { .. } => unreachable!("handled above"),
        Check::OneOf(allowed) => {
            let lower = trimmed.to_ascii_lowercase();
            if allowed.contains(&lower.as_str()) {
                return Ok(canonical(raw, &lower));
            }
            // A quarantined value deserves its reason, not just the list.
            if key == "agent_backend" && QUARANTINED_AGENT_BACKENDS.contains(&lower.as_str()) {
                return Err(fail(format!(
                    "the {lower:?} agent backend is quarantined and cannot be selected."
                )));
            }
            Err(fail(format!(
                "{key} must be one of {allowed:?}; got {raw:?}."
            )))
        }
        Check::IanaZone => match normalize_zone(trimmed) {
            Some(zone) => Ok(canonical(raw, &zone)),
            None => Err(fail(format!(
                "{raw:?} is not an IANA time zone; GET /api/v1/time/zones lists them."
            ))),
        },
        Check::ClockTime => match parse_clock_time(trimmed) {
            Some(hhmm) => Ok(canonical(raw, &hhmm)),
            None => Err(fail(format!(
                "{key} must be a 24-hour time like 22:00; got {raw:?}."
            ))),
        },
        Check::Url { schemes, example } => {
            if schemes.iter().any(|s| trimmed.starts_with(s)) && has_host(trimmed, schemes) {
                Ok(canonical(raw, trimmed))
            } else {
                Err(fail(format!(
                    "{key} must be a URL like {example}; got {raw:?}."
                )))
            }
        }
    }
}

/// `Some(new)` only when it differs, so an unchanged value is never rewritten.
fn canonical(raw: &str, canonical: &str) -> Option<Value> {
    (raw != canonical).then(|| Value::String(canonical.to_string()))
}

/// `HH:MM` on a 24-hour clock, zero-padded.
///
/// Accepts `9:05` and returns `09:05`. Rejects `10pm`, which is the value that
/// silenced a pond for a day.
fn parse_clock_time(s: &str) -> Option<String> {
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    (h < 24 && m < 60).then(|| format!("{h:02}:{m:02}"))
}

/// Whether anything follows the scheme.
///
/// The old route arm checked the prefix alone, so `ws://` and `ws:// ` passed
/// and then failed at connect time — which presents as "Devices stuck on
/// unreachable" rather than as a bad setting.
fn has_host(url: &str, schemes: &[&str]) -> bool {
    schemes.iter().any(|s| {
        url.strip_prefix(s)
            .is_some_and(|rest| !rest.trim().is_empty() && !rest.starts_with('/'))
    })
}

/// Validate and canonicalise a JSON patch in place.
///
/// Only the keys the object carries are checked, so a partial save is not
/// judged on fields it did not touch. Every failing field is reported: fixing
/// one error at a time is a worse experience than seeing all of them.
pub fn validate_patch(patch: &mut Value) -> Result<(), Vec<FieldError>> {
    let Some(object) = patch.as_object_mut() else {
        // Not an object: the caller's own deserialise will say so better.
        return Ok(());
    };
    let mut errors = Vec::new();
    let mut rewrites: Vec<(String, Value)> = Vec::new();

    for (key, value) in object.iter() {
        match check_field(key, value) {
            Ok(None) => {}
            Ok(Some(canonical)) => rewrites.push((key.clone(), canonical)),
            Err(e) => errors.push(e),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    for (key, value) in rewrites {
        object.insert(key, value);
    }
    Ok(())
}

/// Validate one `set_key`-style write, returning the canonical string to store.
///
/// The CLI and any other key/value writer go through here; without it they
/// bypass even the route's checks.
pub fn check_raw_key(key: &str, raw: &str) -> Result<String, FieldError> {
    // A raw write is always textual, even for numeric fields, so a numeric rule
    // is given the parsed number rather than the string.
    let value = match FIELD_RULES.iter().find(|r| r.key == key).map(|r| r.check) {
        Some(Check::Number { .. }) => raw
            .trim()
            .parse::<f64>()
            .map(|n| Value::from(n))
            .unwrap_or_else(|_| Value::String(raw.to_string())),
        _ => Value::String(raw.to_string()),
    };
    match check_field(key, &value)? {
        Some(Value::String(s)) => Ok(s),
        Some(other) => Ok(other.to_string()),
        None => Ok(raw.to_string()),
    }
}

/// One sentence naming every failure, for the body of a refusal.
pub fn render_errors(errors: &[FieldError]) -> String {
    errors
        .iter()
        .map(|e| e.message.clone())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Validate the fields of a whole `Settings`, optionally restricted to the
/// keys a caller touched.
pub fn validate_settings(
    settings: &Settings,
    only: Option<&HashSet<String>>,
) -> Result<(), Vec<FieldError>> {
    let Ok(Value::Object(map)) = serde_json::to_value(settings) else {
        return Ok(());
    };
    let errors: Vec<FieldError> = map
        .iter()
        .filter(|(k, _)| only.is_none_or(|set| set.contains(k.as_str())))
        .filter_map(|(k, v)| check_field(k, v).err())
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Every key the table covers.
pub fn ruled_keys() -> Vec<&'static str> {
    FIELD_RULES.iter().map(|r| r.key).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Value {
        Value::String(v.to_string())
    }

    /// The defect this module exists for, in the field that caused it: a stored
    /// `"10pm"` fails to parse, quiet hours fail CLOSED, and the pond is silent
    /// all day with nothing saying why.
    #[test]
    fn a_clock_time_that_does_not_parse_is_refused_not_stored() {
        assert!(check_field("quiet_hours_start", &s("10pm")).is_err());
        assert!(check_field("quiet_hours_start", &s("24:00")).is_err());
        assert!(check_field("quiet_hours_start", &s("22:60")).is_err());
        assert!(check_field("quiet_hours_start", &s("22:00"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_reasonable_clock_time_is_canonicalised_rather_than_rejected() {
        assert_eq!(
            check_field("quiet_hours_end", &s("9:05")).unwrap(),
            Some(s("09:05"))
        );
    }

    /// The old check was a denylist of one string, so every OTHER typo was
    /// stored and then answered by MockAgent.
    #[test]
    fn an_agent_backend_typo_is_refused_not_only_the_quarantined_one() {
        assert!(check_field("agent_backend", &s("goose")).unwrap().is_none());
        assert!(check_field("agent_backend", &s("gosse")).is_err());
        assert!(check_field("agent_backend", &s("ollama")).is_err());
    }

    /// A quarantined value gets its reason, not just the list of alternatives.
    #[test]
    fn the_quarantined_backend_is_refused_by_name() {
        let err = check_field("agent_backend", &s("pond")).unwrap_err();
        assert!(err.message.contains("quarantined"), "{}", err.message);
    }

    #[test]
    fn an_enum_value_is_matched_case_insensitively_and_stored_canonically() {
        assert_eq!(
            check_field("network_mode", &s("OFFLINE")).unwrap(),
            Some(s("offline"))
        );
        assert!(check_field("network_mode", &s("offline"))
            .unwrap()
            .is_none());
        assert!(check_field("network_mode", &s("off")).is_err());
    }

    #[test]
    fn a_zone_is_canonicalised_and_a_typo_is_refused() {
        assert_eq!(
            check_field("timezone", &s("africa/nairobi")).unwrap(),
            Some(s("Africa/Nairobi"))
        );
        assert!(check_field("timezone", &s("Africa/Nairobbi")).is_err());
        assert!(
            check_field("timezone", &s("")).is_err(),
            "a zone is never blank"
        );
    }

    /// The old arm checked the scheme prefix alone, so these passed and then
    /// failed at connect time — which reads as a broken device, not a setting.
    #[test]
    fn a_url_needs_a_host_and_not_only_a_scheme() {
        assert!(check_field("matter_ws_url", &s("ws://")).is_err());
        assert!(check_field("matter_ws_url", &s("ws:// ")).is_err());
        assert!(
            check_field("matter_ws_url", &s("http://x")).is_err(),
            "wrong scheme"
        );
        assert!(check_field("matter_ws_url", &s("ws://127.0.0.1:5580/giap"))
            .unwrap()
            .is_none());
        // Optional endpoints may be blank; that is how they are turned off.
        assert!(check_field("searxng_url", &s("")).unwrap().is_none());
    }

    #[test]
    fn coordinates_outside_the_earth_are_refused() {
        assert!(check_field("weather_latitude", &Value::from(999.0)).is_err());
        assert!(check_field("weather_latitude", &Value::from(-91.0)).is_err());
        assert!(check_field("weather_latitude", &Value::from(-1.28))
            .unwrap()
            .is_none());
        assert!(check_field("weather_longitude", &Value::from(181.0)).is_err());
        assert!(check_field("weather_longitude", &Value::from(36.8))
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_bounded_count_must_be_a_whole_number_in_range() {
        assert!(check_field("agent_max_turns", &Value::from(50))
            .unwrap()
            .is_none());
        assert!(check_field("agent_max_turns", &Value::from(0)).is_err());
        assert!(check_field("agent_max_turns", &Value::from(99_999)).is_err());
        assert!(check_field("agent_max_turns", &Value::from(2.5)).is_err());
        assert!(check_field("agent_max_turns", &s("many")).is_err());
    }

    #[test]
    fn a_field_with_no_rule_is_accepted_unchanged() {
        assert!(check_field("user_name", &s("Jerry")).unwrap().is_none());
        assert!(check_field("not_a_setting", &s("anything"))
            .unwrap()
            .is_none());
    }

    /// A partial save must be judged only on what it carries.
    #[test]
    fn a_patch_is_checked_only_on_the_keys_it_carries() {
        let mut patch = serde_json::json!({ "user_name": "Jerry" });
        assert!(validate_patch(&mut patch).is_ok());
    }

    #[test]
    fn a_patch_canonicalises_in_place() {
        let mut patch = serde_json::json!({
            "timezone": "africa/nairobi",
            "quiet_hours_start": "9:05",
        });
        validate_patch(&mut patch).expect("both values are reasonable");
        assert_eq!(patch["timezone"], "Africa/Nairobi");
        assert_eq!(patch["quiet_hours_start"], "09:05");
    }

    /// Fixing one error per round-trip is a worse experience than seeing all.
    #[test]
    fn every_failing_field_is_reported_not_just_the_first() {
        let mut patch = serde_json::json!({
            "timezone": "Mars/Olympus",
            "agent_max_turns": 0,
            "user_name": "Jerry",
        });
        let errors = validate_patch(&mut patch).unwrap_err();
        assert_eq!(errors.len(), 2);
        let fields: HashSet<&str> = errors.iter().map(|e| e.field.as_str()).collect();
        assert!(fields.contains("timezone") && fields.contains("agent_max_turns"));
    }

    /// A refused patch must not be half-rewritten.
    #[test]
    fn a_refused_patch_is_left_exactly_as_it_arrived() {
        let mut patch = serde_json::json!({
            "timezone": "africa/nairobi",
            "agent_max_turns": 0,
        });
        assert!(validate_patch(&mut patch).is_err());
        assert_eq!(
            patch["timezone"], "africa/nairobi",
            "canonicalised despite refusing"
        );
    }

    /// The CLI and every other key/value writer go through this.
    #[test]
    fn a_raw_key_write_is_validated_and_canonicalised_too() {
        assert_eq!(
            check_raw_key("timezone", "africa/nairobi").unwrap(),
            "Africa/Nairobi"
        );
        assert!(check_raw_key("timezone", "Mars/Olympus").is_err());
        assert_eq!(check_raw_key("agent_max_turns", "50").unwrap(), "50");
        assert!(check_raw_key("agent_max_turns", "0").is_err());
        assert_eq!(check_raw_key("user_name", "Jerry").unwrap(), "Jerry");
    }

    /// The defaults must satisfy their own rules, or a fresh pond is invalid.
    #[test]
    fn the_shipped_defaults_pass_every_rule() {
        let defaults = Settings::default();
        if let Err(errors) = validate_settings(&defaults, None) {
            panic!("the default Settings violate their own rules: {errors:#?}");
        }
    }

    /// Completeness guard, in the shape `every_settings_field_is_dispositioned`
    /// already established: a new field must be given a rule or explicitly
    /// recorded as needing none, so an unvalidated field cannot arrive quietly.
    #[test]
    fn every_ruled_key_is_a_real_settings_field() {
        let Ok(Value::Object(map)) = serde_json::to_value(Settings::default()) else {
            panic!("Settings did not serialise to an object");
        };
        for key in ruled_keys() {
            assert!(
                map.contains_key(key),
                "{key} has a rule but is not a Settings field — a renamed field \
                 leaves its rule behind, silently unenforced"
            );
        }
    }
}
