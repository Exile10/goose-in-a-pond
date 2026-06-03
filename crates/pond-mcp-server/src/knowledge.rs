//! Knowledge MCP Server — Wikipedia, instant answers, dictionary, and book search.
//!
//! Provides 5 tools: `search_wikipedia`, `get_wikipedia_article`,
//! `instant_answer`, `define_word`, `search_books`.
//! Depends only on a `reqwest::Client` for HTTP fetches.

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, ErrorCode, ErrorData, Implementation, InitializeResult,
        ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;

// ── Parameter structs ──────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WikipediaQueryParams {
    /// The topic to look up — a name, phrase, or question (e.g. "black holes", "Nairobi", "how do volcanoes work").
    pub topic: Option<String>,
    /// Maximum number of search results (default 5, max 10). Only used by search_wikipedia.
    pub limit: Option<u32>,
    /// Catch-all for any extra fields the model sends (e.g. "query", "title", "search").
    /// Not part of the advertised schema — exists purely to absorb unexpected keys.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct DefineWordParams {
    /// The English word to define (e.g. "serendipity", "ephemeral").
    pub word: Option<String>,
    /// Catch-all for any extra fields the model sends (e.g. "term", "query").
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct BookSearchParams {
    /// Search query — a book title, topic, or keyword (e.g. "Dune", "machine learning").
    pub query: Option<String>,
    /// Filter by author name (e.g. "Frank Herbert").
    pub author: Option<String>,
    /// Maximum results to return (default 5, max 10).
    pub limit: Option<u32>,
    /// Catch-all for any extra fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

// ── Constants ──────────────────────────────────────────────────────────────

const WIKI_UA: &str =
    "goose-in-a-pond/0.1 (GIAP MCP; https://github.com/jarida-io/goose-in-a-pond)";

const INSTANT_ANSWER_BUDGET: usize = 2000;
const DEFINE_WORD_BUDGET: usize = 2000;
const SEARCH_BOOKS_BUDGET: usize = 1500;

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct KnowledgeMcpServer {
    http_client: reqwest::Client,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl KnowledgeMcpServer {
    pub fn new(http_client: reqwest::Client) -> Self {
        Self {
            http_client,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "\
Search Wikipedia for articles matching a topic. Returns a ranked list of article \
titles with short descriptions. Only use this when you need to disambiguate \
between multiple topics or show the user a list of options. For direct factual \
questions, prefer get_wikipedia_article instead — it auto-searches on your behalf.")]
    async fn search_wikipedia(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<WikipediaQueryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let query = resolve_topic(&params.0, "search_wikipedia").await;
        println!("[wikipedia] search_wikipedia called: query={:?}", query);

        if query.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "I need a topic to search. Retry with a 'topic' parameter.",
            )]));
        }
        let limit = params.0.limit.unwrap_or(5).min(10);

        let url = format!(
            "https://en.wikipedia.org/w/api.php?action=query&list=search&srsearch={}&srlimit={}&format=json",
            urlencoding::encode(&query),
            limit,
        );
        println!("[wikipedia] GET {}", url);

        let resp = self
            .http_client
            .get(&url)
            .header("user-agent", WIKI_UA)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| {
                println!("[wikipedia] search request failed: {e}");
                ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Wikipedia request failed: {e}"),
                    None,
                )
            })?;

        println!("[wikipedia] search response status: {}", resp.status());

        if !resp.status().is_success() {
            println!("[wikipedia] search failed with HTTP {}", resp.status());
            return Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Wikipedia returned HTTP {}", resp.status()),
                None,
            ));
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            println!("[wikipedia] failed to parse search response: {e}");
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to parse response: {e}"),
                None,
            )
        })?;

        let results = body["query"]["search"].as_array();
        let result_count = results.map(|a| a.len()).unwrap_or(0);
        println!("[wikipedia] search returned {} results", result_count);

        let (text, ui_results) = match results {
            Some(arr) if !arr.is_empty() => {
                let mut lines = Vec::new();
                let mut ui_items = Vec::new();
                for item in arr {
                    if let Some(title) = item["title"].as_str() {
                        let snippet = item["snippet"].as_str().unwrap_or("");
                        let clean = snippet
                            .replace("<span class=\"searchmatch\">", "")
                            .replace("</span>", "")
                            .replace("&quot;", "\"")
                            .replace("&amp;", "&");
                        lines.push(format!("- **{}**: {}", title, clean));
                        ui_items.push(serde_json::json!({
                            "title": title,
                            "snippet": clean,
                        }));
                    }
                }
                (lines.join("\n"), ui_items)
            }
            _ => (
                format!("No Wikipedia articles found for '{}'.", query),
                Vec::new(),
            ),
        };
        println!(
            "[wikipedia] search_wikipedia done, returning {} chars",
            text.len()
        );
        // Prepend UI hint if we have structured results
        let full_result = if ui_results.is_empty() {
            text
        } else {
            let ui_data = serde_json::json!({
                "query": query,
                "results": ui_results,
            });
            format!("[[[mcp-ui:knowledge:{}]]]\n{}", ui_data, text)
        };
        Ok(CallToolResult::success(vec![Content::text(full_result)]))
    }

    #[tool(description = "\
