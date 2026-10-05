//! Whether a conversation model is chosen. With none, nothing is picked or downloaded on the
//! household's behalf: a turn is refused with this state and the Models page offers the picks.

/// No conversation model is chosen yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("No conversation model is chosen yet. Choose one on the Models page to start talking.")]
pub struct NoConversationModel;

impl NoConversationModel {
    /// The API's `code` field.
    pub const CODE: &'static str = "no_model";
}

/// Whether `provider` and `model` name something to answer with. Another pond (`mesh`) and the
/// offline echo agent (`mock`) need no model name; every other provider does, and an empty one
/// is no choice at all.
pub fn conversation_model_chosen(provider: &str, model: &str) -> bool {
    match provider.trim() {
        "" => false,
        "mesh" | "mock" => true,
        _ => !model.trim().is_empty(),
    }
}

/// [`conversation_model_chosen`] as a `Result`, for the paths that refuse a turn.
pub fn require_conversation_model(provider: &str, model: &str) -> Result<(), NoConversationModel> {
    if conversation_model_chosen(provider, model) {
        Ok(())
    } else {
        Err(NoConversationModel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_model_is_no_choice_whatever_the_provider() {
        for provider in ["local", "gguf", "llamafile", "ollama", "mistralrs", ""] {
            assert!(!conversation_model_chosen(provider, ""), "{provider:?}");
            assert!(!conversation_model_chosen(provider, "  "), "{provider:?}");
        }
        assert!(!conversation_model_chosen(
            "",
            "gemma-4-E4B-it-qat-UD-Q4_K_XL"
        ));
        assert!(conversation_model_chosen(
            "local",
            "gemma-4-E4B-it-qat-UD-Q4_K_XL"
        ));
        assert!(conversation_model_chosen("ollama", "qwen3:4b"));
        assert!(
            conversation_model_chosen("mesh", ""),
            "another pond answers"
        );
        assert!(
            conversation_model_chosen("mock", ""),
            "the offline echo needs none"
        );
    }

    #[test]
    fn the_refusal_says_where_to_choose_and_carries_its_code() {
        let refusal = require_conversation_model("local", "").unwrap_err();
        assert_eq!(NoConversationModel::CODE, "no_model");
        assert!(refusal.to_string().contains("Models page"));
        assert!(refusal.to_string().is_ascii());
        assert!(require_conversation_model("local", "m").is_ok());
    }
}
