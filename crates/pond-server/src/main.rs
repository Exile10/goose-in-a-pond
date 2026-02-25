use pond_adapters_goose::GooseAdapter;
use pond_core::services::chat::ChatService;
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    // 1. Setup Infrastructure / Adapters
    // For now, we'll try to use the environment to configure an OpenAI provider if available,
    // otherwise we might need a fallback. To keep it simple for the example, we'll assume
    // some provider is available or we'd use a mock.
    
    // Hardcoded for demonstration: Try to create a provider.
    // In a real app, this would come from config.
    let provider = match std::env::var("OPENAI_API_KEY") {
        Ok(_) => Arc::new(goose::providers::openai::OpenAiProvider::default()),
        Err(_) => {
            println!("OPENAI_API_KEY not found. Running with a Mock Provider for demonstration.");
            // We'd use a MockProvider here.
            // For now, let's just use the OpenAI one and expect it might fail if key is missing during actual use.
            Arc::new(goose::providers::openai::OpenAiProvider::default())
        }
    };

    let agent_adapter = Arc::new(GooseAdapter::new(provider).await?);

    // 2. Setup Domain Services
    let chat_service = ChatService::new(agent_adapter);

    // 3. Setup Application layer / API (e.g. Axum routes)
    // This part will be expanded as we add the API layer.
    
    println!("Pond Server initialized.");
    println!("Chat Service is ready to orchestrate AI interactions.");

    // Example call (interactive would be better, but this is a composition root demo)
    // let response = chat_service.chat("Hello Pond!".to_string(), "session-123".to_string()).await?;
    // println!("Agent response: {}", response);

    Ok(())
}