Look up a topic on Wikipedia. Pass a topic name or natural-language query — the \
tool auto-searches if the exact title is not found. Use this as your first \
choice for ANY factual question (people, places, events, science, history, etc.).\n\
After receiving the result: extract only the facts relevant to the user's \
question and answer concisely in your own words. Do NOT repeat the extract \
verbatim. In voice mode keep it to 1-3 sentences.")]
    async fn get_wikipedia_article(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<WikipediaQueryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        // Definitive: what did the MCP server receive from Goose?
        println!("[wikipedia] ╔═══ MCP SERVER RECEIVED ═══");
        println!("[wikipedia] ║ params.topic: {:?}", params.0.topic);
        println!("[wikipedia] ║ params.extra: {:?}", params.0.extra);
        println!("[wikipedia] ╚═══════════════════════════");

        let topic = resolve_topic(&params.0, "get_wikipedia_article").await;
        println!(
            "[wikipedia] get_wikipedia_article called: topic={:?}",
            topic
        );

        if topic.is_empty() {
            // Nudge: return guidance as content so the model can retry
            println!("[wikipedia] empty topic, nudging model to retry");
            return Ok(CallToolResult::success(vec![Content::text(
                "I need a topic to look up. Retry this tool with a 'topic' parameter \
                 containing the person, place, event, or concept to search for.",
            )]));
        }

        // Try direct lookup first
        match self.fetch_article_summary(&topic).await {
            Ok(text) => {
                let full_result = prepend_knowledge_hint(&topic, &text);
                Ok(CallToolResult::success(vec![Content::text(full_result)]))
            }
            Err(WikiFetchError::NotFound) => {
                // Auto-fallback: search for the topic and fetch the top result
                println!(
                    "[wikipedia] exact title not found, searching for '{}'",
                    topic
                );
                match self.search_and_fetch_best(&topic).await {
                    Ok(text) => {
                        let full_result = prepend_knowledge_hint(&topic, &text);
                        Ok(CallToolResult::success(vec![Content::text(full_result)]))
                    }
                    Err(e) => Err(e),
                }
            }
            Err(WikiFetchError::Mcp(e)) => Err(e),
        }
    }

    #[tool(description = "\
Get a quick factual answer or summary for simple questions. Try this first for \
definitions, quick facts, or 'what is X' questions before using Wikipedia.")]
    async fn instant_answer(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<WikipediaQueryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let query = resolve_topic(&params.0, "instant_answer").await;
        println!("[knowledge] instant_answer called: query={:?}", query);

        if query.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "I need a question or topic. Retry with a 'topic' parameter.",
            )]));
        }

        let url = format!(
            "https://api.duckduckgo.com/?q={}&format=json&no_html=1&skip_disambig=1",
            urlencoding::encode(&query),
        );
        println!("[knowledge] GET {}", url);

        let resp = match self
            .http_client
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                println!("[knowledge] instant_answer request failed: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("DuckDuckGo Instant Answer", &e.to_string()),
                )]));
            }
        };

        if !resp.status().is_success() {
            println!("[knowledge] instant_answer HTTP {}", resp.status());
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_api_error(
                    "DuckDuckGo Instant Answer",
                    &format!("HTTP {}", resp.status()),
                ),
            )]));
        }

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                println!("[knowledge] instant_answer parse failed: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("DuckDuckGo Instant Answer", &e.to_string()),
                )]));
            }
        };

        // Prefer AbstractText, then Answer, then Definition
        let abstract_text = body["AbstractText"].as_str().unwrap_or("");
        let answer = body["Answer"].as_str().unwrap_or("");
        let definition = body["Definition"].as_str().unwrap_or("");

        let (content, source_url) = if !abstract_text.is_empty() {
            let source = body["AbstractSource"].as_str().unwrap_or("DuckDuckGo");
            let url = body["AbstractURL"].as_str().unwrap_or("");
            let text = format!("{}\n\nSource: {} ({})", abstract_text, source, url);
            (text, url.to_string())
        } else if !answer.is_empty() {
            (answer.to_string(), String::new())
        } else if !definition.is_empty() {
            let url = body["DefinitionURL"].as_str().unwrap_or("");
            (
                format!("{}\n\nSource: {}", definition, url),
                url.to_string(),
            )
        } else {
            println!("[knowledge] instant_answer: no result for '{}'", query);
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No instant answer found for '{}'. Try get_wikipedia_article for a deeper lookup.",
                query
            ))]));
        };

        let _ = source_url; // consumed above in formatting
        let truncated = crate::format::truncate_to_budget(&content, INSTANT_ANSWER_BUDGET);
        println!(
            "[knowledge] instant_answer done, returning {} chars",
            truncated.len()
        );
        Ok(CallToolResult::success(vec![Content::text(truncated)]))
    }

    #[tool(description = "\
