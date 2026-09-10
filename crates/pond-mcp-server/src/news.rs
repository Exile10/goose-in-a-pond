//! News MCP Server — news discovery, headlines, and search.
//!
//! Provides 3 tools: `get_top_stories` (Hacker News), `search_news` (The Guardian),
//! `get_headlines` (GNews).
//! Depends on a `reqwest::Client` for HTTP fetches. The Guardian and GNews API
//! keys come from the pond's `SecretRepository` via `crate::secrets` — never
//! from `Settings`, which `GET /api/v1/settings` serialises wholesale (PAI-2 P2).

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, Implementation, InitializeResult, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;

// ── Parameter structs ──────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct TopStoriesParams {
    /// top|best|new|ask|show|job (default top).
    pub category: Option<String>,
    /// Default 5, max 15.
    pub limit: Option<u32>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SearchNewsParams {
    /// Search keywords.
    pub query: Option<String>,
    /// Section filter: world, politics, technology, sport, etc.
    pub section: Option<String>,
    /// Default 5, max 10.
    pub limit: Option<u32>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct HeadlinesParams {
    /// general|world|nation|business|technology|entertainment|sports|science|health.
    pub topic: Option<String>,
    /// 2-letter country code (us, ke, gb). Omit for global.
    pub country: Option<String>,
    /// Language code (default en).
    pub language: Option<String>,
    /// Default 5, max 10.
    pub limit: Option<u32>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

// ── Constants ──────────────────────────────────────────────────────────────

const GUARDIAN_BASE_URL: &str = "https://content.guardianapis.com";
const GNEWS_BASE_URL: &str = "https://gnews.io/api/v4";

/// Where a household gets the keys that make these tools good rather than
/// merely working. Both tiers are free.
const GUARDIAN_SIGNUP: &str = "https://open-platform.theguardian.com/access/";
const GNEWS_SIGNUP: &str = "https://gnews.io/register";
const WIKIMEDIA_FEED_URL: &str = "https://api.wikimedia.org/feed/v1/wikipedia/en/featured";

const SEARCH_NEWS_BUDGET: usize = 2000;
const HEADLINES_BUDGET: usize = 1500;

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct NewsMcpServer {
    http_client: reqwest::Client,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl NewsMcpServer {
    pub fn new(http_client: reqwest::Client) -> Self {
        Self {
            http_client,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "\
Search news by keyword, or today's world events. Use for news about a \
specific topic.")]
    async fn search_news(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<SearchNewsParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        crate::set_current_tool("search_news");
        // 1. Guardian API key — from the secret store, never from `Settings`
        //    (PAI-2 P2: `GET /settings` serialises that struct wholesale).
        //    A missing key is not an error; the tool degrades to the keyless
        //    Wikimedia feed, which is also what happens when no entry point
        //    installed a secret store.
        let guardian_key = crate::secrets::secret("GUARDIAN_API_KEY")
            .await
            .filter(|k| !k.trim().is_empty());

        if let Some(api_key) = guardian_key {
            return self.search_news_guardian(api_key.trim(), &params.0).await;
        }

        // Fallback: Wikimedia Featured Content Feed (no API key required).
        // A real answer, from a worse source. `format_degraded` says so in the
        // result; the old `eprintln!` said so on the server's stderr, where no
        // user has ever looked.
        tracing::warn!(
            target: "giap::trace",
            kind = "tool_degraded",
            tool = "search_news",
            missing = "GUARDIAN_API_KEY",
            "no Guardian key — answering from the Wikimedia feed"
        );
        let query = params.0.query.as_deref().unwrap_or("").trim().to_string();
        let result = self.search_news_wikimedia(&query).await?;
        Ok(crate::format::degrade_result(
            result,
            "Guardian API key",
            GUARDIAN_SIGNUP,
        ))
    }

    #[tool(description = "\
Today's top general/breaking news headlines when no specific topic is asked.")]
    async fn get_headlines(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<HeadlinesParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        crate::set_current_tool("get_headlines");
        // 1. GNews API key — from the secret store (PAI-2 P2).
        let gnews_key = crate::secrets::secret("GNEWS_API_KEY")
            .await
            .filter(|k| !k.trim().is_empty());

        if let Some(api_key) = gnews_key {
            return self.get_headlines_gnews(api_key.trim(), &params.0).await;
        }

        // Fallback: Wikimedia Featured Content Feed (no API key required).
        tracing::warn!(
            target: "giap::trace",
            kind = "tool_degraded",
            tool = "get_headlines",
            missing = "GNEWS_API_KEY",
            "no GNews key — answering from the Wikimedia feed"
        );
        let result = self.get_headlines_wikimedia().await?;
        Ok(crate::format::degrade_result(
            result,
            "GNews API key",
            GNEWS_SIGNUP,
        ))
    }
}

#[tool_handler]
impl ServerHandler for NewsMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-news",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP News server — current events and headlines.\n\n\
                 Tools: get_top_stories (tech news from HN), search_news (world events/search), \
                 get_headlines (today's top stories).\n\n\
                 For tech news: use get_top_stories. For world events or keyword search: use search_news. \
                 For general 'what's in the news': use get_headlines.\n\
                 All tools work without API keys. Guardian/GNews keys add keyword search and filtering.\n\
                 Summarize the most relevant items — don't list everything verbatim.",
            )
    }
}

