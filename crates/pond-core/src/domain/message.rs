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

/// A single message in a chat conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    /// Optional image attachments for multimodal models.
    /// Empty for text-only messages. Backwards-compatible via `serde(default)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageAttachment>,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            images: Vec::new(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            images: Vec::new(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            images: Vec::new(),
        }
    }

    /// Create a tool result message. In the chat template, this renders with
    /// `role: "tool"` so the model recognizes it as a tool response, not user input.
    pub fn tool_result(content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            images: Vec::new(),
        }
    }

    /// Attach an image to this message.
    pub fn with_image(mut self, data: String, mime_type: String) -> Self {
        self.images.push(ImageAttachment { data, mime_type });
        self
    }
}