Define an English word. Returns meanings, pronunciation, and examples. Use \
when asked 'what does X mean' or 'define X'.")]
    async fn define_word(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<DefineWordParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let word = resolve_word(&params.0).await;
        println!("[knowledge] define_word called: word={:?}", word);

        if word.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "I need a word to define. Retry with a 'word' parameter.",
            )]));
        }

        let url = format!(
            "https://api.dictionaryapi.dev/api/v2/entries/en/{}",
            urlencoding::encode(&word),
        );
        println!("[knowledge] GET {}", url);

        let resp = match self
            .http_client
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                println!("[knowledge] define_word request failed: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("Dictionary", &e.to_string()),
                )]));
            }
        };

        // 404 means the word was not found
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            println!("[knowledge] define_word: word '{}' not found", word);
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "Word '{}' not found. Check spelling or try a different word.",
                word,
            ))]));
        }

        if !resp.status().is_success() {
            println!("[knowledge] define_word HTTP {}", resp.status());
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_api_error("Dictionary", &format!("HTTP {}", resp.status())),
            )]));
        }

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                println!("[knowledge] define_word parse failed: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("Dictionary", &e.to_string()),
                )]));
            }
        };

        let text = format_dictionary_response(&body, &word);
        let truncated = crate::format::truncate_to_budget(&text, DEFINE_WORD_BUDGET);
        println!(
            "[knowledge] define_word done, returning {} chars",
            truncated.len()
        );
        Ok(CallToolResult::success(vec![Content::text(truncated)]))
    }

    #[tool(description = "\
Search for books by title, author, or subject. Use when asked about books, \
reading recommendations, or 'who wrote X'.")]
    async fn search_books(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<BookSearchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let query = resolve_book_query(&params.0).await;
        println!("[knowledge] search_books called: query={:?}", query);

        if query.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "I need a book title, author, or subject to search. Retry with a 'query' parameter.",
            )]));
        }

        let limit = params.0.limit.unwrap_or(5).min(10);
        let url = format!(
            "https://openlibrary.org/search.json?q={}&limit={}",
            urlencoding::encode(&query),
            limit,
        );
        println!("[knowledge] GET {}", url);

        let resp = match self
            .http_client
            .get(&url)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                println!("[knowledge] search_books request failed: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("Open Library", &e.to_string()),
                )]));
            }
        };

        if !resp.status().is_success() {
            println!("[knowledge] search_books HTTP {}", resp.status());
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_api_error("Open Library", &format!("HTTP {}", resp.status())),
            )]));
        }

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                println!("[knowledge] search_books parse failed: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("Open Library", &e.to_string()),
                )]));
            }
        };

        let docs = body["docs"].as_array();
        let items: Vec<String> = match docs {
            Some(arr) if !arr.is_empty() => arr
                .iter()
                .filter_map(|doc| {
                    let title = doc["title"].as_str()?;
                    let authors = doc["author_name"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_else(|| "Unknown author".to_string());
                    let year = doc["first_publish_year"]
                        .as_u64()
                        .map(|y| format!(" ({})", y))
                        .unwrap_or_default();
                    let isbn = doc["isbn"]
                        .as_array()
                        .and_then(|a| a.first())
                        .and_then(|v| v.as_str())
                        .map(|i| format!(" -- ISBN: {}", i))
                        .unwrap_or_default();
                    Some(format!("**{}** by {}{}{}", title, authors, year, isbn))
                })
                .collect(),
            _ => Vec::new(),
        };

        if items.is_empty() {
            println!("[knowledge] search_books: no results for '{}'", query);
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No books found for '{}'. Try different keywords or check the spelling.",
                query,
            ))]));
        }

        let header = format!("Books matching '{}':", query);
        let text = crate::format::format_list_result(&items, &header, SEARCH_BOOKS_BUDGET);
        println!(
            "[knowledge] search_books done, returning {} chars",
            text.len()
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }
}

#[tool_handler]
impl ServerHandler for KnowledgeMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-knowledge",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Knowledge server — reference lookups and definitions.\n\n\
                 Tools: instant_answer (quick facts), get_wikipedia_article (deep articles), \
                 search_wikipedia (find articles), define_word (dictionary), search_books (book search).\n\n\
                 For simple 'what is X' questions, try instant_answer first — it's fastest. \
                 For deep factual lookups, use get_wikipedia_article. \
                 For word definitions, use define_word. For book queries, use search_books.\n\
                 After receiving results: synthesize in your own words. Do not parrot verbatim.\n\
                 In voice mode: 1-3 sentences. Offer to elaborate if the user wants more.",
            )
    }
}

// ── MCP-UI hint helpers ───────────────────────────────────────────────────

/// Prepend a `[[[mcp-ui:knowledge:{...}]]]` hint to a Wikipedia article result.
///
/// Extracts the article title from the `# Title` header line and the source
/// URL from `Source: <url>` at the end. If the text doesn't follow the
/// expected format, returns it unchanged (no hint).
fn prepend_knowledge_hint(topic: &str, text: &str) -> String {
    // Article format: "# Title\n\nExtract...\n\nSource: URL"
    let title = text
        .strip_prefix("# ")
        .and_then(|s| s.split('\n').next())
        .unwrap_or(topic);

    let source_url = text
        .rsplit_once("Source: ")
        .map(|(_, url)| url.trim())
        .unwrap_or("");

    // Extract the first ~300 chars of the article body as a summary
    let body_start = text.find("\n\n").map(|i| i + 2).unwrap_or(0);
    let body_end = text.rfind("\n\nSource:").unwrap_or(text.len());
    let summary: String = text[body_start..body_end].chars().take(300).collect();

    let ui_data = serde_json::json!({
        "title": title,
        "summary": summary,
        "source_url": source_url,
        "topic": topic,
    });
    format!("[[[mcp-ui:knowledge:{}]]]\n{}", ui_data, text)
}