// ── Extracted tool implementations ────────────────────────────────────────

impl NewsMcpServer {
    /// Guardian API path for search_news (preferred when key is configured).
    async fn search_news_guardian(
        &self,
        api_key: &str,
        params: &SearchNewsParams,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let query = resolve_query(params).await;
        eprintln!("[news] search_news (Guardian): query={:?}", query);

        if query.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "I need a search query. Retry with a 'query' parameter containing keywords to search for.",
            )]));
        }

        let limit = params.limit.unwrap_or(5).clamp(1, 10);

        let mut url = format!(
            "{}/search?q={}&api-key={}&show-fields=trailText&page-size={}",
            GUARDIAN_BASE_URL,
            urlencoding::encode(&query),
            urlencoding::encode(api_key),
            limit,
        );
        if let Some(ref section) = params.section {
            let trimmed = section.trim();
            if !trimmed.is_empty() {
                url.push_str(&format!("&section={}", urlencoding::encode(trimmed)));
            }
        }
        eprintln!("[news] GET {}", url.replace(api_key, "***"));

        let resp = match crate::http::traced_get_with(&self.http_client, &url, |b| {
            b.timeout(std::time::Duration::from_secs(10))
        })
        .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[news] Guardian request failed: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("The Guardian", &e.to_string()),
                )]));
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            eprintln!("[news] Guardian returned HTTP {status}");
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_api_error("The Guardian", &format!("HTTP {status}")),
            )]));
        }

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[news] failed to parse Guardian response: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("The Guardian", &e.to_string()),
                )]));
            }
        };

        let results = body["response"]["results"].as_array();
        let mut ui_items: Vec<serde_json::Value> = Vec::new();
        let items: Vec<String> = results
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let title = item["webTitle"].as_str()?;
                        let section = item["sectionName"].as_str().unwrap_or("");
                        let date = item["webPublicationDate"]
                            .as_str()
                            .unwrap_or("")
                            .get(..10)
                            .unwrap_or("");
                        let url = item["webUrl"].as_str().unwrap_or("");
                        let trail = item["fields"]["trailText"]
                            .as_str()
                            .unwrap_or("")
                            .replace("<p>", "")
                            .replace("</p>", "");

                        ui_items.push(serde_json::json!({
                            "headline": title,
                            "tag": section,
                            "timeAgo": date,
                            "source": "The Guardian",
                        }));

                        let summary = if trail.len() > 120 {
                            format!(
                                "{}...",
                                &trail[..trail
                                    .char_indices()
                                    .nth(120)
                                    .map(|(i, _)| i)
                                    .unwrap_or(trail.len())]
                            )
                        } else {
                            trail
                        };

                        Some(format!(
                            "**{}** ({}, {}) — {} | {}",
                            title, section, date, summary, url,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let header = format!("Guardian search: \"{}\"", query);
        let text = crate::format::format_list_result(&items, &header, SEARCH_NEWS_BUDGET);
        eprintln!(
            "[news] search_news (Guardian) done, {} items, {} chars",
            items.len(),
            text.len()
        );

        if !ui_items.is_empty() {
            let ui_data = serde_json::json!({ "items": ui_items });
            let hint = format!("[[[mcp-ui:news:{}]]]\n", ui_data);
            let full_result = format!("{}{}", hint, text);
            return Ok(CallToolResult::success(vec![Content::text(full_result)]));
        }

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    /// Wikimedia Featured Content fallback for search_news (no API key needed).
    async fn search_news_wikimedia(
        &self,
        query: &str,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let body = match self.fetch_wikimedia_feed().await {
            Ok(b) => b,
            Err(text) => return Ok(CallToolResult::success(vec![Content::text(text)])),
        };

        let news = body["news"].as_array();
        let items: Vec<String> = news
            .map(|arr| arr.iter().filter_map(format_wikimedia_story).collect())
            .unwrap_or_default();

        if items.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_no_results(
                    // Nothing else on the pond reports current events.
                    "current events from Wikipedia today",
                    &[],
                ),
            )]));
        }

        // Build UI hints from the wiki news stories
        let ui_items: Vec<serde_json::Value> = items
            .iter()
            .map(|item| {
                serde_json::json!({
                    "headline": item.trim_start_matches("**Story**: "),
                    "tag": "World",
                    "timeAgo": "Today",
                    "source": "Wikipedia",
                })
            })
            .collect();

        let header = "Today's World News (via Wikipedia)";
        let mut text = crate::format::format_list_result(&items, header, SEARCH_NEWS_BUDGET);
        if query.is_empty() {
            text.push_str(
                "\n\n(Source: Wikipedia current events — for keyword search, \
                 add a Guardian API key in Settings.)",
            );
        } else {
            text.push_str(&format!(
                "\n\nNOTE: no keyword news source is configured, so this is today's \
                 GENERAL world news — it is NOT a search for '{query}'. If nothing \
                 above is about '{query}', this is NOT the answer: say so plainly, \
                 and that adding a Guardian or GNews API key in Settings would let \
                 you search the news by keyword."
            ));
        }
        eprintln!(
            "[news] search_news (Wikimedia) done, {} items, {} chars",
            items.len(),
            text.len()
        );

        if !ui_items.is_empty() {
            let ui_data = serde_json::json!({ "items": ui_items });
            let hint = format!("[[[mcp-ui:news:{}]]]\n", ui_data);
            let full_result = format!("{}{}", hint, text);
            return Ok(CallToolResult::success(vec![Content::text(full_result)]));
        }

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    /// GNews API path for get_headlines (preferred when key is configured).
    async fn get_headlines_gnews(
        &self,
        api_key: &str,
        params: &HeadlinesParams,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let topic = params
            .topic
            .as_deref()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .unwrap_or("general");
        let language = params
            .language
            .as_deref()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .unwrap_or("en");
        let limit = params.limit.unwrap_or(5).clamp(1, 10);

        let mut url = format!(
            "{}/top-headlines?token={}&topic={}&lang={}&max={}",
            GNEWS_BASE_URL,
            urlencoding::encode(api_key),
            urlencoding::encode(topic),
            urlencoding::encode(language),
            limit,
        );
        if let Some(ref country) = params.country {
            let trimmed = country.trim();
            if !trimmed.is_empty() {
                url.push_str(&format!("&country={}", urlencoding::encode(trimmed)));
            }
        }
        eprintln!("[news] GET {}", url.replace(api_key, "***"));

        let resp = match crate::http::traced_get_with(&self.http_client, &url, |b| {
            b.timeout(std::time::Duration::from_secs(10))
        })
        .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[news] GNews request failed: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("GNews", &e.to_string()),
                )]));
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            eprintln!("[news] GNews returned HTTP {status}");
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_api_error("GNews", &format!("HTTP {status}")),
            )]));
        }

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[news] failed to parse GNews response: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("GNews", &e.to_string()),
                )]));
            }
        };

        let articles = body["articles"].as_array();
        let mut ui_items: Vec<serde_json::Value> = Vec::new();
        let items: Vec<String> = articles
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let title = item["title"].as_str()?;
                        let source = item["source"]["name"].as_str().unwrap_or("Unknown");
                        let date = item["publishedAt"]
                            .as_str()
                            .unwrap_or("")
                            .get(..10)
                            .unwrap_or("");
                        let description = item["description"].as_str().unwrap_or("");
                        let url = item["url"].as_str().unwrap_or("");

                        ui_items.push(serde_json::json!({
                            "headline": title,
                            "tag": topic,
                            "timeAgo": date,
                            "source": source,
                        }));

                        let summary = if description.len() > 120 {
                            format!(
                                "{}...",
                                &description[..description
                                    .char_indices()
                                    .nth(120)
                                    .map(|(i, _)| i)
                                    .unwrap_or(description.len())]
                            )
                        } else {
                            description.to_string()
                        };

                        Some(format!(
                            "**{}** ({}, {}) — {} | {}",
                            title, source, date, summary, url,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let header = format!("Headlines — {} ({})", topic, language);
        let text = crate::format::format_list_result(&items, &header, HEADLINES_BUDGET);
        eprintln!(
            "[news] get_headlines (GNews) done, {} items, {} chars",
            items.len(),
            text.len()
        );

        if !ui_items.is_empty() {
            let ui_data = serde_json::json!({ "items": ui_items });
            let hint = format!("[[[mcp-ui:news:{}]]]\n", ui_data);
            let full_result = format!("{}{}", hint, text);
            return Ok(CallToolResult::success(vec![Content::text(full_result)]));
        }

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    /// Wikimedia Featured Content fallback for get_headlines (no API key needed).
    async fn get_headlines_wikimedia(&self) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let body = match self.fetch_wikimedia_feed().await {
            Ok(b) => b,
            Err(text) => return Ok(CallToolResult::success(vec![Content::text(text)])),
        };

        // News stories
        let news_items: Vec<String> = body["news"]
            .as_array()
            .map(|arr| arr.iter().filter_map(format_wikimedia_story).collect())
            .unwrap_or_default();

        // Trending / most-read articles
        let mostread_items: Vec<String> = body["mostread"]["articles"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .take(5)
                    .filter_map(|article| {
                        let title = article["titles"]["normalized"].as_str()?;
                        let extract = article["extract"].as_str().unwrap_or("");
                        let short = if extract.len() > 120 {
                            format!(
                                "{}...",
                                &extract[..extract
                                    .char_indices()
                                    .nth(120)
                                    .map(|(i, _)| i)
                                    .unwrap_or(extract.len())]
                            )
                        } else {
                            extract.to_string()
                        };
                        Some(format!("**{}** — {}", title, short))
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Build combined output
        let mut text = String::with_capacity(HEADLINES_BUDGET);
        text.push_str("**Today's News** (via Wikipedia)\n\n");

        if news_items.is_empty() {
            text.push_str("No current events available today.\n");
        } else {
            for item in &news_items {
                let line = format!("- {}\n", item);
                if text.len() + line.len() > HEADLINES_BUDGET - 200 {
                    text.push_str("...\n");
                    break;
                }
                text.push_str(&line);
            }
        }

        if !mostread_items.is_empty() {
            text.push_str("\n**Trending Articles**\n\n");
            for item in &mostread_items {
                let line = format!("- {}\n", item);
                if text.len() + line.len() > HEADLINES_BUDGET - 50 {
                    text.push_str("...\n");
                    break;
                }
                text.push_str(&line);
            }
        }

        let trimmed = text.trim_end().to_string();
        eprintln!(
            "[news] get_headlines (Wikimedia) done, {} news + {} trending, {} chars",
            news_items.len(),
            mostread_items.len(),
            trimmed.len()
        );

        // Build UI hint from combined news + trending items
        let mut ui_items: Vec<serde_json::Value> = news_items
            .iter()
            .map(|item| {
                serde_json::json!({
                    "headline": item.trim_start_matches("**Story**: "),
                    "tag": "World",
                    "timeAgo": "Today",
                    "source": "Wikipedia",
                })
            })
            .collect();
        for item in &mostread_items {
            ui_items.push(serde_json::json!({
                "headline": item.trim_start_matches("**").split("**").next().unwrap_or(item),
                "tag": "Trending",
                "timeAgo": "Today",
                "source": "Wikipedia",
            }));
        }

        if !ui_items.is_empty() {
            let ui_data = serde_json::json!({ "items": ui_items });
            let hint = format!("[[[mcp-ui:news:{}]]]\n", ui_data);
            let full_result = format!("{}{}", hint, trimmed);
            return Ok(CallToolResult::success(vec![Content::text(full_result)]));
        }

        Ok(CallToolResult::success(vec![Content::text(trimmed)]))
    }

    /// Fetch today's Wikimedia Featured Content Feed.
    /// Returns the parsed JSON body or a formatted error string.
    async fn fetch_wikimedia_feed(&self) -> Result<serde_json::Value, String> {
        let now = chrono::Utc::now();
        let url = format!(
            "{}/{}/{:02}/{:02}",
            WIKIMEDIA_FEED_URL,
            now.format("%Y"),
            now.format("%m"),
            now.format("%d"),
        );
        eprintln!("[news] GET {} (Wikimedia feed)", url);

        let resp = crate::http::traced_get_with(&self.http_client, &url, |b| {
            b.header(
                "user-agent",
                "goose-in-a-pond/0.1 (GIAP MCP; https://github.com/jarida-io/goose-in-a-pond)",
            )
            .timeout(std::time::Duration::from_secs(10))
        })
        .await
        .map_err(|e| {
            eprintln!("[news] Wikimedia feed request failed: {e}");
            crate::format::format_api_error("Wikipedia (news feed)", &e.to_string())
        })?;

        if !resp.status().is_success() {
            let status = resp.status();
            eprintln!("[news] Wikimedia feed returned HTTP {status}");
            return Err(crate::format::format_api_error(
                "Wikipedia (news feed)",
                &format!("HTTP {status}"),
            ));
        }

        resp.json::<serde_json::Value>().await.map_err(|e| {
            eprintln!("[news] failed to parse Wikimedia feed response: {e}");
            crate::format::format_api_error("Wikipedia (news feed)", &e.to_string())
        })
    }
}

/// Format a single Wikimedia news story into a display string.
/// Each story has `story` (HTML) and `links` (related articles).
fn format_wikimedia_story(story: &serde_json::Value) -> Option<String> {
    let html = story["story"].as_str()?;
    let clean = strip_html_tags(html);
    if clean.trim().is_empty() {
        return None;
    }

    let related: Vec<&str> = story["links"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .take(3)
                .filter_map(|link| link["titles"]["normalized"].as_str())
                .collect()
        })
        .unwrap_or_default();

    if related.is_empty() {
        Some(format!("**Story**: {}", clean.trim()))
    } else {
        Some(format!(
            "**Story**: {} (Related: {})",
            clean.trim(),
            related.join(", "),
        ))
    }
}

/// Strip HTML tags from a string. Simple char-by-char filter — no external dependency.
fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(c),
            _ => {}
        }
    }
    result
}

