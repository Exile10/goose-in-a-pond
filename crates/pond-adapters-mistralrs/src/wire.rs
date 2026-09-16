//! OpenAI chat-completions wire types, as mistral.rs actually speaks them.
//!
//! Two departures from the OpenAI schema are load-bearing here, and both were
//! found by measurement rather than by reading a spec:
//!
//! 1. **`delta.reasoning_content`.** Gemma 4 through mistral.rs puts its
//!    thinking there, not in `delta.content`. A parser that reads only
//!    `content` records a turn that produced nothing — the first run of the
//!    bake-off harness reported 0 tool calls and 0 tok/s across every engine
//!    for exactly this reason.
//! 2. **Explicit `null` where a list is expected.** Every delta frame carries
//!    `"tool_calls":null` rather than omitting the field, and `#[serde(default)]`
//!    does not cover that — it fills a *missing* field, not a null one. A
//!    `Vec` there fails to deserialize, so EVERY frame is skipped and the turn
//!    produces nothing at all: no text, no error, no usage. See
//!    [`nullable`]. The unit tests below missed this for one round because
//!    they were written from the OpenAI schema instead of from a frame this
//!    server actually sent.
//! 3. **An `error` object inside a 200 response body.** When mistral.rs fails
//!    mid-stream it still returns `200 OK` and writes
//!    `data: {"error":{...}}` into the SSE body. Its own `/metrics` counts
//!    that as a healthy request, so a caller that trusts the status code sees
//!    a server with a perfect record and a chat that does not work. Parsing
//!    this field is the only way the failure becomes visible.

use serde::{Deserialize, Deserializer, Serialize};

/// Read a field that may be `null` into its default.
///
/// `#[serde(default)]` alone is not enough: it fills a field the payload left
/// out, and mistral.rs sends the field with a `null` in it.
fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

// ── Request ──────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub messages: Vec<WireMessage>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Raw tool specs, passed through as JSON. They come from
    /// `ToolDispatcher::tools_json`, which already emits the OpenAI shape, so
    /// re-typing them here would only add a place for the schema to drift.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<&'static str>,
    pub stream_options: StreamOptions,
    /// `{"enable_thinking": bool}` — the knob Gemma 4's template reads.
    pub chat_template_kwargs: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct StreamOptions {
    pub include_usage: bool,
}

/// One message on the wire.
///
/// `content` is always present, empty string included: an assistant turn that
/// produced only tool calls has no text, and `null` there is accepted by the
/// OpenAI spec but not by every template that renders it.
#[derive(Debug, Serialize)]
pub struct WireMessage {
    pub role: &'static str,
    pub content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<WireToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WireToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: WireFunctionCall,
}

#[derive(Debug, Serialize)]
pub struct WireFunctionCall {
    pub name: String,
    /// JSON-stringified, per the OpenAI schema — not a nested object.
    pub arguments: String,
}

// ── Streaming response ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct StreamChunk {
    #[serde(default, deserialize_with = "nullable")]
    pub choices: Vec<StreamChoice>,
    #[serde(default)]
    pub usage: Option<WireUsage>,
    /// See the module header: this arrives with HTTP 200.
    #[serde(default)]
    pub error: Option<WireError>,
}

#[derive(Debug, Deserialize, Default)]
pub struct StreamChoice {
    #[serde(default)]
    pub delta: Delta,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct Delta {
    #[serde(default)]
    pub content: Option<String>,
    /// Where Gemma 4's thinking actually lands.
    #[serde(default)]
    pub reasoning_content: Option<String>,
    /// Sent as `null` on every frame that has no call — see the module header.
    #[serde(default, deserialize_with = "nullable")]
    pub tool_calls: Vec<DeltaToolCall>,
}

#[derive(Debug, Deserialize)]
pub struct DeltaToolCall {
    /// Which call this fragment belongs to. Absent on single-call streams.
    #[serde(default)]
    pub index: usize,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<DeltaFunction>,
}

#[derive(Debug, Deserialize)]
pub struct DeltaFunction {
    #[serde(default)]
    pub name: Option<String>,
    /// Arrives in fragments that must be concatenated before parsing.
    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Debug, Deserialize, Default, Clone, Copy)]
pub struct WireUsage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
}

#[derive(Debug, Deserialize)]
pub struct WireError {
    #[serde(default)]
    pub message: String,
}