// ── Wikipedia helpers (outside the #[tool_router] block) ───────────────────

/// Extract the search topic from params.
///
/// Extract the search topic from tool call params.
///
/// Resolve the search topic for a Wikipedia lookup.
///
/// When a ToolCaller specialist is configured, it ALWAYS generates the topic
/// from the user's message — the main LLM's params are ignored. When no
/// ToolCaller is configured (capable models like Ollama), the model's params
/// are used directly.
const WIKI_TOPIC_SCHEMA: &str = r#"{"type":"object","properties":{"topic":{"type":"string","description":"The person, place, event, or concept to look up on Wikipedia"}},"required":["topic"]}"#;

async fn resolve_topic(params: &WikipediaQueryParams, tool_name: &str) -> String {
    // 1. ToolCaller specialist — PRIMARY when configured
    if let Some(args) = crate::generate_params(tool_name, WIKI_TOPIC_SCHEMA).await {
        if let Some(t) = args.get("topic").and_then(|v| v.as_str()) {
            let trimmed = t.trim();
            if !trimmed.is_empty() {
                println!(
                    "[wikipedia] resolve_topic: ToolCaller produced: {:?}",
                    trimmed
                );
                return trimmed.to_string();
            }
        }
    }

    // 2. No ToolCaller — use model's params directly (capable model path)
    if let Some(ref t) = params.topic {
        let trimmed = t.trim();
        if !trimmed.is_empty() {
            println!(
                "[wikipedia] resolve_topic: model param 'topic': {:?}",
                trimmed
            );
            return trimmed.to_string();
        }
    }

    // 3. Scan extras (models send params in unpredictable shapes)
    for key in &[
        "query", "title", "search", "q", "term", "input", "name", "article", "text", "subject",
    ] {
        if let Some(val) = params.extra.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    println!("[wikipedia] resolve_topic: extras '{}': {:?}", key, trimmed);
                    return trimmed.to_string();
                }
            }
        }
    }

    // 4. Last resort: clean user message
    let msg = crate::last_user_message();
    if !msg.is_empty() {
        let cleaned = clean_query_for_search(&msg);
        if !cleaned.is_empty() {
            println!(
                "[wikipedia] resolve_topic: user message: {:?} -> {:?}",
                msg, cleaned
            );
            return cleaned;
        }
    }

    println!("[wikipedia] resolve_topic: no topic found");
    String::new()
}

/// Strip common question prefixes to extract the core topic for search.
///
/// "who is Wangari Maathai?" -> "Wangari Maathai"
/// "tell me about black holes" -> "black holes"
/// "Nairobi" -> "Nairobi" (unchanged)
pub fn clean_query_for_search(raw: &str) -> String {
    let stripped = raw
        .trim()
        .trim_end_matches('?')
        .trim_end_matches('.')
        .trim();
    let lower = stripped.to_lowercase();
    // Ordered longest-first so more specific prefixes match before short ones.
    let prefixes = [
        "can you tell me about ",
        "could you tell me about ",
        "tell me about ",
        "tell me more about ",
        "i want to know about ",
        "i'd like to know about ",
        "what do you know about ",
        "what can you tell me about ",
        "would you recommend ",
        "do you recommend ",
        "should i ",
        "how about ",
        "who is ",
        "who was ",
        "who are ",
        "what is ",
        "what are ",
        "what was ",
        "what were ",
        "what is the ",
        "what are the ",
        "where is ",
        "where are ",
        "when was ",
        "when did ",
        "when is ",
        "how does ",
        "how do ",
        "how did ",
        "how is ",
        "why does ",
        "why do ",
        "why is ",
        "why did ",
        "explain ",
        "describe ",
        "look up ",
        "search for ",
        "search ",
        "find ",
        "define ",
    ];
    for prefix in prefixes {
        if lower.starts_with(prefix) {
            return stripped[prefix.len()..].trim().to_string();
        }
    }
    stripped.to_string()
}

// ── define_word helpers ───────────────────────────────────────────────────

const DICT_WORD_SCHEMA: &str = r#"{"type":"object","properties":{"word":{"type":"string","description":"The English word to define"}},"required":["word"]}"#;

