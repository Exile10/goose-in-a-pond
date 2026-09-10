//! Knowledge MCP Server — Wikipedia, dictionary, book search, and computation.
//!
//! Six tools. Four live here: `search_wikipedia`, `get_wikipedia_article`,
//! `define_word`, `search_books`. Two more — `compute_answer` and
//! `explore_computation` — are a second `#[tool_router]` impl on this same
//! server in [`crate::wolfram`], composed in [`KnowledgeMcpServer::new`].
//! Depends only on a `reqwest::Client` for HTTP fetches.
//!
//! There used to be a fifth tool here, `instant_answer`, over DuckDuckGo's
//! Instant Answer API. DuckDuckGo is gone, and Wolfram|Alpha took its place
//! rather than inheriting its job: what DuckDuckGo returned was overwhelmingly a
//! Wikipedia abstract, which `get_wikipedia_article` already fetches in full, so
//! the tool cost a schema in every turn's prompt to reach a worse copy of a
//! sibling's source. Wolfram computes, which nothing in this pond could do.

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
    /// Name, phrase, or question to look up.
    pub topic: Option<String>,
    /// Max results, default 5, max 10 (search_wikipedia only).
    pub limit: Option<u32>,
    /// Catch-all for any extra fields the model sends (e.g. "query", "title", "search").
    /// Not part of the advertised schema — exists purely to absorb unexpected keys.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct DefineWordParams {
    pub word: Option<String>,
    /// Catch-all for any extra fields the model sends (e.g. "term", "query").
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct BookSearchParams {
    /// Book title, topic, or keyword.
    pub query: Option<String>,
    /// Filter by author name.
    pub author: Option<String>,
    /// Max results, default 5, max 10.
    pub limit: Option<u32>,
    /// Catch-all for any extra fields the model sends.
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
    // `pub(crate)` because the Wolfram tools are a second `#[tool_router]` impl
    // on this same server (see `wolfram.rs`) and share its client pool.
    pub(crate) http_client: reqwest::Client,
    // Read by the generated `tool_handler` code, which is pointed at this field
    // explicitly — see the note on the `ServerHandler` impl below.
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl KnowledgeMcpServer {
    pub fn new(http_client: reqwest::Client) -> Self {
        Self {
            http_client,
            // Two routers, one server: the Wolfram tools live in `wolfram.rs`
            // so this file stays about reference lookups, but they belong to
            // the same extension because they answer the same kind of question.
            tool_router: Self::tool_router() + Self::wolfram_tool_router(),
        }
    }

    #[tool(description = "\
Look up a topic on Wikipedia (auto-searches inexact titles). First choice for \
factual questions. Answer in your own words; never repeat the extract \
verbatim.")]
    async fn get_wikipedia_article(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<WikipediaQueryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("get_wikipedia_article");
        // Definitive: what did the MCP server receive from Goose?
        eprintln!("[wikipedia] ╔═══ MCP SERVER RECEIVED ═══");
        eprintln!("[wikipedia] ║ params.topic: {:?}", params.0.topic);
        eprintln!("[wikipedia] ║ params.extra: {:?}", params.0.extra);
        eprintln!("[wikipedia] ╚═══════════════════════════");

        let topic = resolve_topic(&params.0, "get_wikipedia_article").await;
        eprintln!(
            "[wikipedia] get_wikipedia_article called: topic={:?}",
            topic
        );

        if topic.is_empty() {
            // Nudge: return guidance as content so the model can retry
            eprintln!("[wikipedia] empty topic, nudging model to retry");
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
                eprintln!(
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
}

// `router = self.tool_router` is load-bearing. The default is
// `Self::tool_router()`, the macro-generated function for THIS impl block only —
// so with a bare `#[tool_handler]` the composed field built in `new()` is
// ignored and the four tools below are the only ones `list_tools` ever reports.
// The Wolfram tools compiled, unit-tested and were never offered to the model;
// `both_tools_are_actually_exposed_by_the_server` is what caught it.
#[tool_handler(router = self.tool_router)]
impl ServerHandler for KnowledgeMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-knowledge",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Knowledge server — reference lookups, definitions, and computation.\n\n\
                 Tools: get_wikipedia_article (deep articles), \
                 search_wikipedia (find articles), define_word (dictionary), search_books (book search), \
                 compute_answer (Wolfram|Alpha), explore_computation (open a Wolfram suggestion).\n\n\
                 Reading vs computing is the split. If the answer has to be worked out — \
                 arithmetic, a unit or currency conversion, a date difference, a statistic — \
                 use compute_answer. If it has to be read — who someone was, what happened, \
                 what a place is like — use get_wikipedia_article; it auto-searches when the \
                 title is not exact. Use search_wikipedia only to disambiguate between several \
                 possible articles. For word definitions, use define_word. For book queries, \
                 use search_books.\n\
                 A compute_answer result may end with suggestions, each with an id like 'w3'. \
                 When one of them is what the user actually meant, call explore_computation \
                 with that id rather than guessing or re-asking.\n\
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
                eprintln!(
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
            eprintln!(
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
                    eprintln!("[wikipedia] resolve_topic: extras '{}': {:?}", key, trimmed);
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
            eprintln!(
                "[wikipedia] resolve_topic: user message: {:?} -> {:?}",
                msg, cleaned
            );
            return cleaned;
        }
    }

    eprintln!("[wikipedia] resolve_topic: no topic found");
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
        eprintln!("[wikipedia] GET {}", url);

        let resp = crate::http::traced_get_with(&self.http_client, &url, |b| {
            b.header("user-agent", WIKI_UA)
                .timeout(std::time::Duration::from_secs(15))
        })
        .await
        .map_err(|e| {
            eprintln!("[wikipedia] article request failed: {e}");
            WikiFetchError::Mcp(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Wikipedia request failed: {e}"),
                None,
            ))
        })?;

        eprintln!("[wikipedia] article response status: {}", resp.status());

        if !resp.status().is_success() {
            eprintln!(
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
            eprintln!("[wikipedia] failed to parse article response: {e}");
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

        eprintln!(
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
        eprintln!("[wikipedia] fallback search: GET {}", search_url);

        let resp = crate::http::traced_get_with(&self.http_client, &search_url, |b| {
            b.header("user-agent", WIKI_UA)
                .timeout(std::time::Duration::from_secs(10))
        })
        .await
        .map_err(|e| {
            eprintln!("[wikipedia] fallback search request failed: {e}");
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
                eprintln!("[wikipedia] fallback found: '{}'", found);
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
                eprintln!(
                    "[wikipedia] fallback search returned no results for '{}'",
                    query
                );
                Ok(crate::format::format_no_results(
                    &format!("Wikipedia articles for '{}'", query),
                    &["giap-knowledge__compute_answer"],
                ))
            }
        }
    }
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

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
    // Missing deps = this path never initialised this extension (the voice/CLI
    // binary vs `serve` install different families). A skipped extension is a
    // logged, contained failure; a panic here took down every builtin server's
    // startup at once (2026-08-27, giap-context in the voice child).
    let Some(deps) = KNOWLEDGE_DEPS.get() else {
        tracing::error!(
            "spawn_knowledge_server called before init_knowledge_deps — extension will not start"
        );
        return;
    };
    let server = KnowledgeMcpServer::new(deps.http_client.clone());
    crate::serve_builtin("giap-knowledge", server, reader, writer);
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
        eprintln!("{}", text);
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
        eprintln!("{}", text);
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
        eprintln!("{}", text);
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
        eprintln!("{}", text);
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
        eprintln!("{}", text);
        let lower = text.to_lowercase();
        assert!(
            lower.contains("einstein")
                || lower.contains("physicist")
                || lower.contains("relativity"),
            "should resolve misspelled 'Albert Einsten' to Einstein article"
        );
    }

    // ── query-cleaning tests ──────────────────────────────────────────────

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
    #[ignore] // requires internet
    async fn live_search_books_returns_results() {
        let server = test_server();
        let url = "https://openlibrary.org/search.json?q=Dune+Frank+Herbert&limit=3";
        let resp = server.http_client.get(url).send().await.unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        let docs = body["docs"].as_array().expect("should have docs array");
        eprintln!("search_books found {} results", docs.len());
        assert!(!docs.is_empty(), "should find Dune books");
        let first_title = docs[0]["title"].as_str().unwrap_or("");
        eprintln!("first result: {}", first_title);
        assert!(
            first_title.to_lowercase().contains("dune"),
            "first result should be a Dune book"
        );
    }
}
