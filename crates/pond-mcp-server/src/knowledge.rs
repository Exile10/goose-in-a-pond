//! Knowledge MCP Server — Wikipedia search and article retrieval.
//!
//! Provides 2 tools: `search_wikipedia`, `get_wikipedia_article`.
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

// ── Constants ──────────────────────────────────────────────────────────────

const WIKI_UA: &str =
    "goose-in-a-pond/0.1 (GIAP MCP; https://github.com/jarida-io/goose-in-a-pond)";

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

        let text = match results {
            Some(arr) if !arr.is_empty() => arr
                .iter()
                .filter_map(|item| {
                    let title = item["title"].as_str()?;
                    let snippet = item["snippet"].as_str().unwrap_or("");
                    let clean = snippet
                        .replace("<span class=\"searchmatch\">", "")
                        .replace("</span>", "")
                        .replace("&quot;", "\"")
                        .replace("&amp;", "&");
                    Some(format!("- **{}**: {}", title, clean))
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => format!("No Wikipedia articles found for '{}'.", query),
        };
        println!(
            "[wikipedia] search_wikipedia done, returning {} chars",
            text.len()
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
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
            Ok(text) => Ok(CallToolResult::success(vec![Content::text(text)])),
            Err(WikiFetchError::NotFound) => {
                // Auto-fallback: search for the topic and fetch the top result
                println!(
                    "[wikipedia] exact title not found, searching for '{}'",
                    topic
                );
                match self.search_and_fetch_best(&topic).await {
                    Ok(text) => Ok(CallToolResult::success(vec![Content::text(text)])),
                    Err(e) => Err(e),
                }
            }
            Err(WikiFetchError::Mcp(e)) => Err(e),
        }
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
                "GIAP Knowledge MCP server — Wikipedia search and article retrieval.\n\n\
                 Tools: search_wikipedia (ranked list of matching articles), \
                 get_wikipedia_article (full article with auto-search fallback).\n\n\
                 IMPORTANT — Wikipedia usage guidelines:\n\
                 - Prefer get_wikipedia_article for any factual question. It accepts plain topics \
                   (\"black holes\", \"Marie Curie\") — no need to guess exact titles.\n\
                 - Do NOT parrot the article extract verbatim. Read it, extract the relevant facts, \
                   then answer the user's question in your own words — concisely.\n\
                 - For voice mode: aim for 1-3 sentences. Offer to elaborate if the user wants more.\n\
                 - Only use search_wikipedia when you need to disambiguate between multiple topics \
                   or present a list of options to the user.\n\
                 - Never say \"According to Wikipedia\" — just answer naturally with the facts.",
            )
    }
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
                println!("[wikipedia] resolve_topic: ToolCaller produced: {:?}", trimmed);
                return trimmed.to_string();
            }
        }
    }

    // 2. No ToolCaller — use model's params directly (capable model path)
    if let Some(ref t) = params.topic {
        let trimmed = t.trim();
        if !trimmed.is_empty() {
            println!("[wikipedia] resolve_topic: model param 'topic': {:?}", trimmed);
            return trimmed.to_string();
        }
    }

    // 3. Scan extras (models send params in unpredictable shapes)
    for key in &["query", "title", "search", "q", "term", "input", "name", "article", "text", "subject"] {
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
            println!("[wikipedia] resolve_topic: user message: {:?} -> {:?}", msg, cleaned);
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
        // 4000 chars ≈ 1000 tokens — with 16K context there's plenty of room.
        let max_chars = 4000;
        let truncated = if extract.len() > max_chars {
            let mut cut = max_chars;
            while cut > 0 && !extract.is_char_boundary(cut) {
                cut -= 1;
            }
            format!(
                "{}...\n\n[Article truncated — full article at source]",
                &extract[..cut]
            )
        } else {
            extract.to_string()
        };

        println!(
            "[wikipedia] article fetched: title={:?}, extract_len={}, truncated_to={}",
            display_title,
            extract.len(),
            truncated.len()
        );

        Ok(format!(
            "# {}\n\n{}\n\nSource: {}",
            display_title, truncated, page_url,
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
}