/// Resolve the word to define from tool params.
///
/// Follows the same fallback chain as `resolve_topic`: ToolCaller -> model
/// params -> extras -> cleaned user message.
async fn resolve_word(params: &DefineWordParams) -> String {
    // 1. ToolCaller specialist
    if let Some(args) = crate::generate_params("define_word", DICT_WORD_SCHEMA).await {
        if let Some(w) = args.get("word").and_then(|v| v.as_str()) {
            let trimmed = w.trim();
            if !trimmed.is_empty() {
                println!(
                    "[knowledge] resolve_word: ToolCaller produced: {:?}",
                    trimmed
                );
                return trimmed.to_string();
            }
        }
    }

    // 2. Model's params
    if let Some(ref w) = params.word {
        let trimmed = w.trim();
        if !trimmed.is_empty() {
            println!(
                "[knowledge] resolve_word: model param 'word': {:?}",
                trimmed
            );
            return trimmed.to_string();
        }
    }

    // 3. Scan extras
    for key in &["word", "term", "query", "q", "input", "text"] {
        if let Some(val) = params.extra.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    println!("[knowledge] resolve_word: extras '{}': {:?}", key, trimmed);
                    return trimmed.to_string();
                }
            }
        }
    }

    // 4. Clean user message — strip "define" prefix etc.
    let msg = crate::last_user_message();
    if !msg.is_empty() {
        let cleaned = clean_query_for_search(&msg);
        if !cleaned.is_empty() {
            // Take just the first word if the message is long
            let first_word = cleaned.split_whitespace().next().unwrap_or("");
            if !first_word.is_empty() {
                println!(
                    "[knowledge] resolve_word: user message: {:?} -> {:?}",
                    msg, first_word
                );
                return first_word.to_string();
            }
        }
    }

    println!("[knowledge] resolve_word: no word found");
    String::new()
}

/// Format the Free Dictionary API response into a readable string.
fn format_dictionary_response(body: &serde_json::Value, word: &str) -> String {
    let entries = match body.as_array() {
        Some(arr) if !arr.is_empty() => arr,
        _ => return format!("Word '{}' not found.", word),
    };

    let entry = &entries[0];
    let display_word = entry["word"].as_str().unwrap_or(word);
    let phonetic = entry["phonetic"].as_str().unwrap_or("");

    let mut result = if phonetic.is_empty() {
        format!("**{}**\n", display_word)
    } else {
        format!("**{}** {}\n", display_word, phonetic)
    };

    if let Some(meanings) = entry["meanings"].as_array() {
        for meaning in meanings {
            let pos = meaning["partOfSpeech"].as_str().unwrap_or("unknown");
            result.push_str(&format!("\n*{}*\n", pos));

            if let Some(definitions) = meaning["definitions"].as_array() {
                for (i, def) in definitions.iter().enumerate().take(3) {
                    let definition = def["definition"].as_str().unwrap_or("");
                    result.push_str(&format!("{}. {}\n", i + 1, definition));

                    if let Some(example) = def["example"].as_str() {
                        result.push_str(&format!("   Example: \"{}\"\n", example));
                    }
                }
            }

            if let Some(synonyms) = meaning["synonyms"].as_array() {
                let syns: Vec<&str> = synonyms.iter().filter_map(|v| v.as_str()).take(5).collect();
                if !syns.is_empty() {
                    result.push_str(&format!("Synonyms: {}\n", syns.join(", ")));
                }
            }
        }
    }

    result
}

// ── search_books helpers ─────────────────────────────────────────────────

const BOOK_QUERY_SCHEMA: &str = r#"{"type":"object","properties":{"query":{"type":"string","description":"Book title, author, or subject to search for"}},"required":["query"]}"#;

/// Resolve the search query for Open Library.
///
/// Combines `query` + `author` if both present. Falls back through ToolCaller,
/// extras, and user message.
async fn resolve_book_query(params: &BookSearchParams) -> String {
    // 1. ToolCaller specialist
    if let Some(args) = crate::generate_params("search_books", BOOK_QUERY_SCHEMA).await {
        if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
            let trimmed = q.trim();
            if !trimmed.is_empty() {
                println!(
                    "[knowledge] resolve_book_query: ToolCaller produced: {:?}",
                    trimmed
                );
                return trimmed.to_string();
            }
        }
    }

    // 2. Model's params — combine query + author
    let mut parts = Vec::new();
    if let Some(ref q) = params.query {
        let trimmed = q.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    if let Some(ref a) = params.author {
        let trimmed = a.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    if !parts.is_empty() {
        let combined = parts.join(" ");
        println!(
            "[knowledge] resolve_book_query: model params: {:?}",
            combined
        );
        return combined;
    }

    // 3. Scan extras
    for key in &[
        "query", "title", "search", "q", "book", "subject", "input", "text",
    ] {
        if let Some(val) = params.extra.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    println!(
                        "[knowledge] resolve_book_query: extras '{}': {:?}",
                        key, trimmed
                    );
                    return trimmed.to_string();
                }
            }
        }
    }

    // 4. Clean user message
    let msg = crate::last_user_message();
    if !msg.is_empty() {
        let cleaned = clean_query_for_search(&msg);
        if !cleaned.is_empty() {
            println!(
                "[knowledge] resolve_book_query: user message: {:?} -> {:?}",
                msg, cleaned
            );
            return cleaned;
        }
    }

    println!("[knowledge] resolve_book_query: no query found");
    String::new()
}

// ── Wikipedia fetch internals ──────────────────────────────────────────────

#[derive(Debug)]
pub enum WikiFetchError {
    NotFound,
    Mcp(ErrorData),
}