/// Resolve the search query using the standard param fallback chain.
const NEWS_QUERY_SCHEMA: &str = r#"{"type":"object","properties":{"query":{"type":"string","description":"Keywords to search for in news articles"}},"required":["query"]}"#;

async fn resolve_query(params: &SearchNewsParams) -> String {
    // 1. ToolCaller specialist — PRIMARY when configured
    if let Some(args) = crate::generate_params("search_news", NEWS_QUERY_SCHEMA).await {
        if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
            let trimmed = q.trim();
            if !trimmed.is_empty() {
                eprintln!("[news] resolve_query: ToolCaller produced: {:?}", trimmed);
                return trimmed.to_string();
            }
        }
    }

    // 2. Model params — direct query field
    if let Some(ref q) = params.query {
        let trimmed = q.trim();
        if !trimmed.is_empty() {
            eprintln!("[news] resolve_query: model param 'query': {:?}", trimmed);
            return trimmed.to_string();
        }
    }

    // 3. Scan extras for common synonyms
    for key in &[
        "query", "q", "search", "topic", "keywords", "term", "text", "subject",
    ] {
        if let Some(val) = params.extra.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    eprintln!("[news] resolve_query: extras '{}': {:?}", key, trimmed);
                    return trimmed.to_string();
                }
            }
        }
    }

    // 4. Last resort: clean user message
    let msg = crate::last_user_message();
    if !msg.is_empty() {
        let cleaned = crate::clean_query_for_search(&msg);
        if !cleaned.is_empty() {
            eprintln!(
                "[news] resolve_query: user message: {:?} -> {:?}",
                msg, cleaned
            );
            return cleaned;
        }
    }

    eprintln!("[news] resolve_query: no query found");
    String::new()
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct NewsDeps {
    http_client: reqwest::Client,
}