/// `GET /v1/models`. mistral.rs answers `{"data":[{"id":…}]}` and rejects a
/// model name it does not serve with a 400 rather than defaulting to the one
/// model it loaded — so the served id has to be asked for, not assumed.
#[derive(Debug, Deserialize)]
pub struct ModelsResponse {
    #[serde(default)]
    pub data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ModelEntry {
    #[serde(default)]
    pub id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failure that a 200 hides. If this ever stops deserializing, a broken
    /// mistral.rs looks healthy again.
    #[test]
    fn an_error_frame_inside_a_200_body_still_parses_as_an_error() {
        let raw = r#"{"error":{"message":"Internal server error.","type":"server_error"}}"#;
        let chunk: StreamChunk = serde_json::from_str(raw).expect("error frame must parse");
        assert_eq!(
            chunk.error.map(|e| e.message).as_deref(),
            Some("Internal server error.")
        );
    }

    /// Thinking rides `reasoning_content`; a chunk carrying only that is not empty.
    #[test]
    fn reasoning_content_is_read_from_its_own_field() {
        let raw = r#"{"choices":[{"delta":{"reasoning_content":"weighing it up"}}]}"#;
        let chunk: StreamChunk = serde_json::from_str(raw).unwrap();
        let delta = &chunk.choices[0].delta;
        assert!(delta.content.is_none());
        assert_eq!(delta.reasoning_content.as_deref(), Some("weighing it up"));
    }

    /// Tool-call arguments arrive as fragments keyed by `index`, and the first
    /// fragment is the only one carrying the name.
    #[test]
    fn tool_call_fragments_carry_an_index_and_a_partial_argument_string() {
        let raw = r#"{"choices":[{"delta":{"tool_calls":[
            {"index":0,"id":"call_1","function":{"name":"giap-weather__get","arguments":"{\"ci"}}
        ]}}]}"#;
        let chunk: StreamChunk = serde_json::from_str(raw).unwrap();
        let tc = &chunk.choices[0].delta.tool_calls[0];
        assert_eq!(tc.index, 0);
        assert_eq!(tc.id.as_deref(), Some("call_1"));
        let f = tc.function.as_ref().unwrap();
        assert_eq!(f.name.as_deref(), Some("giap-weather__get"));
        assert_eq!(f.arguments.as_deref(), Some("{\"ci"));
    }

    /// A real frame, copied verbatim off the wire on 2026-09-10 rather than
    /// written from the OpenAI schema. The difference is `"tool_calls":null`,
    /// which made every frame fail to deserialize while the invented ones in
    /// this module passed — a whole turn that produced nothing, silently.
    #[test]
    fn a_frame_this_server_actually_sent_deserializes() {
        let raw = r#"{"id":"2","choices":[{"finish_reason":null,"index":0,"delta":{"content":"I","role":"assistant","tool_calls":null},"logprobs":null}],"created":1788992752,"model":"/models","system_fingerprint":"local","object":"chat.completion.chunk","usage":null}"#;
        let chunk: StreamChunk = serde_json::from_str(raw).expect("a real frame must parse");
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("I"));
        assert!(chunk.choices[0].delta.tool_calls.is_empty());
        assert!(chunk.usage.is_none());
    }

    /// Null everywhere a list is expected, in one frame.
    #[test]
    fn explicit_nulls_read_as_empty_rather_than_failing() {
        let chunk: StreamChunk = serde_json::from_str(r#"{"choices":null,"usage":null}"#).unwrap();
        assert!(chunk.choices.is_empty());
    }

    /// A usage-only frame (`stream_options.include_usage`) has no choices at all.
    #[test]
    fn a_usage_only_frame_has_no_choices() {
        let raw = r#"{"choices":[],"usage":{"prompt_tokens":7212,"completion_tokens":64}}"#;
        let chunk: StreamChunk = serde_json::from_str(raw).unwrap();
        assert!(chunk.choices.is_empty());
        assert_eq!(chunk.usage.unwrap().prompt_tokens, 7212);
    }

    /// An assistant turn with only tool calls still serializes `content`.
    #[test]
    fn an_assistant_message_with_no_text_still_sends_a_content_field() {
        let msg = WireMessage {
            role: "assistant",
            content: String::new(),
            tool_calls: vec![WireToolCall {
                id: "call_1".into(),
                kind: "function",
                function: WireFunctionCall {
                    name: "giap-device__list".into(),
                    arguments: "{}".into(),
                },
            }],
            tool_call_id: None,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json.get("content").and_then(|c| c.as_str()), Some(""));
        assert!(json.get("tool_call_id").is_none());
    }
}