impl KnowledgeMcpServer {
    /// Fetch the full article content for an exact Wikipedia title.
    ///
    /// Uses the MediaWiki `action=query&prop=extracts` endpoint which returns
    /// the complete article as plain text (no HTML). Falls back to the REST
    /// summary API if the full extract is empty.
    pub async fn fetch_article_summary(&self, title: &str) -> Result<String, WikiFetchError> {
        let url = format!(
            "https://en.wikipedia.org/w/api.php?action=query&titles={}&prop=extracts|info&explaintext=1&inprop=url&format=json&redirects=1",
            urlencoding::encode(title),
        );
        println!("[wikipedia] GET {}", url);

        let resp = self
            .http_client
            .get(&url)
            .header("user-agent", WIKI_UA)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| {
                println!("[wikipedia] article request failed: {e}");
                WikiFetchError::Mcp(ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Wikipedia request failed: {e}"),
                    None,
                ))
            })?;

        println!("[wikipedia] article response status: {}", resp.status());

        if !resp.status().is_success() {
            println!(
                "[wikipedia] article fetch failed with HTTP {}",
                resp.status()
            );
            return Err(WikiFetchError::Mcp(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Wikipedia returned HTTP {}", resp.status()),
                None,
            )));
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            println!("[wikipedia] failed to parse article response: {e}");
            WikiFetchError::Mcp(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to parse response: {e}"),
                None,
            ))
        })?;

        // MediaWiki returns pages as { "query": { "pages": { "<id>": { ... } } } }
        let pages = &body["query"]["pages"];
        let page = pages.as_object().and_then(|m| m.values().next());

        let page = match page {
            Some(p) if p.get("missing").is_none() => p,
            _ => return Err(WikiFetchError::NotFound),
        };

        let display_title = page["title"].as_str().unwrap_or(title);
        let extract = page["extract"].as_str().unwrap_or("");
        let fallback_url = format!(
            "https://en.wikipedia.org/wiki/{}",
            urlencoding::encode(title)
        );
        let page_url = page["fullurl"].as_str().unwrap_or(&fallback_url);

        if extract.is_empty() {
            return Err(WikiFetchError::NotFound);
        }

        // Cap article length to fit within context alongside tool schemas + system prompt.
        // 4000 chars ~ 1000 tokens — on a 16K context there's plenty of room for the response.
        let extract = crate::format::truncate_to_budget(extract, 4000);

        println!(
            "[wikipedia] article fetched: title={:?}, extract_len={}",
            display_title,
            extract.len(),
        );

        Ok(format!(
            "# {}\n\n{}\n\nSource: {}",
            display_title, extract, page_url,
        ))
    }

    /// Search Wikipedia and fetch the summary of the best matching article.
    pub async fn search_and_fetch_best(&self, query: &str) -> Result<String, ErrorData> {
        let search_url = format!(
            "https://en.wikipedia.org/w/api.php?action=query&list=search&srsearch={}&srlimit=1&format=json",
            urlencoding::encode(query),
        );
        println!("[wikipedia] fallback search: GET {}", search_url);

        let resp = self
            .http_client
            .get(&search_url)
            .header("user-agent", WIKI_UA)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| {
                println!("[wikipedia] fallback search request failed: {e}");
                ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Wikipedia search failed: {e}"),
                    None,
                )
            })?;

        if !resp.status().is_success() {
            return Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Wikipedia search returned HTTP {}", resp.status()),
                None,
            ));
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to parse search: {e}"),
                None,
            )
        })?;

        let best_title = body["query"]["search"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|item| item["title"].as_str());

        match best_title {
            Some(found) => {
                println!("[wikipedia] fallback found: '{}'", found);
                match self.fetch_article_summary(found).await {
                    Ok(text) => Ok(text),
                    Err(WikiFetchError::NotFound) => Ok(format!(
                        "Wikipedia search matched '{}' but the article could not be loaded.",
                        found
                    )),
                    Err(WikiFetchError::Mcp(e)) => Err(e),
                }
            }
            None => {
                println!(
                    "[wikipedia] fallback search returned no results for '{}'",
                    query
                );
                Ok(format!("No Wikipedia articles found for '{}'.", query))
            }
        }
    }
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use rmcp::ServiceExt;
use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct KnowledgeDeps {
    http_client: reqwest::Client,
}

static KNOWLEDGE_DEPS: OnceLock<KnowledgeDeps> = OnceLock::new();