static NEWS_DEPS: OnceLock<NewsDeps> = OnceLock::new();

/// Initialize news server dependencies. Call once at startup.
///
/// No settings repository: the Guardian and GNews keys come from the secret
/// store, installed separately by `init_secret_deps` (PAI-2 P2).
pub fn init_news_deps(http_client: reqwest::Client) {
    let _ = NEWS_DEPS.set(NewsDeps { http_client });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_news_server(reader: DuplexStream, writer: DuplexStream) {
    // Missing deps = this path never initialised this extension (the voice/CLI
    // binary vs `serve` install different families). A skipped extension is a
    // logged, contained failure; a panic here took down every builtin server's
    // startup at once (2026-08-27, giap-context in the voice child).
    let Some(deps) = NEWS_DEPS.get() else {
        tracing::error!(
            "spawn_news_server called before init_news_deps — extension will not start"
        );
        return;
    };
    let server = NewsMcpServer::new(deps.http_client.clone());
    crate::serve_builtin("giap-news", server, reader, writer);
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // resolve_query tests — no ToolCaller set, so falls through to model params
    #[tokio::test]
    async fn resolve_query_from_direct_field() {
        let params = SearchNewsParams {
            query: Some("climate change".to_string()),
            section: None,
            limit: None,
            extra: Default::default(),
        };
        assert_eq!(resolve_query(&params).await, "climate change");
    }

    #[tokio::test]
    async fn resolve_query_from_extras() {
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "q".to_string(),
            serde_json::Value::String("AI regulation".to_string()),
        );
        let params = SearchNewsParams {
            query: None,
            section: None,
            limit: None,
            extra,
        };
        assert_eq!(resolve_query(&params).await, "AI regulation");
    }

    #[tokio::test]
    async fn resolve_query_empty_when_nothing_provided() {
        let params = SearchNewsParams {
            query: None,
            section: None,
            limit: None,
            extra: Default::default(),
        };
        assert_eq!(resolve_query(&params).await, "");
    }

    #[tokio::test]
    #[ignore] // requires internet + GIAP_GUARDIAN_KEY env var
    async fn live_guardian_search() {
        let key = match std::env::var("GIAP_GUARDIAN_KEY") {
            Ok(k) if !k.is_empty() => k,
            _ => {
                eprintln!("skipping: GIAP_GUARDIAN_KEY not set");
                return;
            }
        };
        let client = reqwest::Client::new();
        let url = format!(
            "{}/search?q=technology&api-key={}&show-fields=trailText&page-size=3",
            GUARDIAN_BASE_URL, key,
        );
        let resp = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .expect("Guardian request failed");
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp
            .json()
            .await
            .expect("failed to parse Guardian response");
        let results = body["response"]["results"].as_array();
        assert!(results.is_some(), "Guardian should return results array");
        eprintln!("Guardian returned {} results", results.unwrap().len());
    }

    // ── strip_html_tags tests ───────────────────────────────────────────

    #[test]
    fn strip_html_tags_removes_basic_tags() {
        assert_eq!(strip_html_tags("<b>bold</b> text"), "bold text");
        assert_eq!(strip_html_tags("no tags"), "no tags");
        assert_eq!(strip_html_tags("<a href=\"url\">link</a>"), "link");
    }

    #[test]
    fn strip_html_tags_handles_empty_and_nested() {
        assert_eq!(strip_html_tags(""), "");
        assert_eq!(strip_html_tags("<div><p>nested</p></div>"), "nested");
        assert_eq!(
            strip_html_tags("<span class=\"x\">styled</span> and plain"),
            "styled and plain"
        );
    }

    #[test]
    fn strip_html_tags_preserves_entities() {
        // HTML entities are not tags — they pass through
        assert_eq!(strip_html_tags("A &amp; B"), "A &amp; B");
    }

    // ── format_wikimedia_story tests ────────────────────────────────────

    #[test]
    fn format_wikimedia_story_with_links() {
        let story = serde_json::json!({
            "story": "<b>Breaking</b>: Something <a href=\"x\">happened</a> today.",
            "links": [
                { "titles": { "normalized": "Event A" } },
                { "titles": { "normalized": "Event B" } },
            ]
        });
        let result = format_wikimedia_story(&story).unwrap();
        assert!(result.contains("Breaking: Something happened today."));
        assert!(result.contains("Related: Event A, Event B"));
    }

    #[test]
    fn format_wikimedia_story_without_links() {
        let story = serde_json::json!({
            "story": "Plain story text.",
            "links": []
        });
        let result = format_wikimedia_story(&story).unwrap();
        assert_eq!(result, "**Story**: Plain story text.");
    }

    #[test]
    fn format_wikimedia_story_returns_none_for_empty() {
        let story = serde_json::json!({ "story": "<br/>" });
        // After stripping HTML, only whitespace remains
        assert!(format_wikimedia_story(&story).is_none());
    }

    // ── Live Wikimedia feed test ────────────────────────────────────────

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_wikimedia_feed_returns_news() {
        let client = reqwest::Client::builder()
            .user_agent("goose-in-a-pond/0.1 (GIAP MCP)")
            .build()
            .unwrap();
        let now = chrono::Utc::now();
        let url = format!(
            "{}/{}/{:02}/{:02}",
            WIKIMEDIA_FEED_URL,
            now.format("%Y"),
            now.format("%m"),
            now.format("%d"),
        );
        let resp = client
            .get(&url)
            .send()
            .await
            .expect("Wikimedia request failed");
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.expect("parse failed");
        let news = body["news"].as_array().expect("news array missing");
        assert!(!news.is_empty(), "should have at least one news item");
        eprintln!("Found {} news stories", news.len());
    }
}
