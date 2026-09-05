use serde::{Deserialize, Serialize};

/// The role of a participant in a chat conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
    System,
    /// Tool response — result of a tool call, attributed to the tool system.
    /// In OpenAI-compatible APIs this maps to `role: "tool"`.
    Tool,
}

/// An image attached to a chat message.
///
/// Used for multimodal input when the active model supports vision
/// (e.g. Gemma 4, LLaVA). Models without vision silently ignore images.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAttachment {
    /// Base64-encoded image data.
    pub data: String,
    /// MIME type, e.g. "image/jpeg", "image/png", "image/webp".
    pub mime_type: String,
}

/// A record of a tool invocation embedded in an assistant message.
///
/// Round-tripped on later turns through the OpenAI-compatible `tool_calls` field so
/// the model sees its own prior tool usage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub id: String,
    pub name: String,
    /// The raw JSON-stringified arguments the model produced.
    pub arguments: String,
}

/// A single message in a chat conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    /// Optional image attachments for multimodal models.
    /// Empty for text-only messages. Backwards-compatible via `serde(default)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageAttachment>,
    /// Tool calls invoked by an assistant message.
    /// Empty for user/system/tool messages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallRecord>,
    /// Identifier linking a tool-result message back to the originating
    /// assistant `tool_calls[].id`. Set only on `Role::Tool` messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// Create a tool result message, rendered by the chat template as `role: "tool"`.
    ///
    /// `tool_call_id` must match the `id` of the originating `ToolCallRecord` on the
    /// preceding assistant message.
    pub fn tool_result(content: impl Into<String>, tool_call_id: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }

    /// Create an assistant message that includes tool-call metadata.
    ///
    /// `content` may be empty when the model emitted only tool calls.
    pub fn assistant_with_tool_calls(
        content: impl Into<String>,
        tool_calls: Vec<ToolCallRecord>,
    ) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            images: Vec::new(),
            tool_calls,
            tool_call_id: None,
        }
    }

    /// Attach an image to this message.
    pub fn with_image(mut self, data: String, mime_type: String) -> Self {
        self.images.push(ImageAttachment { data, mime_type });
        self
    }

    /// Create a user message carrying image attachments (phase F1).
    ///
    /// Order is preserved: attachment ordinal 0 is the first image the user
    /// picked, which is what "the first picture" refers to in a follow-up.
    pub fn user_with_images(content: impl Into<String>, images: Vec<ImageAttachment>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            images,
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}