/// Initialize knowledge server dependencies. Call once at startup.
pub fn init_knowledge_deps(http_client: reqwest::Client) {
    let _ = KNOWLEDGE_DEPS.set(KnowledgeDeps { http_client });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_knowledge_server(reader: DuplexStream, writer: DuplexStream) {
    let deps = KNOWLEDGE_DEPS
        .get()
        .expect("init_knowledge_deps() not called");
    let server = KnowledgeMcpServer::new(deps.http_client.clone());
    tokio::spawn(async move {
        match server.serve((reader, writer)).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(e) => tracing::error!("giap-knowledge MCP server failed: {e}"),
        }
    });
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_server() -> KnowledgeMcpServer {
        KnowledgeMcpServer::new(reqwest::Client::new())
    }

    #[test]
    fn clean_query_strips_prefixes() {
        assert_eq!(
            clean_query_for_search("who is Wangari Maathai?"),
            "Wangari Maathai"
        );
        assert_eq!(
            clean_query_for_search("tell me about black holes"),
            "black holes"
        );
        assert_eq!(clean_query_for_search("Nairobi"), "Nairobi");
        assert_eq!(
            clean_query_for_search("what is the speed of light?"),
            "the speed of light"
        );
    }

    // resolve_topic tests — no ToolCaller set, so it falls through to model params
    #[tokio::test]
    async fn resolve_topic_from_canonical_field() {
        let params = WikipediaQueryParams {
            topic: Some("Nairobi".to_string()),
            limit: None,
            extra: Default::default(),
        };
        assert_eq!(resolve_topic(&params, "test").await, "Nairobi");
    }

    #[tokio::test]
    async fn resolve_topic_from_extras() {
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "query".to_string(),
            serde_json::Value::String("black holes".to_string()),
        );
        let params = WikipediaQueryParams {
            topic: None,
            limit: None,
            extra,
        };
        assert_eq!(resolve_topic(&params, "test").await, "black holes");
    }

    #[tokio::test]
    async fn resolve_topic_empty_when_nothing_provided() {
        let params = WikipediaQueryParams {
            topic: None,
            limit: None,
            extra: Default::default(),
        };
        assert_eq!(resolve_topic(&params, "test").await, "");
    }

    #[test]
    fn server_constructs() {
        let _server = test_server();
    }

    #[test]
    fn prepend_knowledge_hint_extracts_source_url() {
        let text = "# Nairobi\n\nNairobi is the capital of Kenya.\n\nSource: https://en.wikipedia.org/wiki/Nairobi";
        let result = prepend_knowledge_hint("Nairobi", text);
        assert!(result.contains("\"source_url\":\"https://en.wikipedia.org/wiki/Nairobi\""));
    }

    #[test]
    fn prepend_knowledge_hint_missing_source_uses_empty_string() {
        // When there's no "Source: " line, source_url should be "" not the full text.
        let text = "# Test\n\nSome article with no source line.";
        let result = prepend_knowledge_hint("Test", text);
        assert!(result.contains("\"source_url\":\"\""));
    }

    #[test]
    fn fetch_article_truncation_is_applied() {
        // Simulate what happens when extract exceeds 4000 chars.
        let long_extract = "x".repeat(5000);
        let truncated = crate::format::truncate_to_budget(&long_extract, 4000);
        assert!(truncated.len() < 5000);
        assert!(truncated.contains("[Truncated"));
    }

    /// Exact title -> direct fetch succeeds.
    #[tokio::test]
    #[ignore] // requires internet
    async fn live_fetch_exact_title() {
        let server = test_server();
        let text = server.fetch_article_summary("Nairobi").await.unwrap();
        println!("{}", text);
        assert!(text.contains("Nairobi"), "extract should mention Nairobi");
        assert!(
            text.contains("Kenya"),
            "Nairobi article should mention Kenya"
        );
        assert!(text.contains("Source:"), "should include source URL");
    }

    /// Vague query that doesn't match an exact title -> auto-search fallback.
    #[tokio::test]
    #[ignore] // requires internet
    async fn live_vague_query_finds_article() {
        let server = test_server();
        let text = server.search_and_fetch_best("black holes").await.unwrap();
        println!("{}", text);
        assert!(
            text.contains("black hole") || text.contains("Black hole"),
            "should find the Black hole article"
        );
    }

    /// The full get_wikipedia_article flow: vague input -> 404 -> search -> fetch.
    #[tokio::test]
    #[ignore] // requires internet
    async fn live_get_article_auto_resolves_vague_topic() {
        let server = test_server();
        let text = server.search_and_fetch_best("volcanoes").await.unwrap();
        println!("{}", text);
        assert!(
            text.to_lowercase().contains("volcan"),
            "should resolve to a volcano-related article"
        );
    }

    /// Completely nonsensical query returns a graceful "not found" message.
    #[tokio::test]
    #[ignore] // requires internet
    async fn live_nonsense_query_returns_not_found() {
        let server = test_server();
        let text = server
            .search_and_fetch_best("xyzzy99foobar_nonexistent")
            .await
            .unwrap();
        println!("{}", text);
        assert!(
            text.contains("No Wikipedia articles found"),
            "should report no results for nonsense query"
        );
    }

    /// Misspelled topic still finds a relevant article via search.
    #[tokio::test]
    #[ignore] // requires internet
    async fn live_misspelled_topic_resolved() {
        let server = test_server();
        let text = server
            .search_and_fetch_best("Albert Einsten")
            .await
            .unwrap();
        println!("{}", text);
        let lower = text.to_lowercase();
        assert!(
            lower.contains("einstein")
                || lower.contains("physicist")
                || lower.contains("relativity"),
            "should resolve misspelled 'Albert Einsten' to Einstein article"
        );
    }

    // ── instant_answer tests ──────────────────────────────────────────────

    #[test]
    fn clean_query_strips_define_prefix() {
        assert_eq!(clean_query_for_search("define serendipity"), "serendipity");
        // "what does" is not a stripped prefix — only "what is", "what are" etc.
        // The function returns the full string minus trailing punctuation.
        assert_eq!(
            clean_query_for_search("what does ephemeral mean?"),
            "what does ephemeral mean"
        );
        // "what is" IS stripped:
        assert_eq!(clean_query_for_search("what is ephemeral?"), "ephemeral");
    }

    #[tokio::test]
    async fn resolve_topic_for_instant_answer() {
        let params = WikipediaQueryParams {
            topic: Some("Rust programming".to_string()),
            limit: None,
            extra: Default::default(),
        };
        assert_eq!(
            resolve_topic(&params, "instant_answer").await,
            "Rust programming"
        );
    }

    // ── define_word tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn resolve_word_from_param() {
        let params = DefineWordParams {
            word: Some("serendipity".to_string()),
            extra: Default::default(),
        };
        assert_eq!(resolve_word(&params).await, "serendipity");
    }

    #[tokio::test]
    async fn resolve_word_from_extras() {
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "term".to_string(),
            serde_json::Value::String("ephemeral".to_string()),
        );
        let params = DefineWordParams { word: None, extra };
        assert_eq!(resolve_word(&params).await, "ephemeral");
    }

    #[tokio::test]
    async fn resolve_word_empty_when_nothing_provided() {
        let params = DefineWordParams {
            word: None,
            extra: Default::default(),
        };
        assert_eq!(resolve_word(&params).await, "");
    }

    #[test]
    fn format_dictionary_response_handles_valid_json() {
        let body: serde_json::Value = serde_json::json!([{
            "word": "test",
            "phonetic": "/tɛst/",
            "meanings": [{
                "partOfSpeech": "noun",
                "definitions": [{
                    "definition": "A procedure to evaluate something.",
                    "example": "We ran a test."
                }],
                "synonyms": ["trial", "experiment"]
            }]
        }]);
        let result = format_dictionary_response(&body, "test");
        assert!(result.contains("**test**"));
        assert!(result.contains("/tɛst/"));
        assert!(result.contains("*noun*"));
        assert!(result.contains("A procedure to evaluate something."));
        assert!(result.contains("We ran a test."));
        assert!(result.contains("trial"));
    }

    #[test]
    fn format_dictionary_response_handles_empty_array() {
        let body: serde_json::Value = serde_json::json!([]);
        let result = format_dictionary_response(&body, "xyzzy");
        assert!(result.contains("not found"));
    }

    // ── search_books tests ────────────────────────────────────────────────

    #[tokio::test]
    async fn resolve_book_query_from_params() {
        let params = BookSearchParams {
            query: Some("Dune".to_string()),
            author: Some("Frank Herbert".to_string()),
            limit: None,
            extra: Default::default(),
        };
        assert_eq!(resolve_book_query(&params).await, "Dune Frank Herbert");
    }

    #[tokio::test]
    async fn resolve_book_query_from_query_only() {
        let params = BookSearchParams {
            query: Some("machine learning".to_string()),
            author: None,
            limit: None,
            extra: Default::default(),
        };
        assert_eq!(resolve_book_query(&params).await, "machine learning");
    }

    #[tokio::test]
    async fn resolve_book_query_from_extras() {
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "title".to_string(),
            serde_json::Value::String("Neuromancer".to_string()),
        );
        let params = BookSearchParams {
            query: None,
            author: None,
            limit: None,
            extra,
        };
        assert_eq!(resolve_book_query(&params).await, "Neuromancer");
    }

    #[tokio::test]
    async fn resolve_book_query_empty_when_nothing_provided() {
        let params = BookSearchParams {
            query: None,
            author: None,
            limit: None,
            extra: Default::default(),
        };
        assert_eq!(resolve_book_query(&params).await, "");
    }

    // ── Live integration tests (new tools) ────────────────────────────────

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_instant_answer_returns_result() {
        let server = test_server();
        // DuckDuckGo IA for a well-known topic — use the internal HTTP client directly
        let url =
            "https://api.duckduckgo.com/?q=Albert+Einstein&format=json&no_html=1&skip_disambig=1";
        let resp = server.http_client.get(url).send().await.unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        let abstract_text = body["AbstractText"].as_str().unwrap_or("");
        println!("instant_answer abstract: {}", abstract_text);
        assert!(
            !abstract_text.is_empty(),
            "DuckDuckGo should return an abstract for Einstein"
        );
    }

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_define_word_returns_definition() {
        let server = test_server();
        let url = "https://api.dictionaryapi.dev/api/v2/entries/en/serendipity";
        let resp = server.http_client.get(url).send().await.unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        let result = format_dictionary_response(&body, "serendipity");
        println!("define_word result:\n{}", result);
        assert!(result.contains("**serendipity**"));
        assert!(
            result.to_lowercase().contains("fortunate")
                || result.to_lowercase().contains("discovery")
                || result.to_lowercase().contains("happy"),
            "definition should describe serendipity"
        );
    }

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_search_books_returns_results() {
        let server = test_server();
        let url = "https://openlibrary.org/search.json?q=Dune+Frank+Herbert&limit=3";
        let resp = server.http_client.get(url).send().await.unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        let docs = body["docs"].as_array().expect("should have docs array");
        println!("search_books found {} results", docs.len());
        assert!(!docs.is_empty(), "should find Dune books");
        let first_title = docs[0]["title"].as_str().unwrap_or("");
        println!("first result: {}", first_title);
        assert!(
            first_title.to_lowercase().contains("dune"),
            "first result should be a Dune book"
        );
    }
}
